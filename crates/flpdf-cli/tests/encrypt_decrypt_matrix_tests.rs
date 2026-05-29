//! flpdf-9hc.4.12: encrypt → cross-implementation decrypt matrix.
//!
//! Direction **(a) flpdf encrypt → qpdf decrypt**: flpdf encrypts each Standard
//! security-handler variant the writer supports, and qpdf must authenticate
//! and decrypt the result to clean plaintext. This is the writer-side half of
//! the cross-implementation matrix.
//!
//! Directions **(b) qpdf encrypt → flpdf decrypt** and **(c) flpdf --decrypt
//! round-trip** are already covered, for the full qpdf-generated fixture
//! matrix (v1-rc4-40 / v2-rc4-128 / v4-rc4-128 / v4-aes-128 / v5-aes-256-r5 /
//! v5-aes-256-r6), by `encrypted_rewrite_tests.rs`
//! (`encrypted_fixtures_rewrite_to_plaintext_matching_qpdf_decrypt_objects`),
//! so they are intentionally not duplicated here.
//!
//! Documented edge cases per the bead acceptance: the empty-user-password row
//! is covered below; "owner-only restrictions" and "/EncryptMetadata=false"
//! require the `--encrypt` permission sub-flags (flpdf-9hc.4.9.5) and
//! `--cleartext-metadata` (flpdf-9hc.4.9.6) CLI wiring, which are not yet
//! implemented — those rows are tracked on those beads.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const FIXTURE: &str = "../../tests/fixtures/compat/one-page.pdf";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
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
        panic!("qpdf required for the encrypt/decrypt matrix tests on CI");
    }
    eprintln!("skipping: qpdf not available");
    false
}

/// (label, `--encrypt` arg vector incl. `--allow-weak-crypto`/`--` where
/// needed, expected qpdf `R =` line).
const WRITE_MATRIX: &[(&str, &[&str], &str)] = &[
    (
        "v1-rc4-40",
        &["--allow-weak-crypto", "--encrypt", "u", "o", "40", "--"],
        "R = 2",
    ),
    (
        "v2-rc4-128",
        &["--allow-weak-crypto", "--encrypt", "u", "o", "128", "--"],
        "R = 3",
    ),
    (
        "v4-rc4-128",
        &[
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "128",
            "--force-V4",
            "--",
        ],
        "R = 4",
    ),
    (
        "v4-aes-128",
        &["--encrypt", "u", "o", "128", "--use-aes=y", "--"],
        "R = 4",
    ),
    ("v5-aes-256", &["--encrypt", "u", "o", "256", "--"], "R = 6"),
];

/// (a) For every writer-supported handler, flpdf-encrypted output must (1)
/// authenticate under qpdf with the expected revision and (2) decrypt cleanly
/// via `qpdf --decrypt` to plaintext that no longer carries `/Encrypt`.
#[test]
fn flpdf_encrypt_matrix_decrypts_cleanly_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    for (label, enc_args, expected_r) in WRITE_MATRIX {
        let enc = tmp.path().join(format!("{label}.pdf"));
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        for a in *enc_args {
            cmd.arg(a);
        }
        cmd.arg(fixture()).arg(&enc).assert().success();

        // qpdf authenticates the user password and reports the expected R.
        let show = ShellCommand::new("qpdf")
            .arg("--password=u")
            .arg("--show-encryption")
            .arg(&enc)
            .output()
            .unwrap();
        assert!(
            show.status.success(),
            "{label}: qpdf --show-encryption failed: {}",
            String::from_utf8_lossy(&show.stderr)
        );
        let s = String::from_utf8_lossy(&show.stdout);
        assert!(
            s.contains(expected_r) && s.contains("Supplied password is user password"),
            "{label}: expected {expected_r:?} + user-password match, got: {s}"
        );

        // qpdf --decrypt produces clean plaintext (full content round-trip).
        let dec = tmp.path().join(format!("{label}-dec.pdf"));
        let d = ShellCommand::new("qpdf")
            .arg("--password=u")
            .arg("--decrypt")
            .arg(&enc)
            .arg(&dec)
            .output()
            .unwrap();
        assert!(
            d.status.success(),
            "{label}: qpdf --decrypt failed: {}",
            String::from_utf8_lossy(&d.stderr)
        );
        let bytes = std::fs::read(&dec).unwrap();
        assert!(
            !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
            "{label}: qpdf-decrypted plaintext must not carry /Encrypt"
        );
    }
}

/// Edge case (empty user password): a 256-bit file with an EMPTY user password
/// and a non-empty owner password opens without a password. qpdf must decrypt
/// it with no `--password`. (The insecure gate is for non-empty-user +
/// empty-owner, so it does not fire here.)
#[test]
fn flpdf_encrypt_empty_user_password_decrypts_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let enc = tmp.path().join("empty-user.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "", "owner-pw", "256", "--"])
        .arg(fixture())
        .arg(&enc)
        .assert()
        .success();

    let dec = tmp.path().join("empty-user-dec.pdf");
    let d = ShellCommand::new("qpdf")
        .arg("--decrypt")
        .arg(&enc)
        .arg(&dec)
        .output()
        .unwrap();
    assert!(
        d.status.success(),
        "qpdf --decrypt with an empty user password failed: {}",
        String::from_utf8_lossy(&d.stderr)
    );
    let bytes = std::fs::read(&dec).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "decrypted empty-user output must not carry /Encrypt"
    );
}
