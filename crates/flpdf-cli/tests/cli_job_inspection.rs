//! Ordinary read-only inspection routes through the qpdf-shaped QPDFJob boundary.

use assert_cmd::Command;
use std::process::Command as ShellCommand;

const ONE_PAGE_PDF: &str = "../../tests/fixtures/compat/one-page.pdf";
const REPAIRABLE_PDF: &str = "../../tests/fixtures/test_driver/repairable_input.pdf";
const WEAK_RC4_PDF: &str = "../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf";

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf").arg("--version").output().ok();
    let is_expected = version.as_ref().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).lines().next() == Some("qpdf version 11.9.0")
    });
    if is_expected {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for ordinary inspection oracle tests");
    }
    eprintln!("skipping ordinary inspection oracle: qpdf 11.9.0 is not available");
    true
}

fn normalize_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }
    normalized
}

#[test]
fn ordinary_show_npages_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-npages", ONE_PAGE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-npages", ONE_PAGE_PDF])
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_pages_preserves_qpdf_page_identity_line() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-pages", ONE_PAGE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-pages", ONE_PAGE_PDF])
        .output()
        .unwrap();

    assert!(qpdf.status.success());
    assert!(flpdf.status.success());
    let qpdf_stdout = String::from_utf8_lossy(&qpdf.stdout);
    let qpdf_first_line = qpdf_stdout
        .lines()
        .next()
        .expect("qpdf must emit one page identity line");
    assert!(
        String::from_utf8_lossy(&flpdf.stdout).starts_with(qpdf_first_line),
        "flpdf page identity must retain qpdf's first line: {:?}",
        flpdf.stdout
    );
}

#[test]
fn ordinary_show_npages_completes_repair_warnings_with_status_three() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-npages", REPAIRABLE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--repair", "--show-npages", REPAIRABLE_PDF])
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(3));
    assert_eq!(flpdf.status.code(), Some(3));
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_npages_matches_qpdf_without_weak_crypto_advisory() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--show-npages",
            "--allow-weak-crypto",
            "--password=user-v2",
            WEAK_RC4_PDF,
        ])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--show-npages",
            "--allow-weak-crypto",
            "--password=user-v2",
            WEAK_RC4_PDF,
        ])
        .output()
        .unwrap();

    assert!(qpdf.status.success());
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert!(!String::from_utf8_lossy(&flpdf.stderr).contains("encrypted PDF uses weak crypto"));
}
