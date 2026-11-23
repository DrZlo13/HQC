//! Publishing a page or a file by a public link (SPEC §7 `hqc_publish`, §10.1, §12; since
//! 0.209).
//!
//! - The worker packs the folder or the file into a zip and uploads it as an artifact of kind
//!   `site` for the calling session; the call carries that artifact and the canonical path it
//!   packed ([`PublishCall`]).
//! - [`tool`]: without `HQC_PUBLISH_BASE` the call is `unavailable` before anything else. The
//!   artifact must be the owner's, uploaded for this session, a `site` not published yet; its
//!   archive is opened with limits ([`inspect`]). Then the owner is asked, always, whatever the
//!   session's mode and `auto_approve`: a PendingRequest of kind [`KIND`] in the calling
//!   session with the hub's `approval.request` in its feed (the path, the files, that the link
//!   is public; "Publish" / "Deny"), one at a time per session. The call is deferred as
//!   `hqc_ask` is. Only the session owner answers it ([`crate::requests::answer`]); stop,
//!   archive, the agent stopping, `mcp.cancel` (the tool timeout) and a worker restart cancel
//!   it as they cancel a move ([`crate::moves`]).
//! - The owner allows it ([`allowed`]): once per `request_id` ([`crate::mcp::once_id`]) the
//!   artifact gets `published_at` and a random `public_id`, the feed an `attachment` of kind
//!   `site`, and the call `{url, event_id}`; a result that is not stored is stored again after
//!   a pause. A hub that stopped before it stored the result carries the publication out again
//!   right after it starts ([`left_on_start`]). Deny or a cancellation: `unavailable`.
//! - [`serve`]: on the host of `HQC_PUBLISH_BASE` the hub serves only `/<public_id>/<path>`
//!   out of the archive (and `/health`, [`crate::security::guard`]): a path ending in `/` is
//!   its `index.html`; anything else, or an artifact no longer published, is `404`.

use std::{
    collections::HashMap,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use hqc_proto::{
    event::CancelReason,
    frame::{worker_approval, McpCall, McpError, McpResult},
    mcp::{error_code, PublishCall, MAX_SITE_FILES, MAX_SITE_NAME, MAX_SITE_UNPACKED, PUBLISH},
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    artifacts::{self, ArtifactRow},
    config::ArtifactConfig,
    db::{self, PendingRow, SessionRow},
    events,
    mcp::{args, internal, once_id, Answer},
    requests,
};

/// `kind` of the PendingRequest of an `hqc_publish` that waits for the owner (SPEC §3).
pub const KIND: &str = "publish";
/// The page a published folder opens with, and a path ending in `/` names.
pub const INDEX: &str = "index.html";
/// Files the owner's card names; the rest are counted.
pub const MAX_LISTED: usize = 50;
/// Longest `path` of the call shown on the card, in bytes.
const MAX_PATH: usize = 4096;
/// The longest pause before the result of an allowed publication is stored again.
const MAX_STORE_PAUSE: Duration = Duration::from_secs(60);

/// Content-Security-Policy of a published page (SPEC §12): the artifact domain's
/// [`artifacts::CSP_HTML`] with `'self'` beside each source, and `'wasm-unsafe-eval'` so that
/// the page may compile its WebAssembly. The page's own styles, scripts, images, fonts,
/// modules, `fetch` and wasm load from its host: the sandbox gives the page no origin of its
/// own, so its requests to its host are cross-origin, and [`serve`] answers them with
/// `Access-Control-Allow-Origin: *`. Nothing loads from anywhere else. A sandbox without
/// `allow-same-origin`, `allow-popups` or `allow-forms`: Web Workers, `target=_blank` and
/// popups, form submission, cookies and `localStorage` do not work; links open in the same
/// tab.
pub const CSP_SITE: &str = "sandbox allow-scripts; default-src 'self'; \
                            script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; \
                            style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                            font-src 'self' data:";
/// Published files streamed at once: each holds a blocking thread while its reader takes it
/// (SPEC §20); one more is `503` with `Retry-After: 5`.
pub const MAX_STREAMS: usize = 32;
/// `Cache-Control` of published content: short, so a publication that is removed does not live
/// on in caches; the `ETag` lets a cache check it again cheaply.
pub const CACHE: &str = "public, max-age=300";

fn invalid(m: impl Into<String>) -> McpError {
    McpError::new(error_code::INVALID_ARGS, m)
}

fn unavailable(m: impl Into<String>) -> McpError {
    McpError::new(error_code::UNAVAILABLE, m)
}

/// `hqc_publish(path, caption?)` as the worker sends it (SPEC §7): `{artifact_id, path,
/// caption?}`. Refused at once, before any card: without `HQC_PUBLISH_BASE`; a path the card
/// cannot show ([`crate::moves::unshowable`]); an artifact that is not an unpublished `site` of
/// the owner uploaded for this session (`not_found`); an archive over the limits
/// ([`inspect`], `invalid_args`); while another publication of the session waits. Otherwise the
/// owner is asked ([`ask_owner`]) and the call is answered later. A repeat finds the request it
/// opened: still waiting, or its stored outcome.
pub async fn tool(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: PublishCall = args(c.args.clone())?;
    if app.config.artifacts.publish_base.is_none() {
        return Err(unavailable(
            "publishing is not set up on this hub (HQC_PUBLISH_BASE is not set)",
        ));
    }
    if let Some(p) = db::request_by_id(&app.db, caller.id, KIND, &c.request_id)
        .await
        .map_err(internal)?
    {
        // A repeat of a call that asked the owner (SPEC §5.2): waiting, or its outcome.
        return match p.result {
            None => Ok(Answer::Later),
            Some(frame) => {
                crate::peers::result_acked(app, p.id)
                    .await
                    .map_err(internal)?;
                let r: McpResult =
                    serde_json::from_value(frame["payload"].clone()).map_err(internal)?;
                r.outcome().map(Answer::Now)
            }
        };
    }
    if a.path.is_empty() || a.path.len() > MAX_PATH || crate::moves::unshowable(&a.path) {
        return Err(invalid(format!(
            "path must be 1-{MAX_PATH} bytes without control characters or bidirectional controls"
        )));
    }
    let art = artifacts::artifact(&app.db, a.artifact_id)
        .await
        .map_err(internal)?
        .filter(|r| r.owner_id == caller.owner_id && r.kind == "site" && r.public_id.is_none());
    let linked = match &art {
        Some(r) => artifacts::of_session(&app.db, caller.id, r.id)
            .await
            .map_err(internal)?,
        None => false,
    };
    let art = art.filter(|_| linked).ok_or_else(|| {
        McpError::new(
            error_code::NOT_FOUND,
            format!(
                "no unpublished site {} uploaded for this session",
                a.artifact_id
            ),
        )
    })?;
    let site = inspect_blob(&app.config.artifacts, &art)
        .await?
        .map_err(|why| invalid(format!("the archive of {}: {why}", a.path)))?;
    ask_owner(app, caller, &c.request_id, &a, &art, &site).await?;
    Ok(Answer::Later)
}

/// What a published archive holds ([`inspect`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The files (no folder entries) and their sizes unpacked, in the archive's order.
    pub files: Vec<(String, u64)>,
    pub unpacked: u64,
    /// `None`: a folder, its link opens `index.html`; a name: the archive of that one file.
    pub entry: Option<String>,
}

/// The archive of `a`, read off the connection's loop; `Err` inside: why it is refused.
async fn inspect_blob(
    cfg: &ArtifactConfig,
    a: &ArtifactRow,
) -> Result<Result<Site, String>, McpError> {
    let path = artifacts::blob_path(cfg, &a.storage_key);
    tokio::task::spawn_blocking(move || inspect(&path))
        .await
        .map_err(internal)
}

/// The most a central directory of [`MAX_SITE_FILES`] records may take: 46 bytes of header and
/// the name of each (a record carries no extra field and no comment).
const MAX_DIRECTORY: u64 = (MAX_SITE_FILES * (46 + MAX_SITE_NAME)) as u64;

/// The refusal for an end record or a directory that is not the plain one a worker writes.
const NOT_PLAIN: &str =
    "its end record and central directory are not the plain ones a worker writes";

/// Checks the end record and the central directory of the archive at `path` before the zip
/// reader sees them, and returns the number of records in the directory and where it starts.
/// The reader takes every record the end record declares into memory, keeps one per name, and
/// copies a record's whole extra field once per zip64 block in it (a 64 KB field of empty
/// blocks costs it about a gigabyte of copying); when it fails on a directory it searches the
/// file for an earlier end record and reads that one's directory instead. Whatever a worker
/// uploads is checked here first, and the same reader then runs on every request for a
/// published link.
///
/// The end record must be the last 22 bytes of the file (no comment, one disk, no zip64: the
/// reader's search for it then finds this one, and never takes the zip64 path), the directory
/// must end right where it starts and be no larger than [`MAX_DIRECTORY`], and each of its
/// records must have a name of at most [`MAX_SITE_NAME`] bytes, no extra field, no comment and
/// a stored or deflated method: the reader then has nothing in the directory to fail on, as an
/// archive the worker packs has nothing of the kind.
fn check_directory(path: &Path) -> Result<(usize, u64), String> {
    const END: u64 = 22;
    let mut f = std::fs::File::open(path).map_err(|e| format!("cannot open it: {e}"))?;
    let len = f
        .metadata()
        .map_err(|e| format!("cannot read it: {e}"))?
        .len();
    let not_zip = |e: io::Error| format!("not a zip archive: {e}");
    if len < END {
        return Err("not a zip archive: shorter than an end record".into());
    }
    f.seek(SeekFrom::Start(len - END)).map_err(not_zip)?;
    let mut end = [0u8; END as usize];
    f.read_exact(&mut end).map_err(not_zip)?;
    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let u32_at =
        |b: &[u8], at: usize| u32::from(u16_at(b, at)) | u32::from(u16_at(b, at + 2)) << 16;
    if end[..4] != [b'P', b'K', 5, 6] {
        return Err("not a zip archive: no end record".into());
    }
    let (entries, total) = (usize::from(u16_at(&end, 8)), usize::from(u16_at(&end, 10)));
    if total > MAX_SITE_FILES {
        return Err(format!("more than {MAX_SITE_FILES} entries"));
    }
    let (size, start) = (u64::from(u32_at(&end, 12)), u64::from(u32_at(&end, 16)));
    if u16_at(&end, 4) != 0
        || u16_at(&end, 6) != 0
        || entries != total
        || u16_at(&end, 20) != 0
        || start.checked_add(size) != Some(len - END)
        || size > MAX_DIRECTORY
    {
        return Err(NOT_PLAIN.into());
    }
    f.seek(SeekFrom::Start(start)).map_err(not_zip)?;
    let mut f = BufReader::new(f);
    let mut head = [0u8; 46];
    for _ in 0..total {
        if f.read_exact(&mut head).is_err() || head[..4] != [b'P', b'K', 1, 2] {
            return Err(NOT_PLAIN.into());
        }
        let (name, extra, comment) = (
            usize::from(u16_at(&head, 28)),
            usize::from(u16_at(&head, 30)),
            usize::from(u16_at(&head, 32)),
        );
        if name > MAX_SITE_NAME {
            return Err(format!("a name is over {MAX_SITE_NAME} bytes"));
        }
        if extra != 0 || comment != 0 {
            return Err("a record has an extra field or a comment".into());
        }
        if !matches!(u16_at(&head, 10), 0 | 8) {
            return Err("a record is neither stored nor deflated".into());
        }
        f.seek_relative(name as i64).map_err(not_zip)?;
    }
    match f.stream_position() {
        Ok(at) if at == start + size => Ok((total, start)),
        _ => Err(NOT_PLAIN.into()),
    }
}

/// Opens a published archive with the limits of SPEC §20: a zip of at most [`MAX_SITE_FILES`]
/// entries, at most [`MAX_SITE_UNPACKED`] bytes unpacked together (a zip bomb stops here),
/// names of at most [`MAX_SITE_NAME`] bytes, relative, without `.` or `..` components, without
/// control characters or bidirectional controls (they are shown on the card), each once;
/// stored or deflated, not encrypted, no links; `index.html` at the top, or a single file.
/// The end record and the directory are checked first ([`check_directory`]), so the zip reader
/// only ever reads a bounded, plain directory; a reader that opened a directory starting
/// elsewhere is refused, and as it keeps one record per name, fewer entries than records means
/// a name is in the directory more than once.
pub fn inspect(path: &Path) -> Result<Site, String> {
    let (records, start) = check_directory(path)?;
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open it: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("not a zip archive: {e}"))?;
    if z.central_directory_start() != start {
        return Err(NOT_PLAIN.into());
    }
    if z.len() != records {
        return Err("a name is in it more than once".into());
    }
    let (mut files, mut unpacked) = (Vec::new(), 0u64);
    for i in 0..z.len() {
        let e = z.by_index_raw(i).map_err(|e| format!("entry {i}: {e}"))?;
        let name = e
            .name()
            .map_err(|e| format!("entry {i}: {e}"))?
            .into_owned();
        if e.name_raw().len() > MAX_SITE_NAME || name.len() > MAX_SITE_NAME {
            return Err(format!("a name is over {MAX_SITE_NAME} bytes"));
        }
        check_name(&name, e.is_dir())?;
        if e.encrypted() || e.is_symlink() {
            return Err(format!("{name} is encrypted or a link"));
        }
        if !matches!(
            e.compression(),
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        ) {
            return Err(format!("{name} is neither stored nor deflated"));
        }
        if e.is_dir() {
            continue;
        }
        unpacked = unpacked.saturating_add(e.size());
        if unpacked > MAX_SITE_UNPACKED {
            return Err(format!("more than {MAX_SITE_UNPACKED} bytes unpacked"));
        }
        files.push((name, e.size()));
    }
    let entry = site_entry(files.iter().map(|(n, _)| n.as_str()))?;
    Ok(Site {
        files,
        unpacked,
        entry,
    })
}

/// A name in a published archive: relative, its components neither empty nor `.` nor `..` (a
/// folder entry ends with `/`), showable.
fn check_name(name: &str, dir: bool) -> Result<(), String> {
    let body = if dir {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    if body.is_empty() || body.starts_with('/') {
        return Err(format!("{name:?} is not a relative path"));
    }
    if body
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format!("{name:?} has an empty, `.` or `..` component"));
    }
    if crate::moves::unshowable(name) {
        return Err(format!(
            "{} has control characters or bidirectional controls",
            name.escape_debug()
        ));
    }
    Ok(())
}

/// What the link of an archive with these files opens: `None` for a folder with `index.html`,
/// the name of the one file of an archive of a file; otherwise the archive is refused.
fn site_entry<'a>(names: impl Iterator<Item = &'a str>) -> Result<Option<String>, String> {
    let files: Vec<&str> = names.filter(|n| !n.ends_with('/')).collect();
    if files.contains(&INDEX) {
        return Ok(None);
    }
    match files[..] {
        [one] if !one.contains('/') => Ok(Some(one.to_string())),
        _ => Err(format!(
            "a published folder opens with {INDEX}, and it has none"
        )),
    }
}

/// `1.2 KB`, as the client writes sizes.
fn size_text(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    let units = ["KB", "MB", "GB"];
    while v >= 1024.0 && unit < units.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", units[unit])
    } else {
        format!("{} {}", v.round(), units[unit])
    }
}

/// The folder of `name` up to its first hidden component (one starting with `.`) when that
/// component is a folder, not the file itself: `a/.git/x` → `a/.git`, `a/.env` → `None`.
fn hidden_dir(name: &str) -> Option<&str> {
    let mut end = 0;
    for c in name.split('/') {
        end += c.len();
        if c.starts_with('.') {
            return (end < name.len()).then_some(&name[..end]);
        }
        end += 1;
    }
    None
}

/// The owner's card (SPEC §10.1): what is published, that the link is public, the files: the
/// first [`MAX_LISTED`] that are not hidden, every hidden file (a name starting with `.`), and
/// every hidden folder as one line with its count, so that a `.env` cannot hide in the count of
/// the rest and a `.git` does not fill the card.
pub fn card(path: &str, site: &Site) -> String {
    let n = site.files.len();
    let mut d = format!(
        "The agent asks to publish {path} at a public link: anyone who has the link can open \
         it, without signing in to hqc, until you unpublish it or delete this session.\n\
         {n} {}, {}:",
        if n == 1 { "file" } else { "files" },
        size_text(site.unpacked)
    );
    // A line per named file or per hidden folder (an index into `dirs`), in the archive's order.
    enum Line<'a> {
        File(&'a str),
        Dir(usize),
    }
    let mut lines = Vec::new();
    let mut dirs: Vec<(&str, usize)> = Vec::new();
    let mut dir_line: HashMap<&str, usize> = HashMap::new();
    let (mut regular, mut shown) = (0, 0);
    for (name, _) in &site.files {
        let name = name.as_str();
        if let Some(dir) = hidden_dir(name) {
            let i = *dir_line.entry(dir).or_insert_with(|| {
                lines.push(Line::Dir(dirs.len()));
                dirs.push((dir, 0));
                dirs.len() - 1
            });
            dirs[i].1 += 1;
        } else if name.rsplit('/').next().is_some_and(|f| f.starts_with('.')) {
            lines.push(Line::File(name));
        } else if regular < MAX_LISTED {
            regular += 1;
            lines.push(Line::File(name));
        } else {
            continue;
        }
        shown += 1;
    }
    for line in &lines {
        d.push('\n');
        match line {
            Line::File(name) => d.push_str(name),
            Line::Dir(i) => {
                let (dir, k) = dirs[*i];
                d.push_str(&format!(
                    "{dir}/ ({k} {})",
                    if k == 1 { "file" } else { "files" }
                ));
            }
        }
    }
    if n > shown {
        d.push_str(&format!("\nand {} more", n - shown));
    }
    d
}

/// The owner is asked about the publication (SPEC §7, §10.1): an `approval.request` of the hub
/// in the session's feed with the card and the options `once` ("Publish") / `deny`, and an open
/// request of kind [`KIND`] keyed by the call's `request_id`, which keeps what the publication
/// needs (`publish`: the artifact, the caption, what the link opens). No expiry; a repeat of
/// the call opens none and stores no second event. One card per session: while a request of
/// another call waits, the call is `unavailable`.
async fn ask_owner(
    app: &AppState,
    s: &SessionRow,
    request_id: &str,
    a: &PublishCall,
    art: &ArtifactRow,
    site: &Site,
) -> Result<(), McpError> {
    let payload = json!({
        "request_id": request_id,
        "tool": PUBLISH,
        "description": card(&a.path, site),
        "options": [
            {"id": worker_approval::ONCE, "label": "Publish", "canonical": "once"},
            {"id": worker_approval::DENY, "label": "Deny", "canonical": "deny"},
        ],
    });
    let caption = a.caption.as_deref().filter(|c| !c.trim().is_empty());
    let mut stored = payload.clone();
    stored["publish"] = json!({"artifact_id": art.id, "caption": caption, "entry": site.entry});
    hqc_proto::replace_nul_json(&mut stored);
    {
        // Held until the request is stored: parallel calls of one session open one card.
        let _lock = app.owner_lock("publish request", s.owner_id).await;
        let open = db::open_requests_of_session(&app.db, s.id)
            .await
            .map_err(internal)?;
        if open
            .iter()
            .any(|p| p.kind == KIND && p.request_id != request_id)
        {
            return Err(unavailable(
                "a publication already waits for the user's answer",
            ));
        }
        if !db::open_request(&app.db, KIND, s.id, request_id, &stored, None, s.worker_id)
            .await
            .map_err(internal)?
        {
            return Ok(());
        }
    }
    tracing::info!(session_id = %s.id, %request_id, artifact_id = %art.id, files = site.files.len(), "hqc_publish asks the owner");
    let mut payload = payload;
    hqc_proto::replace_nul_json(&mut payload);
    events::store_hub_event(
        app,
        s,
        json!({"kind": "system"}),
        "approval.request",
        payload,
        None,
        None,
    )
    .await
    .map_err(internal)?;
    Ok(())
}

/// The owner allowed the publication of request `p` (closed already, without a result): it is
/// carried out ([`carry_out`]) off the answer's HTTP request, and the call gets the outcome as
/// an `mcp.result` of its own, kept in the request until the worker acknowledges it. A result
/// that is not stored is stored again after a pause, a second at first, doubled each time up to
/// [`MAX_STORE_PAUSE`].
pub fn allowed(app: &AppState, p: PendingRow) {
    let app = app.clone();
    tokio::spawn(async move {
        let r = match carry_out(&app, &p).await {
            Ok(v) => McpResult::ok(&p.request_id, v),
            Err(e) => McpResult::err(&p.request_id, &e.code, e.message),
        };
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, ok = r.error.is_none(), "allowed hqc_publish carried out");
        let frame = json!({"type": "mcp.result", "payload": r});
        let mut pause = Duration::from_secs(1);
        while let Err(e) = db::set_request_result(&app.db, p.id, &frame).await {
            tracing::error!(request_id = %p.request_id, error = %e, retry_in_s = pause.as_secs(), "the result of an allowed publication not stored; storing it again");
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(MAX_STORE_PAUSE);
        }
        requests::deliver_result(
            &app,
            &PendingRow {
                result: Some(frame),
                ..p
            },
        )
        .await;
    });
}

/// On hub start, before it serves anything (SPEC §3 PendingRequest): the publications the owner
/// allowed (the request carries the owner's mark, [`db::close_allowed`]) whose result the
/// previous process did not store are carried out again at once ([`allowed`]); one done before
/// the stop answers as a repeat. The result reaches the worker when it is connected, or after
/// its `hello`.
pub async fn left_on_start(app: &AppState) {
    let rows = match db::allowed_without_result(&app.db, KIND).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "allowed publications without a result");
            return;
        }
    };
    for p in rows {
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, "an allowed hqc_publish carried out after the hub restarted");
        allowed(app, p);
    }
}

/// Publishes the artifact of request `p` once: `published_at` and a `public_id` (kept when it
/// has one: a repeat after a restart), then the `attachment` event of kind `site` authored by
/// the agent, whose id is [`once_id`] of the call; `{url, event_id}`. A publication whose
/// event is stored already answers from it.
async fn carry_out(app: &AppState, p: &PendingRow) -> Result<Value, McpError> {
    let pp = &p.payload["publish"];
    let artifact_id: Uuid = serde_json::from_value(pp["artifact_id"].clone()).map_err(internal)?;
    let entry: Option<String> = serde_json::from_value(pp["entry"].clone()).map_err(internal)?;
    let base = app
        .config
        .artifacts
        .publish_base
        .clone()
        .ok_or_else(|| unavailable("publishing is not set up on this hub any more"))?;
    let s = db::session(&app.db, p.session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| unavailable("the session is gone"))?;
    let event_id = once_id(PUBLISH, s.id, &p.request_id);
    if let Some(row) = db::event(&app.db, event_id).await.map_err(internal)? {
        // Done before (a repeat after a restart): the link as it is now.
        let a = artifacts::artifact(&app.db, artifact_id)
            .await
            .map_err(internal)?;
        let id = a
            .and_then(|a| a.public_id)
            .ok_or_else(|| unavailable("the user has unpublished it since"))?;
        return Ok(json!({"url": url(&base, &id, entry.as_deref()), "event_id": row.event_id}));
    }
    let id = {
        let mut b = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    // `session_id` is the publishing session, whatever an input may have set before: when that
    // session goes, by any path, it is set to null and the publication is taken down with it
    // (SPEC §12).
    let id: Option<String> = sqlx::query_scalar(
        "UPDATE artifacts SET published_at = coalesce(published_at, now()),
                              public_id = coalesce(public_id, $3), session_id = $4,
                              updated_at = now()
         WHERE id = $1 AND owner_id = $2 AND kind = 'site' RETURNING public_id",
    )
    .bind(artifact_id)
    .bind(s.owner_id)
    .bind(&id)
    .bind(s.id)
    .fetch_optional(&app.db)
    .await
    .map_err(internal)?
    .flatten();
    let id = id.ok_or_else(|| unavailable("the archive to publish is gone"))?;
    let a = artifacts::artifact(&app.db, artifact_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| unavailable("the archive to publish is gone"))?;
    let mut payload = json!({"artifact_id": a.id, "kind": "site", "name": a.name});
    if let Some(c) = pp["caption"].as_str().filter(|c| !c.trim().is_empty()) {
        payload["caption"] = json!(c);
    }
    let author = json!({"kind": "agent", "id": s.id});
    let row = match events::store_hub_event_once(app, &s, event_id, author, "attachment", payload)
        .await
        .map_err(internal)?
    {
        Some(row) => row,
        None => db::event(&app.db, event_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("the publication's attachment vanished"))?,
    };
    tracing::info!(session_id = %s.id, artifact_id = %a.id, "published");
    Ok(json!({"url": url(&base, &id, entry.as_deref()), "event_id": row.event_id}))
}

/// `<base>/<public_id>/`, or `<base>/<public_id>/<name>` for the archive of one file.
fn url(base: &str, public_id: &str, entry: Option<&str>) -> String {
    format!(
        "{base}/{public_id}/{}",
        entry.map(artifacts::pct).unwrap_or_default()
    )
}

/// The link of a published `site` for `GET /api/artifacts/{id}`; `None` while it is not
/// published or its publishing session is gone (it is not served then, [`serve`]), on a hub
/// without `HQC_PUBLISH_BASE`, or when its archive cannot be read.
pub async fn public_url(app: &AppState, a: &ArtifactRow) -> Option<String> {
    let (Some(id), Some(_), Some(base)) = (
        &a.public_id,
        a.session_id,
        &app.config.artifacts.publish_base,
    ) else {
        return None;
    };
    let path = artifacts::blob_path(&app.config.artifacts, &a.storage_key);
    let entry = tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        let names = z
            .file_names()
            .map(|n| n.map(std::borrow::Cow::into_owned))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        site_entry(names.iter().map(String::as_str))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    match entry {
        Ok(entry) => Some(url(base, id, entry.as_deref())),
        Err(e) => {
            tracing::warn!(artifact_id = %a.id, "the archive of a publication: {e}");
            None
        }
    }
}

/// The mcp.result of a publication the owner denied: nothing is published.
pub fn denied(request_id: &str) -> Value {
    json!({"type": "mcp.result", "payload": McpResult::err(
        request_id,
        error_code::UNAVAILABLE,
        "the user did not allow the publication; nothing is published",
    )})
}

/// The `mcp.result` of a publication whose request the hub cancelled (SPEC §10.1).
pub fn cancelled(request_id: &str, reason: CancelReason) -> Value {
    let why = serde_json::to_value(reason).unwrap_or_default();
    json!({"type": "mcp.result", "payload": McpResult::err(
        request_id,
        error_code::UNAVAILABLE,
        format!(
            "the request to the user to allow the publication was cancelled ({}); nothing is published",
            why.as_str().unwrap_or("cancelled")
        ),
    )})
}

// ---------------------------------------------------------------- serving

/// The request came to the publication domain (`HQC_PUBLISH_BASE`).
pub fn is_publish_host(cfg: &ArtifactConfig, headers: &HeaderMap) -> bool {
    cfg.publish_host()
        .is_some_and(|want| artifacts::host_is(&want, headers))
}

/// 128 random bits as the hub writes them: 32 lowercase hex characters.
fn is_public_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `Content-Type` of a published file by its extension; anything unknown is
/// `application/octet-stream` (served with `nosniff`).
pub fn content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// Headers of every answer on the publication domain.
fn base_headers(h: &mut HeaderMap, csp: &'static str) {
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    h.insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

fn not_found() -> Response {
    let mut r = (StatusCode::NOT_FOUND, "not found").into_response();
    let h = r.headers_mut();
    base_headers(h, artifacts::CSP);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Every one of the [`MAX_STREAMS`] is taken.
fn busy() -> Response {
    let mut r = (StatusCode::SERVICE_UNAVAILABLE, "busy, try again").into_response();
    let h = r.headers_mut();
    base_headers(h, artifacts::CSP);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    r
}

/// `GET /<public_id>/<path>` on the publication domain (SPEC §12): the entry of a published
/// archive, streamed, with [`CSP_SITE`], [`CACHE`], `ETag` (the archive's sha256; `304` on a
/// matching `If-None-Match`), `nosniff`, `X-Robots-Tag: noindex`, `Referrer-Policy:
/// no-referrer` and `Access-Control-Allow-Origin: *` (the link is the capability; the page's
/// own requests come from its opaque origin). A bad id, an artifact not published, one whose
/// session is gone, a missing entry: `404`. All of [`MAX_STREAMS`] taken: `503`.
pub async fn serve(app: &AppState, req: Request) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let Some((id, inner)) = req
        .uri()
        .path()
        .strip_prefix('/')
        .and_then(|p| p.split_once('/'))
    else {
        return not_found();
    };
    if !is_public_id(id) {
        return not_found();
    }
    let mut name = artifacts::pct_decode(inner);
    if name.is_empty() || name.ends_with('/') {
        name.push_str(INDEX);
    }
    let a = match artifacts::published(&app.db, id).await {
        Ok(Some(a)) => a,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(error = %e, "publication lookup");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let etag = format!("\"{}\"", a.sha256);
    let fresh = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let path = artifacts::blob_path(&app.config.artifacts, &a.storage_key);
    let entry = if fresh {
        None
    } else {
        let Ok(permit) = app.publish_streams.clone().try_acquire_owned() else {
            tracing::warn!(artifact_id = %a.id, "every publication stream is taken");
            return busy();
        };
        match open_entry(path, name.clone(), permit).await {
            Ok(Some(e)) => Some(e),
            Ok(None) => return not_found(),
            Err(e) => {
                tracing::error!(artifact_id = %a.id, error = %e, "published entry");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    };
    let mut r = match entry {
        None => StatusCode::NOT_MODIFIED.into_response(),
        Some((size, body)) => {
            let mut r = body.into_response();
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&name)),
            );
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
            r
        }
    };
    let h = r.headers_mut();
    base_headers(h, CSP_SITE);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    r
}

/// The entry `name` of the archive at `path`, read on a blocking thread and streamed: its size
/// unpacked and the body, at most that many bytes. `None`: no such file in it. The thread holds
/// `permit` (one of [`MAX_STREAMS`]) until the stream ends or its reader goes away.
async fn open_entry(
    path: std::path::PathBuf,
    name: String,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<Option<(u64, Body)>> {
    use std::io::Read;
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<anyhow::Result<Option<u64>>>();
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let opened = (|| -> anyhow::Result<Option<zip::ZipArchive<std::fs::File>>> {
            let f = std::fs::File::open(&path)?;
            let z = zip::ZipArchive::new(f)?;
            Ok(z.index_for_name(&name).map(|_| z))
        })();
        let mut z = match opened {
            Ok(Some(z)) => z,
            Ok(None) => {
                let _ = meta_tx.send(Ok(None));
                return;
            }
            Err(e) => {
                let _ = meta_tx.send(Err(e));
                return;
            }
        };
        let mut e = match z.by_name(&name) {
            Ok(e) if !e.is_dir() => e,
            Ok(_) => {
                let _ = meta_tx.send(Ok(None));
                return;
            }
            Err(e) => {
                let _ = meta_tx.send(Err(e.into()));
                return;
            }
        };
        let size = e.size();
        if meta_tx.send(Ok(Some(size))).is_err() {
            return;
        }
        // No more than the archive says the file has: an entry that unpacks to more (a zip
        // bomb) is cut there, and the sizes counted by [`inspect`] hold.
        let mut r = (&mut e).take(size);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .blocking_send(Ok(Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    let Some(size) = meta_rx.await?? else {
        return Ok(None);
    };
    let stream =
        futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|c| (c, rx)) });
    Ok(Some((size, Body::from_stream(stream))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> tempfile_path::Path {
        let p = tempfile_path::Path::new();
        let f = std::fs::File::create(&p.0).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (n, b) in entries {
            z.start_file(*n, o).unwrap();
            z.write_all(b).unwrap();
        }
        z.finish().unwrap();
        p
    }

    /// A file removed when dropped.
    mod tempfile_path {
        pub struct Path(pub std::path::PathBuf);
        impl Path {
            pub fn new() -> Self {
                Self(std::env::temp_dir().join(format!("hqc-site-{}.zip", uuid::Uuid::new_v4())))
            }
        }
        impl Drop for Path {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }

    #[test]
    fn archives_are_opened_with_limits() {
        let ok = zip_of(&[("css/a.css", b"a"), ("index.html", b"<p>")]);
        let s = inspect(&ok.0).unwrap();
        assert_eq!(s.entry, None);
        assert_eq!(s.unpacked, 4);
        assert_eq!(
            s.files,
            [("css/a.css".to_string(), 1), ("index.html".to_string(), 3)]
        );
        let one = zip_of(&[("chart.png", b"png")]);
        assert_eq!(inspect(&one.0).unwrap().entry.as_deref(), Some("chart.png"));
        for (entries, why) in [
            (vec![("a.html", &b"a"[..]), ("b.html", b"b")], "index.html"),
            (vec![("sub/a.html", &b"a"[..])], "index.html"),
            (vec![("../x", &b"a"[..]), ("index.html", b"i")], "component"),
            (
                vec![("a/./b", &b"a"[..]), ("index.html", b"i")],
                "component",
            ),
            (
                vec![("/etc/x", &b"a"[..]), ("index.html", b"i")],
                "relative",
            ),
            (
                vec![("a\u{202e}lmth.exe", &b"a"[..]), ("index.html", b"i")],
                "bidirectional",
            ),
            (vec![("a\nb", &b"a"[..]), ("index.html", b"i")], "control"),
        ] {
            let z = zip_of(&entries);
            let e = inspect(&z.0).unwrap_err();
            assert!(e.contains(why), "{entries:?}: {e}");
        }
        let long = "a".repeat(MAX_SITE_NAME + 1);
        let z = zip_of(&[(long.as_str(), b"a"), ("index.html", b"i")]);
        assert!(inspect(&z.0).unwrap_err().contains("over 512 bytes"));
        let not = tempfile_path::Path::new();
        std::fs::write(&not.0, b"not a zip").unwrap();
        assert!(inspect(&not.0).unwrap_err().contains("not a zip"));
    }

    #[test]
    fn the_card_names_the_files() {
        let site = Site {
            files: vec![
                ("css/style.css".into(), 1),
                ("img/chart.png".into(), 2),
                ("index.html".into(), 3),
            ],
            unpacked: 31842,
            entry: None,
        };
        // As in the wire example of the card.
        let wire: Value = serde_json::from_str(include_str!(
            "../../hqc-proto/examples/wire/event.approval.request.publish.json"
        ))
        .unwrap();
        assert_eq!(
            card("/w/hqc/report", &site),
            wire["payload"]["description"].as_str().unwrap()
        );
        let many = Site {
            files: (0..60).map(|i| (format!("f{i}.html"), 1)).collect(),
            unpacked: 3 * 1024 * 1024,
            entry: None,
        };
        let d = card("/w/site", &many);
        assert!(d.contains("60 files, 3.0 MB:"), "{d}");
        assert!(d.contains("\nf49.html\nand 10 more"), "{d}");
        assert!(!d.contains("f50.html"), "{d}");
        // Hidden files are named wherever they are, a hidden folder is one line with its count,
        // and neither is counted in the rest.
        let mut hidden = many.clone();
        hidden.files[3].0 = ".htaccess".into();
        hidden.files[55].0 = ".env".into();
        hidden.files[57].0 = "assets/.git/HEAD".into();
        hidden.files[58].0 = "assets/.git/config".into();
        let d = card("/w/site", &hidden);
        assert!(
            d.contains("\nf50.html\n.env\nassets/.git/ (2 files)\nand 6 more"),
            "{d}"
        );
        assert!(
            d.contains("\n.htaccess\n") && !d.contains("f55.html") && !d.contains("config"),
            "{d}"
        );
        assert_eq!(hidden_dir("a/.b/.c"), Some("a/.b"));
        assert_eq!(hidden_dir("assets/.htaccess"), None);
        assert!(!card("/w/site", &many).contains(".env"));
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(1536), "1.5 KB");
    }

    #[test]
    fn links_and_types() {
        assert_eq!(
            url("https://v.example.net", "ab", None),
            "https://v.example.net/ab/"
        );
        assert_eq!(
            url("https://v.example.net", "ab", Some("отчёт 1.pdf")),
            "https://v.example.net/ab/%D0%BE%D1%82%D1%87%D1%91%D1%82%201.pdf"
        );
        assert!(is_public_id(&"0a".repeat(16)));
        assert!(!is_public_id(&"0A".repeat(16)));
        assert!(!is_public_id(&"0a".repeat(32)));
        assert_eq!(content_type("a/B.CSS"), "text/css; charset=utf-8");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("noext"), "application/octet-stream");
        let mut c = ArtifactConfig::new("x".into());
        let mut h = HeaderMap::new();
        h.insert(
            header::HOST,
            HeaderValue::from_static("View.example.net:443"),
        );
        assert!(!is_publish_host(&c, &h));
        c.publish_base = Some("https://view.example.net".into());
        assert!(is_publish_host(&c, &h));
        h.insert(header::HOST, HeaderValue::from_static("hqc.example.net"));
        assert!(!is_publish_host(&c, &h));
    }
}
