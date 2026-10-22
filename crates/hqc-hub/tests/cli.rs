//! The hub binary's command line: only `--version`, everything else is environment.

use std::process::Command;

#[test]
fn version_and_unknown_argument() {
    let bin = env!("CARGO_BIN_EXE_hqc-hub");
    let out = Command::new(bin).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("hqc-hub {}", env!("CARGO_PKG_VERSION"))
    );

    // Must fail before reading the environment or touching the database.
    let out = Command::new(bin)
        .arg("--listen")
        .env_remove("HQC_DATABASE_URL")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown argument"));
}
