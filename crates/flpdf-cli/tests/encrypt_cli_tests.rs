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

/// KEY-LEN=40 is V=1 RC4-40 — weak crypto. flpdf-9hc.4.9.1 wires the writer
/// dispatch, but (like qpdf) refuses to write RC4 without --allow-weak-crypto.
#[test]
fn encrypt_key_len_40_v1_rc4_requires_allow_weak_crypto() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "40", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("RC4"))
        .stderr(predicates::str::contains("--allow-weak-crypto"));
    assert!(!output.exists(), "no output without --allow-weak-crypto");

    // With --allow-weak-crypto it succeeds and qpdf reports R=2 (V=1 RC4-40).
    if !ensure_qpdf_or_skip() {
        return;
    }
    let ok = tmp.path().join("v1.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--allow-weak-crypto", "--encrypt", "u", "o", "40", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&ok)
        .assert()
        .success();
    let check = ShellCommand::new("qpdf")
        .arg("--password=u")
        .arg("--show-encryption")
        .arg(&ok)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 2") && stdout.contains("Supplied password is user password"),
        "qpdf must report R=2 + user-password match for V=1 RC4-40: {stdout}"
    );
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

/// flpdf-9hc.4.14: a non-empty user password with an EMPTY owner password
/// under a 256-bit key is insecure (anyone can open the file as owner), so it
/// is rejected unless `--allow-insecure` is given — matching qpdf's
/// checkConfiguration. No output is written.
#[test]
fn encrypt_v5_r6_empty_owner_nonempty_user_requires_allow_insecure() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user-pw", "", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("insecure"))
        .stderr(predicates::str::contains("--allow-insecure"));
    assert!(!output.exists(), "no output must be written when rejected");
}

/// With `--allow-insecure` (in the sub-flag segment, before `--`) the same
/// empty-owner V=5 R=6 encryption succeeds and qpdf opens it with the user
/// password, reporting R=6.
#[test]
fn encrypt_v5_r6_empty_owner_with_allow_insecure_succeeds_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("insecure.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user-pw", "", "256", "--allow-insecure", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

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
        stdout.contains("R = 6") && stdout.contains("Supplied password is user password"),
        "qpdf must report R=6 + user-password match for the --allow-insecure output: {stdout}"
    );
}

/// The insecure gate only fires for the empty-owner case: a 256-bit
/// encryption with BOTH passwords non-empty succeeds without `--allow-insecure`.
#[test]
fn encrypt_v5_r6_both_passwords_no_allow_insecure_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("secure.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user-pw", "owner-pw", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();
    assert!(output.exists(), "both-password 256 encryption must succeed");
}

/// `--allow-insecure` is a value-less flag. Any `=` form — `--allow-insecure=false`
/// (a user trying to OPT OUT) or the empty `--allow-insecure=` (a typo / a
/// generated empty value) — must be rejected, not silently treated as enabling
/// the insecure path, so the empty-owner gate still fires and no file is written.
#[test]
fn encrypt_allow_insecure_rejects_a_value() {
    for form in ["--allow-insecure=false", "--allow-insecure="] {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("nope.pdf");
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--encrypt", "user-pw", "", "256", form, "--"])
            .arg(fixture(UNENCRYPTED_FIXTURE))
            .arg(&output)
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "--allow-insecure does not take a value",
            ));
        assert!(
            !output.exists(),
            "no output must be written when {form:?} is rejected"
        );
    }
}

/// KEY-LEN=128 without `--use-aes=y` is qpdf's default V=2 R=3 RC4-128 — weak
/// crypto (flpdf-9hc.4.9.2). Refused without --allow-weak-crypto; with it,
/// qpdf reports R=3.
#[test]
fn encrypt_128_no_aes_is_v2_rc4_gated_by_weak_crypto() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "128", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("RC4"))
        .stderr(predicates::str::contains("--allow-weak-crypto"));
    assert!(!output.exists(), "no output without --allow-weak-crypto");

    if !ensure_qpdf_or_skip() {
        return;
    }
    let ok = tmp.path().join("v2.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--allow-weak-crypto", "--encrypt", "u", "o", "128", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&ok)
        .assert()
        .success();
    let check = ShellCommand::new("qpdf")
        .arg("--password=u")
        .arg("--show-encryption")
        .arg(&ok)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 3") && stdout.contains("Supplied password is user password"),
        "qpdf must report R=3 for V=2 RC4-128: {stdout}"
    );
}

/// `--encrypt … 128 --force-V4` without `--use-aes=y` selects the V=4 R=4
/// /CFM V2 (RC4-128) variant — weak crypto (flpdf-9hc.4.9.3). With
/// --allow-weak-crypto, qpdf reports R=4.
#[test]
fn encrypt_128_force_v4_no_aes_is_v4_rc4() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ok = tmp.path().join("v4rc4.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "128",
            "--force-V4",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&ok)
        .assert()
        .success();
    let check = ShellCommand::new("qpdf")
        .arg("--password=u")
        .arg("--show-encryption")
        .arg(&ok)
        .output()
        .unwrap();
    assert!(check.status.success());
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 4") && stdout.contains("Supplied password is user password"),
        "qpdf must report R=4 for V=4 RC4-128: {stdout}"
    );
}

/// Sub-flags incompatible with the chosen KEY-LEN are hard usage errors (qpdf
/// parity: `--use-aes`/`--force-V4` are 128-only, `--allow-insecure` is
/// 256-only). They must NOT be silently ignored — otherwise e.g.
/// `--encrypt … 40 --use-aes=y` would write RC4 while the user expected AES.
#[test]
fn encrypt_incompatible_subflags_for_key_len_are_rejected() {
    // (args-after-`--encrypt` excluding the `--` terminator, expected substring)
    let cases: &[(&[&str], &str)] = &[
        (&["u", "o", "40", "--use-aes=y"], "KEY-LEN=40"),
        (&["u", "o", "40", "--force-V4"], "KEY-LEN=40"),
        (&["u", "o", "40", "--allow-insecure"], "KEY-LEN=40"),
        (&["u", "o", "256", "--use-aes=y"], "KEY-LEN=256"),
        (&["u", "o", "256", "--force-V4"], "KEY-LEN=256"),
        (&["u", "", "128", "--allow-insecure"], "KEY-LEN=128"),
    ];
    for (enc_args, needle) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("nope.pdf");
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        // --allow-weak-crypto so the rejection is about the incompatible flag,
        // not the weak-crypto gate (which would also fire for 40).
        cmd.arg("--allow-weak-crypto").arg("--encrypt");
        for a in *enc_args {
            cmd.arg(a);
        }
        cmd.arg("--")
            .arg(fixture(UNENCRYPTED_FIXTURE))
            .arg(&output)
            .assert()
            .failure()
            .stderr(predicates::str::contains(*needle));
        assert!(
            !output.exists(),
            "no output for incompatible combo {enc_args:?}"
        );
    }
}

/// flpdf-9hc.4.9.5: the R>=3 `--encrypt` permission sub-flags must produce the
/// SAME /P permissions as qpdf for identical flags — including the
/// order-sensitive `--modify`/individual-flag interaction and an owner-only
/// restriction. Compares `qpdf --show-encryption` permission lines for
/// flpdf-encrypted vs qpdf-encrypted output (256-bit, R=6).
#[test]
fn encrypt_permission_sub_flags_match_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }

    // qpdf --show-encryption permission lines (sorted), the cross-impl oracle.
    fn perm_lines(p: &Path) -> Vec<String> {
        let out = ShellCommand::new("qpdf")
            .arg("--password=u")
            .arg("--show-encryption")
            .arg(p)
            .output()
            .unwrap();
        assert!(out.status.success());
        let s = String::from_utf8_lossy(&out.stdout);
        let mut v: Vec<String> = s
            .lines()
            .filter(|l| l.ends_with("allowed"))
            .map(|l| l.to_string())
            .collect();
        v.sort();
        v
    }

    // (label, top-level prefix args, --encrypt key-len + cipher selector args).
    // Each profile exercises a distinct revision branch: 256 = R=6, 128-aes =
    // V=4 R=4, 128-rc4 = V=2 R=3. RC4 profiles need --allow-weak-crypto for
    // BOTH flpdf and qpdf to write.
    let profiles: &[(&str, &[&str], &[&str])] = &[
        ("256-r6", &[], &["--encrypt", "u", "o", "256"]),
        (
            "128-aes-r4",
            &[],
            &["--encrypt", "u", "o", "128", "--use-aes=y"],
        ),
        (
            "128-rc4-r3",
            &["--allow-weak-crypto"],
            &["--encrypt", "u", "o", "128"],
        ),
    ];
    let combos: &[&[&str]] = &[
        &["--modify=none"],
        &["--modify=annotate"],
        &["--print=low", "--extract=n"],
        &["--modify=none", "--annotate=y"], // order-sensitive: annotate re-enabled
        &["--annotate=y", "--modify=none"], // reversed: modify clears it
        &["--print=none", "--modify=none", "--extract=n"], // owner-only-style lockdown
        &["--accessibility=n"],             // ignored at R>3, honored at R=3 — both must match qpdf
    ];

    let tmp = tempfile::tempdir().unwrap();
    for (label, prefix, enc_base) in profiles {
        for combo in combos {
            let flpdf_out = tmp.path().join("flpdf.pdf");
            let qpdf_out = tmp.path().join("qpdf.pdf");

            let mut c = Command::cargo_bin("flpdf").unwrap();
            c.args(*prefix).args(*enc_base);
            for a in *combo {
                c.arg(a);
            }
            c.arg("--")
                .arg(fixture(ONE_PAGE_FIXTURE))
                .arg(&flpdf_out)
                .assert()
                .success();

            let mut q = ShellCommand::new("qpdf");
            q.args(*prefix).args(*enc_base);
            for a in *combo {
                q.arg(a);
            }
            let st = q
                .arg("--")
                .arg(fixture(ONE_PAGE_FIXTURE))
                .arg(&qpdf_out)
                .status()
                .unwrap();
            assert!(st.success(), "qpdf encrypt failed for {label} {combo:?}");

            assert_eq!(
                perm_lines(&flpdf_out),
                perm_lines(&qpdf_out),
                "/P permissions differ from qpdf for {label} {combo:?}"
            );
        }
    }
}

/// flpdf-9hc.4.9.6: `--cleartext-metadata` is accepted for V=4/V=5 (the dict
/// emits `/EncryptMetadata false`) and rejected for V=1/V=2 (40-bit, or 128
/// without AES) which have no `/EncryptMetadata` concept.
#[test]
fn encrypt_cleartext_metadata_accept_reject_matrix() {
    let tmp = tempfile::tempdir().unwrap();

    // Accepted for 256 (V=5): output carries /EncryptMetadata false.
    let ok = tmp.path().join("ct256.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "256", "--cleartext-metadata", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&ok)
        .assert()
        .success();
    let bytes = std::fs::read(&ok).unwrap();
    assert!(
        bytes
            .windows(b"/EncryptMetadata false".len())
            .any(|w| w == b"/EncryptMetadata false"),
        "256 --cleartext-metadata must emit /EncryptMetadata false"
    );

    // Accepted for 128 --use-aes=y (V=4).
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--cleartext-metadata",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(tmp.path().join("ct128aes.pdf"))
        .assert()
        .success();

    // Rejected for 40-bit (V=1) and 128 without AES (V=2).
    for args in [
        vec![
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "40",
            "--cleartext-metadata",
            "--",
        ],
        vec![
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "128",
            "--cleartext-metadata",
            "--",
        ],
    ] {
        let nope = tmp.path().join("nope.pdf");
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(&args)
            .arg(fixture(UNENCRYPTED_FIXTURE))
            .arg(&nope)
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "--cleartext-metadata requires V=4 or V=5",
            ));
        assert!(!nope.exists(), "no output for rejected combo {args:?}");
    }
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

/// `--copy-encryption-from` (a V=4 AES-128 donor) applied to a low-version
/// input must floor the output PDF header to 1.5 — the output carries /V 4, so
/// a 1.3 header would be a spec violation. (one-page.pdf is %PDF-1.3.)
#[test]
fn copy_encryption_floors_pdf_header_to_1_5() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "user-pw", "owner-pw");
    let out = tmp.path().join("copied.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--copy-encryption-from")
        .arg(&donor)
        .args(["--encryption-file-password", "user-pw", "--"])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&out)
        .assert()
        .success();
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.5"),
        "copy-encryption (V=4 donor) must floor the header to 1.5, got {:?}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(12)])
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

// ── --force-R5 tests (flpdf-9hc.4.15) ──────────────────────────────────────

/// `--force-R5` produces V=5 R=5 AES-256 output as reported by flpdf's own
/// `show-encryption` — no qpdf dependency, pins flpdf's self-view.
#[test]
fn encrypt_force_r5_flpdf_show_encryption_reports_r5() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("r5.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            // R=5 is deprecated weak crypto; creating it requires the opt-in.
            "--allow-weak-crypto",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "256",
            "--force-R5",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let show = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "show-encryption",
            "--allow-weak-crypto",
            "--password=user-pw",
        ])
        .arg(&output)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&show.get_output().stdout).into_owned();
    for needle in ["V = 5", "R = 5", "Length = 256", "AESv3"] {
        assert!(
            stdout.contains(needle),
            "flpdf show-encryption must report {needle:?} for --force-R5 output: {stdout}"
        );
    }
    // Explicitly verify R=6 is NOT reported
    assert!(
        !stdout.contains("R = 6"),
        "flpdf show-encryption must NOT report R=6 for --force-R5 output: {stdout}"
    );
}

/// `--force-R5` is a 256-bit-only flag; KEY-LEN=128 must be rejected with a
/// diagnostic that names the offending flag.
#[test]
fn encrypt_force_r5_rejected_for_128_bit() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "128", "--force-R5", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--force-R5"));
}

/// `--force-R5` is a 256-bit-only flag; KEY-LEN=40 must be rejected with a
/// diagnostic that names the offending flag.
#[test]
fn encrypt_force_r5_rejected_for_40_bit() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "40",
            "--force-R5",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--force-R5"));
}

/// `--force-R5=value` is rejected: the flag takes no value.
#[test]
fn encrypt_force_r5_rejects_value_form() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "256", "--force-R5=y", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("does not take a value"));
}

/// Creating deprecated R=5 (AES-256) output is gated behind
/// `--allow-weak-crypto`, symmetric with the reader (which rejects R=5 input
/// without the same opt-in). Without the flag the write is refused, and the
/// diagnostic names the flag the user must add.
#[test]
fn encrypt_force_r5_rejected_without_allow_weak_crypto() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("r5-gated.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        // Non-empty owner password keeps the --allow-insecure gate out of the
        // way, so this isolates the weak-crypto (R=5) gate.
        .args([
            "--encrypt",
            "user-pw",
            "owner-pw",
            "256",
            "--force-R5",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--allow-weak-crypto"));
    assert!(
        !output.exists(),
        "no output file must be written when the R=5 weak-crypto gate fires"
    );
}

/// The default 256-bit method is R=6 (not weak), so `--encrypt … 256 --`
/// without `--force-R5` must NOT require `--allow-weak-crypto`. Guards against
/// the R=5 gate accidentally catching the R=6 default.
#[test]
fn encrypt_r6_default_not_gated_without_allow_weak_crypto() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("r6.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "user-pw", "owner-pw", "256", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    // The output is genuinely R=6 (not R=5), confirming the default path is
    // unchanged. R=6 is not weak crypto, so reading it needs no opt-in.
    let show = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-encryption", "--password=user-pw"])
        .arg(&output)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&show.get_output().stdout).into_owned();
    assert!(
        stdout.contains("R = 6"),
        "default 256-bit output must be R=6: {stdout}"
    );
}

/// `--force-R5` produces V=5 R=5 AES-256 output that qpdf authenticates with
/// both user and owner passwords (cross-implementation gate for
/// flpdf-9hc.4.15).
#[test]
fn encrypt_force_r5_round_trips_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("r5-qpdf.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            // R=5 is deprecated weak crypto; creating it requires the opt-in.
            "--allow-weak-crypto",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "256",
            "--force-R5",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    // qpdf should recognise R=5 (needs --allow-weak-crypto since R=5 is deprecated)
    let check = ShellCommand::new("qpdf")
        .arg("--password=user-pw")
        .arg("--allow-weak-crypto")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --show-encryption failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("R = 5") && stdout.contains("Supplied password is user password"),
        "qpdf must report R=5 + user password match: {stdout}"
    );

    // Owner password also authenticates
    let owner_check = ShellCommand::new("qpdf")
        .arg("--password=owner-pw")
        .arg("--allow-weak-crypto")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(owner_check.status.success());
    let owner_out = String::from_utf8_lossy(&owner_check.stdout);
    assert!(
        owner_out.contains("Supplied password is owner password"),
        "qpdf must accept the owner password: {owner_out}"
    );
}

/// --encrypt + --object-streams=generate の組み合わせ:
/// ObjStm コンテナを含む暗号化 PDF を出力し qpdf が復号できること。
#[test]
fn encrypt_with_generate_object_streams_round_trips_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted_objstm.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--object-streams=generate",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    // ObjStm コンテナが存在すること
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.windows(b"/ObjStm".len()).any(|w| w == b"/ObjStm"),
        "output must contain at least one /ObjStm container"
    );

    // /Encrypt が存在すること
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "output must carry /Encrypt"
    );

    // qpdf がユーザーパスワードで復号できること
    let check = std::process::Command::new("qpdf")
        .arg("--password=user-pw")
        .arg("--show-encryption")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --show-encryption failed:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(stdout.contains("R = 4"), "qpdf must report R=4: {stdout}");

    // ObjStm コンテナが実際に正しく復号できることを qpdf --decrypt で確認する。
    // qpdf --check を直接暗号化 PDF に実行すると assertion エラーになるケースがあるため
    // (qpdf 11.x の known issue)、decrypt → check の 2 ステップで行う。
    let decrypted = tmp.path().join("decrypted_from_objstm.pdf");
    let decrypt_result = std::process::Command::new("qpdf")
        .arg("--password=user-pw")
        .arg("--decrypt")
        .arg("--static-id")
        .arg(&output)
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        decrypt_result.status.success(),
        "qpdf --decrypt failed:\n{}",
        String::from_utf8_lossy(&decrypt_result.stderr)
    );

    let check_result = std::process::Command::new("qpdf")
        .arg("--check")
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        check_result.status.success(),
        "qpdf --check on decrypted PDF failed:\n{}",
        String::from_utf8_lossy(&check_result.stderr)
    );
}

/// flpdf-9hc.4.17: xref-stream ソース + --object-streams=disable + --encrypt
///
/// source がすでに xref stream 形式を持つ場合、ObjStm を無効化して暗号化しても
/// xref stream 形式が保持され、qpdf で復号できること。
///
/// これは 4.16/4.17 で実装された「--encrypt は classic xref table を強制しない」
/// 動作を、ObjStm バッチが空の場合（preserve 元の xref form）について検証する。
#[test]
fn encrypt_preserves_xref_stream_form_when_objstm_disabled() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();

    // Step 1: xref stream ソースを生成（--object-streams=generate）
    let xref_stream_source = tmp.path().join("xref_stream_source.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--full-rewrite", "--object-streams=generate"])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&xref_stream_source)
        .assert()
        .success();

    // Step 2: ObjStm を無効化して暗号化（4.17 固有パス: plan.batches が空、source form を継承）
    let encrypted = tmp.path().join("encrypted_xref_stream.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--full-rewrite",
            "--object-streams=disable",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(&xref_stream_source)
        .arg(&encrypted)
        .assert()
        .success();

    let bytes = std::fs::read(&encrypted).unwrap();

    // xref stream 形式が保持されていること（positive: /Type /XRef が存在する）
    assert!(
        bytes.windows(b"/XRef".len()).any(|w| w == b"/XRef"),
        "output must use xref stream form (/Type /XRef), not a classic xref table"
    );
    // classic xref table が出力されていないこと（negative: "\nxref\n" が存在しない）
    // "startxref\n" は classic table でも xref stream でも現れるため使えない。
    // "\nxref\n" は classic table の xref セクション開始を示すキーワードで
    // xref stream 形式では現れない。
    assert!(
        !bytes.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"),
        "output must not contain a classic xref table (\\nxref\\n keyword found)"
    );

    // ObjStm が存在しないこと（disable モードなので）
    assert!(
        !bytes.windows(b"/ObjStm".len()).any(|w| w == b"/ObjStm"),
        "output must not contain ObjStm containers when --object-streams=disable"
    );

    // /Encrypt が存在すること
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "output must carry /Encrypt"
    );

    // qpdf がユーザーパスワードで認証できること
    let check = std::process::Command::new("qpdf")
        .arg("--password=user-pw")
        .arg("--show-encryption")
        .arg(&encrypted)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --show-encryption failed:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(stdout.contains("R = 4"), "qpdf must report R=4: {stdout}");

    // qpdf --decrypt で完全に復号できること
    let decrypted = tmp.path().join("decrypted_xref_stream.pdf");
    let decrypt_result = std::process::Command::new("qpdf")
        .arg("--password=user-pw")
        .arg("--decrypt")
        .arg("--static-id")
        .arg(&encrypted)
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        decrypt_result.status.success(),
        "qpdf --decrypt failed:\n{}",
        String::from_utf8_lossy(&decrypt_result.stderr)
    );

    let check_result = std::process::Command::new("qpdf")
        .arg("--check")
        .arg(&decrypted)
        .output()
        .unwrap();
    assert!(
        check_result.status.success(),
        "qpdf --check on decrypted PDF failed:\n{}",
        String::from_utf8_lossy(&check_result.stderr)
    );
}
