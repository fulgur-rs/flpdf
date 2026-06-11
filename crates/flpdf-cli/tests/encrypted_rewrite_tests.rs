use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const ENCRYPTED_FIXTURES: &[(&str, &str, bool)] = &[
    ("v1-rc4-40-r2.pdf", "user-v1", true),
    ("v2-rc4-128-r3.pdf", "user-v2", true),
    ("v4-rc4-128-r4.pdf", "user-v4-rc4", true),
    ("v4-aes-128-r4.pdf", "user-v4-aes", false),
    ("v5-aes-256-r5.pdf", "user-v5-r5", true),
    ("v5-aes-256-r6.pdf", "user-v5-r6", false),
];

#[test]
fn encrypted_fixtures_rewrite_to_plaintext_matching_qpdf_decrypt_objects() {
    if !ensure_qpdf_or_skip() {
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    for (file_name, password, allow_weak_crypto) in ENCRYPTED_FIXTURES {
        let input = encrypted_fixture(file_name);
        let qpdf_plaintext = tmp.path().join(format!("qpdf-{file_name}"));
        let flpdf_plaintext = tmp.path().join(format!("flpdf-{file_name}"));

        run_qpdf_decrypt(&input, password, *allow_weak_crypto, &qpdf_plaintext);
        run_flpdf_rewrite(&input, password, *allow_weak_crypto, &flpdf_plaintext);

        assert_plaintext_pdf_is_readable(&flpdf_plaintext, file_name);

        // Byte equality is intentionally not required: flpdf rewrites plaintext
        // with its own incidental serialization. Compare qpdf's object JSON
        // instead, with static IDs to remove trailer-ID churn from the oracle.
        let qpdf_objects = qpdf_objects_json(&qpdf_plaintext);
        let flpdf_objects = qpdf_objects_json(&flpdf_plaintext);
        assert_eq!(
            flpdf_objects, qpdf_objects,
            "{file_name}: plaintext rewrite differs from qpdf --decrypt under qpdf --json=1 --json-key=objects"
        );
    }
}

fn encrypted_fixture(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted")
        .join(file_name)
}

fn run_flpdf_rewrite(input: &Path, password: &str, allow_weak_crypto: bool, output: &Path) {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("rewrite")
        .arg("--static-id")
        .arg(format!("--password={password}"));
    if allow_weak_crypto {
        cmd.arg("--allow-weak-crypto");
    }
    cmd.arg(input).arg(output).assert().success();
}

fn run_qpdf_decrypt(input: &Path, password: &str, allow_weak_crypto: bool, output: &Path) {
    let mut cmd = ShellCommand::new("qpdf");
    cmd.arg(format!("--password={password}"));
    if allow_weak_crypto {
        cmd.arg("--allow-weak-crypto");
    }
    cmd.arg("--static-id");
    let result = cmd
        .arg("--decrypt")
        .arg(input)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "qpdf --decrypt failed for {}: {}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn assert_plaintext_pdf_is_readable(output: &Path, file_name: &str) {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("check")
        .arg(output)
        .assert()
        .success()
        .stdout(predicates::str::contains("File is not encrypted\n"));

    let bytes = std::fs::read(output).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "{file_name}: plaintext rewrite must not contain /Encrypt"
    );
}

fn qpdf_objects_json(path: &Path) -> Vec<u8> {
    let result = ShellCommand::new("qpdf")
        .args(["--json=1", "--json-key=objects"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "qpdf --json=1 --json-key=objects failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

// ---------------------------------------------------------------------------
// flpdf-9hc.3.18: `rewrite --remove-restrictions`
//
// `--remove-restrictions` adds no new decryption logic: a plaintext rewrite of
// an authenticated encrypted input already strips /Encrypt and the advisory
// permission bits live only inside /Encrypt /P, so the output is inherently
// unrestricted. These tests pin the acceptance criteria: the flag de-restricts
// an encrypted+restricted fixture (one-line diagnostic, no /Encrypt,
// `show-encryption` reports "File is not encrypted"), it does NOT bypass
// authentication, and it is a no-op exit-0 rewrite on unencrypted input.
// ---------------------------------------------------------------------------

const UNENCRYPTED_FIXTURE: &str = "../../tests/fixtures/minimal.pdf";
const REMOVE_RESTRICTIONS_DIAGNOSTIC: &str =
    "flpdf: removed restrictions (encryption and advisory permissions stripped)";

#[test]
fn remove_restrictions_strips_encryption_and_emits_diagnostic() {
    // v4-aes-128-r4 needs no --allow-weak-crypto, keeping the case clean.
    let input = encrypted_fixture("v4-aes-128-r4.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("derestricted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-restrictions", "--password=user-v4-aes"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicates::str::contains(REMOVE_RESTRICTIONS_DIAGNOSTIC));

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "remove-restrictions output must not contain /Encrypt"
    );

    // Layer-4 show-encryption is qpdf-verbatim: must report unencrypted, exit 0.
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("show-encryption")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicates::str::contains("File is not encrypted"));
}

#[test]
fn remove_restrictions_does_not_bypass_authentication() {
    // Auth-requiring input WITHOUT a password must be rejected exactly as a
    // plain `rewrite` would: the flag must not bypass authentication.
    let input = encrypted_fixture("v4-aes-128-r4.pdf");
    let tmp = tempfile::tempdir().unwrap();

    let plain_out = tmp.path().join("plain.pdf");
    let plain = Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(&input)
        .arg(&plain_out)
        .assert()
        .failure();
    let plain_code = plain.get_output().status.code();

    let flag_out = tmp.path().join("flag.pdf");
    let flagged = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-restrictions"])
        .arg(&input)
        .arg(&flag_out)
        .assert()
        .failure();

    assert_eq!(
        flagged.get_output().status.code(),
        plain_code,
        "--remove-restrictions must reject auth-requiring input identically to plain rewrite"
    );
    assert!(
        !flag_out.exists(),
        "no output must be produced when authentication fails"
    );
}

#[test]
fn remove_restrictions_on_unencrypted_input_is_a_noop_rewrite() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join(UNENCRYPTED_FIXTURE);
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("noop.pdf");

    // Exit 0, valid output, and no de-restriction diagnostic (nothing was
    // restricted) — matching qpdf's lenient handling.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--remove-restrictions"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicates::str::contains(REMOVE_RESTRICTIONS_DIAGNOSTIC).not());

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("check")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicates::str::contains("File is not encrypted\n"));
}

// ---------------------------------------------------------------------------
// flpdf-9hc.4.10: `--decrypt`
//
// `--decrypt` is the silent qpdf-compatible alias of `--remove-restrictions`
// on the current rewrite path: both flags drop /Encrypt entirely, and on
// flpdf's plaintext-by-default writer that's a no-op vs. the implicit
// behavior. The flag exists so that qtest cases (`unrecognized: --decrypt`,
// 4 cases) parse and so that the eventual `--encrypt` flag (flpdf-9hc.4.9)
// has a documented inverse. These tests pin the silence + no-op semantics.
// ---------------------------------------------------------------------------

#[test]
fn decrypt_on_encrypted_input_produces_plaintext_silently_at_top_level() {
    // Top-level alias surface: `flpdf --decrypt --password ... in out`.
    let input = encrypted_fixture("v4-aes-128-r4.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("decrypted-top.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--decrypt", "--password=user-v4-aes"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        // qpdf parity: --decrypt is silent. In particular it must NOT emit
        // the --remove-restrictions diagnostic, otherwise behaviorally
        // indistinguishable scripts would see different output.
        .stderr(predicates::str::contains(REMOVE_RESTRICTIONS_DIAGNOSTIC).not());

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "--decrypt output must not contain /Encrypt"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("show-encryption")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicates::str::contains("File is not encrypted"));
}

#[test]
fn decrypt_on_encrypted_input_produces_plaintext_silently_at_subcommand() {
    // Rewrite subcommand surface: `flpdf rewrite --decrypt --password ...`.
    let input = encrypted_fixture("v4-aes-128-r4.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("decrypted-sub.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--decrypt", "--password=user-v4-aes"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicates::str::contains(REMOVE_RESTRICTIONS_DIAGNOSTIC).not());

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "--decrypt output must not contain /Encrypt"
    );
}

#[test]
fn decrypt_on_unencrypted_input_is_a_silent_noop_rewrite() {
    // qpdf `--decrypt` on plaintext input exits 0 silently — match that.
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join(UNENCRYPTED_FIXTURE);
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("noop-decrypt.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--decrypt"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicates::str::contains(REMOVE_RESTRICTIONS_DIAGNOSTIC).not());

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("check")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicates::str::contains("File is not encrypted\n"));
}

#[test]
fn decrypt_does_not_bypass_authentication() {
    // Encrypted input without a password must be rejected exactly as a
    // plain `rewrite` would: --decrypt must not bypass authentication.
    let input = encrypted_fixture("v4-aes-128-r4.pdf");
    let tmp = tempfile::tempdir().unwrap();

    let plain_out = tmp.path().join("plain.pdf");
    let plain = Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg(&input)
        .arg(&plain_out)
        .assert()
        .failure();
    let plain_code = plain.get_output().status.code();

    let flag_out = tmp.path().join("flag.pdf");
    let flagged = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--decrypt"])
        .arg(&input)
        .arg(&flag_out)
        .assert()
        .failure();

    assert_eq!(
        flagged.get_output().status.code(),
        plain_code,
        "--decrypt must reject auth-requiring input identically to plain rewrite"
    );
    assert!(
        !flag_out.exists(),
        "no output must be produced when authentication fails"
    );
}

/// `--decrypt` and `--remove-restrictions` together must not conflict —
/// they are documented as semantically overlapping on the current rewrite
/// path. Passing both should succeed and the `--remove-restrictions`
/// diagnostic must still fire (since it gates only on its own flag, not on
/// the absence of `--decrypt`).
#[test]
fn decrypt_combined_with_remove_restrictions_keeps_diagnostic() {
    let input = encrypted_fixture("v4-aes-128-r4.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("decrypt-and-rm-restrictions.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--decrypt",
            "--remove-restrictions",
            "--password=user-v4-aes",
        ])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        // The remove-restrictions diagnostic is gated on its own flag, so it
        // must still fire when both flags are passed.
        .stderr(predicates::str::contains(REMOVE_RESTRICTIONS_DIAGNOSTIC));

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        !bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "combined --decrypt --remove-restrictions output must not contain /Encrypt"
    );
}

#[test]
fn decrypt_conflicts_with_inspection_subcommands() {
    // The conflicts_with_all on the top-level --decrypt must reject
    // combinations with --check (and the rest of the inspection group) as
    // usage errors. Without this, `flpdf --check --decrypt in out` would
    // silently take the inspection path and ignore the flag (and OUTPUT).
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join(UNENCRYPTED_FIXTURE);
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("conflict.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", "--decrypt"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        // clap emits an "argument cannot be used with" diagnostic on conflicts.
        .stderr(predicates::str::contains("cannot be used"));
}

/// `--decrypt` combined with the page-ops pipeline (`--pages` / `--rotate` /
/// `--split-pages` / `--collate`) must be rejected upfront, mirroring the
/// existing `--remove-restrictions` rejection. The page-ops pipeline does not
/// thread the rewrite-only flags, and on encrypted input it rejects the file
/// outright — so silently passing through `--decrypt` would leave the user
/// guessing whether decryption actually happened.
#[test]
fn decrypt_is_rejected_when_combined_with_page_operations() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join(UNENCRYPTED_FIXTURE);
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("decrypt-pages.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--decrypt", "--pages", ".", "1-z", "--"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--decrypt"))
        .stderr(predicates::str::contains("are not applied in the --pages"));
    assert!(
        !output.exists(),
        "no output must be produced when the unsupported combination is rejected"
    );
}

fn ensure_qpdf_or_skip() -> bool {
    let available = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if available {
        return true;
    }

    let on_ci = std::env::var_os("CI").is_some();
    if on_ci {
        panic!(
            "qpdf is required for encrypted plaintext rewrite tests on CI; install qpdf before running this test suite"
        );
    }
    eprintln!(
        "skipping: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    false
}
