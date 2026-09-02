//! CLI tests for the writer-side `--encrypt` flag (flpdf-9hc.4.9): V=4
//! AES-128 (KEY-LEN 128 `--use-aes=y`) and V=5 R=6 AES-256 (KEY-LEN 256,
//! flpdf-9hc.4.9.4).
//!
//! Strategy: invoke `flpdf --encrypt …` on a plaintext fixture, then verify
//! the resulting encrypted PDF round-trips through qpdf's reader (the
//! independent oracle). The CLI's accept/reject matrix is also pinned here
//! so user-visible diagnostics remain stable.

mod common;
use common::PdfCanonicalTestExt;

use assert_cmd::Command;
use flpdf::{Pdf, PdfOpenOptions};
use predicates::prelude::*;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

/// Collapse a live qpdf subprocess's CRLF-terminated text lines to bare `\n`.
/// On Windows, `qpdf.exe`'s own C-runtime stdout is opened in text mode and
/// translates every `\n` write to `\r\n`; flpdf's CLI writes plain `\n`
/// everywhere, matching qpdf's C++ source (`cout << "...\n"`) rather than
/// that platform-specific translation. Comparing raw bytes on Windows would
/// therefore flag a line-ending artifact of the oracle process, not a real
/// content difference (see the identical pattern already established in
/// `cli_logger_routing.rs`/`cli_attachment_lifecycle.rs`).
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

const UNENCRYPTED_FIXTURE: &str = "../../tests/fixtures/minimal.pdf";
const ONE_PAGE_FIXTURE: &str = "../../tests/fixtures/compat/one-page.pdf";
// Exercise actual page/content traversal in qpdf's QDF + encryption oracle.
// `minimal.pdf` has an empty page tree; qpdf 12.3.0--12.3.2 crashes on it
// during `--check` (qpdf#1674) before reaching the QDF/AES properties.
const QDF_ENCRYPTION_FIXTURE: &str = ONE_PAGE_FIXTURE;
// Only referenced by the qpdf-zlib-compat-gated byte-identical oracle test
// below; under default features they'd otherwise be dead code.
#[cfg(feature = "qpdf-zlib-compat")]
const TWO_PAGE_FIXTURE: &str = "../../tests/fixtures/compat/two-page.pdf";
#[cfg(feature = "qpdf-zlib-compat")]
const THREE_PAGE_FIXTURE: &str = "../../tests/fixtures/compat/three-page.pdf";

/// All direct Standard-handler revisions exposed by the CLI. The boolean
/// records qpdf's write-time weak-crypto opt-in; it is also passed to qpdf
/// when reading the R=5 output in the semantic gate.
const DIRECT_ENCRYPTION_MATRIX: &[(&str, &[&str], bool, &str)] = &[
    (
        "v1-r2",
        &["--allow-weak-crypto", "--encrypt", "u", "o", "40", "--"],
        true,
        "R = 2",
    ),
    (
        "v2-r3",
        &["--allow-weak-crypto", "--encrypt", "u", "o", "128", "--"],
        true,
        "R = 3",
    ),
    (
        "v4-rc4-r4",
        &[
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "128",
            "--force-V4",
            "--",
        ],
        true,
        "R = 4",
    ),
    (
        "v4-aes-r4",
        &["--encrypt", "u", "o", "128", "--use-aes=y", "--"],
        false,
        "R = 4",
    ),
    (
        "v5-r5",
        &[
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "256",
            "--force-R5",
            "--",
        ],
        true,
        "R = 5",
    ),
    (
        "v5-r6",
        &["--encrypt", "u", "o", "256", "--"],
        false,
        "R = 6",
    ),
];

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

fn assert_qdf_encrypted_output(output: &Path, password: &str) {
    let bytes = std::fs::read(output).expect("read encrypted QDF output");
    assert!(
        bytes.windows(b"%QDF-1.0".len()).any(|w| w == b"%QDF-1.0"),
        "encrypted QDF output must carry the QDF marker"
    );
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "encrypted QDF output must carry /Encrypt"
    );
    let trailer_marker = b"\ntrailer <<\n";
    let trailer_start = bytes
        .windows(trailer_marker.len())
        .rposition(|window| window == trailer_marker)
        .expect("encrypted QDF output has a classic trailer")
        + trailer_marker.len();
    let trailer = &bytes[trailer_start..];
    let id_offset = trailer
        .windows(b"  /ID ".len())
        .position(|window| window == b"  /ID ")
        .expect("encrypted QDF trailer has /ID");
    let encrypt_offset = trailer
        .windows(b" /Encrypt ".len())
        .position(|window| window == b" /Encrypt ")
        .expect("encrypted QDF trailer has /Encrypt");
    let encrypt_line_end = trailer[encrypt_offset..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|offset| encrypt_offset + offset)
        .expect("encrypted QDF trailer /Encrypt line is terminated");
    let encrypt_fields: Vec<&[u8]> = trailer[encrypt_offset..encrypt_line_end]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect();
    assert!(
        id_offset < encrypt_offset
            && !trailer[id_offset..encrypt_offset].contains(&b'\n')
            && encrypt_fields.len() == 4
            && encrypt_fields[0] == b"/Encrypt"
            && !encrypt_fields[1].is_empty()
            && encrypt_fields[1].iter().all(|byte| byte.is_ascii_digit())
            && encrypt_fields[2] == b"0"
            && encrypt_fields[3] == b"R"
            && trailer[encrypt_line_end..].starts_with(b"\n>>\n"),
        "qpdf writes /ID then a final /Encrypt N 0 R on the same QDF trailer line: {}",
        String::from_utf8_lossy(trailer)
    );

    let check = ShellCommand::new("qpdf")
        .arg(format!("--password={password}"))
        .arg("--check")
        .arg(output)
        .output()
        .expect("run qpdf --check on encrypted QDF output");
    assert!(
        check.status.success(),
        "qpdf --check failed: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );

    let mut reopened = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            password: password.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("flpdf must reopen and authenticate encrypted QDF output");
    assert!(
        reopened.is_encrypted(),
        "reopened QDF output must remain encrypted"
    );
    let root_ref = reopened.root_ref().expect("encrypted QDF output has /Root");
    reopened
        .resolve_canonical_object(root_ref)
        .expect("authenticated reader resolves the QDF root");
}

fn xref_stream_dictionary(bytes: &[u8]) -> &[u8] {
    let type_marker = b"/Type /XRef";
    let type_start = bytes
        .windows(type_marker.len())
        .rposition(|window| window == type_marker)
        .expect("xref stream dictionary has /Type /XRef");
    let object_header = b" obj\n";
    let object_end = bytes[..type_start]
        .windows(object_header.len())
        .rposition(|window| window == object_header)
        .map(|offset| offset + object_header.len())
        .expect("xref stream has an indirect-object header");
    let dictionary_start = object_end
        + bytes[object_end..type_start]
            .windows(2)
            .position(|window| window == b"<<")
            .expect("xref stream has a dictionary");
    let dictionary_end = type_start
        + bytes[type_start..]
            .windows(b">>\nstream".len())
            .position(|window| window == b">>\nstream")
            .expect("xref stream dictionary has a stream terminator")
        + 2;
    &bytes[dictionary_start..dictionary_end]
}

fn xref_stream_dictionary_keys(dictionary: &[u8]) -> Vec<&'static str> {
    const KEYS: [&str; 10] = [
        "/Type",
        "/Length",
        "/Filter",
        "/DecodeParms",
        "/W",
        "/Info",
        "/Root",
        "/Size",
        "/ID",
        "/Encrypt",
    ];
    let mut positions = KEYS
        .iter()
        .filter_map(|key| {
            dictionary
                .windows(key.len())
                .position(|window| window == key.as_bytes())
                .map(|position| (position, *key))
        })
        .collect::<Vec<_>>();
    positions.sort_unstable_by_key(|(position, _)| *position);
    positions.into_iter().map(|(_, key)| key).collect()
}

fn assert_qpdf_xref_stream_dictionary_contract(dictionary: &[u8]) {
    assert!(dictionary
        .windows(b"/Filter /FlateDecode".len())
        .any(|window| { window == b"/Filter /FlateDecode" }));
    assert!(dictionary
        .windows(b"/DecodeParms << /Columns 4 /Predictor 12 >>".len())
        .any(|window| window == b"/DecodeParms << /Columns 4 /Predictor 12 >>"));
    assert!(dictionary
        .windows(b"/W [ 1 2 1 ]".len())
        .any(|window| window == b"/W [ 1 2 1 ]"));
    assert!(!dictionary
        .windows(b"/Index".len())
        .any(|window| window == b"/Index"));
}

fn assert_qpdf_uncompressed_xref_stream_dictionary_contract(dictionary: &[u8]) {
    assert!(dictionary
        .windows(b"/W [ 1 2 1 ]".len())
        .any(|window| window == b"/W [ 1 2 1 ]"));
    assert!(!dictionary
        .windows(b"/Filter".len())
        .any(|window| window == b"/Filter"));
    assert!(!dictionary
        .windows(b"/DecodeParms".len())
        .any(|window| window == b"/DecodeParms"));
    assert!(!dictionary
        .windows(b"/Index".len())
        .any(|window| window == b"/Index"));
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

/// qpdf treats a bare `-` inside the positional `--encrypt` password triple
/// as a password value, not as an option. The CLI must preserve that value
/// through writing so qpdf can authenticate the resulting document with `-`.
#[test]
fn top_level_encrypt_accepts_bare_hyphen_password() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--allow-weak-crypto", "--encrypt", "-", "-", "128", "--"])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let check = ShellCommand::new("qpdf")
        .arg("--password=-")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf must authenticate with bare-hyphen password: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
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

#[test]
fn qdf_direct_encryption_works_on_top_level_and_rewrite_surfaces() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();

    for (surface, rewrite) in [("top-level", false), ("rewrite", true)] {
        let output = tmp.path().join(format!("{surface}-direct-qdf.pdf"));
        let mut command = Command::cargo_bin("flpdf").unwrap();
        if rewrite {
            command.arg("rewrite");
        }
        command
            .args([
                "--qdf",
                "--static-id",
                "--static-aes-iv",
                "--encrypt",
                "qdf-user",
                "qdf-owner",
                "128",
                "--use-aes=y",
                "--",
            ])
            .arg(fixture(QDF_ENCRYPTION_FIXTURE))
            .arg(&output)
            .assert()
            .success();

        assert_qdf_encrypted_output(&output, "qdf-user");
    }
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

/// The native `show-encryption` subcommand shares the qpdf-verbatim report
/// route with the top-level qpdf-shaped flag.
#[test]
fn encrypt_v5_r6_aes256_flpdf_show_encryption_reports_scheme() {
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

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-encryption", "--password=user-pw"])
        .arg(&output)
        .output()
        .expect("run qpdf --show-encryption");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let show = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-encryption"])
        .arg(&output)
        .arg("--password=user-pw")
        .assert()
        .success();
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&show.get_output().stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&show.get_output().stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(show.get_output().stdout, qpdf.stdout);
        assert_eq!(show.get_output().stderr, qpdf.stderr);
    }
}

/// The qtest shim forwards qpdf's option-shaped inspection command unchanged.
/// This is the first RED test for the top-level QPDFJob route: the native
/// `show-encryption` subcommand is not an equivalent parser surface.
#[test]
fn top_level_show_encryption_matches_qpdf_for_user_password() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = fixture("../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-encryption", "--password=user-v2"])
        .arg(&input)
        .output()
        .expect("run qpdf --show-encryption");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-encryption", "--password=user-v2"])
        .arg(&input)
        .output()
        .expect("run flpdf --show-encryption");

    assert_eq!(flpdf.status, qpdf.status);
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
    }
}

#[test]
fn top_level_show_encryption_recovers_v2_user_password_for_owner_password() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = fixture("../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-encryption", "--password=owner-v2"])
        .arg(&input)
        .output()
        .expect("run qpdf --show-encryption with owner password");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-encryption", "--password=owner-v2"])
        .arg(&input)
        .output()
        .expect("run flpdf --show-encryption with owner password");

    assert_eq!(flpdf.status, qpdf.status);
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
    }
}

#[test]
fn top_level_show_encryption_reports_wrong_password_without_failing() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = fixture("../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-encryption", "--password=wrong"])
        .arg(&input)
        .output()
        .expect("run qpdf --show-encryption with wrong password");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-encryption", "--password=wrong"])
        .arg(&input)
        .output()
        .expect("run flpdf --show-encryption with wrong password");

    assert_eq!(flpdf.status, qpdf.status);
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
    }
}

#[test]
fn json_encrypt_section_recovers_v2_user_password_from_owner_password() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = fixture("../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2", "--json-key=encrypt", "--password=owner-v2"])
        .arg(&input)
        .output()
        .expect("run qpdf JSON encryption inspection");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=encrypt", "--password=owner-v2"])
        .arg(&input)
        .output()
        .expect("run flpdf JSON encryption inspection");
    assert_eq!(flpdf.status, qpdf.status);

    let qpdf_json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let flpdf_json: serde_json::Value = serde_json::from_slice(&flpdf.stdout).unwrap();
    assert_eq!(
        flpdf_json["encrypt"]["recovereduserpassword"],
        qpdf_json["encrypt"]["recovereduserpassword"]
    );
    assert_eq!(
        flpdf_json["encrypt"]["recovereduserpassword"],
        serde_json::Value::String("user-v2".into())
    );
}

#[test]
fn top_level_show_encryption_matches_qpdf_for_plaintext() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = fixture(UNENCRYPTED_FIXTURE);

    let qpdf = ShellCommand::new("qpdf")
        .arg("--show-encryption")
        .arg(&input)
        .output()
        .expect("run qpdf --show-encryption on plaintext");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-encryption"])
        .arg(&input)
        .output()
        .expect("run flpdf --show-encryption on plaintext");

    assert_eq!(flpdf.status, qpdf.status);
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
    }
}

#[test]
fn top_level_show_encryption_matches_qpdf_for_r5_without_write_opt_in() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let input = fixture("../../tests/fixtures/encrypted/v5-aes-256-r5.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-encryption", "--password=user-v5-r5"])
        .arg(&input)
        .output()
        .expect("run qpdf --show-encryption on R5 fixture");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-encryption", "--password=user-v5-r5"])
        .arg(&input)
        .output()
        .expect("run flpdf --show-encryption on R5 fixture");

    assert_eq!(flpdf.status, qpdf.status);
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
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

/// qpdf's bare-option parser discards an attached `=value` for
/// `--allow-insecure`, so both forms enable the option.
#[test]
fn encrypt_allow_insecure_ignores_an_attached_value() {
    for form in ["--allow-insecure=false", "--allow-insecure="] {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("insecure.pdf");
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--encrypt", "user-pw", "", "256", form, "--"])
            .arg(fixture(UNENCRYPTED_FIXTURE))
            .arg(&output)
            .assert()
            .success();
        assert!(
            output.exists(),
            "attached value must be discarded for {form:?}"
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
    // (args-after-`--encrypt` excluding the `--` terminator, expected qpdf
    // diagnostic substring)
    let cases: &[(&[&str], &str)] = &[
        (
            &["u", "o", "40", "--use-aes=y"],
            "unrecognized argument --use-aes=y (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--force-V4"],
            "unrecognized argument --force-V4 (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--allow-insecure"],
            "unrecognized argument --allow-insecure (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--cleartext-metadata"],
            "unrecognized argument --cleartext-metadata (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--form=y"],
            "unrecognized argument --form=y (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--assemble=y"],
            "unrecognized argument --assemble=y (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--accessibility=y"],
            "unrecognized argument --accessibility=y (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "40", "--modify-other=y"],
            "unrecognized argument --modify-other=y (40-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "128", "--force-R5"],
            "unrecognized argument --force-R5 (128-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "", "128", "--allow-insecure"],
            "unrecognized argument --allow-insecure (128-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "256", "--use-aes=y"],
            "unrecognized argument --use-aes=y (256-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "256", "--force-V4"],
            "unrecognized argument --force-V4 (256-bit encryption options must be terminated with --)",
        ),
        (
            &["u", "o", "256", "--potato"],
            "unrecognized argument --potato (256-bit encryption options must be terminated with --)",
        ),
        // A sub-flag missing its leading `-` must not be silently reinterpreted
        // as the corresponding named flag (qpdf rejects it as an unrecognized
        // positional argument, not as `--force-R5`).
        (
            &["u", "o", "256", "force-R5"],
            "unrecognized argument force-R5 (256-bit encryption options must be terminated with --)",
        ),
        // In the dashed `--user-password=`/`--bits=` form, qpdf stays in its
        // password-argument table (no named options recognized) until `--bits`
        // selects the key-length-specific table.
        (
            &["--user-password=u", "--allow-insecure", "--bits=256"],
            "unrecognized argument",
        ),
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

/// A zero-token `--encrypt --` segment must be rejected the same as an
/// absent one is accepted — clap's `num_args = 0..` means both an omitted
/// `--encrypt` and one given with no sub-arguments produce an empty list, so
/// the two must stay distinguishable or `--encrypt --` would silently write
/// an unencrypted (plaintext) output despite the user asking for encryption
/// (qpdf itself rejects this with "encryption key length is required").
#[test]
fn encrypt_empty_segment_is_rejected_not_treated_as_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--encrypt requires USER-PW OWNER-PW KEY-LEN",
        ));
    assert!(
        !output.exists(),
        "no output must be written for an empty --encrypt segment"
    );
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

/// flpdf-9hc.4.9.6: `--cleartext-metadata` is accepted for V=4/V=5 (including
/// the 128-bit default's forced V=4 path) and rejected by the 40-bit option
/// table, which has no `/EncryptMetadata` concept.
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

    // The 128-bit default selects V=4 when cleartext metadata is requested.
    let ok_v4_rc4 = tmp.path().join("ct128rc4.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--allow-weak-crypto",
            "--encrypt",
            "u",
            "o",
            "128",
            "--cleartext-metadata",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&ok_v4_rc4)
        .assert()
        .success();
    let bytes = std::fs::read(&ok_v4_rc4).unwrap();
    assert!(bytes
        .windows(b"/EncryptMetadata false".len())
        .any(|w| { w == b"/EncryptMetadata false" }));

    // Rejected by qpdf's 40-bit option table.
    let args = [
        "--allow-weak-crypto",
        "--encrypt",
        "u",
        "o",
        "40",
        "--cleartext-metadata",
        "--",
    ];
    let nope = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&nope)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "unrecognized argument --cleartext-metadata",
        ));
    assert!(!nope.exists(), "no output for rejected combo {args:?}");
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
/// pipeline does not thread `WriterOptions.encrypt` through to its
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

/// `--static-aes-iv` exists so that output can be compared with qpdf's, so the
/// bytes have to match *qpdf's*, not merely be stable across flpdf runs. The
/// test above pins determinism and would stay green for any vector at all.
///
/// qpdf's static vector is `14 * (1 + i)` (`libqpdf/Pl_AES_PDF.cc:133-137`,
/// reached from `QPDFWriter::setStaticAesIV`, `libqpdf/QPDFWriter.cc:292-297`)
/// and CBC writes it at the head of every ciphertext (`:161-163`), so it is
/// observable in the output.
///
/// This compares the vector itself rather than the whole document — see
/// `encrypted_document_is_byte_identical_to_qpdf` below for the full-file
/// comparison, which covers 128-bit only (qpdf's own 256-bit output is not
/// reproducible run-to-run; see that test's doc for why). That test also
/// requires the `qpdf-zlib-compat` feature (DEFLATE output must match qpdf's
/// zlib backend); this one does not, because the initialization vector
/// precedes the ciphertext and is independent of how the payload was
/// compressed.
#[test]
fn static_aes_iv_matches_the_vector_qpdf_writes() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ours = tmp.path().join("flpdf.pdf");
    let theirs = tmp.path().join("qpdf.pdf");
    let input = fixture(ONE_PAGE_FIXTURE);

    // 128 exercises V=4 (AESV2) and needs --use-aes to select AES over RC4;
    // 256 exercises V=5 (AESV3), where AES is implied and --use-aes is
    // rejected. Both write a CBC vector ahead of the stream ciphertext.
    for (bits, aes_flag) in [("128", Some("--use-aes=y")), ("256", None)] {
        let mut args = vec!["--static-id", "--static-aes-iv", "--encrypt", "", "", bits];
        args.extend(aes_flag);
        args.push("--");

        Command::cargo_bin("flpdf")
            .unwrap()
            .args(&args)
            .arg(&input)
            .arg(&ours)
            .assert()
            .success();

        let qpdf = std::process::Command::new("qpdf")
            .args(&args)
            .arg(&input)
            .arg(&theirs)
            .output()
            .unwrap();
        assert!(
            qpdf.status.success(),
            "qpdf reference run failed for {bits}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let mine = leading_stream_vector(&std::fs::read(&ours).unwrap());
        let reference = leading_stream_vector(&std::fs::read(&theirs).unwrap());

        assert_eq!(
            mine, reference,
            "{bits}-bit AES: the initialization vector must be the one qpdf writes"
        );
    }
}

/// Whole-document byte parity for AES-128 (V=4/AESV2) encrypted output
/// against real qpdf.
///
/// With the static IV (`static_aes_iv_matches_the_vector_qpdf_writes` above),
/// the `/U` value's padding, and the trailer `/Encrypt` position all lining up
/// with qpdf, the only remaining divergence for
/// `--static-id --static-aes-iv --encrypt <user> <owner> 128 --use-aes=y`
/// output is the DEFLATE backend — hence the `qpdf-zlib-compat` gate below.
/// This supersedes the vector-only comparison above as the byte-identical
/// proof; that test stays too, since it documents the narrower IV claim on
/// its own and needs no feature gate.
///
/// This intentionally does not cover 256-bit (V=5/AESV3, R=6): its file
/// encryption key is generated with random bytes rather than derived from the
/// password (`QPDF::compute_encryption_parameters_V5`,
/// `libqpdf/QPDF_encryption.cc:1198`), and its `/U`, `/UE`, `/O`, `/OE`, and
/// `/Perms` values each mix in further random salt bytes
/// (`compute_U_UE_value_V5:610`, `compute_O_OE_value_V5:629`,
/// `compute_Perms_value_V5_clear:652`). None of that randomness is seeded by
/// `--static-id` or `--static-aes-iv`, so even qpdf's own 256-bit output is
/// not stable across repeated runs of the same invocation: two consecutive
/// `qpdf --static-id --static-aes-iv --encrypt "" "" 256 --` runs on the same
/// input diverge partway into the first encrypted string. A byte-identical
/// comparison is therefore not meaningful for the 256-bit case, independent
/// of flpdf's implementation.
#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_document_is_byte_identical_to_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ours = tmp.path().join("flpdf.pdf");
    let theirs = tmp.path().join("qpdf.pdf");
    let input = fixture(ONE_PAGE_FIXTURE);

    let args = [
        "--static-id",
        "--static-aes-iv",
        "--encrypt",
        "",
        "",
        "128",
        "--use-aes=y",
        "--",
    ];

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(&input)
        .arg(&ours)
        .assert()
        .success();

    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(&input)
        .arg(&theirs)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf reference run failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let mine = std::fs::read(&ours).unwrap();
    let reference = std::fs::read(&theirs).unwrap();
    assert_eq!(
        mine, reference,
        "AES-128 (V=4/AESV2): flpdf output must be byte-identical to qpdf 11.9.0"
    );
}

/// Decrypt both sides through qpdf's canonical QDF writer so the random V5
/// security-handler entries are removed from the comparison. This is the
/// semantic and structural gate for every direct Standard-handler revision.
#[test]
fn encrypted_writer_direct_handler_matrix_matches_qpdf_after_decrypt() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let input = fixture(ONE_PAGE_FIXTURE);

    for (label, encrypt_args, allow_weak_crypto, expected_revision) in DIRECT_ENCRYPTION_MATRIX {
        let qpdf_output = tmp.path().join(format!("{label}-qpdf.pdf"));
        let flpdf_output = tmp.path().join(format!("{label}-flpdf.pdf"));
        write_qpdf_direct_encryption(&input, &qpdf_output, encrypt_args);

        Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--static-id", "--static-aes-iv", "--object-streams=disable"])
            .args(*encrypt_args)
            .arg(&input)
            .arg(&flpdf_output)
            .assert()
            .success();

        let show = ShellCommand::new("qpdf")
            .arg("--password=u")
            .arg("--allow-weak-crypto")
            .arg("--show-encryption")
            .arg(&flpdf_output)
            .output()
            .unwrap();
        assert!(
            show.status.success(),
            "{label}: qpdf could not inspect flpdf output: {}",
            String::from_utf8_lossy(&show.stderr)
        );
        assert!(
            String::from_utf8_lossy(&show.stdout).contains(expected_revision),
            "{label}: expected {expected_revision}, got {}",
            String::from_utf8_lossy(&show.stdout)
        );

        let qpdf_qdf = tmp.path().join(format!("{label}-qpdf.qdf"));
        let flpdf_qdf = tmp.path().join(format!("{label}-flpdf.qdf"));
        decrypt_to_qdf(&qpdf_output, &qpdf_qdf, *allow_weak_crypto);
        decrypt_to_qdf(&flpdf_output, &flpdf_qdf, *allow_weak_crypto);
        assert_eq!(
            std::fs::read(&flpdf_qdf).unwrap(),
            std::fs::read(&qpdf_qdf).unwrap(),
            "{label}: decrypted semantic/structural QDF must match qpdf 11.9.0"
        );
    }
}

/// V1/V2/V4 direct encryption has no security-handler randomness once qpdf's
/// static ID and AES-IV controls are enabled. Keep every deterministic
/// revision in the qpdf-zlib byte gate; V5 is covered semantically above and
/// has its test-only randomness seam in the core writer tests.
#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_writer_deterministic_direct_handler_matrix_is_byte_identical_to_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let input = fixture(ONE_PAGE_FIXTURE);

    for (label, encrypt_args, _allow_weak_crypto, _expected_revision) in
        DIRECT_ENCRYPTION_MATRIX.iter().take(4)
    {
        let qpdf_output = tmp.path().join(format!("{label}-qpdf.pdf"));
        let flpdf_output = tmp.path().join(format!("{label}-flpdf.pdf"));
        write_qpdf_direct_encryption(&input, &qpdf_output, encrypt_args);

        Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--static-id", "--static-aes-iv", "--object-streams=disable"])
            .args(*encrypt_args)
            .arg(&input)
            .arg(&flpdf_output)
            .assert()
            .success();

        assert_eq!(
            std::fs::read(&flpdf_output).unwrap(),
            std::fs::read(&qpdf_output).unwrap(),
            "{label}: deterministic direct encryption must be byte-identical to qpdf 11.9.0"
        );
    }
}

/// The donor-based copy route is independently gated from direct encryption.
/// The donor is qpdf-authored with fixed ID/IV controls, so qpdf's copied V4
/// AES handler and flpdf's recovered donor tuple must produce identical bytes.
#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_writer_copy_encryption_tuple_is_byte_identical_to_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let input = fixture(ONE_PAGE_FIXTURE);
    let donor = tmp.path().join("donor-qpdf.pdf");
    let donor_result = ShellCommand::new("qpdf")
        .args([
            "--static-id",
            "--static-aes-iv",
            "--encrypt",
            "",
            "",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(&input)
        .arg(&donor)
        .output()
        .unwrap();
    assert!(
        donor_result.status.success(),
        "qpdf donor generation failed: {}",
        String::from_utf8_lossy(&donor_result.stderr)
    );

    let qpdf_output = tmp.path().join("copy-qpdf.pdf");
    let flpdf_output = tmp.path().join("copy-flpdf.pdf");
    let qpdf = ShellCommand::new("qpdf")
        .args(["--static-id", "--static-aes-iv", "--object-streams=disable"])
        .arg(format!("--copy-encryption={}", donor.display()))
        .args(["--encryption-file-password=", "--"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf copy-encryption reference failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--static-id", "--static-aes-iv", "--object-streams=disable"])
        .args(["--copy-encryption"])
        .arg(&donor)
        .args(["--encryption-file-password", "", "--"])
        .arg(&input)
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "fixed V4 AES-128 copy-encryption tuple must be byte-identical to qpdf 11.9.0"
    );
}

fn write_qpdf_direct_encryption(input: &Path, output: &Path, encrypt_args: &[&str]) {
    let result = ShellCommand::new("qpdf")
        .args(["--static-id", "--static-aes-iv", "--object-streams=disable"])
        .args(encrypt_args)
        .arg(input)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "qpdf direct encryption failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn decrypt_to_qdf(input: &Path, output: &Path, allow_weak_crypto: bool) {
    let mut command = ShellCommand::new("qpdf");
    command
        .arg("--password=u")
        .arg("--decrypt")
        .arg("--qdf")
        .arg("--object-streams=disable")
        .arg("--static-id");
    if allow_weak_crypto {
        command.arg("--allow-weak-crypto");
    }
    let result = command.arg(input).arg(output).output().unwrap();
    assert!(
        result.status.success(),
        "qpdf decrypt-to-QDF failed for {}: {}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// qpdf's non-linearized Generate writer assigns an ObjStm container and all
/// of its members when the first member is reached. The encrypted coordinator
/// must preserve that container-first numbering for both freshly derived
/// encryption and copy-encryption from a fixed V4 AES-128 donor.
///
/// The fixture has more than one generated ObjStm, so a route that merely
/// keeps the output valid or moves one container can not satisfy this gate.
#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_generate_objstm_direct_and_copy_are_byte_identical_to_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let input = fixture("../../tests/fixtures/compat/objstm-gen-nostream-130rev.pdf");

    let direct_args = [
        "--static-id",
        "--static-aes-iv",
        "--object-streams=generate",
        "--encrypt",
        "",
        "",
        "128",
        "--use-aes=y",
        "--",
    ];
    let direct_qpdf = tmp.path().join("direct-qpdf.pdf");
    let direct_flpdf = tmp.path().join("direct-flpdf.pdf");
    let qpdf = ShellCommand::new("qpdf")
        .args(direct_args)
        .arg(&input)
        .arg(&direct_qpdf)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf direct Generate reference failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(direct_args)
        .arg(&input)
        .arg(&direct_flpdf)
        .assert()
        .success();
    assert_eq!(
        std::fs::read(&direct_flpdf).unwrap(),
        std::fs::read(&direct_qpdf).unwrap(),
        "direct encrypted Generate output must be byte-identical to qpdf 11.9.0"
    );

    let donor = tmp.path().join("donor.pdf");
    let donor_args = [
        "--static-id",
        "--static-aes-iv",
        "--encrypt",
        "",
        "",
        "128",
        "--use-aes=y",
        "--",
    ];
    let donor_result = ShellCommand::new("qpdf")
        .args(donor_args)
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&donor)
        .output()
        .unwrap();
    assert!(
        donor_result.status.success(),
        "qpdf donor generation failed: {}",
        String::from_utf8_lossy(&donor_result.stderr)
    );

    let copy_qpdf = tmp.path().join("copy-qpdf.pdf");
    let copy_flpdf = tmp.path().join("copy-flpdf.pdf");
    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--static-id",
            "--static-aes-iv",
            "--object-streams=generate",
        ])
        .arg(format!("--copy-encryption={}", donor.display()))
        .args(["--encryption-file-password=", "--"])
        .arg(&input)
        .arg(&copy_qpdf)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf copy-encryption Generate reference failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--static-aes-iv",
            "--object-streams=generate",
            "--copy-encryption",
        ])
        .arg(&donor)
        .args(["--encryption-file-password", "", "--"])
        .arg(&input)
        .arg(&copy_flpdf)
        .assert()
        .success();
    assert_eq!(
        std::fs::read(&copy_flpdf).unwrap(),
        std::fs::read(&copy_qpdf).unwrap(),
        "copy-encryption Generate output must be byte-identical to qpdf 11.9.0"
    );
}

/// The 16 bytes at the head of the first stream payload. Under AES-CBC that is
/// the initialization vector, which the encrypting side writes ahead of the
/// ciphertext.
fn leading_stream_vector(pdf: &[u8]) -> Vec<u8> {
    let needle = b"stream\n";
    let at = pdf
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("an encrypted document has at least one stream")
        + needle.len();
    pdf[at..at + 16].to_vec()
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

// ── --copy-encryption tests (flpdf-9hc.4.11) ───────────────────────────

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

#[test]
fn qdf_copy_encryption_works_on_top_level_and_rewrite_surfaces() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "donor-user", "donor-owner");

    for (surface, rewrite) in [("top-level", false), ("rewrite", true)] {
        let output = tmp.path().join(format!("{surface}-copied-qdf.pdf"));
        let mut command = Command::cargo_bin("flpdf").unwrap();
        if rewrite {
            command.arg("rewrite");
        }
        command
            .args(["--qdf", "--static-id", "--static-aes-iv"])
            .arg("--copy-encryption")
            .arg(&donor)
            .args(["--encryption-file-password", "donor-user"])
            .arg(fixture(QDF_ENCRYPTION_FIXTURE))
            .arg(&output)
            .assert()
            .success();

        assert_qdf_encrypted_output(&output, "donor-user");
    }
}

/// `--copy-encryption` produces an output that carries /Encrypt and that
/// flpdf itself can round-trip through its own reader with both user and
/// owner passwords.
#[test]
fn copy_encryption_output_has_encrypt_dict() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "secretuser", "secretowner");
    let out = tmp.path().join("copy_out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption",
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
        "copy-encryption output must carry /Encrypt"
    );
}

/// The output of `--copy-encryption` decrypts with the donor's user
/// password through qpdf and reports V=4 / R=4 AESv2 — confirming the
/// /Encrypt scheme was copied, not re-derived.
#[test]
fn copy_encryption_decrypts_with_donor_user_password_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "donoruser", "donorowner");
    let out = tmp.path().join("copy_out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption",
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
        "qpdf must report R=4 on copy-encryption output: {stdout}"
    );
    assert!(
        stdout.contains("Supplied password is user password"),
        "qpdf must accept donor user password: {stdout}"
    );
}

/// The output of `--copy-encryption` also decrypts with the donor's
/// owner password.
#[test]
fn copy_encryption_decrypts_with_donor_owner_password_via_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "userpass", "ownerpass");
    let out = tmp.path().join("copy_out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption",
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

/// `--copy-encryption` on a one-page fixture produces output that
/// qpdf can fully decrypt (not just inspect) — confirming stream encryption
/// is consistent with the copied /Encrypt dict.
#[test]
fn copy_encryption_round_trip_decrypts_cleanly_via_qpdf() {
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
            "--copy-encryption",
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
        "qpdf --decrypt failed on copy-encryption output: {}",
        String::from_utf8_lossy(&decrypt.stderr)
    );
}

/// `rewrite` subcommand also supports `--copy-encryption`.
#[test]
fn rewrite_subcommand_copy_encryption_succeeds() {
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
            "--copy-encryption",
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
        "rewrite --copy-encryption: qpdf must report R=4 + user-password match: {stdout}"
    );
}

/// `--copy-encryption` (a V=4 AES-128 donor) applied to a low-version
/// input must floor the output PDF header to 1.6 per qpdf QPDFWriter.cc L810:
/// V=4 with AES needs at least 1.6 (AES-128 CBC arrived in PDF 1.6). Prior to
/// the encryption-floor fix flpdf floored to 1.5, matching what RC4 needs but
/// under-shooting AES; the donor here is AES-128 (see `make_donor_pdf`).
/// (one-page.pdf is %PDF-1.3.)
#[test]
fn copy_encryption_floors_pdf_header_to_1_6() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "user-pw", "owner-pw");
    let out = tmp.path().join("copied.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--copy-encryption")
        .arg(&donor)
        .args(["--encryption-file-password", "user-pw", "--"])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&out)
        .assert()
        .success();
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-1.6"),
        "copy-encryption (V=4 AES-128 donor) must floor the header to 1.6, got {:?}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(12)])
    );
}

/// `--copy-encryption` applied to a plaintext donor is rejected with a
/// clear "not encrypted" diagnostic.
#[test]
fn copy_encryption_unencrypted_donor_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption",
            fixture(UNENCRYPTED_FIXTURE).to_str().unwrap(),
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains("not encrypted"));
}

/// `--copy-encryption` accepts V=4 AES-128 donors only. An encrypted
/// donor outside that shape (here V=5 AES-256) is rejected with a message
/// naming the accepted shape rather than failing silently.
#[test]
fn copy_encryption_non_v4_aes128_donor_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = tmp.path().join("donor256.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--encrypt",
            "donoruser",
            "donorowner",
            "256",
            "--",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&donor)
        .assert()
        .success();

    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption",
            donor.to_str().unwrap(),
            "--encryption-file-password",
            "donoruser",
        ])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "only V=4 AES-128 donors are accepted",
        ));
}

/// `--copy-encryption` with a wrong password is rejected with an error
/// (the donor cannot be opened with the supplied password).
#[test]
fn copy_encryption_wrong_password_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "correctpw", "ownerpw");
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--copy-encryption",
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
        // --copy-encryption prefix, so we just pin that.
        .stderr(predicates::str::contains("--copy-encryption"));
    assert!(!out.exists());
}

// ── --force-R5 tests (flpdf-9hc.4.15) ──────────────────────────────────────

/// `--force-R5` produces the qpdf-verbatim V=5 R=5 AES-256 report.
#[test]
fn encrypt_force_r5_flpdf_show_encryption_reports_r5() {
    if !ensure_qpdf_or_skip() {
        return;
    }
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

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-encryption", "--password=user-pw"])
        .arg(&output)
        .output()
        .expect("run qpdf --show-encryption");
    assert!(
        qpdf.status.success(),
        "qpdf oracle failed: {:?}",
        qpdf.status
    );

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
    if cfg!(windows) {
        assert_eq!(
            normalize_text_newlines(&show.get_output().stdout),
            normalize_text_newlines(&qpdf.stdout)
        );
        assert_eq!(
            normalize_text_newlines(&show.get_output().stderr),
            normalize_text_newlines(&qpdf.stderr)
        );
    } else {
        assert_eq!(show.get_output().stdout, qpdf.stdout);
        assert_eq!(show.get_output().stderr, qpdf.stderr);
    }
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

/// qpdf's bare-option parser discards an attached value on `--force-R5`.
#[test]
fn encrypt_force_r5_ignores_attached_value() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("nope.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--encrypt", "u", "o", "256", "--force-R5=y", "--"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg(&output)
        .assert()
        .success();
    assert!(
        output.exists(),
        "attached bare-option value must be discarded"
    );
}

/// qpdf accepts `--force-R5` without the RC4 weak-crypto opt-in. R=5 is
/// deprecated, but it uses AES-256 and is not covered by QPDFJob's RC4 gate.
#[test]
fn encrypt_force_r5_succeeds_without_allow_weak_crypto() {
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
        .success();
    assert!(
        output.exists(),
        "qpdf's force-R5 path must write an output PDF"
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

    // Compare the structural xref-stream contract with qpdf 11.9.0. Object
    // numbering and offsets are intentionally not byte-compared here, but the
    // qpdf-owned dictionary order, dynamic widths, PNG predictor, and full
    // range handling must agree.
    let qpdf_output = tmp.path().join("qpdf_encrypted_objstm.pdf");
    let qpdf_result = ShellCommand::new("qpdf")
        .args([
            "--static-id",
            "--static-aes-iv",
            "--object-streams=generate",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf_result.status.success(),
        "qpdf xref-stream reference failed:\n{}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );
    let qpdf_bytes = std::fs::read(qpdf_output).unwrap();
    let flpdf_dictionary = xref_stream_dictionary(&bytes);
    let qpdf_dictionary = xref_stream_dictionary(&qpdf_bytes);
    assert_eq!(
        xref_stream_dictionary_keys(flpdf_dictionary),
        xref_stream_dictionary_keys(qpdf_dictionary),
        "flpdf and qpdf xref-stream dictionaries must use the same key order:\nflpdf={}\nqpdf={}",
        String::from_utf8_lossy(flpdf_dictionary),
        String::from_utf8_lossy(qpdf_dictionary)
    );
    assert_qpdf_xref_stream_dictionary_contract(flpdf_dictionary);
    assert_qpdf_xref_stream_dictionary_contract(qpdf_dictionary);

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

#[test]
fn encrypt_with_generate_object_streams_uncompressed_xref_matches_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("encrypted_objstm_uncompressed_xref.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--object-streams=generate",
            "--compress-streams=n",
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

    let qpdf_output = tmp
        .path()
        .join("qpdf_encrypted_objstm_uncompressed_xref.pdf");
    let qpdf_result = ShellCommand::new("qpdf")
        .args([
            "--static-id",
            "--static-aes-iv",
            "--object-streams=generate",
            "--compress-streams=n",
            "--encrypt",
            "user-pw",
            "owner-pw",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf_result.status.success(),
        "qpdf uncompressed xref-stream reference failed:\n{}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    let flpdf_bytes = std::fs::read(&output).unwrap();
    let qpdf_bytes = std::fs::read(&qpdf_output).unwrap();
    let flpdf_dictionary = xref_stream_dictionary(&flpdf_bytes);
    let qpdf_dictionary = xref_stream_dictionary(&qpdf_bytes);
    assert_eq!(
        xref_stream_dictionary_keys(flpdf_dictionary),
        xref_stream_dictionary_keys(qpdf_dictionary),
        "uncompressed flpdf and qpdf xref-stream dictionaries must use the same key order:\nflpdf={}\nqpdf={}",
        String::from_utf8_lossy(flpdf_dictionary),
        String::from_utf8_lossy(qpdf_dictionary)
    );
    assert_qpdf_uncompressed_xref_stream_dictionary_contract(flpdf_dictionary);
    assert_qpdf_uncompressed_xref_stream_dictionary_contract(qpdf_dictionary);
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
        .args(["rewrite", "--object-streams=generate"])
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

// ── --linearize + --encrypt / --copy-encryption (flpdf-txag) ──────────
//
// qpdf itself supports `--linearize --encrypt ... --` and
// `--linearize --copy-encryption=...` (verified empirically against qpdf
// 11.9.0), so both combinations must remain reachable through the CLI.

/// `rewrite --linearize --encrypt ... --` succeeds end-to-end and produces a
/// file `qpdf --check` accepts as both valid and linearized.
#[test]
fn rewrite_linearize_encrypt_produces_valid_linearized_encrypted_pdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("lin-enc.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--encrypt",
            "",
            "",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "output must carry /Encrypt"
    );

    let check = ShellCommand::new("qpdf")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --check failed on linearized+encrypted output: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("File is linearized"),
        "qpdf --check must report the output as linearized: {stdout}"
    );
    // `check.status.success()` above is qpdf's own pass/fail signal (a
    // successful --check exits 0 even though its trailing summary line always
    // contains the substring "errors", e.g. "No syntax or stream encoding
    // errors found"), so no separate stdout text scan for "error" is needed
    // or would be reliable.
}

/// The top-level `--linearize` alias (not the `rewrite` subcommand) must also
/// thread `--encrypt` through to `write_linearized`. This exercises the
/// top-level dispatch branch separately, since it builds its own
/// `WriterOptions` rather than sharing the `rewrite` subcommand's.
#[test]
fn top_level_linearize_encrypt_produces_valid_linearized_encrypted_pdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("lin-enc-top.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--linearize",
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

    let bytes = std::fs::read(&output).unwrap();
    assert!(
        bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
        "output must carry /Encrypt"
    );

    let check = ShellCommand::new("qpdf")
        .arg("--password=user-pw")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --check failed on top-level linearized+encrypted output: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("File is linearized"),
        "qpdf --check must report the output as linearized: {stdout}"
    );
    assert!(
        stdout.contains("Supplied password is user password"),
        "qpdf must authenticate the user password on the top-level path: {stdout}"
    );
}

/// `--linearize --copy-encryption` succeeds on both the `rewrite`
/// subcommand and the top-level `--linearize` alias. The resulting bytes must
/// be accepted by qpdf as both linearized and encrypted with the donor's
/// password.
#[test]
fn linearize_copy_encryption_produces_valid_encrypted_output() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let donor = make_donor_pdf(&tmp, "donor-user", "donor-owner");

    for (surface, args) in [
        (
            "rewrite",
            vec![
                "rewrite",
                "--linearize",
                "--copy-encryption",
                donor.to_str().unwrap(),
                "--encryption-file-password",
                "donor-user",
            ],
        ),
        (
            "top-level",
            vec![
                "--linearize",
                "--copy-encryption",
                donor.to_str().unwrap(),
                "--encryption-file-password",
                "donor-user",
            ],
        ),
    ] {
        let output = tmp.path().join(format!("lin-copy-enc-{surface}.pdf"));
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(args)
            .arg(fixture(ONE_PAGE_FIXTURE))
            .arg(&output)
            .assert()
            .success();

        let check = ShellCommand::new("qpdf")
            .arg("--password=donor-user")
            .arg("--check")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "qpdf --check failed on {surface} linearized copy-encryption output: stdout={} stderr={}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let stdout = String::from_utf8_lossy(&check.stdout);
        assert!(
            stdout.contains("File is linearized")
                && stdout.contains("Supplied password is user password"),
            "qpdf must report a valid linearized encrypted output on {surface}: {stdout}"
        );
    }
}

/// `rewrite --linearize --object-streams=generate --encrypt ...` is accepted
/// and produces a qpdf-checkable linearized encrypted PDF. qpdf supports this
/// combination, so it must not retain the former compatibility guard.
#[test]
fn rewrite_linearize_encrypt_object_streams_generate_produces_valid_output() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("lin-objstm-enc.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            "--object-streams=generate",
            "--encrypt",
            "",
            "",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(fixture(ONE_PAGE_FIXTURE))
        .arg(&output)
        .assert()
        .success();

    let check = ShellCommand::new("qpdf")
        .arg("--password=")
        .arg("--check")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "qpdf --check failed on linearized object-stream encrypted output: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(
        stdout.contains("File is linearized")
            && stdout.contains("Supplied password is user password"),
        "qpdf must report a valid linearized object-stream encrypted output: {stdout}"
    );
}

/// Whole-document byte parity for linearized AES-128 (V=4/AESV2) encrypted
/// output against real qpdf.
///
/// Same rationale as `encrypted_document_is_byte_identical_to_qpdf` above
/// (V=4/AESV2 only, `qpdf-zlib-compat`-gated for the DEFLATE backend), plus
/// `--linearize`: qpdf's linearized+encrypted object-number sequence
/// (`QPDFWriter.cc:2563-2624`) places the `/Encrypt` dictionary right before
/// the hint stream, writes `/Encrypt <ref> 0 R` only in the first-half
/// trailer, and encrypts the hint stream's payload but not the classic xref
/// table or the `/Encrypt` dictionary object itself. This test is the
/// end-to-end proof that all of that lines up byte-for-byte with qpdf 11.9.0,
/// not just "looks right" per the unit-level assertions that pinned each of
/// those pieces individually.
///
/// Uses one/two/three-page fixtures, not just `ONE_PAGE_FIXTURE`:
/// `RenumberMap::reserve_encrypt_dict_slot`'s object-number shift and the
/// hint stream's shared-object table are close to degenerate on a single
/// page (almost nothing sits after the hint-stream slot to shift, and the
/// shared-object table is trivially small), so a one-page-only comparison
/// would barely exercise the object-renumbering and hint-table machinery
/// this feature actually added.
///
/// Covers both flpdf write surfaces — the top-level `--linearize` alias and
/// the `rewrite --linearize` subcommand — against the *same* qpdf oracle
/// output per fixture. They build `WriterOptions` independently (see
/// `fix(cli): allow --linearize with --encrypt / --copy-encryption`,
/// which fixed a real bug where the top-level alias silently dropped
/// `--encrypt`), so comparing only one surface would leave the other
/// unverified.
///
/// 256-bit (V=5/AESV3) is excluded for the same reason as the non-linearized
/// test: its random salt bytes are not seeded by `--static-id`/
/// `--static-aes-iv`, so even two qpdf runs of the same invocation diverge.
///
/// The determinism re-run below repeats BOTH surfaces (not just the
/// top-level one) — the two surfaces build `WriterOptions` independently
/// (see the byte-parity check above), so a nondeterminism bug specific to
/// one surface's own option-construction path (e.g. IV-seeding order)
/// could otherwise slip past a determinism check that only re-ran the
/// other.
#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn cli_linearize_encrypt_aes128_byte_identical_to_qpdf() {
    if !ensure_qpdf_or_skip() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();

    let qpdf_args = [
        "--linearize",
        "--static-id",
        "--static-aes-iv",
        "--encrypt",
        "",
        "",
        "128",
        "--use-aes=y",
        "--",
    ];

    // Runs `flpdf [rewrite] <qpdf_args> input output` and returns the
    // output bytes. `subcommand` is `Some("rewrite")` for the `rewrite
    // --linearize` surface, `None` for the top-level `--linearize` alias.
    let run_flpdf = |subcommand: Option<&str>, input: &Path, tag: &str| -> Vec<u8> {
        let out = tmp.path().join(format!("{tag}.pdf"));
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        if let Some(sub) = subcommand {
            cmd.arg(sub);
        }
        cmd.args(qpdf_args).arg(input).arg(&out).assert().success();
        std::fs::read(&out).unwrap()
    };

    for (case, fixture_path) in [
        ("one-page", ONE_PAGE_FIXTURE),
        ("two-page", TWO_PAGE_FIXTURE),
        ("three-page", THREE_PAGE_FIXTURE),
    ] {
        let input = fixture(fixture_path);

        let theirs = tmp.path().join(format!("{case}-qpdf.pdf"));
        let qpdf = std::process::Command::new("qpdf")
            .args(qpdf_args)
            .arg(&input)
            .arg(&theirs)
            .output()
            .unwrap();
        assert!(
            qpdf.status.success(),
            "{case}: qpdf reference run failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let reference = std::fs::read(&theirs).unwrap();

        // Surface 1: the top-level `--linearize` alias.
        let top_level_bytes = run_flpdf(None, &input, &format!("{case}-flpdf-top-level"));
        assert_eq!(
            top_level_bytes, reference,
            "{case}: top-level `flpdf --linearize --encrypt` (AES-128) must be \
             byte-identical to qpdf 11.9.0"
        );

        // Surface 2: the `rewrite --linearize` subcommand.
        let rewrite_bytes = run_flpdf(Some("rewrite"), &input, &format!("{case}-flpdf-rewrite"));
        assert_eq!(
            rewrite_bytes, reference,
            "{case}: `flpdf rewrite --linearize --encrypt` (AES-128) must be \
             byte-identical to qpdf 11.9.0"
        );

        // Determinism check: a second run of each surface's identical
        // invocation must also be byte-identical, proving --static-id
        // --static-aes-iv genuinely eliminate every source of
        // nondeterminism on BOTH paths (not just that this one run
        // happened to match this one qpdf sample).
        let top_level_again = run_flpdf(None, &input, &format!("{case}-flpdf-top-level-again"));
        assert_eq!(
            top_level_bytes, top_level_again,
            "{case}: two flpdf top-level runs of the same --static-id \
             --static-aes-iv invocation must be byte-identical to each other"
        );
        let rewrite_again = run_flpdf(
            Some("rewrite"),
            &input,
            &format!("{case}-flpdf-rewrite-again"),
        );
        assert_eq!(
            rewrite_bytes, rewrite_again,
            "{case}: two `flpdf rewrite` runs of the same --static-id \
             --static-aes-iv invocation must be byte-identical to each other"
        );
    }
}

// ── Round-2 Codex review findings on the top-level --show-encryption ──────
// surface (dispatch conflicts, --no-warn threading, --update-from-json
// routing). Verified against qpdf 11.9.0 directly (see comments below).

/// `--check-linearization` must reject `--show-encryption`: without this
/// clap conflict, `check_linearization` wins the dispatch chain in `main()`
/// and `--show-encryption` is silently dropped rather than surfaced as a
/// usage error.
#[test]
fn top_level_check_linearization_conflicts_with_show_encryption() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check-linearization", "--show-encryption"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// `--json` must reject `--show-encryption` for the same reason: `run_json`
/// wins the dispatch chain in `main()`, so the combination would otherwise
/// silently run JSON mode and drop `--show-encryption`.
#[test]
fn top_level_json_conflicts_with_show_encryption() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json", "--show-encryption"])
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// `--overlay`/`--underlay` are rewrite-output modifiers; the top-level
/// dispatch predicate that decides whether the target is a rewrite must
/// exclude `--show-encryption`, or the overlay would be silently dropped
/// instead of producing the usage-error diagnostic.
#[test]
fn top_level_overlay_rejected_with_show_encryption() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--overlay")
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg("--")
        .arg("--show-encryption")
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "--overlay/--underlay can only be used with rewrite output",
        ));
}

/// `--show-encryption` must reject an output file argument outright, matching
/// qpdf: `QPDFJob::Config::showEncryption()` sets `require_outfile = false`
/// (`QPDFJob_config.cc:554-559`), and `checkConfiguration()`
/// (`QPDFJob.cc:593-594`) turns any output filename into a hard usage error
/// ("no output file may be given for this option", exit 2) regardless of
/// what other flags accompany it -- verified directly against `qpdf
/// --show-encryption --password=... in.pdf out.pdf` (exit 2, same message).
/// Without this clap conflict, flpdf silently accepted and ignored the
/// output argument (exit 0, no file written), which is the wrong axis: the
/// gap is not specific to `--linearize` or any other rewrite-only flag, it
/// is any output filename at all.
#[test]
fn top_level_show_encryption_rejects_output_file() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--show-encryption")
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .arg("/tmp/flpdf-show-encryption-output-must-be-rejected.pdf")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// `--update-from-json` combined with `--show-encryption` must route through
/// the JSON-input-inspection consumer (which applies the update before
/// rendering the report), not the plain file-backed `--show-encryption`
/// path (which would silently ignore `--update-from-json` entirely). A
/// nonexistent `--update-from-json` path distinguishes the two: routed
/// correctly, opening that path fails; silently ignored, the command would
/// succeed and print the encryption report as if `--update-from-json` had
/// never been given.
#[test]
fn top_level_show_encryption_honors_update_from_json_routing() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--update-from-json=/nonexistent/does-not-exist.json")
        .arg("--show-encryption")
        .arg(fixture(UNENCRYPTED_FIXTURE))
        .assert()
        .failure();
}

/// `--no-warn` must suppress *all* diagnostic output for `--show-encryption`
/// on a damaged-but-repairable input, matching qpdf's `--no-warn
/// --show-encryption` (verified: `qpdf --no-warn --show-encryption
/// repairable_input.pdf` prints only "File is not encrypted", with no
/// `WARNING: ...` lines and no trailing "operation succeeded with warnings"
/// summary) -- unlike `--check`, `--show-encryption` has no report body that
/// defers and conditionally replays collected diagnostics, so `--no-warn`
/// must drop them at open time instead.
#[test]
fn top_level_show_encryption_no_warn_suppresses_all_warning_output() {
    let damaged = "../../tests/fixtures/test_driver/repairable_input.pdf";

    let with_warnings = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-encryption", damaged])
        .output()
        .unwrap();
    assert_eq!(with_warnings.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&with_warnings.stderr).contains("WARNING"),
        "sanity check: the undamaged-path run must still show warnings live: {:?}",
        String::from_utf8_lossy(&with_warnings.stderr)
    );

    let no_warn = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--no-warn", "--show-encryption", damaged])
        .output()
        .unwrap();
    assert_eq!(no_warn.status.code(), Some(3));
    assert!(
        no_warn.stderr.is_empty(),
        "--no-warn must suppress all --show-encryption diagnostic output \
         (live WARNING lines and the trailing summary), matching qpdf: {:?}",
        String::from_utf8_lossy(&no_warn.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&no_warn.stdout),
        b"File is not encrypted\n",
        "--no-warn --show-encryption must print only the qpdf report body"
    );
}
