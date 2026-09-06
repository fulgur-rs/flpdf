//! CLI integration tests for the qpdf-compatible encryption inspection
//! subcommands: `show-encryption`, `is-encrypted`,
//! `requires-password`, `show-encryption-key`.
//!
//! # Exit-code semantics
//!
//! Source: qpdf manual (option tables + "Exit Status")
//!   <https://qpdf.readthedocs.io/en/stable/cli.html>
//! Confirmed by `qpdf/include/qpdf/Constants.h` `enum qpdf_exit_code_e`:
//!   qpdf_exit_success          = 0
//!   qpdf_exit_error            = 2
//!   qpdf_exit_is_not_encrypted = 2   (--is-encrypted / --requires-password)
//!   qpdf_exit_correct_password = 3   (--requires-password)
//!
//! # Fixture matrix (ground truth captured from qpdf 11.9.0)
//!
//! | Fixture                       | password    | is-enc | req-pw | file key |
//! |-------------------------------|-------------|--------|--------|----------|
//! | compat/encrypted-r4-three-page| (empty)     | 0      | 3      | n/a      |
//! | encrypted/v4-aes-128-r4       | (none/wrong)| 0      | 0      | (auth fails) |
//! | encrypted/v4-aes-128-r4       | user-v4-aes | 0      | 3      | 5042ec…  |
//! | encrypted/v5-aes-256-r6       | user-v5-r6  | 0      | 3      | fc4594…  |
//! | encrypted/v2-rc4-128-r3 (weak)| user-v2     | 0      | 3      | 09d565…  |
//! | encrypted/v2-rc4-128-r3 (weak)| (none/wrong)| 0      | 0      | (auth fails) |
//! | encrypted/v5-aes-256-r5 (weak)| user-v5-r5  | 0      | 3      | c3d812…  |
//! | encrypted/v5-aes-256-r5 (weak)| (none/wrong)| 0      | 0      | (auth fails) |
//! | fixtures/minimal.pdf          | —           | 2      | 2      | n/a      |
//!
//! Reference keys verified with
//!   `qpdf --show-encryption-key --check --password=… FIXTURE`.
//! Weak-crypto (RC4 / R=5) req-pw codes verified with
//!   `qpdf --requires-password [--password=…] FIXTURE`: qpdf does
//! NOT require `--allow-weak-crypto` for this read-only inspection. The same
//! applies to `show-encryption` / `show-encryption-key`: qpdf derives the key
//! and prints the encryption block for a weak file with the correct password
//! and no `--allow-weak-crypto`, so flpdf does too.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;
use std::process::{Command as ShellCommand, Stdio};

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

/// Collapse a live qpdf subprocess's CRLF-terminated text lines to bare `\n`.
/// On Windows, `qpdf.exe`'s own C-runtime stdout is opened in text mode and
/// translates every `\n` write to `\r\n`; flpdf's shared CLI logger applies
/// the same platform conversion for text output. Comparing raw bytes remains
/// safe for the qpdf differential checks, while the helper also documents the
/// platform boundary (same pattern as `cli_logger_routing.rs`/
/// `cli_attachment_lifecycle.rs`/`encrypt_cli_tests.rs`).
fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
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

const R4_EMPTY_PW: &str = "../../tests/fixtures/compat/encrypted-r4-three-page.pdf";
const V4_AES: &str = "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf";
const V5_R6: &str = "../../tests/fixtures/encrypted/v5-aes-256-r6.pdf";
// Weak-crypto fixtures (RC4 / R=5): qpdf answers --requires-password on these
// without --allow-weak-crypto.
const V2_RC4: &str = "../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf";
const V5_R5: &str = "../../tests/fixtures/encrypted/v5-aes-256-r5.pdf";
const UNENCRYPTED: &str = "../../tests/fixtures/minimal.pdf";

fn flpdf() -> Command {
    Command::cargo_bin("flpdf").unwrap()
}

// ---------------------------------------------------------------------------
// is-encrypted: exit 0 if encrypted, exit 2 if not (qpdf --is-encrypted)
// ---------------------------------------------------------------------------

#[test]
fn is_encrypted_encrypted_no_password_exits_0() {
    // qpdf --is-encrypted works without the password; flpdf must too.
    flpdf().args(["is-encrypted", V4_AES]).assert().success();
}

#[test]
fn is_encrypted_encrypted_empty_password_exits_0() {
    flpdf()
        .args(["is-encrypted", R4_EMPTY_PW])
        .assert()
        .success();
}

#[test]
fn is_encrypted_unencrypted_exits_2() {
    // qpdf_exit_is_not_encrypted = 2.
    flpdf().args(["is-encrypted", UNENCRYPTED]).assert().code(2);
}

#[test]
fn is_encrypted_weak_rc4_no_password_exits_0() {
    // A weak (RC4) file is still encrypted; is-encrypted reports 0 without a
    // password and without --allow-weak-crypto.
    flpdf().args(["is-encrypted", V2_RC4]).assert().success();
}

#[test]
fn top_level_is_encrypted_matches_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let qpdf = ShellCommand::new("qpdf")
        .args(["--is-encrypted", "--password=", R4_EMPTY_PW])
        .output()
        .unwrap();
    assert_eq!(qpdf.status.code(), Some(0));

    flpdf()
        .args(["--is-encrypted", "--password=", R4_EMPTY_PW])
        .assert()
        .code(qpdf.status.code().unwrap());
}

#[test]
fn top_level_requires_password_matches_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let qpdf = ShellCommand::new("qpdf")
        .args(["--requires-password", "--password=user-v4-aes", V4_AES])
        .output()
        .unwrap();
    assert_eq!(qpdf.status.code(), Some(3));

    flpdf()
        .args(["--requires-password", "--password=user-v4-aes", V4_AES])
        .assert()
        .code(qpdf.status.code().unwrap());
}

#[test]
fn top_level_empty_is_encrypted_matches_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let qpdf = ShellCommand::new("qpdf")
        .args(["--empty", "--is-encrypted"])
        .output()
        .unwrap();
    assert_eq!(qpdf.status.code(), Some(2));
    assert!(qpdf.stdout.is_empty());
    assert!(qpdf.stderr.is_empty());

    flpdf()
        .args(["--empty", "--is-encrypted"])
        .assert()
        .code(2)
        .stdout(predicate::eq(""))
        .stderr(predicate::eq(""));
}

#[test]
fn top_level_empty_requires_password_matches_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let qpdf = ShellCommand::new("qpdf")
        .args(["--empty", "--requires-password"])
        .output()
        .unwrap();
    assert_eq!(qpdf.status.code(), Some(2));
    assert!(qpdf.stdout.is_empty());
    assert!(qpdf.stderr.is_empty());

    flpdf()
        .args(["--empty", "--requires-password"])
        .assert()
        .code(2)
        .stdout(predicate::eq(""))
        .stderr(predicate::eq(""));
}

#[test]
fn top_level_is_encrypted_requires_an_input() {
    flpdf()
        .arg("--is-encrypted")
        .assert()
        .code(2)
        .stderr(predicate::eq(format!(
            "{EOL}flpdf: an input file name is required{EOL}{EOL}For help:{EOL}  flpdf --help=usage       usage information{EOL}  flpdf --help=topic       help on a topic{EOL}  flpdf --help=--option    help on an option{EOL}  flpdf --help             general help and a topic list{EOL}{EOL}"
        )));
}

#[test]
fn top_level_requires_password_requires_an_input() {
    flpdf()
        .arg("--requires-password")
        .assert()
        .code(2)
        .stderr(predicate::eq(format!(
            "{EOL}flpdf: an input file name is required{EOL}{EOL}For help:{EOL}  flpdf --help=usage       usage information{EOL}  flpdf --help=topic       help on a topic{EOL}  flpdf --help=--option    help on an option{EOL}  flpdf --help             general help and a topic list{EOL}{EOL}"
        )));
}

#[test]
fn password_file_uses_only_the_first_line() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let password_file = temp.path().join("password.txt");
    std::fs::write(&password_file, b"user-v4-aes\nignored\n").unwrap();
    let password_arg = format!("--password-file={}", password_file.display());

    let qpdf = ShellCommand::new("qpdf")
        .args(["--check", &password_arg, V4_AES])
        .output()
        .unwrap();
    assert!(qpdf.status.success());

    let flpdf = flpdf()
        .args(["--check", &password_arg, V4_AES])
        .output()
        .unwrap();
    assert!(flpdf.status.success());
    assert_eq!(
        flpdf.stderr,
        format!("flpdf: WARNING: all but the first line of the password file are ignored{EOL}")
            .into_bytes()
    );
}

#[test]
fn password_file_dash_reads_from_stdin() {
    if !ensure_qpdf_or_skip() {
        return;
    }

    let mut child = ShellCommand::new(env!("CARGO_BIN_EXE_flpdf"))
        .args(["--check", "--password-file=-", V4_AES])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"user-v4-aes\n")
        .unwrap();
    let flpdf = child.wait_with_output().unwrap();
    assert!(flpdf.status.success());
    assert!(flpdf.stderr.is_empty());
}

// ---------------------------------------------------------------------------
// requires-password: 2 = not encrypted, 3 = opens w/ supplied pw,
//                     0 = a different password is required.
// ---------------------------------------------------------------------------

#[test]
fn requires_password_unencrypted_exits_2() {
    flpdf()
        .args(["requires-password", UNENCRYPTED])
        .assert()
        .code(2);
}

#[test]
fn requires_password_encrypted_empty_password_opens_exits_3() {
    // encrypted-r4-three-page opens with the empty password →
    // qpdf_exit_correct_password = 3.
    flpdf()
        .args(["requires-password", R4_EMPTY_PW])
        .assert()
        .code(3);
}

#[test]
fn requires_password_encrypted_correct_password_exits_3() {
    flpdf()
        .args(["requires-password", "--password=user-v4-aes", V4_AES])
        .assert()
        .code(3);
}

#[test]
fn requires_password_encrypted_wrong_or_no_password_exits_0() {
    // No password (empty) does NOT open v4-aes-128-r4 → a different
    // password is required → exit 0.
    flpdf()
        .args(["requires-password", V4_AES])
        .assert()
        .success();
}

// Weak-crypto (RC4 / R=5): qpdf answers --requires-password purely on the
// password — a correct password yields 3 and a wrong/absent one yields 0,
// with NO --allow-weak-crypto opt-in required. flpdf previously reported 0
// for the correct-password case because the library's post-auth weak-crypto
// gate surfaced as "a different password is required".

#[test]
fn requires_password_weak_rc4_correct_password_exits_3() {
    // v2-rc4-128-r3 (RC4, weak) with the correct user password → qpdf 3.
    flpdf()
        .args(["requires-password", "--password=user-v2", V2_RC4])
        .assert()
        .code(3);
}

#[test]
fn requires_password_weak_rc4_wrong_or_no_password_exits_0() {
    // Empty password does NOT authenticate v2-rc4-128-r3 → a different
    // password is required → exit 0 (auth fails before the weak-crypto gate).
    flpdf()
        .args(["requires-password", V2_RC4])
        .assert()
        .success();
}

#[test]
fn requires_password_weak_r5_correct_password_exits_3() {
    // v5-aes-256-r5 (R=5, weak) with the correct user password → qpdf 3.
    flpdf()
        .args(["requires-password", "--password=user-v5-r5", V5_R5])
        .assert()
        .code(3);
}

#[test]
fn requires_password_weak_r5_wrong_or_no_password_exits_0() {
    // Empty password does NOT authenticate v5-aes-256-r5 → a different
    // password is required → exit 0 (auth fails before the weak-crypto gate).
    // Symmetry with the RC4 wrong/absent-password case above.
    flpdf()
        .args(["requires-password", V5_R5])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// show-encryption-key: lowercase hex of the derived file key after auth.
// Reference keys captured from qpdf 11.9.0 (see module header).
// ---------------------------------------------------------------------------

#[test]
fn show_encryption_key_v4_aes_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v4-aes", V4_AES])
        .assert()
        .success()
        .stdout(format!("5042ec4efa389ea32a149ab2a34e84fc{EOL}"));
}

#[test]
fn show_encryption_key_v5_r6_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v5-r6", V5_R6])
        .assert()
        .success()
        .stdout(format!(
            "fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290{EOL}"
        ));
}

/// Skip the live qpdf-oracle comparison when `qpdf` is not installed locally
/// (still required on CI, matching `encrypt_cli_tests.rs`'s
/// `ensure_qpdf_or_skip`; see `AGENTS.md`'s note that qpdf-dependent
/// compatibility tests are skippable when the executable is absent).
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
        panic!("qpdf required for encryption inspection oracle tests on CI");
    }
    eprintln!("skipping: qpdf not available");
    false
}

#[test]
fn check_show_encryption_key_matches_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(V5_R6);
    let qpdf = ShellCommand::new("qpdf")
        .args(["--check", "--show-encryption-key", "--password=user-v5-r6"])
        .arg(&input)
        .output()
        .expect("run qpdf --check --show-encryption-key");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = ShellCommand::new(env!("CARGO_BIN_EXE_flpdf"))
        .args(["--check", "--show-encryption-key", "--password=user-v5-r6"])
        .arg(&input)
        .output()
        .expect("run flpdf --check --show-encryption-key");

    assert_eq!(flpdf.status, qpdf.status);
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr)
    );
}

#[test]
fn show_encryption_key_unencrypted_errors() {
    flpdf()
        .args(["show-encryption-key", UNENCRYPTED])
        .assert()
        .code(2);
}

#[test]
fn show_encryption_key_wrong_password_errors() {
    flpdf()
        .args(["show-encryption-key", "--password=wrong", V4_AES])
        .assert()
        .code(2);
}

// Weak-crypto (RC4 / R=5): qpdf derives and prints the key for a weak file
// authenticated with the correct password WITHOUT --allow-weak-crypto, treating
// key display as a read-only inspection (qpdf `--show-encryption
// --show-encryption-key`, verified qpdf 11.9.0). flpdf previously errored
// (exit 2) because show-encryption-key opened via the weak-crypto-gated path
// The read-only inspection path follows the same behavior as requires-password.
// Reference
// keys captured from qpdf 11.9.0.

#[test]
fn show_encryption_key_weak_rc4_correct_password_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v2", V2_RC4])
        .assert()
        .success()
        .stdout(format!("09d56583e16481df964f95df779c97d4{EOL}"));
}

#[test]
fn show_encryption_key_weak_r5_correct_password_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v5-r5", V5_R5])
        .assert()
        .success()
        .stdout(format!(
            "c3d812902c9433c0cc9648e00ccf66c205b6b1563feb7d5d31a66bd762ed8614{EOL}"
        ));
}

#[test]
fn show_encryption_key_weak_correct_password_emits_no_weak_crypto_warning() {
    // The gate is forced open for this read-only inspection, so the
    // "processing because --allow-weak-crypto was supplied" warning must NOT
    // fire (the user supplied no such flag, and qpdf emits no warning here).
    flpdf()
        .args(["show-encryption-key", "--password=user-v2", V2_RC4])
        .assert()
        .success()
        .stderr(predicate::str::contains("weak crypto").not());
}

#[test]
fn show_encryption_key_weak_with_allow_weak_crypto_emits_no_warning() {
    // qpdf emits no weak-crypto warning for this inspection regardless of
    // flags, so the inspection path suppresses it even when the user *does*
    // pass --allow-weak-crypto (the flag is a no-op here; the key is the same).
    flpdf()
        .args([
            "show-encryption-key",
            "--allow-weak-crypto",
            "--password=user-v2",
            V2_RC4,
        ])
        .assert()
        .success()
        .stdout(format!("09d56583e16481df964f95df779c97d4{EOL}"))
        .stderr(predicate::str::contains("weak crypto").not());
}

#[test]
fn show_encryption_key_weak_wrong_password_still_errors() {
    // Forcing the weak-crypto gate open must not bypass authentication: a
    // wrong password still fails before any key can be derived (exit 2).
    flpdf()
        .args(["show-encryption-key", "--password=wrong", V2_RC4])
        .assert()
        .code(2);
}

// ---------------------------------------------------------------------------
// show-encryption: the qpdf `--show-encryption` report is emitted verbatim.
// ---------------------------------------------------------------------------

#[test]
fn show_encryption_unencrypted_prints_qpdf_message_exits_0() {
    flpdf()
        .args(["show-encryption", UNENCRYPTED])
        .assert()
        .success()
        .stdout(predicate::str::contains("File is not encrypted"));
}

#[test]
fn show_encryption_v4_aes_lists_required_fields() {
    let out = flpdf()
        .args(["show-encryption", "--password=user-v4-aes", V4_AES])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();

    // qpdf's report fields (parseable / greppable).
    for needle in [
        "R = 4",
        "P = -4",
        "User password = user-v4-aes",
        "stream encryption method: AESv2",
        "string encryption method: AESv2",
        "file encryption method: AESv2",
        "Supplied password is user password",
    ] {
        assert!(
            text.contains(needle),
            "show-encryption output missing {needle:?}; full output:\n{text}"
        );
    }
}

#[test]
fn show_encryption_qpdf_lines_match_qpdf_verbatim() {
    // The complete qpdf `--show-encryption` report must match qpdf byte-for-
    // byte so scripts grepping qpdf output keep working. Hard-coded from
    // qpdf 11.9.0:
    //   qpdf --show-encryption --password=user-v4-aes v4-aes-128-r4.pdf
    let expected_qpdf_block = format!(
        "R = 4{EOL}\
         P = -4{EOL}\
         User password = user-v4-aes{EOL}\
         Supplied password is user password{EOL}\
         extract for accessibility: allowed{EOL}\
         extract for any purpose: allowed{EOL}\
         print low resolution: allowed{EOL}\
         print high resolution: allowed{EOL}\
         modify document assembly: allowed{EOL}\
         modify forms: allowed{EOL}\
         modify annotations: allowed{EOL}\
         modify other: allowed{EOL}\
         modify anything: allowed{EOL}\
         stream encryption method: AESv2{EOL}\
         string encryption method: AESv2{EOL}\
         file encryption method: AESv2{EOL}"
    );
    let out = flpdf()
        .args(["show-encryption", "--password=user-v4-aes", V4_AES])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        text, expected_qpdf_block,
        "qpdf-compatible block diverged from qpdf 11.9.0 output"
    );
}

// Weak-crypto (RC4 / R=5): qpdf `--show-encryption` opens a weak file
// authenticated with the correct password WITHOUT --allow-weak-crypto and
// prints the full block (exit 0), treating it as a read-only inspection
// (verified qpdf 11.9.0). flpdf previously errored (exit 2) via the
// weak-crypto-gated open path.

#[test]
fn show_encryption_weak_rc4_correct_password_exits_0() {
    let out = flpdf()
        .args(["show-encryption", "--password=user-v2", V2_RC4])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    for needle in ["R = 3", "P = -4", "Supplied password is user password"] {
        assert!(
            text.contains(needle),
            "show-encryption (weak RC4) output missing {needle:?}; full output:\n{text}"
        );
    }
}

#[test]
fn show_encryption_weak_r5_correct_password_exits_0() {
    let out = flpdf()
        .args(["show-encryption", "--password=user-v5-r5", V5_R5])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    for needle in ["R = 5", "Supplied password is user password"] {
        assert!(
            text.contains(needle),
            "show-encryption (weak R5) output missing {needle:?}; full output:\n{text}"
        );
    }
}

#[test]
fn show_encryption_weak_correct_password_emits_no_weak_crypto_warning() {
    // Read-only inspection forces the gate open; the false "--allow-weak-crypto
    // was supplied" warning must not fire (qpdf emits none here).
    flpdf()
        .args(["show-encryption", "--password=user-v2", V2_RC4])
        .assert()
        .success()
        .stderr(predicate::str::contains("weak crypto").not());
}

#[test]
fn show_encryption_output_is_stable() {
    // Determinism: two runs produce identical output.
    let run = || {
        String::from_utf8(
            flpdf()
                .args(["show-encryption", "--password=user-v5-r6", V5_R6])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .unwrap()
    };
    assert_eq!(run(), run());
}

// ---------------------------------------------------------------------------
// documented help present for all four subcommands
// ---------------------------------------------------------------------------

#[test]
fn all_subcommands_have_help() {
    for sub in [
        "show-encryption",
        "is-encrypted",
        "requires-password",
        "show-encryption-key",
    ] {
        flpdf()
            .args([sub, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains(sub));
    }
}

// ---------------------------------------------------------------------------
// show-encryption: crypt-filter method reporting
//
// qpdf keeps an unrecognised /CFM as `e_unknown` and a missing one as `e_none`
// rather than refusing the document (libqpdf/QPDF_encryption.cc:865-880).
// Ground truth captured from qpdf 11.9.0 by rewriting `/CFM /AESV2` in place
// (same byte length, so the xref stays valid) in a V=4 AES file:
//
//   $ qpdf --password=u --show-encryption cfm_unknown.pdf   # /CFM /AESVX
//   stream encryption method: unknown
//   string encryption method: unknown
//   file encryption method: unknown
//   $ echo $?
//   0
//   $ qpdf --password=u --show-encryption cfm_absent.pdf    # /CFM /AESV2 blanked
//   stream encryption method: none
//   string encryption method: none
//   file encryption method: none
//   $ echo $?
//   0
//
// flpdf used to exit 2 on the first (`unsupported encryption handler`) and
// report RC4 on the second, which additionally tripped the weak-crypto gate.
// ---------------------------------------------------------------------------

/// Rewrite `/CFM /AESV2` in the committed V=4 AES fixture, keeping the byte
/// length so every recorded offset stays valid, and return the new file's path.
fn v4_aes_with_cfm(replacement: &[u8], dir: &tempfile::TempDir) -> std::path::PathBuf {
    const CFM: &[u8] = b"/CFM /AESV2";
    assert_eq!(
        replacement.len(),
        CFM.len(),
        "substitution must be in place"
    );
    let bytes = std::fs::read(V4_AES).expect("committed V=4 AES fixture");
    let at = bytes
        .windows(CFM.len())
        .position(|window| window == CFM)
        .expect("fixture declares /CFM /AESV2");
    let mut rewritten = bytes.clone();
    rewritten[at..at + CFM.len()].copy_from_slice(replacement);
    let path = dir.path().join("rewritten.pdf");
    std::fs::write(&path, rewritten).expect("write rewritten fixture");
    path
}

#[test]
fn show_encryption_reports_an_unrecognised_cfm_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = v4_aes_with_cfm(b"/CFM /AESVX", &dir);

    flpdf()
        .args(["show-encryption", "--password=user-v4-aes"])
        .arg(&path)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "stream encryption method: unknown",
        ))
        .stdout(predicate::str::contains(
            "string encryption method: unknown",
        ))
        .stdout(predicate::str::contains("file encryption method: unknown"));
}

#[test]
fn show_encryption_reports_a_missing_cfm_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = v4_aes_with_cfm(b"           ", &dir);

    flpdf()
        .args(["show-encryption", "--password=user-v4-aes"])
        .arg(&path)
        .assert()
        .success()
        .stdout(predicate::str::contains("stream encryption method: none"))
        .stdout(predicate::str::contains("string encryption method: none"))
        .stdout(predicate::str::contains("file encryption method: none"));
}
