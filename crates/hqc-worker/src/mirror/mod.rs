//! Mirror and import of Claude Code sessions (SPEC §9).
//!
//! The hub names the mirrored workspaces with `mirror.set`; the mirror follows every
//! `<config>/projects/*/<uuid>.jsonl` whose working directory (from the records' `cwd`, kept
//! in [`store::Store`]) lies inside one of them, and turns new main-chain records into
//! `mirror.event`s. The first pass over a file is the import (SPEC §9.1): it starts at
//! offset 0 and sends the whole history with the records' own timestamps.
//!
//! Polling, every [`POLL`] (2 s), not file notifications. A poll is a `read_dir` of the
//! projects folder and a `stat` per transcript, and reads only the bytes appended since the
//! offset on disk: cheap for the hundreds of files a developer machine has. Notifications
//! would need a new dependency (`notify`) with its own platform quirks (FSEvents coalesces
//! and reports directories, inotify needs a watch per folder and misses folders created
//! before the watch is set), and the mirror would still need the offsets and a periodic
//! rescan to be safe; a 2 s delay is well under the 30 s that matter for takeover.
//!
//! Who writes (SPEC §9.2, §9.3). Records the worker's own Claude Code process produced are
//! known by uuid ([`OwnRecords`], fed by the adapter) and skipped: the adapter sent them.
//! Any other main-chain record is someone else's:
//! - while a process of this worker runs the session (`locked_by: hqc`) the mirror does not
//!   emit it and does not move past it; once it has been foreign for [`GRACE`] (stream-json
//!   may lag behind the file) the supervisor is told ([`Output::ForeignWriter`]), stops the
//!   agent after its turn and reports `locked_by: vscode`; then the process is gone and the
//!   held records go out;
//! - otherwise it is emitted at once with `recent` set when the file was written within the
//!   last 30 s; the hub turns such a session into `locked_by: vscode`.
//!
//! Who holds a transcript open (SPEC §9.3, T20). Every poll also reads Claude Code's registry
//! of live sessions ([`holders`], `<config>/sessions`): a live process outside the worker
//! that has the transcript open is its holder. A change (appeared, gone, other status) is
//! reported to the supervisor ([`Output::Holder`]), at most once per [`POLL`] per transcript,
//! and every `mirror.event` carries the current holder (`held_by`).
//!
//! Forks (SPEC §9.3, T20). The mirror keeps the tree of each transcript's records
//! ([`fork::Tree`]); a prompt that starts a second line of turns from an earlier record
//! gives `system session_forked` through `mirror.event`.

pub mod fork;
pub mod holders;
pub mod map;
pub mod registry;
pub mod store;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use hqc_claude_jsonl::{parse_line, Link, Record};
use hqc_proto::event::{Author, Event, EventKind, SessionForked, SESSION_FORKED};
use hqc_proto::frame::{Candidate, CandidatesResult, HeldBy, MirrorEvent, MIRROR_RECENT_S};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use registry::OwnRecords;
use store::{FileRow, Store};

use crate::link::HubLink;
use crate::supervisor::Inbound;

pub const POLL: Duration = Duration::from_secs(2);
/// How long an unknown record must stay unknown while the worker's own process runs the
/// session before it counts as another writer's.
pub const GRACE: Duration = Duration::from_secs(5);
/// Lines read to find a transcript's `cwd` before giving up on it for this poll.
const CWD_LINES: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Event(Box<MirrorEvent>),
    /// Another writer continues transcript `runtime_session_id` while this worker's process
    /// runs it (SPEC §9.3).
    ForeignWriter {
        runtime_session_id: Uuid,
    },
    /// The holder of transcript `runtime_session_id` changed: a live process outside the
    /// worker opened it (or changed status), or `None`: no such process any more.
    Holder {
        runtime_session_id: Uuid,
        held_by: Option<HeldBy>,
    },
}

pub struct Mirror {
    projects: PathBuf,
    store: Arc<Store>,
    own: Arc<OwnRecords>,
    workspaces: BTreeSet<PathBuf>,
    files: HashMap<PathBuf, FileRow>,
    /// Foreign records held while the session's process runs: when first seen.
    held: HashMap<Uuid, Instant>,
    /// Transcripts whose foreign writer was reported during the current process.
    signalled: HashSet<Uuid>,
    pub grace: Duration,
    /// `<claude config>/sessions`, Claude Code's registry of live sessions; `None`: holders
    /// are not looked at.
    sessions: Option<PathBuf>,
    procs: Box<dyn holders::Processes + Send>,
    /// The registry folder existed at the last poll: `held_by` is known.
    registry: bool,
    /// Holders found at the last poll.
    current: HashMap<Uuid, HeldBy>,
    /// Holders last reported, and when.
    reported: HashMap<Uuid, HeldBy>,
    reported_at: HashMap<Uuid, Instant>,
    /// Record trees of the transcripts read in this process, for forks.
    trees: HashMap<PathBuf, fork::Tree>,
}

impl Mirror {
    /// `projects`: `<claude config>/projects`.
    pub fn new(projects: PathBuf, store: Arc<Store>, own: Arc<OwnRecords>) -> Self {
        Self {
            projects,
            store,
            own,
            workspaces: BTreeSet::new(),
            files: HashMap::new(),
            held: HashMap::new(),
            signalled: HashSet::new(),
            grace: GRACE,
            sessions: None,
            procs: Box::new(holders::System),
            registry: false,
            current: HashMap::new(),
            reported: HashMap::new(),
            reported_at: HashMap::new(),
            trees: HashMap::new(),
        }
    }

    /// Read holders from `sessions` (`<claude config>/sessions`), checking processes with
    /// `procs`.
    pub fn with_registry(
        mut self,
        sessions: PathBuf,
        procs: Box<dyn holders::Processes + Send>,
    ) -> Self {
        self.sessions = Some(sessions);
        self.procs = procs;
        self
    }

    /// `mirror.set`: `path` is canonical and inside `allowed_roots` (the supervisor checked).
    pub fn set(&mut self, path: PathBuf, enabled: bool) {
        tracing::info!(path = %path.display(), enabled, "mirror.set");
        if enabled {
            self.workspaces.insert(path);
        } else {
            self.workspaces.remove(&path);
        }
    }

    pub fn workspaces(&self) -> impl Iterator<Item = &PathBuf> {
        self.workspaces.iter()
    }

    /// One pass over the registry of live sessions and every transcript.
    pub fn poll(&mut self, now: Instant, wall: SystemTime) -> Vec<Output> {
        let mut out = Vec::new();
        self.poll_holders(now, &mut out);
        if self.workspaces.is_empty() {
            return out;
        }
        for (path, runtime) in transcripts(&self.projects) {
            if let Err(e) = self.scan(&path, runtime, now, wall, &mut out) {
                tracing::warn!(path = %path.display(), "mirror: {e:#}");
            }
        }
        out
    }

    /// Holders of all transcripts, mirrored or not: a session hqc started may be opened in
    /// VS Code too. Changes go out at most once per [`POLL`] per transcript; one that comes
    /// sooner waits for a later poll.
    fn poll_holders(&mut self, now: Instant, out: &mut Vec<Output>) {
        let Some(dir) = &self.sessions else {
            return;
        };
        self.registry = dir.is_dir();
        if !self.registry {
            self.current.clear();
            return;
        }
        let own = self.own.clone();
        self.current = holders::read(dir, &own.pids(), |s| own.is_live(s), &*self.procs)
            .into_iter()
            .map(|(k, h)| (k, h.held_by()))
            .collect();
        self.reported_at
            .retain(|_, t| now.saturating_duration_since(*t) < POLL);
        let keys: BTreeSet<Uuid> = self
            .current
            .keys()
            .chain(self.reported.keys())
            .copied()
            .collect();
        for k in keys {
            let new = self.current.get(&k);
            if new == self.reported.get(&k) || self.reported_at.contains_key(&k) {
                continue;
            }
            tracing::info!(runtime_session_id = %k, held_by = ?new, "holder of a transcript changed");
            self.reported_at.insert(k, now);
            match new {
                Some(h) => self.reported.insert(k, h.clone()),
                None => self.reported.remove(&k),
            };
            out.push(Output::Holder {
                runtime_session_id: k,
                held_by: new.cloned(),
            });
        }
    }

    /// `held_by` of a `mirror.event` for transcript `runtime`: absent without the registry.
    fn held_by(&self, runtime: Uuid) -> Option<Option<HeldBy>> {
        self.registry.then(|| self.current.get(&runtime).cloned())
    }

    /// Add a consumed record to the transcript's tree; the `session_forked` event if it
    /// starts a second line of turns (SPEC §9.3).
    fn track(
        &mut self,
        path: &Path,
        runtime: Uuid,
        link: Option<Link>,
        rec: &Record,
    ) -> Option<Event> {
        let link = link?;
        let f = self.trees.entry(path.to_path_buf()).or_default().add(
            link.uuid,
            link.parent_uuid,
            is_prompt(rec),
        )?;
        let hqc = self.own.is_own(runtime, link.uuid);
        let ts = match rec {
            Record::User(m) | Record::Assistant(m) => map::ts(m),
            _ => Utc::now(),
        };
        tracing::warn!(runtime_session_id = %runtime, at = %f.at, orphaned_turns = f.orphaned_turns, from_hqc = hqc, "transcript forked");
        Some(Event {
            session_id: Uuid::nil(),
            event_id: Event::id_of_fork(&f.at),
            seq: None,
            ts,
            author: Author::System,
            kind: EventKind::System {
                text: fork::fork_text(hqc, f.orphaned_turns),
                code: SESSION_FORKED.into(),
                fork: Some(SessionForked {
                    at_uuid: f.at,
                    branch_from: if hqc { "hqc" } else { "vscode" }.into(),
                    orphaned_turns: f.orphaned_turns,
                }),
            },
            chain: None,
            client_op_id: None,
            delivery: None,
            delivered_at: None,
        })
    }

    fn row(&mut self, path: &Path) -> anyhow::Result<FileRow> {
        if let Some(r) = self.files.get(path) {
            if r.cwd.is_some() {
                return Ok(r.clone());
            }
        }
        let mut row = self.store.get(path)?.unwrap_or_default();
        if row.cwd.is_none() {
            row.cwd = read_cwd(path);
            if row.cwd.is_some() {
                self.store.put(path, &row)?;
            }
        }
        self.files.insert(path.to_path_buf(), row.clone());
        Ok(row)
    }

    fn save(&mut self, path: &Path, row: FileRow) -> anyhow::Result<()> {
        if self.files.get(path) != Some(&row) {
            self.store.put(path, &row)?;
            self.files.insert(path.to_path_buf(), row);
        }
        Ok(())
    }

    fn scan(
        &mut self,
        path: &Path,
        runtime: Uuid,
        now: Instant,
        wall: SystemTime,
        out: &mut Vec<Output>,
    ) -> anyhow::Result<()> {
        let meta = std::fs::metadata(path)?;
        let mut row = self.row(path)?;
        let Some(cwd) = row.cwd.clone() else {
            return Ok(());
        };
        let Some(ws) = self
            .workspaces
            .iter()
            .find(|w| Path::new(&cwd).starts_with(w))
            .cloned()
        else {
            return Ok(());
        };
        let size = meta.len();
        if size < row.offset {
            tracing::info!(path = %path.display(), "transcript shrank; reading it again");
            row.offset = 0;
        }
        if size == row.offset {
            return Ok(());
        }
        if !self.trees.contains_key(path) {
            // First read in this process: the records already handled shape the tree, and
            // forks among them were reported before.
            self.trees
                .insert(path.to_path_buf(), read_tree(path, row.offset)?);
        }
        let live = self.own.is_live(runtime);
        if !live {
            self.signalled.remove(&runtime);
        }
        let recent = meta
            .modified()
            .ok()
            .and_then(|m| wall.duration_since(m).ok())
            .is_none_or(|age| age < Duration::from_secs(MIRROR_RECENT_S));

        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(row.offset))?;
        let mut buf = Vec::new();
        f.take(size - row.offset).read_to_end(&mut buf)?;

        let held_by = self.held_by(runtime);
        let wrap = |event: Event, title: Option<String>| {
            Output::Event(Box::new(MirrorEvent {
                workspace_path: ws.display().to_string(),
                runtime_session_id: runtime.to_string(),
                cwd: cwd.clone(),
                event,
                title,
                recent,
                held_by: held_by.clone(),
            }))
        };
        let mut pos = row.offset;
        for chunk in buf.split_inclusive(|b| *b == b'\n') {
            if chunk.last() != Some(&b'\n') {
                break; // a line still being written
            }
            let end = pos + chunk.len() as u64;
            let line = String::from_utf8_lossy(chunk);
            let line = line.trim();
            let rec = match parse_line(line) {
                Ok(r) => r,
                Err(e) => {
                    if !line.is_empty() {
                        tracing::debug!(path = %path.display(), "unparseable transcript line: {e}");
                    }
                    pos = end;
                    continue;
                }
            };
            if let Record::AiTitle(t) = &rec {
                let t = t.ai_title.trim();
                if !t.is_empty() {
                    row.title = Some(hqc_proto::replace_nul(t).into_owned());
                }
                pos = end;
                continue;
            }
            // A record joins the tree once consumed: a held one may still turn out ours.
            let link = hqc_claude_jsonl::link(line).filter(|l| !l.is_sidechain);
            let Some(uuid) = map::counted_uuid(&rec) else {
                if let Some(f) = self.track(path, runtime, link, &rec) {
                    out.push(wrap(f, row.title.clone()));
                }
                pos = end;
                continue;
            };
            if self.own.is_own(runtime, uuid) {
                if let Some(f) = self.track(path, runtime, link, &rec) {
                    out.push(wrap(f, row.title.clone()));
                }
                row.last_prompt =
                    map::prompt_id(&map::record_events(&rec, None)).or(row.last_prompt);
                self.held.remove(&uuid);
                pos = end;
                continue;
            }
            if live {
                let first = *self.held.entry(uuid).or_insert(now);
                if now.duration_since(first) >= self.grace && self.signalled.insert(runtime) {
                    tracing::info!(runtime_session_id = %runtime, record = %uuid, "another writer continues a session the worker runs");
                    out.push(Output::ForeignWriter {
                        runtime_session_id: runtime,
                    });
                }
                break;
            }
            if let Some(f) = self.track(path, runtime, link, &rec) {
                out.push(wrap(f, row.title.clone()));
            }
            let events = map::record_events(&rec, row.last_prompt);
            row.last_prompt = map::prompt_id(&events).or(row.last_prompt);
            for event in events {
                out.push(wrap(event, row.title.clone()));
            }
            self.held.remove(&uuid);
            pos = end;
        }
        row.offset = pos;
        self.save(path, row)
    }
}

/// `<projects>/*/<uuid>.jsonl`: subagent folders and other files are not sessions (SPEC §9).
pub fn transcripts(projects: &Path) -> Vec<(PathBuf, Uuid)> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(projects) else {
        return out;
    };
    for d in dirs.flatten() {
        if !d.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            let id = p
                .extension()
                .filter(|e| *e == "jsonl")
                .and_then(|_| p.file_stem()?.to_str()?.parse::<Uuid>().ok());
            if let Some(id) = id {
                if f.file_type().is_ok_and(|t| t.is_file()) {
                    out.push((p, id));
                }
            }
        }
    }
    out.sort();
    out
}

/// A prompt a person typed: what counts as a turn in a line of the tree.
fn is_prompt(rec: &Record) -> bool {
    matches!(rec, Record::User(m) if m.is_main())
        && map::prompt_id(&map::record_events(rec, None)).is_some()
}

/// The record tree of the first `upto` bytes of a transcript (whole lines only).
fn read_tree(path: &Path, upto: u64) -> anyhow::Result<fork::Tree> {
    let mut tree = fork::Tree::default();
    let f = std::fs::File::open(path)?;
    let mut read = 0u64;
    for line in std::io::BufReader::new(f).split(b'\n') {
        let line = line?;
        read += line.len() as u64 + 1;
        if read > upto {
            break;
        }
        let line = String::from_utf8_lossy(&line);
        let Some(link) = hqc_claude_jsonl::link(&line).filter(|l| !l.is_sidechain) else {
            continue;
        };
        let prompt = parse_line(&line).is_ok_and(|r| is_prompt(&r));
        // Forks in there were reported when the lines were first read.
        let _ = tree.add(link.uuid, link.parent_uuid, prompt);
    }
    Ok(tree)
}

/// `cwd` of the first record that has one, reading at most [`CWD_LINES`] lines.
fn read_cwd(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(f)
        .lines()
        .take(CWD_LINES)
        .map_while(Result::ok)
        .find_map(|l| hqc_claude_jsonl::first_cwd(&l))
}

/// `workspace.list_candidates` (SPEC §9.1): transcripts grouped by working directory, only
/// directories `inside` accepts (the worker's `allowed_roots`), sorted by path.
pub fn candidates(
    projects: &Path,
    store: &Store,
    inside: impl Fn(&Path) -> bool,
) -> CandidatesResult {
    let mut by_cwd: BTreeMap<String, (u32, SystemTime)> = BTreeMap::new();
    for (path, _) in transcripts(projects) {
        let cwd = match store.get(&path) {
            Ok(Some(FileRow { cwd: Some(c), .. })) => c,
            Ok(row) => {
                let Some(c) = read_cwd(&path) else { continue };
                let mut row = row.unwrap_or_default();
                row.cwd = Some(c.clone());
                if let Err(e) = store.put(&path, &row) {
                    tracing::warn!("mirror state: {e:#}");
                }
                c
            }
            Err(e) => {
                tracing::warn!("mirror state: {e:#}");
                continue;
            }
        };
        if !inside(Path::new(&cwd)) {
            continue;
        }
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let e = by_cwd.entry(cwd).or_insert((0, SystemTime::UNIX_EPOCH));
        e.0 += 1;
        e.1 = e.1.max(mtime);
    }
    CandidatesResult {
        candidates: by_cwd
            .into_iter()
            .map(|(path, (n, t))| Candidate {
                path,
                sessions_count: n,
                last_active: DateTime::<Utc>::from(t),
            })
            .collect(),
    }
}

/// `mirror.set` from the supervisor.
#[derive(Debug)]
pub enum MirrorCmd {
    Set { path: PathBuf, enabled: bool },
}

/// What the supervisor needs of the mirror: its commands and, for candidates, the state.
#[derive(Clone)]
pub struct MirrorHandle {
    pub cmds: mpsc::UnboundedSender<MirrorCmd>,
    pub store: Arc<Store>,
    pub projects: PathBuf,
}

/// The mirror task: a poll every `every` and right after `mirror.set`; events go to the
/// outbox, foreign writers and holders to the supervisor.
pub async fn run(
    mut mirror: Mirror,
    mut cmds: mpsc::UnboundedReceiver<MirrorCmd>,
    link: HubLink,
    supervisor: mpsc::Sender<Inbound>,
    every: Duration,
    stop: CancellationToken,
) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            c = cmds.recv() => match c {
                Some(MirrorCmd::Set { path, enabled }) => mirror.set(path, enabled),
                None => break,
            },
            _ = tick.tick() => {}
        }
        let r = tokio::task::spawn_blocking(move || {
            let out = mirror.poll(Instant::now(), SystemTime::now());
            (mirror, out)
        })
        .await;
        let out;
        (mirror, out) = match r {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("mirror poll panicked: {e}");
                return;
            }
        };
        for o in out {
            match o {
                Output::Event(e) => {
                    if let Err(err) = link.mirror_event(*e) {
                        tracing::error!("outbox write failed, mirrored event lost: {err:#}");
                    }
                }
                Output::ForeignWriter { runtime_session_id } => {
                    let _ = supervisor
                        .send(Inbound::ForeignWriter { runtime_session_id })
                        .await;
                }
                Output::Holder {
                    runtime_session_id,
                    held_by,
                } => {
                    // To the hub behind the events of earlier polls, which carry the
                    // holder they saw; to the supervisor to release a session it runs.
                    if let Err(err) = link.holder_report(runtime_session_id, held_by.clone()) {
                        tracing::error!("outbox write failed, holder report lost: {err:#}");
                    }
                    let _ = supervisor
                        .send(Inbound::Holder {
                            runtime_session_id,
                            held_by,
                        })
                        .await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
