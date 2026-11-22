//! `hqc_publish` on the worker (SPEC §7, §12): the daemon packs the folder or the file into a
//! zip itself and uploads it as an artifact of kind `site` (`PUT /worker/artifacts`) before the
//! `mcp.call` goes to the hub, which gets the artifact id and the canonical path packed (for
//! the card that asks the owner).
//!
//! The rules of `hqc_attach` hold ([`super::attach::resolve`]): `path` lies inside the
//! session's workspace and `allowed_roots` after resolving symlinks. Beyond them:
//! - a folder needs an `index.html` (a regular file, possibly through a link inside the
//!   folder); a single file becomes an archive of that one file;
//! - every entry is resolved to its canonical path, which must stay inside the folder and
//!   `allowed_roots`: a symlink inside pointing out is `invalid_args` naming the entry, never
//!   its target; a link to a folder that contains it is left out (it would never end);
//! - only regular files and folders: anything else is `invalid_args`;
//! - the packing is deterministic: entries in the byte order of their paths, one fixed time
//!   (1980-01-01 00:00) and mode (0644) for all, so the same files give the same archive;
//! - the limits of the hub are applied while walking (at most [`MAX_SITE_FILES`] files of at
//!   most [`MAX_SITE_UNPACKED`] bytes together, names of at most [`MAX_SITE_NAME`] bytes) and
//!   the archive is cut at [`crate::inbox::MAX_ARTIFACT_BYTES`] while it is written;
//! - the archive is a temporary file unlinked as soon as it is made: nothing is left behind
//!   when the call is cancelled or the worker stops.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use hqc_proto::event::ArtifactKind;
use hqc_proto::frame::McpError;
use hqc_proto::mcp::{error_code, PublishCall, MAX_SITE_FILES, MAX_SITE_NAME, MAX_SITE_UNPACKED};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::attach::{open_regular, resolve, Attacher, Folders};

/// The page a published folder opens with.
pub const INDEX: &str = "index.html";

/// The arguments as the agent gives them (the `inputSchema` of [`hqc_proto::mcp::PUBLISH`]).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    caption: Option<String>,
}

fn invalid(m: impl Into<String>) -> McpError {
    McpError::new(error_code::INVALID_ARGS, m)
}

/// A packed archive: the open temporary file (unlinked already), at its start.
#[derive(Debug)]
pub struct Packed {
    pub file: std::fs::File,
    pub size: u64,
    /// The artifact's name: the folder's or the file's.
    pub name: String,
    /// The paths in the archive, in its order.
    pub entries: Vec<String>,
}

impl Attacher {
    /// Packs and uploads what `args.path` names for `session_id`; the arguments of the
    /// `mcp.call` that follows.
    pub async fn prepare_publish(
        &self,
        session_id: Uuid,
        folders: &Folders,
        args: Value,
    ) -> Result<Value, McpError> {
        let a: Args = serde_json::from_value(if args.is_null() {
            Value::Object(Default::default())
        } else {
            args
        })
        .map_err(|e| invalid(e.to_string()))?;
        let real = resolve(&self.allowed_roots, folders, &a.path)?;
        let (roots, max, given, at) = (
            self.allowed_roots.clone(),
            self.client.max_artifact(),
            a.path.clone(),
            real.clone(),
        );
        let packed = tokio::task::spawn_blocking(move || pack(&roots, &at, &given, max))
            .await
            .map_err(|e| McpError::new(error_code::INTERNAL, format!("packing: {e}")))??;
        let artifact_id = self
            .client
            .upload(
                session_id,
                packed.file,
                packed.size,
                &packed.name,
                Some(ArtifactKind::Site),
            )
            .await
            .inspect_err(|e| {
                tracing::warn!(%session_id, path = %real.display(), code = %e.code, "publish upload: {}", e.message);
            })?;
        tracing::info!(%session_id, path = %real.display(), %artifact_id, files = packed.entries.len(), size = packed.size, "packed and uploaded for hqc_publish");
        Ok(serde_json::to_value(PublishCall {
            artifact_id,
            path: real.display().to_string(),
            caption: a.caption,
        })
        .expect("PublishCall serializes"))
    }
}

/// Packs `real` (the canonical path of `given`, inside `roots`) into an archive of at most
/// `max` bytes: a folder with its files, or one file under its name.
pub fn pack(roots: &[PathBuf], real: &Path, given: &str, max: u64) -> Result<Packed, McpError> {
    let md = std::fs::metadata(real).map_err(|e| invalid(format!("{given}: {e}")))?;
    let (name, files) = if md.is_dir() {
        let mut files = Vec::new();
        let mut walk = Walk {
            roots,
            root: real,
            given,
            files: &mut files,
            unpacked: 0,
            stack: vec![real.to_path_buf()],
        };
        walk.dir(real, "")?;
        files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        if !files.iter().any(|(rel, _)| rel == INDEX) {
            return Err(invalid(format!(
                "{given} has no {INDEX}: a published folder opens with it"
            )));
        }
        (file_name(real), files)
    } else if md.is_file() {
        let name = Path::new(given)
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap_or_else(|| file_name(real));
        if name.is_empty() || name.len() > MAX_SITE_NAME {
            return Err(invalid(format!("{given}: the file's name does not fit")));
        }
        (name.clone(), vec![(name, real.to_path_buf())])
    } else {
        return Err(invalid(format!("{given} is not a regular file or folder")));
    };
    write_zip(&files, &name, given, max)
}

/// The last component of a canonical path, `site` when it has none.
fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "site".into())
}

/// The walk of a folder being published: its files as `(path in the archive, canonical path)`.
struct Walk<'a> {
    roots: &'a [PathBuf],
    root: &'a Path,
    given: &'a str,
    files: &'a mut Vec<(String, PathBuf)>,
    unpacked: u64,
    /// The canonical folders from the root down to the one being walked.
    stack: Vec<PathBuf>,
}

impl Walk<'_> {
    fn dir(&mut self, dir: &Path, prefix: &str) -> Result<(), McpError> {
        let given = self.given;
        let read = std::fs::read_dir(dir).map_err(|e| invalid(format!("{given}/{prefix}: {e}")))?;
        let mut names = Vec::new();
        for e in read {
            let e = e.map_err(|e| invalid(format!("{given}/{prefix}: {e}")))?;
            let raw = e.file_name();
            let Some(name) = raw.to_str() else {
                return Err(invalid(format!(
                    "{given}/{prefix}{} is not a UTF-8 name",
                    raw.to_string_lossy()
                )));
            };
            names.push(name.to_string());
        }
        // Sorted, so that what the walk meets first does not depend on the file system.
        names.sort();
        for name in names {
            let rel = format!("{prefix}{name}");
            if rel.len() > MAX_SITE_NAME {
                return Err(invalid(format!(
                    "{given}/{rel}: a path in a published folder is at most {MAX_SITE_NAME} bytes"
                )));
            }
            // A link is followed to what it names, which must stay in the folder; a broken one
            // is refused without saying where it points.
            let real = dir
                .join(&name)
                .canonicalize()
                .map_err(|e| invalid(format!("{given}/{rel}: {}", e.kind())))?;
            if !real.starts_with(self.root) || !self.roots.iter().any(|r| real.starts_with(r)) {
                return Err(invalid(format!(
                    "{given}/{rel} is a link that points outside {given}"
                )));
            }
            let md =
                std::fs::metadata(&real).map_err(|e| invalid(format!("{given}/{rel}: {e}")))?;
            if md.is_dir() {
                // A link to a folder that contains it would never end.
                if self.stack.contains(&real) {
                    continue;
                }
                self.stack.push(real.clone());
                self.dir(&real, &format!("{rel}/"))?;
                self.stack.pop();
            } else if md.is_file() {
                if self.files.len() >= MAX_SITE_FILES {
                    return Err(invalid(format!(
                        "{given} has more than {MAX_SITE_FILES} files"
                    )));
                }
                self.unpacked = self.unpacked.saturating_add(md.len());
                if self.unpacked > MAX_SITE_UNPACKED {
                    return Err(invalid(format!(
                        "the files of {given} are more than {MAX_SITE_UNPACKED} bytes"
                    )));
                }
                self.files.push((rel, real));
            } else {
                return Err(invalid(format!(
                    "{given}/{rel} is not a regular file or folder"
                )));
            }
        }
        Ok(())
    }
}

/// A file written no further than `max` bytes; `over` is set when a write would pass it.
struct Capped {
    f: std::fs::File,
    pos: u64,
    max: u64,
    over: Arc<AtomicBool>,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.pos.saturating_add(buf.len() as u64) > self.max {
            self.over.store(true, Ordering::SeqCst);
            return Err(std::io::Error::other("the archive is over the limit"));
        }
        let n = self.f.write(buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.f.flush()
    }
}

impl Seek for Capped {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let n = self.f.seek(to)?;
        self.pos = n;
        Ok(n)
    }
}

/// A new temporary file, read-write, mode 0600, already unlinked.
fn temp_file() -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!("hqc-publish-{}.zip", Uuid::new_v4()));
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(f)
}

/// The zip of `files` (sorted), at most `max` bytes.
fn write_zip(
    files: &[(String, PathBuf)],
    name: &str,
    given: &str,
    max: u64,
) -> Result<Packed, McpError> {
    let internal = |e: &dyn std::fmt::Display| {
        McpError::new(error_code::INTERNAL, format!("packing {given}: {e}"))
    };
    let over = Arc::new(AtomicBool::new(false));
    let too_large = || invalid(format!("{given} packs to more than {max} bytes"));
    let f = temp_file().map_err(|e| internal(&e))?;
    let mut z = zip::ZipWriter::new(Capped {
        f,
        pos: 0,
        max,
        over: over.clone(),
    });
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let fail = |e: &dyn std::fmt::Display| {
        if over.load(Ordering::SeqCst) {
            too_large()
        } else {
            internal(e)
        }
    };
    for (rel, real) in files {
        // Opened without following a link in the last component and checked on the handle,
        // as `hqc_attach` opens its file: one swapped since the walk is refused.
        let (file, size) = open_regular(real, max)
            .map_err(|e| invalid(format!("{given}/{rel}: {}", e.message)))?;
        z.start_file(rel.as_str(), opts).map_err(|e| fail(&e))?;
        std::io::copy(&mut file.take(size), &mut z).map_err(|e| fail(&e))?;
    }
    let mut capped = z.finish().map_err(|e| fail(&e))?;
    let size = capped.f.seek(SeekFrom::End(0)).map_err(|e| internal(&e))?;
    capped
        .f
        .seek(SeekFrom::Start(0))
        .map_err(|e| internal(&e))?;
    Ok(Packed {
        file: capped.f,
        size,
        name: name.to_string(),
        entries: files.iter().map(|(rel, _)| rel.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace `ws` inside the root, with a site `ws/site` and a file outside the root.
    fn setup() -> (tempfile::TempDir, Vec<PathBuf>, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap().join("root");
        let site = root.join("ws/site");
        std::fs::create_dir_all(site.join("css")).unwrap();
        std::fs::create_dir_all(site.join("img")).unwrap();
        std::fs::write(site.join("index.html"), b"<link href=css/a.css>").unwrap();
        std::fs::write(site.join("css/a.css"), b"body{}").unwrap();
        std::fs::write(site.join("img/b.png"), b"png").unwrap();
        std::fs::write(root.join("secret.env"), b"KEY=1").unwrap();
        (t, vec![root], site)
    }

    fn bytes(p: &mut Packed) -> Vec<u8> {
        let mut v = Vec::new();
        p.file.read_to_end(&mut v).unwrap();
        assert_eq!(v.len() as u64, p.size);
        v
    }

    fn names(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        (0..a.len())
            .map(|i| {
                let mut f = a.by_index(i).unwrap();
                let mut v = Vec::new();
                f.read_to_end(&mut v).unwrap();
                (f.name().unwrap().to_string(), v)
            })
            .collect()
    }

    #[test]
    fn a_folder_packs_the_same_every_time() {
        let (_t, roots, site) = setup();
        let mut a = pack(&roots, &site, "site", 1 << 20).unwrap();
        assert_eq!(a.name, "site");
        assert_eq!(a.entries, ["css/a.css", "img/b.png", "index.html"]);
        let first = bytes(&mut a);
        // Other times and modes, files written in another order: the same archive.
        std::fs::remove_file(site.join("css/a.css")).unwrap();
        std::fs::write(site.join("css/a.css"), b"body{}").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            site.join("index.html"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut b = pack(&roots, &site, "site", 1 << 20).unwrap();
        assert_eq!(bytes(&mut b), first);
        let files = names(&first);
        assert_eq!(
            files,
            [
                ("css/a.css".to_string(), b"body{}".to_vec()),
                ("img/b.png".to_string(), b"png".to_vec()),
                ("index.html".to_string(), b"<link href=css/a.css>".to_vec()),
            ]
        );
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(&first)).unwrap();
        for i in 0..z.len() {
            let f = z.by_index_raw(i).unwrap();
            assert_eq!(f.last_modified(), Some(zip::DateTime::default()));
            assert_eq!(f.unix_mode().map(|m| m & 0o777), Some(0o644));
        }
    }

    #[test]
    fn one_file_is_an_archive_of_that_file() {
        let (_t, roots, site) = setup();
        let mut p = pack(&roots, &site.join("img/b.png"), "img/b.png", 1 << 20).unwrap();
        assert_eq!(p.name, "b.png");
        assert_eq!(
            names(&bytes(&mut p)),
            [("b.png".to_string(), b"png".to_vec())]
        );
    }

    #[test]
    fn a_folder_needs_index_html() {
        let (_t, roots, site) = setup();
        let e = pack(&roots, &site.join("css"), "site/css", 1 << 20).unwrap_err();
        assert_eq!(e.code, "invalid_args");
        assert!(e.message.contains("has no index.html"), "{}", e.message);
    }

    #[test]
    fn the_archive_stops_at_the_limit() {
        let (_t, roots, site) = setup();
        // Random bytes do not compress: two files each under the limit, together over it.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut noise = || -> Vec<u8> {
            (0..6_000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect()
        };
        std::fs::write(site.join("noise1.bin"), noise()).unwrap();
        std::fs::write(site.join("noise2.bin"), noise()).unwrap();
        let e = pack(&roots, &site, "site", 10_000).unwrap_err();
        assert_eq!(e.code, "invalid_args");
        assert!(
            e.message.contains("packs to more than 10000"),
            "{}",
            e.message
        );
        assert!(pack(&roots, &site, "site", 100_000).is_ok());
    }

    #[test]
    fn a_link_out_of_the_folder_is_refused_by_its_name() {
        let (t, roots, site) = setup();
        let root = &roots[0];
        // Out of the folder (still in the roots), out of the roots.
        std::os::unix::fs::symlink(root.join("secret.env"), site.join("css/env.css")).unwrap();
        let e = pack(&roots, &site, "site", 1 << 20).unwrap_err();
        assert_eq!(e.code, "invalid_args");
        assert!(e.message.contains("site/css/env.css"), "{}", e.message);
        assert!(!e.message.contains("secret"), "{}", e.message);
        std::fs::remove_file(site.join("css/env.css")).unwrap();
        std::fs::write(t.path().join("beyond"), b"x").unwrap();
        std::os::unix::fs::symlink(t.path(), site.join("up")).unwrap();
        let e = pack(&roots, &site, "site", 1 << 20).unwrap_err();
        assert!(e.message.contains("site/up is a link"), "{}", e.message);
        std::fs::remove_file(site.join("up")).unwrap();
        // A broken link is refused without its target.
        std::os::unix::fs::symlink(root.join("gone"), site.join("gone.css")).unwrap();
        let e = pack(&roots, &site, "site", 1 << 20).unwrap_err();
        assert!(e.message.contains("site/gone.css"), "{}", e.message);
        assert!(!e.message.contains("root/gone"), "{}", e.message);
        std::fs::remove_file(site.join("gone.css")).unwrap();

        // Links that stay inside: a file is packed under the link's path, a folder that
        // contains the link is left out.
        std::os::unix::fs::symlink(site.join("img/b.png"), site.join("logo.png")).unwrap();
        std::os::unix::fs::symlink(&site, site.join("css/loop")).unwrap();
        let mut p = pack(&roots, &site, "site", 1 << 20).unwrap();
        assert_eq!(
            p.entries,
            ["css/a.css", "img/b.png", "index.html", "logo.png"]
        );
        assert_eq!(names(&bytes(&mut p))[3].1, b"png");
    }

    #[test]
    fn only_files_and_folders() {
        let (_t, roots, site) = setup();
        let fifo = site.join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let e = pack(&roots, &site, "site", 1 << 20).unwrap_err();
        assert!(
            e.message.contains("site/pipe is not a regular file"),
            "{}",
            e.message
        );
    }
}
