//! CLI tests for the writer-side `--encrypt` flag (flpdf-9hc.4.9): V=4
//! AES-128 (KEY-LEN 128 `--use-aes=y`) and V=5 R=6 AES-256 (KEY-LEN 256,
//! flpdf-9hc.4.9.4).
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

    // `qpdf --check` on an encrypted minimal-fixture output reliably
    // triggers a libstdc++/libc++ vector range-check assertion in qpdf
    // 11.x on macOS (brew) and Windows (choco) — same shape as the bug
    // tracked in flpdf-d4k (resolved for the writer_tests path in
    // PR #209 by reinstalling matching qpdf versions, but the
    // encrypted-output code path here surfaces it again on those
    // platforms). Linux qpdf accepts the same bytes cleanly. Use
    // `qpdf --show-encryption` instead — it does enough work to prove
    // the password authenticates and the dict shape is valid, without
    // walking every content stream where the qpdf bug fires.
    let check = ShellCommand::new("qpdf")
        .arg("--password=user-pw")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --show-encryption failed: stderr={}",
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 4") && stdout.contains("Supplied password is user password"),
        "qpdf must report R=4 + user-password match: {stdout}"
    );
}

/// `flpdf --encrypt USER OWNER 256 -- IN OUT` produces a V=5 R=6 AES-256
/// document that qpdf authenticates with BOTH the user and owner passwords —
/// the cross-implementation gate for flpdf-9hc.4.9.4. qpdf recovering the user
/// password from `/O` via the owner password proves `/O` `/OE` are correct.
#[test]
fn top_level_encrypt_v5_r6_aes256_round_trips_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted-v5.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user-pw", "owner-pw", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "encrypted output must carry /Encrypt"
    );

    // qpdf authenticates the user password and reports R=6 (V=5 AES-256).
    let user = ShellCommand::new("qpdf")
        .arg("--password=user-pw")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        user.status.success(),
        "qpdf --show-encryption (user) failed: stderr={}",
        String::from_utf8_lossy(&user.stderr)
    );
    let user_out = String::from_utf8_lossy(&user.stdout);
    assert!(
        user_out.contains("R = 6") && user_out.contains("Supplied password is user password"),
        "qpdf must report R=6 + user-password match: {user_out}"
    );

    // The owner password also authenticates against the same output.
    let owner = ShellCommand::new("qpdf")
        .arg("--password=owner-pw")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(owner.status.success());
    let owner_out = String::from_utf8_lossy(&owner.stdout);
    assert!(
        owner_out.contains("Supplied password is owner password"),
        "qpdf must accept the owner password: {owner_out}"
    );
}

/// flpdf's own `show-encryption` reports the V=5 R=6 AES-256 scheme for a
/// `--encrypt … 256` output. No qpdf dependency — pins flpdf's self-view.
#[test]
fn encrypt_v5_r6_aes256_flpdf_show_encryption_reports_scheme() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted-v5.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user-pw", "owner-pw", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let show = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-encryption"])
        .arg(&output)
        .arg("--password=user-pw")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&show.get_output().stdout).into_owned();
    for needle in ["V = 5", "Length = 256", "R = 6", "AESv3"] {
        assert!(
            stdout.contains(needle),
            "flpdf show-encryption must report {needle:?} for V=5 R=6 output: {stdout}"
        );
    }
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
fn encrypt_key_len_256_is_accepted_as_v5_r6() {
    // KEY-LEN=256 used to be rejected ("not yet supported"); flpdf-9hc.4.9.4
    // wires the V=5 R=6 AES-256 writer dispatch, so it now succeeds and emits
    // an encrypted document.
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("ok.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();
    assert!(
        output.exists(),
        "256 encryption must produce an output file"
    );
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "V=5 R=6 output must carry /Encrypt"
    );
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

/// `--encrypt` combined with page operations (`--pages`, `--rotate`,
/// `--split-pages`, `--collate`) must be rejected upfront: the page-op
/// pipeline does not thread `WriteOptions.encrypt` through to its
/// extraction/rewrite paths, so silently honoring `--encrypt` here would
/// produce plaintext output despite the user's request. Mirrors the
/// existing `--decrypt` / `--remove-restrictions` rejection in the same
/// dispatch.
#[test]
fn encrypt_is_rejected_when_combined_with_page_operations_top_level() {
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
            "--",
            "--pages",
            ".",
            "1-z",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--encrypt"))
        .stderr(predicates::str::contains("--pages"));
    assert!(!output.exists());
}

#[test]
fn encrypt_is_rejected_when_combined_with_page_operations_subcommand() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--",
            "--pages",
            ".",
            "1-z",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--encrypt"))
        .stderr(predicates::str::contains("--pages"));
    assert!(!output.exists());
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

// ── --static-aes-iv tests (flpdf-9hc.4.13) ───────────────────────────────────

/// `--static-id --static-aes-iv --encrypt …` must produce byte-identical
/// output on two consecutive runs (deterministic encryption).
#[test]
fn static_aes_iv_with_static_id_produces_deterministic_output() {
    let tmp = tempfile::tempdir().unwrap();
    let out1 = tmp.path().join("encrypted1.pdf");
    let out2 = tmp.path().join("encrypted2.pdf");
    let input = fixture(UNENCRYPTED_FIXTURE);

    for output in [&out1, &out2] {
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--static-id",
                "--static-aes-iv",
                "--encrypt",
                "user",
                "owner",
                "128",
                "--use-aes=y",
                "--",
            ])
            .arg(&input)
            .arg(output)
            .assert()
            .success();
    }

    let bytes1 = std::fs::read(&out1).unwrap();
    let bytes2 = std::fs::read(&out2).unwrap();
    assert_eq!(
        bytes1, bytes2,
        "--static-id --static-aes-iv must produce byte-identical output on two runs"
    );

    // Confirm it is really encrypted (not a plaintext passthrough).
    assert!(
        bytes1.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "output must carry /Encrypt"
    );
}

/// Without `--static-aes-iv` (but with `--static-id` to pin `/ID`),
/// two encryptions of the same file produce different bytes because
/// AES IVs are freshly random each run.
#[test]
fn without_static_aes_iv_two_runs_produce_different_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let out1 = tmp.path().join("encrypted1.pdf");
    let out2 = tmp.path().join("encrypted2.pdf");
    let input = fixture(ONE_PAGE_FIXTURE); // has streams, so IVs are emitted

    for output in [&out1, &out2] {
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--static-id",
                "--encrypt",
                "user",
                "owner",
                "128",
                "--use-aes=y",
                "--",
            ])
            .arg(&input)
            .arg(output)
            .assert()
            .success();
    }

    let bytes1 = std::fs::read(&out1).unwrap();
    let bytes2 = std::fs::read(&out2).unwrap();
    assert_ne!(
        bytes1, bytes2,
        "without --static-aes-iv, two encrypted runs with streams must differ (random IVs)"
    );
}

/// qpdf can decrypt the `--static-aes-iv` output: the deterministic IV
/// does not break the AES-CBC ciphertext structure.
#[test]
fn static_aes_iv_output_decrypts_cleanly_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let encrypted = tmp.path().join("encrypted.pdf");
    let decrypted = tmp.path().join("decrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--static-aes-iv",
            "--encrypt",
            "user",
            "owner",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&encrypted)
        .assert()
        .success();

    let decrypt = std::process::Command::new("qpdf")
        .arg("--password=user")
        .arg("--decrypt")
        .arg(&encrypted)
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        decrypt.status.success(),
        "qpdf --decrypt failed on --static-aes-iv output: {}",
        String::from_utf8_lossy(&decrypt.stderr)
    );
}

/// `rewrite` subcommand surface also accepts `--static-aes-iv`.
#[test]
fn rewrite_subcommand_static_aes_iv_produces_deterministic_output() {
    let tmp = tempfile::tempdir().unwrap();
    let out1 = tmp.path().join("encrypted1.pdf");
    let out2 = tmp.path().join("encrypted2.pdf");
    let input = fixture(UNENCRYPTED_FIXTURE);

    for output in [&out1, &out2] {
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "rewrite",
                "--static-id",
                "--static-aes-iv",
                "--encrypt",
                "user",
                "owner",
                "128",
                "--use-aes=y",
                "--",
            ])
            .arg(&input)
            .arg(output)
            .assert()
            .success();
    }

    let bytes1 = std::fs::read(&out1).unwrap();
    let bytes2 = std::fs::read(&out2).unwrap();
    assert_eq!(
        bytes1, bytes2,
        "rewrite --static-id --static-aes-iv must produce byte-identical output"
    );
}

// ── --copy-encryption-from tests (flpdf-9hc.4.11) ───────────────────────────

/// Build a donor PDF encrypted with V=4 AES-128 and return the path.
/// Uses `--static-id --static-aes-iv` so the donor is deterministic, but the
/// CSPRNG path is exercised by the copy-encryption tests themselves.
fn make_donor_pdf(tmp: &tempfile::TempDir, user_pw: &str, owner_pw: &str) -> PathBuf {
    let donor = tmp.path().join("donor.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--static-aes-iv",
            "--encrypt",
            user_pw,
            owner_pw,
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&donor)
        .assert()
        .success();
    donor
}

/// `--copy-encryption-from` produces an output that carries /Encrypt and that
/// flpdf itself can round-trip through its own reader with both user and
/// owner passwords.
#[test]
fn copy_encryption_from_output_has_encrypt_dict() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "secretuser", "secretowner");
    let out = tmp.path().join("copy_out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption-from",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "secretuser",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .success();

    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "copy-encryption-from output must carry /Encrypt"
    );
}

/// The output of `--copy-encryption-from` decrypts with the donor's user
/// password through qpdf and reports V=4 / R=4 AESv2 — confirming the
/// /Encrypt scheme was copied, not re-derived.
#[test]
fn copy_encryption_from_decrypts_with_donor_user_password_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "donoruser", "donorowner");
    let out = tmp.path().join("copy_out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption-from",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "donoruser",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .success();

    // qpdf must accept the donor's user password.
    let check = ShellCommand::new("qpdf")
        .arg("--password=donoruser")
        .arg("--show-encryption")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --show-encryption failed with donor user password: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 4"),
        "qpdf must report R=4 on copy-encryption-from output: {stdout}"
    );
    assert!(
        stdout.contains("Supplied password is user password"),
        "qpdf must accept donor user password: {stdout}"
    );
}

/// The output of `--copy-encryption-from` also decrypts with the donor's
/// owner password.
#[test]
fn copy_encryption_from_decrypts_with_donor_owner_password_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "userpass", "ownerpass");
    let out = tmp.path().join("copy_out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption-from",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "userpass",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .success();

    let check = ShellCommand::new("qpdf")
        .arg("--password=ownerpass")
        .arg("--show-encryption")
        .arg(&out)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("Supplied password is owner password"),
        "qpdf must accept donor owner password: {stdout}"
    );
}

/// `--copy-encryption-from` on a one-page fixture produces output that
/// qpdf can fully decrypt (not just inspect) — confirming stream encryption
/// is consistent with the copied /Encrypt dict.
#[test]
fn copy_encryption_from_round_trip_decrypts_cleanly_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "pw", "pw");
    let encrypted = tmp.path().join("encrypted.pdf");
    let decrypted = tmp.path().join("decrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption-from",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "pw",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&encrypted)
        .assert()
        .success();

    let decrypt = ShellCommand::new("qpdf")
        .arg("--password=pw")
        .arg("--decrypt")
        .arg(&encrypted)
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        decrypt.status.success(),
        "qpdf --decrypt failed on copy-encryption-from output: {}",
        String::from_utf8_lossy(&decrypt.stderr)
    );
}

/// `rewrite` subcommand also supports `--copy-encryption-from`.
#[test]
fn rewrite_subcommand_copy_encryption_from_succeeds() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "donorpw", "ownerpw");
    let out = tmp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--copy-encryption-from",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "donorpw",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .success();

    let check = ShellCommand::new("qpdf")
        .arg("--password=donorpw")
        .arg("--show-encryption")
        .arg(&out)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 4") && stdout.contains("Supplied password is user password"),
        "rewrite --copy-encryption-from: qpdf must report R=4 + user-password match: {stdout}"
    );
}

/// `--copy-encryption-from` applied to a plaintext donor is rejected with a
/// clear "not encrypted" diagnostic.
#[test]
fn copy_encryption_from_unencrypted_donor_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption-from",
            fixture(UNENCRYPTED_FIXTURE).to_str().unwrap(),
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains("not encrypted"));
}

/// `--copy-encryption-from` with a wrong password is rejected with an error
/// (the donor cannot be opened with the supplied password).
#[test]
fn copy_encryption_from_wrong_password_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "correctpw", "ownerpw");
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption-from",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "wrongpw",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .failure()
        // The error surfaces as either "failed to open" (wrong password
        // rejected by the reader at open time) or "failed to recover file
        // key" (auth passes but key recovery fails). Both include the
        // --copy-encryption-from prefix, so we just pin that.
        .stderr(predicates::str::contains("--copy-encryption-from"));
    assert!(!out.exists());
}
