//! CLI tests for the writer-side `--encrypt` flag (flpdf-9hc.4.9 walking
//! skeleton: V=4 AES-128 only).
//!
//! Strategy: invoke `flpdf --encrypt …` on a plaintext fixture, then verify
//! the resulting encrypted PDF round-trips through qpdf's reader (the
//! independent oracle). The CLI's accept/reject matrix is also pinned here
//! so user-visible diagnostics remain stable.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const UNENCRYPTED_FIXTURE: &str = "../../tests/fixtures/minimal.pdf";
const ONE_PAGE_FIXTURE: &str = "../../tests/fixtures/compat/one-page.pdf";

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn ensure_qpdf_or_skip() -> bool {
    let available = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if available {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf required for --encrypt CLI tests on CI");
    }
    eprintln!("skipping: qpdf not available");
    false
}

/// Top-level alias: `flpdf --encrypt USER OWNER 128 --use-aes=y -- IN OUT`
/// produces an encrypted PDF that qpdf accepts with the user password.
#[test]
fn top_level_encrypt_v4_aes_128_round_trips_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--encrypt",
            "user-pw",
            "owner-pw",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    // Encrypted output must contain /Encrypt.
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "encrypted output must carry /Encrypt"
    );

    // qpdf accepts the user password and reports V=4 AESv2.
    let check = ShellCommand::new("qpdf")
        .arg("--password=user-pw")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(stdout.contains("R = 4"), "qpdf must report R=4: {stdout}");
    assert!(
        stdout.contains("Supplied password is user password"),
        "qpdf must accept user password: {stdout}"
    );
}

/// `rewrite` subcommand surface: identical semantics to the top-level alias.
#[test]
fn rewrite_subcommand_encrypt_v4_aes_128_round_trips_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let check = ShellCommand::new("qpdf")
        .arg("--password=user-pw")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --check failed: stderr={}",
        String::from_utf8_lossy(&check.stderr)
    );
}

/// Owner password also authenticates against the same output.
#[test]
fn encrypt_owner_password_authenticates_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user", "owner", "128", "--use-aes=y", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let check = ShellCommand::new("qpdf")
        .arg("--password=owner")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("Supplied password is owner password"),
        "qpdf must accept owner password: {stdout}"
    );
}

/// Encrypting a fixture with streams + content strings
/// (`compat/one-page.pdf`) and then decrypting via qpdf must produce a
/// structurally valid plaintext PDF that passes `qpdf --check`.
///
/// Object-graph byte-equality between the original input and the
/// round-tripped output is deferred to flpdf-9hc.4.12 (the explicit
/// "encrypt round-trip + cross-implementation cross-check" task): flpdf's
/// `full_rewrite` path doesn't preserve source object numbering, so a
/// byte-level qpdf JSON v1 comparison diverges in a way that says
/// nothing about encryption correctness.
#[test]
fn encrypt_round_trip_on_one_page_decrypts_cleanly_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let encrypted = tmp.path().join("encrypted.pdf");
    let decrypted = tmp.path().join("decrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user", "owner", "128", "--use-aes=y", "--"])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&encrypted)
        .assert()
        .success();

    // qpdf --decrypt produces a plaintext output without error.
    let decrypt = ShellCommand::new("qpdf")
        .arg("--password=user")
        .arg("--decrypt")
        .arg(&encrypted)
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        decrypt.status.success(),
        "qpdf --decrypt failed: {}",
        String::from_utf8_lossy(&decrypt.stderr)
    );

    // The decrypted output passes structural validation.
    let check = ShellCommand::new("qpdf")
        .arg("--check")
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --check on round-tripped plaintext failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

// ── Validation matrix ───────────────────────────────────────────────────────
//
// User-visible diagnostics are pinned here so future scope expansions to the
// `parse_encrypt_segment` accept matrix don't silently change error messages
// that users may grep for.

#[test]
fn encrypt_key_len_40_is_rejected_with_v1_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "40", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("KEY-LEN=40"))
        .stderr(predicates::str::contains("V=1"));
    assert!(!output.exists());
}

#[test]
fn encrypt_key_len_256_is_rejected_with_v5_r6_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("KEY-LEN=256"))
        .stderr(predicates::str::contains("V=5 R=6"));
    assert!(!output.exists());
}

#[test]
fn encrypt_128_without_use_aes_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "128", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--use-aes=y"));
    assert!(!output.exists());
}

#[test]
fn encrypt_use_aes_n_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "128", "--use-aes=n", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("RC4"));
}

#[test]
fn encrypt_permission_sub_flags_are_rejected_with_followup_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--print=none",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("not yet supported"))
        .stderr(predicates::str::contains("flpdf-9hc.4.9"));
}

#[test]
fn encrypt_invalid_key_len_value_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "not-a-number", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("KEY-LEN"));
}

#[test]
fn encrypt_conflicts_with_check_inspection_path() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", "--encrypt", "u", "o", "128", "--use-aes=y", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used"));
}

#[test]
fn encrypt_conflicts_with_decrypt_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--decrypt",
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used"));
}
