//! The skills of the hub (SPEC 0.245, iteration 8): the archives `session.start.skills` names,
//! fetched once per hash, and the plugin folders the agents get with `--plugin-dir`.
//!
//! `$HQC_HOME/hub-skills/`:
//! - `zips/<sha256>.zip` — an archive as the hub served it, its hash checked and its content
//!   checked ([`check_archive`]) before it is kept;
//! - `sets/<hash of the set>/` — `.claude-plugin/plugin.json` (`{"name": "hqc"}`, written here)
//!   and `skills/<name>/` for each skill of a set, built once and shared by the sessions with
//!   that set. Nothing of the hub's lands outside `skills/<name>/`, so it never becomes a
//!   plugin's hooks, MCP servers, agents or commands.
//!
//! The worker does not trust the hub for what runs outside its `deny` (SPEC §2.1): a skill with
//! `hooks` is refused here as on the hub ([`hqc_proto::skills::check_skill_md`]).

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context};
use hqc_proto::skills::{
    check_path, check_skill_md, SkillRef, MAX_SKILL_BYTES, MAX_SKILL_FILES, SKILL_MD,
};
use sha2::{Digest, Sha256};

use crate::inbox::ArtifactClient;

/// An archive is fetched up to this size (its content is capped at [`MAX_SKILL_BYTES`]).
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024;
/// Archives and sets not used for this long go at the worker's start.
pub const UNUSED: Duration = Duration::from_secs(7 * 24 * 3600);

/// A file of a checked archive.
struct Entry {
    path: String,
    data: Vec<u8>,
    executable: bool,
}

pub struct HubSkills {
    root: PathBuf,
    client: ArtifactClient,
    /// One build at a time: two sessions starting with the same set share one folder.
    lock: tokio::sync::Mutex<()>,
}

impl HubSkills {
    pub fn new(root: PathBuf, client: ArtifactClient) -> Self {
        Self {
            root,
            client,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The plugin folder with the `skills` that could be had, and those skills. A skill that
    /// cannot be fetched or fails the checks is left out and logged; `None` when none is left.
    pub async fn prepare(
        &self,
        session_id: uuid::Uuid,
        skills: &[SkillRef],
    ) -> Option<(PathBuf, Vec<SkillRef>)> {
        if skills.is_empty() {
            return None;
        }
        let _g = self.lock.lock().await;
        let mut have = vec![];
        for s in skills {
            match self.archive(s).await {
                Ok(()) => have.push(s.clone()),
                Err(e) => tracing::warn!(
                    session_id = %session_id,
                    skill = %s.name,
                    "the hub's skill is left out of this start: {e:#}"
                ),
            }
        }
        if have.is_empty() {
            return None;
        }
        let root = self.root.clone();
        let set = have.clone();
        match tokio::task::spawn_blocking(move || build_set(&root, &set)).await {
            Ok(Ok(dir)) => Some((dir, have)),
            Ok(Err(e)) => {
                tracing::warn!(session_id = %session_id, "the hub's skills are left out of this start: {e:#}");
                None
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, "the hub's skills are left out of this start: {e}");
                None
            }
        }
    }

    /// `zips/<sha256>.zip`, fetched and checked unless it is there already.
    async fn archive(&self, s: &SkillRef) -> anyhow::Result<()> {
        let path = zip_path(&self.root, &s.sha256);
        if path.is_file() {
            let _ = touch(&path);
            // Checked when kept; the name is checked against the set's.
            let name = s.name.clone();
            return tokio::task::spawn_blocking(move || {
                check_archive(&std::fs::read(&path)?, &name).map(drop)
            })
            .await?;
        }
        let data = self
            .client
            .fetch_skill(&s.sha256, MAX_ARCHIVE_BYTES)
            .await?;
        let got = hex(&Sha256::digest(&data));
        if got != s.sha256 {
            bail!("the archive's sha256 is {got}, not {}", s.sha256);
        }
        let name = s.name.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            check_archive(&data, &name)?;
            let dir = path.parent().expect("zips/");
            std::fs::create_dir_all(dir)?;
            let tmp = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
            std::fs::write(&tmp, &data)?;
            std::fs::rename(&tmp, &path).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })?;
            Ok(())
        })
        .await?
    }
}

fn zip_path(root: &Path, sha256: &str) -> PathBuf {
    root.join("zips").join(format!("{sha256}.zip"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn touch(p: &Path) -> std::io::Result<()> {
    std::fs::File::options()
        .append(true)
        .open(p)?
        .set_modified(SystemTime::now())
}

/// The files of a skill's archive, checked: relative paths that stay inside it, no links,
/// at most [`MAX_SKILL_FILES`] files and [`MAX_SKILL_BYTES`] unpacked, `SKILL.md` at the top
/// whose `name`, if any, is `name`, and no `SKILL.md` with `hooks` or a frontmatter the check
/// cannot read for sure.
fn check_archive(data: &[u8], name: &str) -> anyhow::Result<Vec<Entry>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).context("not a zip")?;
    if z.len() > MAX_SKILL_FILES * 2 {
        bail!("more than {MAX_SKILL_FILES} files");
    }
    let mut out: Vec<Entry> = vec![];
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let raw = f.name()?.to_string();
        if f.is_symlink() {
            bail!("{raw:?} is a link");
        }
        if f.is_dir() {
            check_path(raw.trim_end_matches('/')).map_err(|e| anyhow!(e))?;
            continue;
        }
        check_path(&raw).map_err(|e| anyhow!(e))?;
        if !seen.insert(raw.clone()) {
            bail!("{raw:?} is in the archive twice");
        }
        if out.len() >= MAX_SKILL_FILES {
            bail!("more than {MAX_SKILL_FILES} files");
        }
        let executable = f.unix_mode().is_some_and(|m| m & 0o111 != 0);
        let mut buf = vec![];
        (&mut f)
            .take(MAX_SKILL_BYTES - total + 1)
            .read_to_end(&mut buf)?;
        total += buf.len() as u64;
        if total > MAX_SKILL_BYTES {
            bail!("over {MAX_SKILL_BYTES} bytes unpacked");
        }
        out.push(Entry {
            path: raw,
            data: buf,
            executable,
        });
    }
    let mut top = false;
    for e in &out {
        let file = e.path.rsplit('/').next().unwrap_or(&e.path);
        if !file.eq_ignore_ascii_case(SKILL_MD) {
            continue;
        }
        let text = std::str::from_utf8(&e.data).map_err(|_| anyhow!("{} is not UTF-8", e.path))?;
        let fm = check_skill_md(text).map_err(|m| anyhow!("{}: {m}", e.path))?;
        if e.path == SKILL_MD {
            top = true;
            if let Some(n) = fm.name.filter(|n| n != name) {
                bail!("SKILL.md names it {n:?}, not {name:?}");
            }
        }
    }
    if !top {
        bail!("no SKILL.md at the top");
    }
    // Paths must not clash as a file and a folder ("a" and "a/b").
    for e in &out {
        let mut p = e.path.as_str();
        while let Some((parent, _)) = p.rsplit_once('/') {
            if seen.contains(parent) {
                bail!("{parent:?} is a file and a folder");
            }
            p = parent;
        }
    }
    Ok(out)
}

/// `sets/<hash>/` with `skills` (their archives in `zips/`), built unless it is there.
fn build_set(root: &Path, skills: &[SkillRef]) -> anyhow::Result<PathBuf> {
    let mut key = Sha256::new();
    for s in skills {
        key.update(format!("{}:{}\n", s.name, s.sha256));
    }
    let sets = root.join("sets");
    let dir = sets.join(&hex(&key.finalize())[..32]);
    let manifest = dir.join(".claude-plugin/plugin.json");
    if manifest.is_file() {
        let _ = touch(&manifest);
        return Ok(dir);
    }
    std::fs::create_dir_all(&sets)?;
    let tmp = sets.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let r = (|| -> anyhow::Result<()> {
        std::fs::create_dir_all(tmp.join(".claude-plugin"))?;
        std::fs::write(
            tmp.join(".claude-plugin/plugin.json"),
            serde_json::json!({
                "name": "hqc",
                "description": "Skills the owner keeps on the hqc hub",
            })
            .to_string(),
        )?;
        for s in skills {
            let data = std::fs::read(zip_path(root, &s.sha256))?;
            let base = tmp.join("skills").join(&s.name);
            std::fs::create_dir_all(&base)?;
            for e in check_archive(&data, &s.name)? {
                let p = base.join(&e.path);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_new(&p, &e.data, if e.executable { 0o755 } else { 0o644 })
                    .with_context(|| format!("{}/{}", s.name, e.path))?;
            }
        }
        Ok(())
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &dir) {
        let _ = std::fs::remove_dir_all(&tmp);
        if !manifest.is_file() {
            return Err(e).context("sets/");
        }
    }
    Ok(dir)
}

fn write_new(p: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    o.mode(mode);
    #[cfg(not(unix))]
    let _ = mode;
    o.open(p)?.write_all(data)
}

/// Removes the archives and sets not used for `unused`, and what a build left half done; at
/// the worker's start.
pub fn sweep(root: &Path, unused: Duration) {
    let old = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|age| age > unused)
    };
    for (sub, marker) in [("zips", None), ("sets", Some(".claude-plugin/plugin.json"))] {
        let Ok(rd) = std::fs::read_dir(root.join(sub)) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let tmp = e.file_name().to_string_lossy().starts_with(".tmp-");
            let stale = match marker {
                Some(m) => old(&p.join(m)),
                None => old(&p),
            };
            if tmp || stale {
                let r = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                if let Err(e) = r {
                    tracing::warn!("removing {}: {e}", p.display());
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A zip of `files` (path, content, mode).
    pub(crate) fn zip_of(files: &[(&str, &str, u32)]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (p, c, mode) in files {
            let o = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            z.start_file(*p, o).unwrap();
            z.write_all(c.as_bytes()).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    fn sha(d: &[u8]) -> String {
        hex(&Sha256::digest(d))
    }

    #[test]
    fn an_archive_is_checked() {
        let ok = zip_of(&[
            ("SKILL.md", "---\nname: bench\n---\nRun it.\n", 0o644),
            ("scripts/run.sh", "echo hi\n", 0o755),
        ]);
        let e = check_archive(&ok, "bench").unwrap();
        assert_eq!(e.len(), 2);
        assert!(e[1].executable);
        assert!(check_archive(&ok, "other").is_err(), "the name must match");
        for (bad, why) in [
            (zip_of(&[("x.md", "", 0o644)]), "no SKILL.md"),
            (
                zip_of(&[("SKILL.md", "", 0o644), ("../x", "", 0o644)]),
                "out",
            ),
            (
                zip_of(&[("SKILL.md", "", 0o644), ("/etc/x", "", 0o644)]),
                "absolute",
            ),
            (
                zip_of(&[("SKILL.md", "---\nhooks:\n  Stop: []\n---\n", 0o644)]),
                "hooks",
            ),
            (
                zip_of(&[
                    ("SKILL.md", "", 0o644),
                    ("sub/SKILL.md", "---\n\"hooks\": 1\n---\n", 0o644),
                ]),
                "nested hooks",
            ),
            (
                zip_of(&[
                    ("SKILL.md", "", 0o644),
                    ("a", "", 0o644),
                    ("a/b", "", 0o644),
                ]),
                "clash",
            ),
        ] {
            assert!(check_archive(&bad, "bench").is_err(), "{why}");
        }
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("SKILL.md", o).unwrap();
        z.add_symlink("link", "/etc/passwd", o).unwrap();
        let link = z.finish().unwrap().into_inner();
        assert!(check_archive(&link, "bench").is_err(), "a link");
        let big = "x".repeat(MAX_SKILL_BYTES as usize + 1);
        assert!(check_archive(
            &zip_of(&[("SKILL.md", "", 0o644), ("big", &big, 0o644)]),
            "bench"
        )
        .is_err());
    }

    #[tokio::test]
    async fn a_set_is_built_once_per_hash_and_bad_skills_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let good = zip_of(&[
            ("SKILL.md", "---\nname: bench\ndescription: d\n---\n", 0o644),
            ("scripts/run.sh", "echo hi\n", 0o755),
        ]);
        let hooked = zip_of(&[("SKILL.md", "---\nhooks: {}\n---\n", 0o644)]);
        let (url, hits) = serve(vec![good.clone(), hooked.clone()]).await;
        let client = ArtifactClient::new(&url, crate::token::Secret::new("t"));
        let hs = HubSkills::new(dir.path().to_path_buf(), client);
        let refs = vec![
            SkillRef {
                name: "bench".into(),
                sha256: sha(&good),
            },
            SkillRef {
                name: "hooked".into(),
                sha256: sha(&hooked),
            },
            SkillRef {
                name: "liar".into(),
                sha256: "0".repeat(64),
            },
        ];
        let id = uuid::Uuid::new_v4();
        let (plugin, have) = hs.prepare(id, &refs).await.unwrap();
        assert_eq!(have, refs[..1]);
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(plugin.join(".claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["name"], "hqc");
        assert!(plugin.join("skills/bench/SKILL.md").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(plugin.join("skills/bench/scripts/run.sh")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o755);
        }
        assert!(!plugin.join("skills/hooked").exists());
        let before = hits.load(std::sync::atomic::Ordering::SeqCst);
        let (again, _) = hs.prepare(id, &refs[..1]).await.unwrap();
        assert_eq!(again, plugin, "the same set shares its folder");
        // `bench` was kept, the others were asked for again.
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            before,
            "fetched once per hash"
        );
        assert!(hs.prepare(id, &refs[1..]).await.is_none());

        // The sweep keeps what is in use and removes what is old.
        sweep(dir.path(), UNUSED);
        assert!(plugin.is_dir());
        sweep(dir.path(), Duration::ZERO);
        assert!(!plugin.exists());
        assert!(!zip_path(dir.path(), &sha(&good)).exists());
    }

    /// A hub that serves `archives` at `/worker/skills/{sha256}`, counting the requests.
    pub(crate) async fn serve(
        archives: Vec<Vec<u8>>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut c, _)) = l.accept().await else {
                    return;
                };
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let archives = archives.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = c.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                    let body = archives
                        .iter()
                        .find(|a| path == format!("/worker/skills/{}", sha(a)))
                        .cloned();
                    let head = match &body {
                        Some(b) => format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", b.len()),
                        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                    };
                    let _ = c.write_all(head.as_bytes()).await;
                    if let Some(b) = body {
                        let _ = c.write_all(&b).await;
                    }
                });
            }
        });
        (url, hits)
    }
}
