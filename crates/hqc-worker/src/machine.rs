//! Machine fingerprint (T21, SPEC §5.1): sent at enrollment and in every `hello` so that the
//! hub can offer replacing the worker that ran on the same machine before.
//!
//! The stable id of the machine — macOS `IOPlatformUUID` (`ioreg`), Linux `/etc/machine-id`,
//! Windows `MachineGuid` from the registry, otherwise a random id kept in
//! `$HQC_HOME/machine_id` — never leaves the machine: only `sha256("hqc" + id)` in hex does.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The fingerprint of this machine; `None` only if no id could be read or kept.
pub fn fingerprint(hqc_home: Option<&Path>) -> Option<String> {
    let id = platform_id().or_else(|| hqc_home.and_then(stored_id))?;
    Some(hash(&id))
}

/// `sha256("hqc" + id)`, lowercase hex.
pub fn hash(id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"hqc");
    h.update(id.trim().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn platform_id() -> Option<String> {
    let id = if cfg!(target_os = "macos") {
        let out = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        io_platform_uuid(&String::from_utf8_lossy(&out.stdout))
    } else if cfg!(target_os = "windows") {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ])
            .output()
            .ok()?;
        machine_guid(&String::from_utf8_lossy(&out.stdout))
    } else {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
    };
    id.filter(|s| !s.is_empty())
}

/// `"IOPlatformUUID" = "…"` in the output of `ioreg -rd1 -c IOPlatformExpertDevice`.
fn io_platform_uuid(out: &str) -> Option<String> {
    let line = out.lines().find(|l| l.contains("\"IOPlatformUUID\""))?;
    let value = line.split_once('=')?.1.trim().trim_matches('"');
    Some(value.to_string())
}

/// `MachineGuid    REG_SZ    …` in the output of `reg query`.
fn machine_guid(out: &str) -> Option<String> {
    let line = out.lines().find(|l| l.contains("MachineGuid"))?;
    line.split_whitespace().last().map(str::to_string)
}

/// A random id kept in `<hqc_home>/machine_id` (0600), created once.
fn stored_id(home: &Path) -> Option<String> {
    let file = home.join("machine_id");
    if let Ok(s) = std::fs::read_to_string(&file) {
        if !s.trim().is_empty() {
            return Some(s.trim().to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(home).ok()?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    std::io::Write::write_all(&mut o.open(&file).ok()?, id.as_bytes()).ok()?;
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_platform_output() {
        let ioreg = r#"+-o J316sAP  <class IOPlatformExpertDevice, id 0x100000213>
    {
      "IOPlatformSerialNumber" = "XYZ"
      "IOPlatformUUID" = "8D1C2E3F-0000-1111-2222-333344445555"
    }"#;
        assert_eq!(
            io_platform_uuid(ioreg).as_deref(),
            Some("8D1C2E3F-0000-1111-2222-333344445555")
        );
        assert_eq!(io_platform_uuid("nothing"), None);
        let reg = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Cryptography\r\n    MachineGuid    REG_SZ    1f2e3d4c-aaaa-bbbb-cccc-0123456789ab\r\n";
        assert_eq!(
            machine_guid(reg).as_deref(),
            Some("1f2e3d4c-aaaa-bbbb-cccc-0123456789ab")
        );
    }

    #[test]
    fn hash_is_salted_and_hides_the_id() {
        let h = hash("8D1C2E3F-0000-1111-2222-333344445555");
        assert_eq!(h.len(), 64);
        assert!(h
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(!h.contains("8d1c2e3f"));
        assert_eq!(h, hash(" 8D1C2E3F-0000-1111-2222-333344445555\n"));
        // Salted: not the plain sha256 of the id.
        let plain: String = Sha256::digest(b"8D1C2E3F-0000-1111-2222-333344445555")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_ne!(h, plain);
    }

    #[test]
    fn stored_id_is_stable() {
        let d = tempfile::tempdir().unwrap();
        let a = stored_id(d.path()).unwrap();
        assert_eq!(stored_id(d.path()).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.path().join("machine_id"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn this_machine_has_a_fingerprint() {
        let d = tempfile::tempdir().unwrap();
        let f = fingerprint(Some(d.path())).unwrap();
        assert_eq!(f, fingerprint(Some(d.path())).unwrap());
    }
}
