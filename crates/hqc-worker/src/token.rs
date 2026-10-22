//! Worker token storage (SPEC §5.1): macOS Keychain, elsewhere a 0600 file.
//!
//! `HQC_TOKEN_STORE=file` forces the file backend (`$HQC_HOME/token`) on any OS; tests use
//! it so they never touch the Keychain. The token is wrapped in [`Secret`] so that it
//! cannot end up in logs through `Debug`.

use std::fmt;
use std::path::PathBuf;

use anyhow::Context;

/// A worker token. `Debug` and `Display` never print the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone)]
pub enum TokenStore {
    /// One generic password per hub URL, service [`KEYCHAIN_SERVICE`].
    #[cfg(target_os = "macos")]
    Keychain {
        account: String,
    },
    File(PathBuf),
}

#[cfg(target_os = "macos")]
pub const KEYCHAIN_SERVICE: &str = "hqc-worker";

impl TokenStore {
    /// The store for tokens of `hub_url`: `HQC_TOKEN_STORE=file` or a non-macOS OS selects
    /// the file `$HQC_HOME/token`, otherwise the Keychain.
    pub fn from_env(hub_url: &str) -> anyhow::Result<Self> {
        let file = std::env::var("HQC_TOKEN_STORE").is_ok_and(|v| v == "file");
        #[cfg(target_os = "macos")]
        if !file {
            return Ok(Self::Keychain {
                account: hub_url.trim_end_matches('/').to_string(),
            });
        }
        let _ = (file, hub_url);
        Ok(Self::File(crate::config::hqc_home()?.join("token")))
    }

    pub fn save(&self, token: &Secret) -> anyhow::Result<()> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain { account } => security_framework::passwords::set_generic_password(
                KEYCHAIN_SERVICE,
                account,
                token.expose().as_bytes(),
            )
            .context("saving the worker token to the Keychain"),
            Self::File(path) => write_private(path, token.expose().as_bytes())
                .with_context(|| format!("saving the worker token to {}", path.display())),
        }
    }

    /// `Ok(None)`: no token yet, run `hqc-worker enroll`.
    pub fn load(&self) -> anyhow::Result<Option<Secret>> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain { account } => {
                match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, account)
                {
                    Ok(b) => Ok(Some(Secret(
                        String::from_utf8(b).context("Keychain token is not UTF-8")?,
                    ))),
                    // errSecItemNotFound
                    Err(e) if e.code() == -25300 => Ok(None),
                    Err(e) => Err(e).context("reading the worker token from the Keychain"),
                }
            }
            Self::File(path) => match std::fs::read_to_string(path) {
                Ok(s) => Ok(Some(Secret(s.trim().to_string()))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
            },
        }
    }
}

/// Write via a temp file created with mode 0600 and rename, so the token is never readable
/// by others, not even for a moment. The directory is created 0700.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)?;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_roundtrip_0600() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::File(dir.path().join("sub/token"));
        assert_eq!(store.load().unwrap(), None);
        store.save(&Secret::new("wt_1")).unwrap();
        store.save(&Secret::new("wt_2")).unwrap();
        assert_eq!(store.load().unwrap(), Some(Secret::new("wt_2")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(dir.path().join("sub/token")), 0o600);
            assert_eq!(mode(dir.path().join("sub")), 0o700);
        }
    }

    #[test]
    fn secret_is_not_printed() {
        let s = Secret::new("wt_very_secret");
        assert!(!format!("{s:?}").contains("very_secret"));
    }
}
