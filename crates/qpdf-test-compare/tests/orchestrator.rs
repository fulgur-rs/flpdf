//! Integration tests for [`qpdf_test_compare::compare_files`].
//!
//! Fixtures are byte constants declared inline. `MINIMAL_PDF` is the
//! 2-object Catalog + Pages document shipped at `tests/fixtures/minimal.pdf`
//! at the workspace root (byte-copy so this crate stays self-contained).
//! The variants layered on top vary a single field so the branch under
//! test is the one that fires.

use qpdf_test_compare::compare_files;

// -------- fixtures --------

/// Baseline: matches `tests/fixtures/minimal.pdf`. 2 in-use objects:
/// obj 1 = Catalog, obj 2 = Pages (Count 0, Kids []). No /ID in trailer.
const MINIMAL_PDF: &[u8] = b"\
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 0 /Kids [] >>
endobj
xref
0 3
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
trailer
<< /Size 3 /Root 1 0 R >>
startxref
110
%%EOF
";

/// Same as `MINIMAL_PDF` but obj 2's `/Count` is `1` instead of `0`. Same
/// byte length (both digits are one byte), so xref offsets and startxref
/// need no adjustment. Trailer is unchanged.
const MINIMAL_PDF_COUNT1: &[u8] = b"\
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 1 /Kids [] >>
endobj
xref
0 3
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
trailer
<< /Size 3 /Root 1 0 R >>
startxref
110
%%EOF
";

/// 3 in-use objects: obj 1 = Catalog, obj 2 = Pages, obj 3 = null. `/Size
/// 4` in the trailer (matching the padded xref) so the trailer compare
/// passes against `MINIMAL_PDF_SIZE4_PADDED_FREE` (which also declares
/// `/Size 4`), and the count-mismatch branch fires on
/// `live_object_refs().len()` (3 vs 2).
///
/// Layout (each `endobj\n` counted):
/// - `%PDF-1.7\n`                                             bytes  0..=  8 ( 9 bytes)
/// - `1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n`   bytes  9..= 57 (49 bytes)
/// - `2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n` bytes 58..=109 (52 bytes)
/// - `3 0 obj\nnull\nendobj\n`                                bytes 110..=129 (20 bytes)
/// - xref block starts at byte 130
const THREE_OBJECT_PDF_SIZE4: &[u8] = b"\
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 0 /Kids [] >>
endobj
3 0 obj
null
endobj
xref
0 4
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
0000000110 00000 n
trailer
<< /Size 4 /Root 1 0 R >>
startxref
130
%%EOF
";

/// 2 in-use objects (obj 1 and obj 2) plus a free-list entry for slot 3.
/// `/Size 4` — matches the trailer of `THREE_OBJECT_PDF_SIZE4` so the
/// trailer compare passes, leaving the count-mismatch branch to fire.
///
/// The free-list entry `0000000000 00001 f` marks slot 3 as free, which
/// `live_object_refs()` filters out (`CacheEntry::Deleted`).
const TWO_OBJECT_PDF_SIZE4_PADDED_FREE: &[u8] = b"\
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 0 /Kids [] >>
endobj
xref
0 4
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
0000000000 00001 f
trailer
<< /Size 4 /Root 1 0 R >>
startxref
110
%%EOF
";

// -------- tests --------

#[test]
fn identical_pdfs_report_no_diff() {
    let out = compare_files(MINIMAL_PDF, MINIMAL_PDF, b"").expect("open + compare");
    assert_eq!(out, None, "identical bytes must compare equal");
}

#[test]
fn different_object_count_reports_different_number_of_objects() {
    // Both trailers say /Size 4 (cleaned trailer bytes match), but one PDF
    // has 3 in-use objects and the other only 2 (with a free-list entry
    // padding slot 3). live_object_refs().len() diverges → count branch.
    let out = compare_files(
        THREE_OBJECT_PDF_SIZE4,
        TWO_OBJECT_PDF_SIZE4_PADDED_FREE,
        b"",
    )
    .expect("open + compare");
    assert_eq!(out.as_deref(), Some("different number of objects"));
}

#[test]
fn per_object_content_diff_labels_as_n_g_without_r() {
    // MINIMAL_PDF and MINIMAL_PDF_COUNT1 share trailer / object count / obj
    // 1's contents; only obj 2's /Count differs. Expected label is "2 0"
    // (matches qpdf's QPDFObjGen::unparse), NOT "2 0 R" (ObjectRef::Display).
    let out = compare_files(MINIMAL_PDF, MINIMAL_PDF_COUNT1, b"").expect("open + compare");
    assert_eq!(out.as_deref(), Some("2 0: object contents differ"));
}
