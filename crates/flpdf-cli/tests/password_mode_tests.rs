use assert_cmd::Command;
use flpdf::{EncryptParams, Pdf, PdfWriter};
use std::fs;
use std::io::Cursor;
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

fn encrypted_fixture_with_password(password: &[u8]) -> Vec<u8> {
    let input_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = fs::read(input_path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(input)).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_encryption_parameters(EncryptParams::v5_r6(password.to_vec(), b"owner".to_vec()));
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    writer.get_buffer().unwrap()
}

#[test]
fn auto_mode_authenticates_composed_nfc_password() {
    // The fixture was qpdf-encrypted with user password "café" (NFC composed).
    check_cmd("v5-aes-256-r6-utf8.pdf", "café", None)
        .assert()
        .success();
}

#[test]
fn unicode_mode_reads_non_utf8_password_file_as_raw_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("encrypted.pdf");
    let password_file = temp.path().join("password.bin");
    let password = b"\xff\xfeA";
    fs::write(&input, encrypted_fixture_with_password(password)).unwrap();
    fs::write(&password_file, password).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "check",
            "--password-mode=unicode",
            &format!("--password-file={}", password_file.display()),
        ])
        .arg(&input)
        .assert()
        .success();
}

/// qpdf's `getTrimmedUserPassword()`/`QPDFJob::showEncryption` report the raw
/// authenticated password bytes verbatim; a password read from
/// `--password-file` can be non-UTF-8. The CLI must not lossy-convert those
/// bytes (which would replace them with U+FFFD) when rendering the
/// `--show-encryption` report.
#[test]
fn top_level_show_encryption_preserves_non_utf8_password_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("encrypted.pdf");
    let password_file = temp.path().join("password.bin");
    let password = b"\xff\xfeA";
    fs::write(&input, encrypted_fixture_with_password(password)).unwrap();
    fs::write(&password_file, password).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--show-encryption",
            "--password-mode=unicode",
            &format!("--password-file={}", password_file.display()),
        ])
        .arg(&input)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "show-encryption must succeed with the correct (non-UTF8) password: {:?}",
        output.status
    );
    let needle = b"User password = \xff\xfeA\n";
    assert!(
        output
            .stdout
            .windows(needle.len())
            .any(|window| window == needle),
        "the raw password bytes must appear verbatim in the report, not \
         lossy-converted to U+FFFD replacement characters: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn explicit_unicode_mode_authenticates_composed_password() {
    check_cmd("v5-aes-256-r6-utf8.pdf", "café", Some("unicode"))
        .assert()
        .success();
}

#[test]
fn unicode_mode_preserves_decomposed_password_bytes() {
    // qpdf 11.9.0 validates UTF-8 in Unicode mode but does not apply SASLprep
    // or NFC normalization. The decomposed bytes therefore do not match the
    // fixture encrypted with composed "café".
    check_cmd("v5-aes-256-r6-utf8.pdf", "cafe\u{301}", Some("unicode"))
        .assert()
        .failure();
}

#[test]
fn bytes_mode_does_not_normalize_decomposed_password() {
    // The raw decomposed UTF-8 bytes do not match the fixture's composed key.
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
fn unicode_mode_is_raw_on_legacy_revision() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check")
        .arg(fixture("v2-rc4-128-r3.pdf"))
        .arg("--password=user-v2")
        .arg("--allow-weak-crypto")
        .arg("--password-mode=unicode");
    cmd.assert().success();
}

#[test]
fn auto_mode_is_regression_free_for_legacy_ascii_password() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check")
        .arg(fixture("v2-rc4-128-r3.pdf"))
        .arg("--password=user-v2")
        .arg("--allow-weak-crypto");
    // The legacy ASCII password authenticates under auto mode, so `check`
    // inspects the file and exits 0. `check` treats weak (RC4) files as a
    // read-only inspection with no weak-crypto warning, matching qpdf (which
    // exits 0 here with or without --allow-weak-crypto).
    cmd.assert().code(0);
}
