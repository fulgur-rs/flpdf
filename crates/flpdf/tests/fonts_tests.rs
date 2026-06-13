//! Integration tests for [`flpdf::fonts::font_entries`].
//!
//! All tests build in-memory PDFs without touching the filesystem. They
//! exercise the page-tree walk that aggregates `/Resources /Font` entries,
//! covering the indirect-reference, inline-dictionary, stream, dedup
//! ("latest wins"), and the depth/cycle/error guard paths described in the
//! module documentation.

use flpdf::fonts::{font_entries, font_entries_with_max_depth};
use flpdf::{Error, Object, Pdf};
use std::io::Cursor;

mod common;
use common::build_pdf;

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
fn font_entries_indirect_stream_value_is_normalized_to_dictionary() {
    // A /Font value that resolves (via an indirect reference) to a stream is
    // normalized to the stream's font dictionary, matching the documented
    // contract ("for streams, the stream's font dictionary"). Because PDF
    // streams are always stored as indirect objects, this is the only path on
    // which a stream-valued font is ever observed, so normalization must happen
    // here in the reference arm rather than only on a direct Object::Stream
    // value (which the parser never produces).
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
    let dict = entry
        .as_dict()
        .expect("indirect stream normalized to its dictionary");
    assert_eq!(
        dict.get("BaseFont").and_then(Object::as_name),
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
    assert!(!fonts.contains_key(b"F1".as_slice()));
    assert!(fonts.contains_key(b"F2".as_slice()));
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
    assert!(fonts.contains_key(b"F1".as_slice()));
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
    assert!(fonts.contains_key(b"F1".as_slice()));
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

#[test]
fn font_entries_propagates_font_reference_resolution_error() {
    // A /Font value referencing an object whose body fails to parse must make
    // font_entries surface the error rather than silently returning an
    // incomplete list. (A missing/deleted reference is not an error: it
    // resolves to Null and is skipped.)
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            // Truncated dictionary: parsing the object body hits EOF mid-dict.
            (7, "<< /Type /Font /BaseFont".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let result = font_entries(&mut pdf);
    assert!(result.is_err(), "expected resolution error, got {result:?}");
}

// ---------------------------------------------------------------------------
// Holder chains (double-indirect ref -> ref -> value). A single-hop resolve
// returns the intermediate reference, not the terminal dictionary, so the
// font would be silently dropped unless the full chain is followed.
// ---------------------------------------------------------------------------

#[test]
fn font_entries_resolves_holder_chain_resources_dictionary() {
    // /Resources is stored behind two indirect hops: 8 0 R -> 10 0 R -> dict.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (3, "<< /Type /Page /Parent 2 0 R /Resources 8 0 R >>".into()),
            (8, "10 0 R".into()),
            (10, "<< /Font << /F1 7 0 R >> >>".into()),
            (
                7,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.contains_key(b"F1".as_slice()));
}

#[test]
fn font_entries_resolves_holder_chain_font_dictionary() {
    // The /Font value is stored behind two indirect hops: 9 0 R -> 11 0 R -> dict.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font 9 0 R >> >>".into(),
            ),
            (9, "11 0 R".into()),
            (11, "<< /F1 7 0 R >>".into()),
            (
                7,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.contains_key(b"F1".as_slice()));
}

#[test]
fn font_entries_resolves_holder_chain_font_entry() {
    // An individual font entry is stored behind two indirect hops:
    // /F1 7 0 R -> 12 0 R -> font dict.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (7, "12 0 R".into()),
            (
                12,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(fonts.contains_key(b"F1".as_slice()));
}

#[test]
fn font_entries_skips_holder_chain_terminating_at_non_dictionary() {
    // A font entry holder chain that terminates at a non-dictionary value:
    // /F1 7 0 R -> 12 0 R -> 42 (an integer). Following the chain to a
    // non-dict terminal skips the font gracefully rather than erroring.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
            ),
            (7, "12 0 R".into()),
            (12, "42".into()),
        ],
        1,
    );
    let mut pdf = open(bytes);
    let fonts = font_entries(&mut pdf).unwrap();
    assert!(!fonts.contains_key(b"F1".as_slice()));
}
