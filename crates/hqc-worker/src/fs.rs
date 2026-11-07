//! Folder requests of the hub (SPEC §5.3): `fs.stat` checks a folder before the hub makes it
//! a workspace, `fs.list_dirs` feeds the folder picker of the client. Both look only inside
//! `allowed_roots`, after resolving symlinks: the hub learns nothing about other paths
//! (SPEC §2.1), except that they are outside.

use std::path::{Component, Path, PathBuf};

use hqc_proto::frame::{DirEntry, FsListDirsResult, FsStatResult, MAX_DIR_ENTRIES};

use crate::config::Config;

/// `fs.stat`; with `create` a missing folder inside `allowed_roots` is made first, at the
/// path its existing ancestor resolves to. Inside `allowed_roots` it names the environment a
/// session there runs in ([`Config::environment`] of the canonical path, or of the path a
/// missing folder would be made at). `Err` (a relative or non-UTF-8 path, a folder that could
/// not be made) is answered with `error {invalid_frame}`.
pub fn stat(cfg: &Config, path: &str, create: bool) -> Result<FsStatResult, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    if create && p.canonicalize().is_err() {
        let target = resolve_missing(p);
        if cfg.inside_roots(&target) {
            std::fs::create_dir_all(&target).map_err(|e| format!("cannot make {path}: {e}"))?;
            tracing::info!(path = %target.display(), "folder made for a workspace");
        }
    }
    let outside = FsStatResult {
        exists: false,
        is_dir: false,
        canonical: None,
        inside_allowed_roots: false,
        environment: None,
    };
    let environment = |p: &Path| Some(cfg.environment(p).name.clone());
    match p.canonicalize() {
        Ok(c) if cfg.inside_roots(&c) => Ok(FsStatResult {
            exists: true,
            is_dir: c.is_dir(),
            canonical: Some(utf8(&c)?),
            inside_allowed_roots: true,
            environment: environment(&c),
        }),
        Ok(_) => Ok(outside),
        Err(_) => {
            // Missing: inside or not by where it would be, its existing ancestor resolved.
            let target = resolve_missing(p);
            if !cfg.inside_roots(&target) {
                return Ok(outside);
            }
            Ok(FsStatResult {
                inside_allowed_roots: true,
                environment: environment(&target),
                ..outside
            })
        }
    }
}

/// `fs.list_dirs`: `None` lists `allowed_roots`; a path lists its subfolders — no files, no
/// hidden entries, symlinks only to folders inside `allowed_roots` — sorted by name, at most
/// [`MAX_DIR_ENTRIES`]. A path outside `allowed_roots` is an `Err`.
pub fn list_dirs(cfg: &Config, path: Option<&str>) -> Result<FsListDirsResult, String> {
    let Some(path) = path else {
        let entries = cfg
            .allowed_roots
            .iter()
            .filter_map(|r| {
                let path = r.to_str()?.to_string();
                let name = r
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&path)
                    .to_string();
                Some(DirEntry { name, path })
            })
            .collect();
        return Ok(FsListDirsResult { entries });
    };
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    let dir = match p.canonicalize() {
        Ok(d) if cfg.inside_roots(&d) => d,
        Ok(_) => return Err(format!("{path} is outside allowed_roots")),
        Err(_) if !cfg.inside_roots(&resolve_missing(p)) => {
            return Err(format!("{path} is outside allowed_roots"))
        }
        Err(e) => return Err(format!("{path}: {e}")),
    };
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{path}: {e}"))?;
    let mut entries: Vec<DirEntry> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let ft = e.file_type().ok()?;
            let full = dir.join(&name);
            let is_dir = if ft.is_symlink() {
                full.canonicalize()
                    .is_ok_and(|t| t.is_dir() && cfg.inside_roots(&t))
            } else {
                ft.is_dir()
            };
            if !is_dir {
                return None;
            }
            Some(DirEntry {
                path: full.to_str()?.to_string(),
                name,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    entries.truncate(MAX_DIR_ENTRIES);
    Ok(FsListDirsResult { entries })
}

fn utf8(p: &Path) -> Result<String, String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

/// A path that does not exist, with its deepest existing ancestor canonicalized and the
/// rest normalized lexically: where it would be if it were created.
fn resolve_missing(p: &Path) -> PathBuf {
    let mut rest = vec![];
    let mut base = p;
    loop {
        if let Ok(c) = base.canonicalize() {
            let mut out = c;
            for comp in rest.iter().rev() {
                match comp {
                    Component::ParentDir => {
                        out.pop();
                    }
                    Component::CurDir => {}
                    c => out.push(c),
                }
            }
            return out;
        }
        match (base.parent(), base.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last);
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(root: &Path) -> Config {
        Config::parse(&format!(
            "hub_url = \"https://h\"\nallowed_roots = [{:?}]\n",
            root.display().to_string()
        ))
        .unwrap()
    }

    #[test]
    fn stat_inside_and_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("hqc")).unwrap();
        std::fs::write(root.join("notes.txt"), "").unwrap();
        let c = cfg(&root);
        let root = c.allowed_roots[0].clone();
        let s = |p: &Path| stat(&c, p.to_str().unwrap(), false).unwrap();

        let r = s(&root.join("hqc/."));
        assert!(r.exists && r.is_dir && r.inside_allowed_roots);
        assert_eq!(r.canonical.as_deref(), root.join("hqc").to_str());
        let r = s(&root.join("notes.txt"));
        assert!(r.exists && !r.is_dir && r.inside_allowed_roots);
        let r = s(&root.join("missing/deeper"));
        assert!(!r.exists && r.inside_allowed_roots && r.canonical.is_none());
        // Made on request, with its parents, only inside.
        let r = stat(&c, root.join("missing/deeper").to_str().unwrap(), true).unwrap();
        assert!(r.exists && r.is_dir && r.inside_allowed_roots);
        assert_eq!(r.canonical.as_deref(), root.join("missing/deeper").to_str());
        let r = stat(&c, dir.path().join("made-outside").to_str().unwrap(), true).unwrap();
        assert!(!r.exists && !r.inside_allowed_roots);
        assert!(!dir.path().join("made-outside").exists());
        // Outside: not even whether it exists.
        for p in [
            dir.path().to_path_buf(),
            root.join("../root2"),
            root.join(".."),
        ] {
            assert_eq!(
                s(&p),
                FsStatResult {
                    exists: false,
                    is_dir: false,
                    canonical: None,
                    inside_allowed_roots: false,
                    environment: None,
                },
                "{}",
                p.display()
            );
        }
        assert!(stat(&c, "relative/path", false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::create_dir_all(dir.path().join("secret/inner")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), root.join("out")).unwrap();
        let c = cfg(&root);
        let root = c.allowed_roots[0].clone();

        let r = stat(&c, root.join("link").to_str().unwrap(), false).unwrap();
        assert_eq!(r.canonical.as_deref(), root.join("real").to_str());
        let r = stat(&c, root.join("out").to_str().unwrap(), false).unwrap();
        assert!(!r.inside_allowed_roots && !r.exists);
        let r = stat(&c, root.join("out/missing").to_str().unwrap(), false).unwrap();
        assert!(!r.inside_allowed_roots, "missing below a link out");
        // Nor made there.
        let r = stat(&c, root.join("out/missing").to_str().unwrap(), true).unwrap();
        assert!(!r.inside_allowed_roots && !r.exists);
        assert!(!dir.path().join("secret/missing").exists());
        let r = stat(&c, root.join("link/made").to_str().unwrap(), true).unwrap();
        assert_eq!(r.canonical.as_deref(), root.join("real/made").to_str());

        let names: Vec<String> = list_dirs(&c, root.to_str())
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["link", "real"], "a link out is not listed");
        assert!(list_dirs(&c, root.join("out").to_str()).is_err());
        assert!(list_dirs(&c, root.join("out/inner").to_str()).is_err());
    }

    /// `fs.stat` names the environment a session in the folder runs in, by the bindings as
    /// they resolve now: a binding written before its folder existed, through a symlink made
    /// later, names the link's target; a missing folder is judged where it would be made;
    /// outside `allowed_roots` nothing is named.
    #[cfg(unix)]
    #[test]
    fn stat_names_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("root");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::create_dir_all(root.join("plain")).unwrap();
        let c = Config::parse(&format!(
            "hub_url = \"https://h\"\nallowed_roots = [{root:?}]\n[environments.infra]\n\
             [[env_bindings]]\npath = \"{}\"\nenvironment = \"infra\"\n",
            root.join("later").display()
        ))
        .unwrap();
        let env = |p: &Path| stat(&c, p.to_str().unwrap(), false).unwrap().environment;
        assert_eq!(env(&root.join("real")).as_deref(), Some("default"));
        std::os::unix::fs::symlink(root.join("real"), root.join("later")).unwrap();
        assert_eq!(env(&root.join("real")).as_deref(), Some("infra"));
        assert_eq!(env(&root.join("later")).as_deref(), Some("infra"));
        assert_eq!(env(&root.join("real/missing")).as_deref(), Some("infra"));
        assert_eq!(env(&root.join("plain")).as_deref(), Some("default"));
        assert_eq!(env(dir.path()), None);
    }

    #[test]
    fn list_dirs_inside_roots_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        for d in ["b", "A", "c/d", ".git"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("file"), "").unwrap();
        let c = cfg(&root);
        let root = c.allowed_roots[0].clone();

        let roots = list_dirs(&c, None).unwrap();
        assert_eq!(
            roots.entries,
            [DirEntry {
                name: "root".into(),
                path: root.to_str().unwrap().into()
            }]
        );
        let l = list_dirs(&c, root.to_str()).unwrap();
        let got: Vec<(&str, String)> = l
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.path.clone()))
            .collect();
        let want: Vec<(&str, String)> = ["A", "b", "c"]
            .iter()
            .map(|n| (*n, root.join(n).to_str().unwrap().to_string()))
            .collect();
        assert_eq!(got, want);

        for bad in [
            dir.path().to_path_buf(),
            root.join(".."),
            PathBuf::from("/"),
        ] {
            let e = list_dirs(&c, bad.to_str()).unwrap_err();
            assert!(e.contains("outside allowed_roots"), "{e}");
        }
        assert!(list_dirs(&c, Some("root")).is_err());
        assert!(list_dirs(&c, root.join("missing").to_str()).is_err());

        for i in 0..MAX_DIR_ENTRIES + 5 {
            std::fs::create_dir(root.join(format!("c/n{i:04}"))).unwrap();
        }
        let l = list_dirs(&c, root.join("c").to_str()).unwrap();
        assert_eq!(l.entries.len(), MAX_DIR_ENTRIES);
        assert_eq!(l.entries[0].name, "d");
    }
}
