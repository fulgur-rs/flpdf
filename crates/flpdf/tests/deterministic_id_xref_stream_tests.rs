//! End-to-end checks for the deterministic-`/ID` cross-reference *stream* path.
//!
//! `ObjectStreamMode::Generate` routes to the non-linearized generate writer,
//! which emits qpdf's fixed-key-order xref stream (`/Type /Length /Filter
//! /DecodeParms /W [/Index] [/Info] /Root /Size /ID`, with `/ID` last) and a
//! `/Predictor 12` `/W [1 2 1]` compressed payload. The writer direct-writes the
//! deterministic `/ID` inline at its position, computed from the bytes up to and
//! including the array's opening `[`. This is flpdf's own content-derived
//! identifier (qpdf does not produce byte-parity for the xref-stream form), so
//! the regression baseline below is flpdf's own output: the golden SHA-256 /
//! `/ID` words were captured from the writer on the exact fixture and pin
//! byte-stability across changes.

use flpdf::{write_pdf_with_options, ObjectStreamMode, Pdf, WriteOptions};
#[cfg(not(feature = "qpdf-zlib-compat"))]
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// A minimal one-page classic-xref PDF with an `/Info` dictionary, used to force
/// the writer down the xref-stream output form (via `ObjectStreamMode::Generate`)
/// while exercising the deterministic-`/ID` seed's `/Info` path.
fn one_page_with_info_fixture() -> Vec<u8> {
    let objs: [&[u8]; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        b"<< /Title (Hello World) /Author (Bob) /Count 3 >>",
    ];
    let mut out = b"%PDF-1.5\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Full-rewrite the fixture into xref-stream form with deterministic `/ID`.
/// `ObjectStreamMode::Generate` upgrades the classic-xref-table input to an
/// xref *stream* (xref streams are required to point at ObjStm members), so this
/// exercises the `XrefForm::Stream` writer arm.
fn write_xref_stream_deterministic(src: &[u8]) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(src.to_vec())).expect("fixture must open");
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.object_streams = ObjectStreamMode::Generate;
    opts.deterministic_id = true;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).expect("deterministic xref-stream write");
    out
}

/// The 70-byte all-zero `/ID` placeholder array `[<0×32><0×32>]`.
fn zero_id_placeholder() -> Vec<u8> {
    let mut p = vec![b'['];
    p.push(b'<');
    p.extend(std::iter::repeat_n(b'0', 32));
    p.push(b'>');
    p.push(b'<');
    p.extend(std::iter::repeat_n(b'0', 32));
    p.push(b'>');
    p.push(b']');
    p
}

/// Extract the two hex `/ID` words from the LAST `/ID` array in the output.
#[cfg(not(feature = "qpdf-zlib-compat"))]
fn id_words(bytes: &[u8]) -> (String, String) {
    let id_pos = bytes
        .windows(3)
        .rposition(|w| w == b"/ID")
        .expect("output must contain /ID");
    let bracket = id_pos
        + 3
        + bytes[id_pos + 3..]
            .iter()
            .position(|&b| b == b'[')
            .expect("/ID must be followed by an array");
    let after = &bytes[bracket + 1..];
    let o0 = after.iter().position(|&b| b == b'<').unwrap();
    let c0 = after.iter().position(|&b| b == b'>').unwrap();
    let id0 = String::from_utf8(after[o0 + 1..c0].to_vec()).unwrap();
    let rest = &after[c0 + 1..];
    let o1 = rest.iter().position(|&b| b == b'<').unwrap();
    let c1 = rest.iter().position(|&b| b == b'>').unwrap();
    let id1 = String::from_utf8(rest[o1 + 1..c1].to_vec()).unwrap();
    (id0, id1)
}

// Golden constants captured from the writer (pre-direct-write commit) on
// `one_page_with_info_fixture()`. The direct-write change is byte-preserving, so
// these pin that the output did NOT change.
//
// These goldens depend on the deflate backend: the deterministic `/ID` is a
// digest over the whole output body, which includes the *compressed* xref-stream
// payload. The `qpdf-zlib-compat` feature swaps the deflate backend, producing
// different compressed bytes and therefore a different `/ID` and SHA-256. The
// byte-exact goldens below are pinned for the default backend only; the
// backend-agnostic structural checks (no placeholder, run-stability) run under
// either backend.
// Re-blessed for flpdf-g6hb.1: non-linearized --object-streams=generate now uses
// qpdf's generate-mode numbering (ObjStm container numbered first, members
// renumbered ascending-source) and qpdf's `/Predictor 12` `/W [1 2 1]` xref
// stream (was `/W [1 8 4]`, no predictor, container-above-max). Both change the
// container/xref bytes and therefore this content-derived /ID + SHA-256. The new
// output is qpdf --check clean (verified manually on the fixture).
// Re-blessed again when the library default for `NewlineBeforeEndstream` flipped
// from `Yes` to `Never` (qpdf-parity default). Bodies no longer carry a `\n`
// before each `endstream`, so the digest over the write output shifts.
#[cfg(not(feature = "qpdf-zlib-compat"))]
const GOLDEN_SHA256: &str = "a348ebd012621cc7c5700c2a4c335da27c70d5b7734e7e382a999a2bb6967a0d";
#[cfg(not(feature = "qpdf-zlib-compat"))]
const GOLDEN_ID0: &str = "4b2f80f3a45e4d60d2731eb7604f831d";
#[cfg(not(feature = "qpdf-zlib-compat"))]
const GOLDEN_ID1: &str = "4b2f80f3a45e4d60d2731eb7604f831d";

#[test]
fn xref_stream_deterministic_id_has_no_zero_placeholder() {
    let out = write_xref_stream_deterministic(&one_page_with_info_fixture());
    let placeholder = zero_id_placeholder();
    assert!(
        !out.windows(placeholder.len())
            .any(|w| w == placeholder.as_slice()),
        "direct-write output must not contain the all-zero /ID placeholder"
    );
}

#[test]
fn xref_stream_deterministic_id_is_byte_stable() {
    let a = write_xref_stream_deterministic(&one_page_with_info_fixture());
    let b = write_xref_stream_deterministic(&one_page_with_info_fixture());
    assert_eq!(a, b, "deterministic xref-stream output must be run-stable");
}

#[cfg(not(feature = "qpdf-zlib-compat"))]
#[test]
fn xref_stream_deterministic_id_matches_golden_words() {
    let out = write_xref_stream_deterministic(&one_page_with_info_fixture());
    let (id0, id1) = id_words(&out);
    assert_eq!(id0, GOLDEN_ID0, "/ID[0] (permanent) diverged from golden");
    assert_eq!(id1, GOLDEN_ID1, "/ID[1] (changing) diverged from golden");
    // No source /ID in the fixture, so permanent == changing.
    assert_eq!(id0, id1, "with no source /ID, /ID[0] must equal /ID[1]");
}

#[cfg(not(feature = "qpdf-zlib-compat"))]
#[test]
fn xref_stream_deterministic_id_is_byte_identical_to_golden() {
    let out = write_xref_stream_deterministic(&one_page_with_info_fixture());
    let sha: String = Sha256::digest(&out)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        sha, GOLDEN_SHA256,
        "direct-write xref-stream output must be byte-identical to the pre-change golden"
    );
}
