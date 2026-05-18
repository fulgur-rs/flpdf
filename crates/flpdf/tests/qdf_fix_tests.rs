//! Tests for [`flpdf::fix_qdf`].
//!
//! The committed fixtures under `tests/fixtures/qdf-fix/` make these tests
//! deterministic without requiring `qpdf`/`fix-qdf` at run time:
//!
//! * `*-clean.qdf`        — a pristine `qpdf --qdf` output (the QDF form).
//! * `corrupt-*.qdf`      — a hand-corrupted copy (stale length / shifted
//!   offsets / wrong `/Size` / wrong `startxref`).
//! * `corrupt-*.golden.qdf` — the byte-exact output of the system
//!   `fix-qdf < corrupt-*.qdf` oracle (qpdf 11.9.0).
//!
//! `flpdf::fix_qdf` must reproduce the oracle golden byte-for-byte.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("qdf-fix")
}

fn read(name: &str) -> Vec<u8> {
    fs::read(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Each corrupted fixture, fixed by `flpdf::fix_qdf`, must equal the committed
/// oracle golden byte-for-byte.
#[test]
fn matches_oracle_golden_byte_for_byte() {
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
    ] {
        let input = read(&format!("{case}.qdf"));
        let golden = read(&format!("{case}.golden.qdf"));
        let got = flpdf::fix_qdf(&input).unwrap_or_else(|e| panic!("{case}: fix_qdf: {e}"));
        assert_eq!(
            got,
            golden,
            "{case}: flpdf::fix_qdf output does not match the system fix-qdf golden\n\
             got {} bytes, golden {} bytes\nfirst diff at {:?}",
            got.len(),
            golden.len(),
            got.iter().zip(golden.iter()).position(|(a, b)| a != b)
        );
    }
}

/// Running `fix_qdf` on an already-valid QDF file is a no-op (true for both a
/// file with streams and one without).
#[test]
fn no_op_on_clean_qdf() {
    for clean in ["one-page-clean.qdf", "minimal-clean.qdf"] {
        let data = read(clean);
        let got = flpdf::fix_qdf(&data).unwrap();
        assert_eq!(got, data, "{clean}: fix_qdf should be a no-op on clean QDF");
    }
}

/// `fix_qdf(fix_qdf(x)) == fix_qdf(x)` for every corrupted input.
#[test]
fn idempotent() {
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
    ] {
        let input = read(&format!("{case}.qdf"));
        let once = flpdf::fix_qdf(&input).unwrap();
        let twice = flpdf::fix_qdf(&once).unwrap();
        assert_eq!(once, twice, "{case}: fix_qdf is not idempotent");
    }
}

/// The repaired output must be a valid PDF accepted by `qpdf --check`.
/// Gated on `qpdf` availability so the suite still runs without it.
#[test]
fn repaired_output_passes_qpdf_check() {
    if Command::new("qpdf").arg("--version").output().is_err() {
        eprintln!("qpdf not available; skipping qpdf --check verification");
        return;
    }
    let tmp = std::env::temp_dir().join("flpdf_qdf_fix_check.pdf");
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
    ] {
        let input = read(&format!("{case}.qdf"));
        let fixed = flpdf::fix_qdf(&input).unwrap();
        fs::write(&tmp, &fixed).unwrap();
        let out = Command::new("qpdf")
            .arg("--check")
            .arg(&tmp)
            .output()
            .expect("run qpdf --check");
        assert!(
            out.status.success(),
            "{case}: qpdf --check failed on repaired output:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let _ = fs::remove_file(&tmp);
}

/// If the live `fix-qdf` oracle is present, confirm our committed goldens still
/// match it (guards against fixture drift). Skipped when the tool is absent.
#[test]
fn committed_goldens_still_match_live_oracle() {
    if Command::new("fix-qdf").arg("--version").output().is_err() {
        eprintln!("fix-qdf not available; skipping live oracle re-check");
        return;
    }
    for case in [
        "corrupt-length",
        "corrupt-shift",
        "corrupt-size",
        "corrupt-startxref",
        "corrupt-combo",
    ] {
        use std::io::Write;
        let input = read(&format!("{case}.qdf"));
        let golden = read(&format!("{case}.golden.qdf"));
        let mut child = Command::new("fix-qdf")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fix-qdf");
        child.stdin.take().unwrap().write_all(&input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.stdout, golden,
            "{case}: committed golden no longer matches live fix-qdf"
        );
    }
}

/// An object stream in the input is rejected with an `Unsupported` error
/// (QDF mode disables ObjStm; this is the documented failure mode).
#[test]
fn objstm_input_is_unsupported() {
    let mut data = read("one-page-clean.qdf");
    // Inject a fake /ObjStm type into the first object's dictionary.
    let pos = data
        .windows(7)
        .position(|w| w == b"/Type /")
        .expect("a /Type entry to mutate");
    data.splice(pos..pos, b"/Type /ObjStm ".iter().copied());
    let err = flpdf::fix_qdf(&data).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected Unsupported for ObjStm input, got {err:?}"
    );
}

/// Regression for roborev job 989 (qdf_fix.rs robustness):
///   1. A decompressed stream body that contains a line-anchored `xref` must
///      NOT be mistaken for the cross-reference table (use the LAST one).
///   2. A `stream` byte sequence inside a dictionary string value must NOT be
///      mistaken for the `stream` keyword (match it line-anchored).
#[test]
fn ignores_xref_and_stream_inside_object_body() {
    // obj 1: stream whose dict has a string containing the word "stream" and
    // whose decompressed content contains a line `xref`. /Length is indirect
    // (held by obj 4). Initial xref offsets are intentionally bogus zeros —
    // fix_qdf must regenerate them and still pick the real table at the tail.
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n\n");
    pdf.extend_from_slice(b"%% Original object ID: 1 0\n1 0 obj\n");
    pdf.extend_from_slice(b"<<\n  /Length 4 0 R\n  /Note (the word stream appears here)\n>>\n");
    pdf.extend_from_slice(b"stream\nline one\nxref\nendstream\nendobj\n\n");
    pdf.extend_from_slice(
        b"%% Original object ID: 2 0\n2 0 obj\n<<\n  /Type /Catalog\n>>\nendobj\n\n",
    );
    pdf.extend_from_slice(b"%% Original object ID: 4 0\n4 0 obj\n0\nendobj\n\n");
    // Real (tail) xref table with deliberately wrong offsets.
    pdf.extend_from_slice(b"xref\n0 5\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"0000000000 00000 f \n");
    pdf.extend_from_slice(b"0000000000 00000 n \n");
    pdf.extend_from_slice(b"trailer <<\n  /Root 2 0 R\n  /Size 5\n>>\nstartxref\n0\n%%EOF\n");

    let fixed = flpdf::fix_qdf(&pdf).expect("fix_qdf must succeed");
    let s = &fixed;

    // The `xref` line inside obj 1's stream body is preserved verbatim.
    assert!(
        find(s, b"stream\nline one\nxref\nendstream").is_some(),
        "stream body (incl. its inner `xref` line) must be preserved verbatim"
    );

    // Exactly ONE regenerated cross-reference table: a line-anchored `xref`
    // immediately followed by the `0 5` subsection header.
    assert!(
        find(s, b"\nxref\n0 5\n").is_some(),
        "real xref table must be regenerated at the tail"
    );

    // /Length holder (obj 4) recomputed to the verbatim content byte count:
    // "line one\nxref\n" == 14 bytes (after `stream`+EOL, up to line `endstream`).
    assert!(
        find(s, b"4 0 obj\n14\nendobj").is_some(),
        "indirect /Length holder must be recomputed to 14, got:\n{}",
        String::from_utf8_lossy(s)
    );

    // Idempotent.
    let again = flpdf::fix_qdf(&fixed).expect("fix_qdf idempotent");
    assert_eq!(again, fixed, "fix_qdf must be idempotent");
}

/// Tiny substring search helper (tests only).
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
