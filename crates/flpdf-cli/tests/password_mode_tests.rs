use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted")
        .join(name)
}

fn check_cmd(fixture_name: &str, password: &str, mode: Option<&str>) -> Command {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check").arg(fixture(fixture_name));
    cmd.arg(format!("--password={password}"));
    if let Some(mode) = mode {
        cmd.arg(format!("--password-mode={mode}"));
    }
    cmd
}

#[test]
fn auto_mode_authenticates_composed_nfc_password() {
    // The fixture was qpdf-encrypted with user password "café" (NFC composed).
    check_cmd("v5-aes-256-r6-utf8.pdf", "café", None)
        .assert()
        .success();
}

#[test]
fn explicit_unicode_mode_authenticates_composed_password() {
    check_cmd("v5-aes-256-r6-utf8.pdf", "café", Some("unicode"))
        .assert()
        .success();
}

#[test]
fn unicode_mode_normalizes_decomposed_password_to_composed() {
    // "cafe" + COMBINING ACUTE ACCENT (U+0301) — SASLprep / NFC must fold
    // this to the composed form the fixture was encrypted with.
    check_cmd("v5-aes-256-r6-utf8.pdf", "cafe\u{301}", Some("unicode"))
        .assert()
        .success();
}

#[test]
fn bytes_mode_does_not_normalize_decomposed_password() {
    // Without SASLprep the decomposed UTF-8 bytes do not match the fixture's
    // composed key, so authentication must fail.
    check_cmd("v5-aes-256-r6-utf8.pdf", "cafe\u{301}", Some("bytes"))
        .assert()
        .failure();
}

#[test]
fn hex_bytes_mode_decodes_password() {
    // Composed UTF-8 "café" = 0x63 0x61 0x66 0xC3 0xA9.
    check_cmd("v5-aes-256-r6-utf8.pdf", "636166c3a9", Some("hex-bytes"))
        .assert()
        .success();
}

#[test]
fn hex_bytes_mode_tolerates_whitespace_separators() {
    check_cmd(
        "v5-aes-256-r6-utf8.pdf",
        "63 61 66 c3 a9",
        Some("hex-bytes"),
    )
    .assert()
    .success();
}

#[test]
fn auto_mode_is_regression_free_for_ascii_password() {
    check_cmd("v5-aes-256-r6.pdf", "user-v5-r6", None)
        .assert()
        .success();
}

#[test]
fn bytes_mode_is_regression_free_for_ascii_password_on_v5() {
    check_cmd("v5-aes-256-r6.pdf", "user-v5-r6", Some("bytes"))
        .assert()
        .success();
}

#[test]
fn unicode_mode_works_for_ascii_password_on_v5() {
    check_cmd("v5-aes-256-r6.pdf", "user-v5-r6", Some("unicode"))
        .assert()
        .success();
}

#[test]
fn unicode_mode_rejected_on_legacy_revision() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check")
        .arg(fixture("v2-rc4-128-r3.pdf"))
        .arg("--password=user-v2")
        .arg("--allow-weak-crypto")
        .arg("--password-mode=unicode");
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("only supported for V=5"));
}

#[test]
fn auto_mode_is_regression_free_for_legacy_ascii_password() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check")
        .arg(fixture("v2-rc4-128-r3.pdf"))
        .arg("--password=user-v2")
        .arg("--allow-weak-crypto");
    // RC4 (weak crypto) triggers a warning → exit 3 (qpdf-compatible:
    // warnings found, no errors).
    cmd.assert().code(3);
}
