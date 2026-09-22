//! qpdf 11.9.0 parity for a malformed trailer with a misspelled `/Size` key.

use assert_cmd::Command;
use std::process::Command as ProcessCommand;

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let available = ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false);
    if !available && std::env::var_os("CI").is_some() {
        panic!("{EXPECTED_QPDF_VERSION} is required for trailer-size parity");
    }
    available
}

fn malformed_trailer_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = [0usize; 4];
    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets[3] = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Root 1 0 R /Siqe 7 >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// Same shape, but the size key is spelled correctly and its value is null.
///
/// qpdf's `getKeys()` omits a null-valued entry (`QPDF_Dictionary.cc:getKeys`),
/// so `writeTrailer` never sees this `/Size` and emits no computed one either.
fn null_size_trailer_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = [0usize; 4];
    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets[3] = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size null /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}
fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn misspelled_trailer_size_key_is_preserved_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("bad9.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, malformed_trailer_fixture()).expect("write malformed trailer fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must rewrite malformed trailer fixture");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must rewrite malformed trailer fixture");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "diagnostics differ from qpdf"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert!(
        !contains(&qpdf_bytes, b"/Size"),
        "qpdf oracle must omit /Size"
    );
    assert!(
        contains(&qpdf_bytes, b"/Siqe 7"),
        "qpdf oracle must preserve /Siqe"
    );
    assert_eq!(flpdf_bytes, qpdf_bytes, "output differs from qpdf");
}

#[test]
fn misspelled_trailer_size_key_is_not_added_to_generated_xref_stream() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("bad9.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, malformed_trailer_fixture()).expect("write malformed trailer fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id", "--object-streams=generate"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must generate an xref stream");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id", "--object-streams=generate"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must generate an xref stream");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "diagnostics differ from qpdf"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert!(
        !contains(&qpdf_bytes, b"/Size") && !contains(&flpdf_bytes, b"/Size"),
        "neither normal trailer nor generated xref stream may add /Size"
    );
    assert!(contains(&flpdf_bytes, b"/Siqe 7"));
}

/// A present-but-null `/Size` is invisible to qpdf, so no computed size appears.
///
/// `try_has_key` already treats a null value as absent, but the two trailer
/// serializers had their own predicates: the live xref-stream path used
/// `entries.contains_key` and the classic path special-cased `/Size` *before*
/// its general null suppression. Both now make the same visible-key test.
#[test]
fn null_trailer_size_value_is_invisible_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("null-size.pdf");
    std::fs::write(&input, null_size_trailer_fixture()).expect("write null-size fixture");

    for extra in [Vec::new(), vec!["--object-streams=generate"]] {
        let qpdf_output = directory.path().join(format!("qpdf{}.pdf", extra.len()));
        let flpdf_output = directory.path().join(format!("flpdf{}.pdf", extra.len()));

        let qpdf = ProcessCommand::new("qpdf")
            .arg("--static-id")
            .args(&extra)
            .arg(&input)
            .arg(&qpdf_output)
            .output()
            .expect("qpdf must rewrite the null-size fixture");
        let flpdf = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .env("FLPDF_PROGNAME", "qpdf")
            .arg("--static-id")
            .args(&extra)
            .arg(&input)
            .arg(&flpdf_output)
            .output()
            .expect("flpdf must rewrite the null-size fixture");

        assert_eq!(flpdf.status.code(), qpdf.status.code(), "exit codes differ");
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr),
            "diagnostics differ from qpdf"
        );

        let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
        let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
        assert!(
            !contains(&qpdf_bytes, b"/Size"),
            "oracle guard: qpdf must omit /Size for a null value"
        );
        assert!(
            !contains(&flpdf_bytes, b"/Size"),
            "flpdf must not add a /Size that qpdf treats as absent"
        );
        #[cfg(feature = "qpdf-zlib-compat")]
        assert_eq!(flpdf_bytes, qpdf_bytes, "output must be byte-identical");
    }
}
