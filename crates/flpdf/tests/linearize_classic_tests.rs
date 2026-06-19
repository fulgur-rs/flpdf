//! Classic (non-ObjStm) linearize: structural tests that run without
//! `qpdf-zlib-compat`, covering the outline section-routing fix (flpdf-vvjr.2).
//!
//! These tests drive the public `write_linearized` API with default
//! `WriteOptions` (classic xref-table path, no ObjStm containers) and assert
//! structural properties of the back-patched bytes directly, so they run on
//! every build. Byte-identity against qpdf goldens is gated on `qpdf-zlib-compat`
//! in `cmp_linearize_tests.rs`.

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{Pdf, WriteOptions};
use std::io::Cursor;
use std::path::Path;

/// Linearize `fixture` with default `WriteOptions` (classic xref-table, no ObjStm)
/// via the public API and return the complete back-patched bytes.
fn linearize_classic(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);

    let f1 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f1)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let renumber = RenumberMap::from_plan(&plan);

    let f2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(f2)).unwrap();

    let mut opts = WriteOptions::default();
    opts.deterministic_id = true;

    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_e_offset(bytes: &[u8]) -> usize {
    let needle = b"/E ";
    let pos = find(bytes, needle).expect("param dict /E key present");
    let mut i = pos + needle.len();
    let mut val = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        val = val * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    val
}

// flpdf-vvjr.2: classic (non-ObjStm) linearize with /PageMode /UseOutlines.
// Outline objects (dict + 80 items) must appear before /E (first-page section).
// Exercises the plain part6_outline_objects emission path and the plain branch
// of compute_outline_hint_info (unit_of returns the object's own renumbered
// number when it's not in an ObjStm container).
#[test]
fn useoutlines_classic_routes_outlines_to_first_page_and_round_trips() {
    let bytes = linearize_classic("objstm-lin-useoutlines-80-80.pdf");

    // The output must parse as a valid linearized PDF and every object resolves.
    let mut pdf = Pdf::open(Cursor::new(&bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // The hint-stream dict must carry /O (outline objects present in part6).
    let hint_dict_start =
        find(&bytes, b"/Filter /FlateDecode /S ").expect("hint stream dict present");
    let dict_end =
        hint_dict_start + find(&bytes[hint_dict_start..], b">>").expect("hint dict close");
    let hint_dict = &bytes[hint_dict_start..dict_end];
    assert!(
        hint_dict.windows(4).any(|w| w == b" /O "),
        "hint stream dict must carry /O key when /PageMode /UseOutlines: {:?}",
        String::from_utf8_lossy(hint_dict)
    );

    // /Type /Outlines must appear BEFORE the /E boundary (first-page section).
    let e_offset = parse_e_offset(&bytes);
    let outlines_pos = find(&bytes, b"/Type /Outlines").expect("/Type /Outlines in output");
    assert!(
        outlines_pos < e_offset,
        "outline dict must appear before /E ({e_offset}) in UseOutlines mode; \
         found at byte {outlines_pos}"
    );
}

// flpdf-vvjr.2: classic (non-ObjStm) linearize without /PageMode /UseOutlines.
// Outline objects (dict + 80 items) must appear AFTER /E (second-half, part9).
#[test]
fn outlines_classic_routes_outlines_to_second_half_and_round_trips() {
    let bytes = linearize_classic("objstm-lin-outlines-80-80.pdf");

    // The output must parse as a valid linearized PDF and every object resolves.
    let mut pdf = Pdf::open(Cursor::new(&bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // /Type /Outlines must appear AFTER the /E boundary (second-half).
    let e_offset = parse_e_offset(&bytes);
    let outlines_pos = find(&bytes, b"/Type /Outlines").expect("/Type /Outlines in output");
    assert!(
        outlines_pos > e_offset,
        "outline dict must appear after /E ({e_offset}) when UseOutlines is not set; \
         found at byte {outlines_pos}"
    );
}
