//! CLI integration tests for the qpdf-compatible encryption inspection
//! subcommands (flpdf-9hc.3.17): `show-encryption`, `is-encrypted`,
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
//!   `qpdf --requires-password [--password=…] FIXTURE` (flpdf-63g): qpdf does
//! NOT require `--allow-weak-crypto` for this read-only inspection. The same
//! applies to `show-encryption` / `show-encryption-key`: qpdf derives the key
//! and prints the encryption block for a weak file with the correct password
//! and no `--allow-weak-crypto`, so flpdf does too (flpdf-ysb5).

use assert_cmd::Command;
use predicates::prelude::*;

const R4_EMPTY_PW: &str = "../../tests/fixtures/compat/encrypted-r4-three-page.pdf";
const V4_AES: &str = "../../tests/fixtures/encrypted/v4-aes-128-r4.pdf";
const V5_R6: &str = "../../tests/fixtures/encrypted/v5-aes-256-r6.pdf";
// Weak-crypto fixtures (RC4 / R=5): qpdf answers --requires-password on these
// without --allow-weak-crypto (flpdf-63g).
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
    // password and without --allow-weak-crypto (guards the probe's forced
    // weak-crypto opt-in, flpdf-63g).
    flpdf().args(["is-encrypted", V2_RC4]).assert().success();
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
// gate surfaced as "a different password is required" (flpdf-63g).

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
        .stdout("5042ec4efa389ea32a149ab2a34e84fc\n");
}

#[test]
fn show_encryption_key_v5_r6_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v5-r6", V5_R6])
        .assert()
        .success()
        .stdout("fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290\n");
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
// (flpdf-ysb5; same alignment as requires-password in flpdf-63g). Reference
// keys captured from qpdf 11.9.0.

#[test]
fn show_encryption_key_weak_rc4_correct_password_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v2", V2_RC4])
        .assert()
        .success()
        .stdout("09d56583e16481df964f95df779c97d4\n");
}

#[test]
fn show_encryption_key_weak_r5_correct_password_matches_qpdf() {
    flpdf()
        .args(["show-encryption-key", "--password=user-v5-r5", V5_R5])
        .assert()
        .success()
        .stdout("c3d812902c9433c0cc9648e00ccf66c205b6b1563feb7d5d31a66bd762ed8614\n");
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
        .stdout("09d56583e16481df964f95df779c97d4\n")
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
// show-encryption: parseable, contains the DESIGN-required fields, and the
// qpdf `--show-encryption` lines are emitted verbatim.
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

    // DESIGN-mandated minimum fields (parseable / greppable).
    for needle in [
        "V = 4",
        "R = 4",
        "Length = 128",
        "P = -4",
        "EncryptMetadata = true",
        "Filter = Standard",
        "CF /StdCF = AESv2",
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
    // The qpdf `--show-encryption` block (everything except flpdf's leading
    // V/Length/Filter/EncryptMetadata/CF lines and qpdf's omitted
    // "User password = …" line) must match qpdf byte-for-byte so scripts
    // grepping qpdf output keep working. Hard-coded from qpdf 11.9.0:
    //   qpdf --show-encryption --password=user-v4-aes v4-aes-128-r4.pdf
    let expected_qpdf_block = "\
R = 4
P = -4
Supplied password is user password
extract for accessibility: allowed
extract for any purpose: allowed
print low resolution: allowed
print high resolution: allowed
modify document assembly: allowed
modify forms: allowed
modify annotations: allowed
modify other: allowed
modify anything: allowed
stream encryption method: AESv2
string encryption method: AESv2
file encryption method: AESv2
";
    let out = flpdf()
        .args(["show-encryption", "--password=user-v4-aes", V4_AES])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    // Drop flpdf's leading lines; the remainder must equal the qpdf block.
    let qpdf_part: String = text
        .lines()
        .skip_while(|l| !l.starts_with("R = "))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        format!("{qpdf_part}\n"),
        expected_qpdf_block,
        "qpdf-compatible block diverged from qpdf 11.9.0 output"
    );
}

// Weak-crypto (RC4 / R=5): qpdf `--show-encryption` opens a weak file
// authenticated with the correct password WITHOUT --allow-weak-crypto and
// prints the full block (exit 0), treating it as a read-only inspection
// (verified qpdf 11.9.0). flpdf previously errored (exit 2) via the
// weak-crypto-gated open path (flpdf-ysb5).

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
