//! Integration tests for [`flpdf::fonts::font_entries`].
//!
//! All tests build in-memory PDFs without touching the filesystem. They
//! exercise the page-tree walk that aggregates `/Resources /Font` entries,
//! covering the indirect-reference, inline-dictionary, stream, dedup
//! ("latest wins"), and the depth/cycle/error guard paths described in the
//! module documentation.

use flpdf::fonts::{font_entries, font_entries_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH};
use flpdf::{Error, Object, Pdf};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Minimal PDF builder
// ---------------------------------------------------------------------------

/// Build a PDF from a set of already-serialised indirect objects.
///
/// `objects` is a slice of `(object_number, "<<...>>" body)` where the body is
/// everything between `N 0 obj\n` and `\nendobj\n`. The cross-reference table
/// and trailer are generated automatically; `root` names the `/Root` object.
fn build_pdf(objects: &[(u32, String)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let max_num = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in objects {
        let off = out.len() as u64;
        offsets.push((*num, off));
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }

    let total = max_num as usize + 1;
    let xref_start = out.len() as u64;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    for i in 1..=max_num {
        if let Some((_, off)) = offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Build a serialized font-stream object body (`<< dict >>\nstream\n...endstream`).
fn font_stream_body(dict_extras: &str, data: &[u8]) -> String {
    format!(
        "<< /Type /Font /Subtype /Type0 {dict_extras} /Length {} >>\nstream\n{}\nendstream",
        data.len(),
        String::from_utf8_lossy(data)
    )
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

// ---------------------------------------------------------------------------
// Happy path: no fonts
// ---------------------------------------------------------------------------

#[test]
fn font_entries_empty_when_page_has_no_resources() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.is_empty());
}

#[test]
fn font_entries_empty_when_resources_has_no_font_key() {
    // Resources present but without a /Font sub-dictionary.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /XObject << >> >> >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.is_empty());
}

// ---------------------------------------------------------------------------
// Font value variants: indirect reference, inline dict, stream
// ---------------------------------------------------------------------------

#[test]
fn font_entries_collects_indirect_font() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (
                7,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert_eq!(fonts.len(), 1);
    let f1 = fonts.get(b"F1".as_slice()).expect("F1 present");
    let dict = f1.as_dict().expect("resolved to dictionary");
    assert_eq!(
        dict.get("BaseFont").and_then(Object::as_name),
        Some(b"Helvetica".as_slice())
    );
}

#[test]
fn font_entries_collects_inline_font_dictionary() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Courier >> >> >> >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert_eq!(fonts.len(), 1);
    let dict = fonts.get(b"F1".as_slice()).unwrap().as_dict().unwrap();
    assert_eq!(
        dict.get("BaseFont").and_then(Object::as_name),
        Some(b"Courier".as_slice())
    );
}

#[test]
fn font_entries_indirect_stream_value_is_returned_as_stream() {
    // A /Font value that resolves (via an indirect reference) to a stream is
    // returned as the resolved Object::Stream, NOT normalized to its
    // dictionary. The module doc mentions normalizing streams to their font
    // dictionary, but that only happens for a *direct* Object::Stream value
    // (collect_page_fonts' Stream arm), which the parser never produces because
    // PDF streams are always indirect objects. This test pins the reachable
    // behaviour so the divergence is visible if either side changes.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (7, font_stream_body("/BaseFont /Embedded", b"font-bytes")),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    let entry = fonts.get(b"F1".as_slice()).expect("F1 present");
    let stream = entry
        .as_stream()
        .expect("indirect stream returned verbatim");
    assert_eq!(
        stream.dict.get("BaseFont").and_then(Object::as_name),
        Some(b"Embedded".as_slice())
    );
}

#[test]
fn font_entries_skips_non_dictionary_font_value() {
    // A /Font entry whose value is neither reference/dict/stream is skipped.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 42 /F2 7 0 R >> >> >>"
                    .into(),
            ),
            (
                7,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    // F1 (integer) skipped, F2 (reference) collected.
    assert!(fonts.get(b"F1".as_slice()).is_none());
    assert!(fonts.get(b"F2".as_slice()).is_some());
}

// ---------------------------------------------------------------------------
// Indirect /Resources and indirect /Font dictionaries
// ---------------------------------------------------------------------------

#[test]
fn font_entries_resolves_indirect_resources_dictionary() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (3, "<< /Type /Page /Parent 2 0 R /Resources 8 0 R >>".into()),
            (8, "<< /Font << /F1 7 0 R >> >>".into()),
            (
                7,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.get(b"F1".as_slice()).is_some());
}

#[test]
fn font_entries_resolves_indirect_font_dictionary() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font 9 0 R >> >>".into(),
            ),
            (9, "<< /F1 7 0 R >>".into()),
            (
                7,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.get(b"F1".as_slice()).is_some());
}

#[test]
fn font_entries_resources_wrong_type_yields_no_fonts() {
    // /Resources is neither a dictionary nor a reference: the page is skipped.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources [1 2 3] >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.is_empty());
}

#[test]
fn font_entries_font_wrong_type_yields_no_fonts() {
    // /Font is neither a dictionary nor a reference: no fonts are collected.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font [9 9] >> >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.is_empty());
}

// ---------------------------------------------------------------------------
// Dedup across pages: latest definition wins (qpdf --show-fonts semantics)
// ---------------------------------------------------------------------------

#[test]
fn font_entries_latest_definition_wins() {
    // Two pages both define /F1; the later page (in Kids order) wins.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 8 0 R >> >> >>".into(),
            ),
            (7, "<< /Type /Font /BaseFont /First >>".into()),
            (8, "<< /Type /Font /BaseFont /Second >>".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert_eq!(fonts.len(), 1);
    let dict = fonts.get(b"F1".as_slice()).unwrap().as_dict().unwrap();
    assert_eq!(
        dict.get("BaseFont").and_then(Object::as_name),
        Some(b"Second".as_slice())
    );
}

#[test]
fn font_entries_distinct_names_across_pages_all_kept() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F2 8 0 R >> >> >>".into(),
            ),
            (7, "<< /Type /Font /BaseFont /First >>".into()),
            (8, "<< /Type /Font /BaseFont /Second >>".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert_eq!(fonts.len(), 2);
    assert!(fonts.contains_key(b"F1".as_slice()));
    assert!(fonts.contains_key(b"F2".as_slice()));
}

// ---------------------------------------------------------------------------
// Nested page tree, cycle, depth limit
// ---------------------------------------------------------------------------

#[test]
fn font_entries_walks_nested_pages_tree() {
    // Catalog -> Pages -> Pages -> Page exercises the recursive Kids descent.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 >>".into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 3 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (7, "<< /Type /Font /BaseFont /Helvetica >>".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.contains_key(b"F1".as_slice()));
}

#[test]
fn font_entries_tolerates_cycle_in_pages_tree() {
    // A Pages node that lists itself as a Kid must not loop forever; the
    // visited-set guard stops the descent and no fonts are returned.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [2 0 R] /Count 1 >>".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.is_empty());
}

#[test]
fn font_entries_skips_non_dictionary_kid() {
    // A /Kids entry that resolves to a non-dictionary object is skipped rather
    // than aborting the whole walk; the sibling page is still collected.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [9 0 R 3 0 R] /Count 1 >>".into()),
            (9, "42".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (7, "<< /Type /Font /BaseFont /Helvetica >>".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.contains_key(b"F1".as_slice()));
}

#[test]
fn font_entries_default_depth_constant_is_positive() {
    assert!(DEFAULT_MAX_PAGE_TREE_DEPTH > 0);
}

#[test]
fn font_entries_with_max_depth_rejects_deep_tree() {
    // max_depth = 1 lets the root Pages node process but errors when it tries
    // to descend one level into its Page child.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (7, "<< /Type /Font /BaseFont /Helvetica >>".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let err = font_entries_with_max_depth(&mut pdf, 1).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Catalog / page-tree structural errors
// ---------------------------------------------------------------------------

#[test]
fn font_entries_catalog_not_dictionary_errors() {
    // /Root resolves to a non-dictionary object (here an integer): the walk
    // must surface an Unsupported error instead of panicking.
    let bytes = build_pdf(&[(1, "42".into())], 1);
    let mut pdf = open(bytes);
    let err = font_entries(&mut pdf).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

#[test]
fn font_entries_missing_pages_key_errors() {
    // Catalog without /Pages must surface a Missing("/Pages") error.
    let bytes = build_pdf(&[(1, "<< /Type /Catalog >>".into())], 1);
    let mut pdf = open(bytes);
    let err = font_entries(&mut pdf).unwrap_err();
    assert!(matches!(err, Error::Missing("/Pages")), "got {err:?}");
}
