//! qpdf 11.9.0 parity for V=5 `/Perms` warning text and exit status.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "11.9.0";
const V5_R5: &str = "../../tests/fixtures/encrypted/v5-aes-256-r5.pdf";
const V5_R6: &str = "../../tests/fixtures/encrypted/v5-aes-256-r6.pdf";

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            let version = String::from_utf8_lossy(&output.stdout);
            let expected = format!("qpdf version {EXPECTED_QPDF_VERSION}");
            output.status.success()
                && version.lines().next().map(str::trim) == Some(expected.as_str())
        })
        .unwrap_or(false)
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn corrupted_hex_field(source: &str, output_directory: &Path, revision: u8, key: &str) -> PathBuf {
    let input = fs::read(fixture_path(source)).expect("read committed qpdf V=5 fixture");
    let mut bytes = input.clone();
    let marker = format!("/{key} <");
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .unwrap_or_else(|| panic!("fixture is missing /{key}"))
        + marker.len();
    let first = bytes[start];
    assert!(
        first.is_ascii_hexdigit(),
        "/{key} must start with hex digits"
    );
    bytes[start] = if first == b'0' { b'1' } else { b'0' };

    let path = output_directory.join(format!("r{revision}-bad-{key}.pdf"));
    fs::write(&path, bytes).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    path
}

fn run_qpdf(path: &Path, password: &str) -> Output {
    ShellCommand::new("qpdf")
        .args([
            format!("--password={password}"),
            "--show-encryption".to_owned(),
            "--show-encryption-key".to_owned(),
        ])
        .arg(path)
        .output()
        .expect("run qpdf encryption inspection")
}

fn run_flpdf(path: &Path, password: &str) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf CLI binary")
        .args([
            format!("--password={password}"),
            "--show-encryption".to_owned(),
            "--show-encryption-key".to_owned(),
        ])
        .arg(path)
        .output()
        .expect("run flpdf encryption inspection")
}

#[test]
fn v5_perms_warning_text_context_and_exit_match_qpdf_for_r5_and_r6() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf {EXPECTED_QPDF_VERSION} is required for the V=5 /Perms oracle test");
        }
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} not available; skipping V=5 /Perms warning test");
        return;
    }

    let directory = tempfile::tempdir().expect("create /Perms parity directory");
    for (revision, source, user_password, owner_password) in [
        (5, V5_R5, "user-v5-r5", "owner-v5-r5"),
        (6, V5_R6, "user-v5-r6", "owner-v5-r6"),
    ] {
        for (key, password) in [
            ("UE", user_password),
            ("OE", owner_password),
            ("Perms", user_password),
        ] {
            let path = corrupted_hex_field(source, directory.path(), revision, key);
            let qpdf = run_qpdf(&path, password);
            let flpdf = run_flpdf(&path, password);

            assert_eq!(
                qpdf.status.code(),
                Some(3),
                "qpdf should accept the password and report the /Perms warning for R{revision} /{key}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&qpdf.stdout),
                String::from_utf8_lossy(&qpdf.stderr)
            );
            assert_eq!(
                flpdf.status.code(),
                qpdf.status.code(),
                "flpdf exit differs for R{revision} /{key}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&flpdf.stdout),
                String::from_utf8_lossy(&flpdf.stderr)
            );
            assert_eq!(
                flpdf.stdout, qpdf.stdout,
                "R{revision} /{key} report differs"
            );
            let normalized_flpdf_stderr =
                String::from_utf8_lossy(&flpdf.stderr).replace("flpdf:", "qpdf:");
            assert_eq!(
                normalized_flpdf_stderr.as_bytes(),
                qpdf.stderr,
                "R{revision} /{key} warning text or context differs"
            );
        }
    }
}
