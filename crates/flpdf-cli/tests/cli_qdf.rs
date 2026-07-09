//! `--qdf` flag and `qdf-fix` subcommand integration tests (flpdf-9hc.6.8).
//!
//! Pins the CLI wiring for QDF:
//!  - `flpdf rewrite --qdf` produces canonical QDF (uncompressed, normalized,
//!    `%QDF-1.0`, `%% Original object ID:`, classic `xref` table, no XRef /
//!    ObjStm stream, indirect `/Length H 0 R` with a holder object).
//!  - Top-level `flpdf --qdf` behaves identically to `rewrite --qdf`.
//!  - The `qdf` subcommand is a byte-for-byte alias of `rewrite --qdf`
//!    (modulo the random trailer `/ID`, which neither path makes
//!    deterministic — see the `/ID`-line normalization in the alias test).
//!  - `qdf-fix` repairs a hand-edited stream's `/Length` holder.
//!  - Conflict diagnostics: `--qdf` + `--object-streams=generate|disable`
//!    is non-fatal (stderr diagnostic, ObjStm still off); `--qdf` +
//!    `--linearize` is fatal (exit 1).
//!
//! qpdf is used only as an external `--check` oracle and follows the same
//! skip policy as `cli_object_streams_qpdf_parity.rs`: hard-fail on CI,
//! soft-skip locally.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;
use std::process::Command as ShellCommand;

// ---------------------------------------------------------------------------
// qpdf availability guard (same policy as cli_object_streams_qpdf_parity.rs)
// ---------------------------------------------------------------------------

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns `true` when the caller should return early (qpdf missing and skip
/// allowed). Panics on CI where qpdf is a hard dependency.
#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    if on_ci {
        panic!(
            "qpdf is required for cli_qdf tests on CI; install qpdf \
             in the workflow before running this test suite"
        );
    }
    eprintln!(
        "skipping: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    true
}

/// `qpdf --check <path>` exit status.
fn qpdf_check(path: &std::path::Path) -> std::process::ExitStatus {
    ShellCommand::new("qpdf")
        .args(["--check", path.to_str().unwrap()])
        .status()
        .expect("failed to spawn qpdf")
}

// ---------------------------------------------------------------------------
// Fixture: a minimal single-page PDF with one content stream (so the QDF
// indirect `/Length H 0 R` holder is exercised).
// ---------------------------------------------------------------------------

fn fixture_with_stream() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec(),
        // `/Resources` is a required (inheritable) Page attribute; qpdf 12.x
        // warns and bumps `qpdf --check` to exit 3 without it (qpdf 11.x was
        // silent), so the empty (but valid) resources dict is spelled out.
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> /Contents 4 0 R >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Length 9 >>\nstream\nHello PDF\nendstream\nendobj\n".to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for object in &objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    fixture.as_file_mut().write_all(&bytes).unwrap();
    fixture
}

/// Assert the bytes look like canonical QDF.
fn assert_canonical_qdf(rendered: &[u8]) {
    let contains = |needle: &[u8]| rendered.windows(needle.len()).any(|w| w == needle);

    assert!(contains(b"%QDF-1.0"), "expected %QDF-1.0 header marker");
    assert!(
        contains(b"%% Original object ID:"),
        "expected %% Original object ID: comments"
    );
    assert!(
        contains(b"\nxref\n"),
        "expected a classic `xref` table keyword"
    );
    assert!(!contains(b"/Type /XRef"), "QDF must not use an xref stream");
    assert!(
        !contains(b"/Type /ObjStm"),
        "QDF must not use object streams"
    );
    // Indirect stream length: `/Length H 0 R` (not an inline integer).
    let has_indirect_length = rendered
        .windows(b"/Length ".len())
        .enumerate()
        .filter(|(_, w)| *w == b"/Length ")
        .any(|(i, _)| {
            let tail = &rendered[i + b"/Length ".len()..];
            // first non-digit run then ` 0 R`
            let mut j = 0;
            while j < tail.len() && tail[j].is_ascii_digit() {
                j += 1;
            }
            j > 0 && tail[j..].starts_with(b" 0 R")
        });
    assert!(
        has_indirect_length,
        "expected an indirect `/Length H 0 R` stream length"
    );
}

/// Strip the line containing the trailer `/ID [...]` so two QDF outputs that
/// differ only by the per-process random `/ID` compare equal. The `qdf`
/// subcommand keeps its back-compat arg set (no `--static-id`), so raw-byte
/// identity is not achievable by flag alignment.
fn strip_id_line(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        if line.windows(4).any(|w| w == b"/ID ") {
            continue;
        }
        out.extend_from_slice(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn rewrite_qdf_produces_canonical_qdf() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let rendered = std::fs::read(&output).unwrap();
    assert_canonical_qdf(&rendered);

    if skip_if_qpdf_missing() {
        return;
    }
    assert!(
        qpdf_check(&output).success(),
        "qpdf --check should pass on rewrite --qdf output"
    );
}

#[test]
fn top_level_qdf_matches_rewrite_qdf() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let rewrite_out = temp.path().join("rewrite.pdf");
    let toplevel_out = temp.path().join("toplevel.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            input.path().to_str().unwrap(),
            rewrite_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            input.path().to_str().unwrap(),
            toplevel_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let a = strip_id_line(&std::fs::read(&rewrite_out).unwrap());
    let b = strip_id_line(&std::fs::read(&toplevel_out).unwrap());
    assert_eq!(
        a, b,
        "top-level --qdf must be identical to `rewrite --qdf` (modulo /ID)"
    );
    assert_canonical_qdf(&std::fs::read(&toplevel_out).unwrap());
}

#[test]
fn qdf_subcommand_is_alias_of_rewrite_qdf() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let rewrite_out = temp.path().join("rewrite.pdf");
    let subcmd_out = temp.path().join("sub.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            input.path().to_str().unwrap(),
            rewrite_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "qdf",
            input.path().to_str().unwrap(),
            subcmd_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let a = strip_id_line(&std::fs::read(&rewrite_out).unwrap());
    let b = strip_id_line(&std::fs::read(&subcmd_out).unwrap());
    assert_eq!(
        a, b,
        "`qdf` subcommand must be a byte-for-byte alias of `rewrite --qdf` \
         (modulo the per-process random trailer /ID)"
    );
}

#[test]
fn qdf_fix_repairs_hand_edited_stream_length() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let qdf_out = temp.path().join("qdf.pdf");
    let edited = temp.path().join("edited.pdf");
    let fixed = temp.path().join("fixed.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            input.path().to_str().unwrap(),
            qdf_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Hand-edit the stream payload: grow "Hello PDF" -> "Hello PDF EDITED!!!"
    // The QDF indirect /Length holder still says the old length until fixed.
    // QDF form always inserts a newline before `endstream` (qpdf --qdf
    // parity), regardless of the caller's `newline_before_endstream` setting,
    // so `endstream` sits on its own line.
    let original = std::fs::read(&qdf_out).unwrap();
    let edited_bytes = {
        let from = b"stream\nHello PDF\nendstream".as_slice();
        let to = b"stream\nHello PDF EDITED!!!\nendstream".as_slice();
        let pos = original
            .windows(from.len())
            .position(|w| w == from)
            .expect("stream payload present in QDF output");
        let mut v = original[..pos].to_vec();
        v.extend_from_slice(to);
        v.extend_from_slice(&original[pos + from.len()..]);
        v
    };
    std::fs::write(&edited, &edited_bytes).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["qdf-fix", edited.to_str().unwrap(), fixed.to_str().unwrap()])
        .assert()
        .success();

    if skip_if_qpdf_missing() {
        return;
    }
    // The hand-edited (unfixed) file has a stale /Length and fails qpdf;
    // after qdf-fix the indirect length holder is corrected and it passes.
    // NOTE: qpdf emits "WARNING ... expected endstream / recovered stream
    // length" lines on the *pre-fix* (intentionally corrupt) file. Those
    // warnings are expected here and are not a test failure.
    assert!(
        !qpdf_check(&edited).success(),
        "the hand-edited (unfixed) QDF should fail qpdf --check"
    );
    assert!(
        qpdf_check(&fixed).success(),
        "qdf-fix output should pass qpdf --check"
    );
}

#[test]
fn qdf_object_streams_generate_emits_diagnostic_and_proceeds() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            "--object-streams=generate",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "--qdf forces object streams off; ignoring --object-streams=generate",
        ));

    let rendered = std::fs::read(&output).unwrap();
    assert!(
        !rendered
            .windows(b"/Type /ObjStm".len())
            .any(|w| w == b"/Type /ObjStm"),
        "QDF must not emit ObjStm even with --object-streams=generate"
    );
}

#[test]
fn qdf_object_streams_disable_emits_diagnostic() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            "--object-streams=disable",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "--qdf forces object streams off; ignoring --object-streams=disable",
        ));
}

#[test]
fn qdf_linearize_is_rejected_rewrite() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--qdf",
            "--linearize",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "--qdf and --linearize cannot be used together",
        ));
}

#[test]
fn qdf_linearize_is_rejected_top_level() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--linearize",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "--qdf and --linearize cannot be used together",
        ));
}

/// Regression for roborev #194: top-level `--qdf` combined with a page
/// operation must be rejected (exit 1) rather than silently emitting a
/// non-QDF document.
#[test]
fn qdf_page_ops_is_rejected_top_level() {
    let input = fixture_with_stream();
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "--rotate=+90",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "--qdf cannot be combined with --pages/--rotate/--split-pages",
        ));
}
