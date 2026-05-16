//! CLI integration tests for the `--password-is-hex-key` and
//! `--suppress-password-recovery` flags (flpdf-9hc.3.19, stack layer 6/6).
//!
//! `--password-is-hex-key` (qpdf parity): the --password value is the
//! precomputed file encryption key as hex, NOT a user/owner password. All
//! password→key derivation (Algorithm 2 / 2.A / 2.B / 6 / 7) is skipped and
//! the decoded bytes are used directly as the file key.
//!
//! `--suppress-password-recovery` (qpdf parity): a documented no-op — flpdf
//! performs a single authentication attempt with no encoding fallback, so
//! there is no recovery to suppress. It must parse without error and change
//! nothing.
//!
//! End-to-end strategy: recover the hex key from a known password via the
//! layer-4 `show-encryption-key` subcommand, then reopen the file with that
//! key and `--password-is-hex-key`. This proves the layer-4 dependency and
//! the round-trip together.

use assert_cmd::Command;

/// V=5 R=6 AES-256 fixture, user password `user-v5-r6`. Reference key
/// captured from qpdf 11.9.0 (see cli_encryption_inspect.rs module header).
const V5_R6: &str = "../../tests/fixtures/encrypted/v5-aes-256-r6.pdf";
const V5_R6_PASSWORD: &str = "user-v5-r6";
const V5_R6_HEX_KEY: &str = "fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290";

/// V=4 R=4 AES-128 fixture, empty password. 16-byte key → 32 hex chars; a
/// non-weak V<5-class handler, guarding the revision-aware mode split in the
/// hex-key branch (the V<5/V=4 helper returns RC4/RC4 for V≠4 unconditionally
/// and must not be misapplied).
const R4_EMPTY_PW: &str = "../../tests/fixtures/compat/encrypted-r4-three-page.pdf";
const R4_HEX_KEY: &str = "43ca065209d492256d845f57f8b95da2";

/// RC4 fixture (weak crypto) — used to prove the post-key weak-crypto gate is
/// honored consistently on the hex-key path (qpdf parity: a raw key does NOT
/// bypass `--allow-weak-crypto`).
const V2_RC4: &str = "../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf";

fn flpdf() -> Command {
    Command::cargo_bin("flpdf").unwrap()
}

/// Run `show-encryption-key` to recover the hex key, asserting it matches the
/// qpdf reference, then return it (proves the layer-4 dependency live).
fn recover_key(file: &str, password: &str, expected: &str) -> String {
    let out = flpdf()
        .args([
            "show-encryption-key",
            &format!("--password={password}"),
            file,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let key = String::from_utf8(out).unwrap().trim().to_string();
    assert_eq!(key, expected, "recovered key must match qpdf reference");
    key
}

// ---------------------------------------------------------------------------
// Acceptance 1: V=5 R=6 — show-encryption-key → reopen with --password-is-hex-key
// ---------------------------------------------------------------------------

#[test]
fn hex_key_v5_r6_check_succeeds_with_recovered_key() {
    let key = recover_key(V5_R6, V5_R6_PASSWORD, V5_R6_HEX_KEY);
    flpdf()
        .args([
            "check",
            &format!("--password={key}"),
            "--password-is-hex-key",
            V5_R6,
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("PDF check succeeded"));
}

#[test]
fn hex_key_v5_r6_decrypts_equivalently_to_password() {
    // `show-encryption` decrypts and reports handler params. The hex-key path
    // must produce output equivalent to the real-password path EXCEPT for the
    // `Supplied password is …` line: a raw key authenticates nothing, so
    // user_password_matched / owner_password_matched are both false (qpdf
    // does not report a password match for --password-is-hex-key).
    let key = recover_key(V5_R6, V5_R6_PASSWORD, V5_R6_HEX_KEY);

    let pw_out = flpdf()
        .args([
            "show-encryption",
            &format!("--password={V5_R6_PASSWORD}"),
            V5_R6,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let pw_stdout = String::from_utf8(pw_out).unwrap();

    let hex_out = flpdf()
        .args([
            "show-encryption",
            &format!("--password={key}"),
            "--password-is-hex-key",
            V5_R6,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let hex_stdout = String::from_utf8(hex_out).unwrap();

    // Every line except the password-match line must be identical.
    let pw_filtered: Vec<&str> = pw_stdout
        .lines()
        .filter(|l| !l.starts_with("Supplied password is"))
        .collect();
    let hex_filtered: Vec<&str> = hex_stdout
        .lines()
        .filter(|l| !l.starts_with("Supplied password is"))
        .collect();
    assert_eq!(
        pw_filtered, hex_filtered,
        "hex-key decryption must be equivalent to password decryption \
         (modulo the password-match report line)"
    );
    // The password path reports a match; the hex-key path must NOT.
    assert!(
        pw_stdout.contains("Supplied password is"),
        "password path should report a password match"
    );
    assert!(
        !hex_stdout.contains("Supplied password is"),
        "hex-key path must NOT report a password match (raw key auths nothing)"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 2: 32-byte hex works; non-hex / over-length → clear error, no panic
// ---------------------------------------------------------------------------

#[test]
fn hex_key_32_byte_key_works() {
    // V=5 R=6 key is exactly 32 bytes (64 hex chars).
    assert_eq!(V5_R6_HEX_KEY.len(), 64);
    flpdf()
        .args([
            "check",
            &format!("--password={V5_R6_HEX_KEY}"),
            "--password-is-hex-key",
            V5_R6,
        ])
        .assert()
        .success();
}

#[test]
fn hex_key_16_byte_key_works_on_v4() {
    // 16-byte (32 hex char) AES-128 key on a V=4 R=4 document — exercises the
    // revision-aware mode split in the hex-key branch.
    let key = recover_key(R4_EMPTY_PW, "", R4_HEX_KEY);
    assert_eq!(key.len(), 32);
    flpdf()
        .args([
            "check",
            &format!("--password={key}"),
            "--password-is-hex-key",
            R4_EMPTY_PW,
        ])
        .assert()
        .success();
}

#[test]
fn hex_key_non_hex_input_errors_cleanly() {
    let assert = flpdf()
        .args([
            "check",
            "--password=not-hex-zz",
            "--password-is-hex-key",
            V5_R6,
        ])
        .assert()
        .failure();
    let out = assert.get_output();
    // Clear, attributed error — and definitely not a panic.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--password-is-hex-key") && stderr.contains("not valid hex"),
        "expected a clear non-hex error, got: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "must not panic, got: {stderr}"
    );
}

#[test]
fn hex_key_over_length_input_errors_cleanly() {
    // 40 bytes (80 hex chars) > the 32-byte Standard-handler maximum.
    let too_long = "ab".repeat(40);
    let assert = flpdf()
        .args([
            "check",
            &format!("--password={too_long}"),
            "--password-is-hex-key",
            V5_R6,
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("--password-is-hex-key") && stderr.contains("at most 32 bytes"),
        "expected a clear over-length error, got: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "must not panic, got: {stderr}"
    );
}

#[test]
fn hex_key_uppercase_and_whitespace_tolerant() {
    // qpdf accepts upper-case hex and embedded whitespace.
    let spaced = format!(
        "{} {}",
        &V5_R6_HEX_KEY.to_uppercase()[..32],
        &V5_R6_HEX_KEY.to_uppercase()[32..]
    );
    flpdf()
        .args([
            "check",
            &format!("--password={spaced}"),
            "--password-is-hex-key",
            V5_R6,
        ])
        .assert()
        .success();
}

#[test]
fn hex_key_weak_crypto_gate_still_honored() {
    // A raw key does NOT bypass the weak-crypto gate. An RC4 file opened with
    // a hex key and no --allow-weak-crypto must still be rejected with
    // WeakCryptoNotAllowed (qpdf parity; keep existing post-key behavior).
    // The exact key value is irrelevant: the gate fires after the key is
    // accepted, before decryption, so any well-formed hex triggers it.
    let assert = flpdf()
        .args([
            "check",
            "--password=00112233445566778899aabbccddeeff",
            "--password-is-hex-key",
            V2_RC4,
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("weak crypto"),
        "RC4 file with a hex key must still hit the weak-crypto gate, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 3: --suppress-password-recovery is accepted and is a no-op
// ---------------------------------------------------------------------------

#[test]
fn suppress_password_recovery_is_accepted_and_noop() {
    // Baseline: normal password auth.
    let baseline = flpdf()
        .args([
            "show-encryption",
            &format!("--password={V5_R6_PASSWORD}"),
            V5_R6,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // With --suppress-password-recovery: parses without error, output
    // unchanged (documented no-op — flpdf has no encoding recovery).
    let with_flag = flpdf()
        .args([
            "show-encryption",
            &format!("--password={V5_R6_PASSWORD}"),
            "--suppress-password-recovery",
            V5_R6,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        baseline, with_flag,
        "--suppress-password-recovery must be a no-op (output unchanged)"
    );
}

#[test]
fn suppress_password_recovery_accepted_on_check() {
    // Accepted on an unrelated command without error.
    flpdf()
        .args([
            "check",
            &format!("--password={V5_R6_PASSWORD}"),
            "--suppress-password-recovery",
            V5_R6,
        ])
        .assert()
        .success();
}

#[test]
fn suppress_and_hex_key_combine() {
    // Both flags together: no-op alongside the hex-key path.
    flpdf()
        .args([
            "check",
            &format!("--password={V5_R6_HEX_KEY}"),
            "--password-is-hex-key",
            "--suppress-password-recovery",
            V5_R6,
        ])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Acceptance 4: regression — without --password-is-hex-key the normal
// password path is unchanged (layer-2 behavior intact).
// ---------------------------------------------------------------------------

#[test]
fn regression_normal_password_path_unchanged() {
    // Correct password still authenticates and reports a password match.
    flpdf()
        .args([
            "show-encryption",
            &format!("--password={V5_R6_PASSWORD}"),
            V5_R6,
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Supplied password is"));

    // The hex key passed WITHOUT --password-is-hex-key is treated as an
    // ordinary (wrong) password and rejected — proving the flag gates the
    // branch and the layer-2 path is untouched.
    flpdf()
        .args([
            "show-encryption-key",
            &format!("--password={V5_R6_HEX_KEY}"),
            V5_R6,
        ])
        .assert()
        .code(2);
}
