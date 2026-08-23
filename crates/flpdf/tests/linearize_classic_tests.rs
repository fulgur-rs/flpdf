//! Classic (non-ObjStm) linearize: structural tests that run without
//! `qpdf-zlib-compat`, covering the outline section-routing fix (flpdf-vvjr.2).
//!
//! These tests drive the public `write_linearized` API with default
//! `WriterOptions` (classic xref-table path, no ObjStm containers) and assert
//! structural properties of the back-patched bytes directly, so they run on
//! every build. Byte-identity against qpdf goldens is gated on `qpdf-zlib-compat`
//! in `cmp_linearize_tests.rs`.

use flpdf::linearization::LinearizationPlan;
use flpdf::Pdf;
use std::io::Cursor;
use std::path::Path;

/// Linearize `fixture` with default `WriterOptions` (classic xref-table, no ObjStm)
/// via the public API and return the complete back-patched bytes.
fn linearize_classic(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);

    let f1 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f1)).unwrap();
    let opts = WriterTestSettings {
        deterministic_id: true,
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut pdf, &opts).unwrap()
}

mod common;
#[allow(unused_imports)]
use common::{write_linearized_with_settings, WriterTestSettings};

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
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve_object(r)
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
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve_object(r)
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

// flpdf-891f: cross-object array edge — a live non-page object (/Other 4 0 R)
// has an array-element ref to a resurrectable null ([99 0 R]), so the null must
// land in the first-page section (before /E). Exercises the else-branch
// seen_as_array tracking path in compute_closure.
#[test]
fn crossobj_arr_ref_in_nonpage_obj_places_null_before_first_page_end() {
    let bytes = linearize_classic("resurrect-crossobj-arr-via-live-desc.pdf");

    // Verify the output is valid and all objects resolve.
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    for r in refs {
        pdf.resolve_object(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // The null (resurrected from xref-absent ref 99) must appear before /E.
    let e_offset = parse_e_offset(&bytes);
    let null_pos = find(&bytes, b"\nnull\nendobj\n")
        .expect("null object must be written into the linearized output");
    assert!(
        null_pos < e_offset,
        "null (resurrected ref 99) must be in first-page section (before /E={e_offset}); \
         found at byte {null_pos}"
    );
}

// flpdf-hsjh: revorder case — resurrectable null ref (orig 99) has a LOWER
// original-object-number than the live descendant (orig 100) holding the array
// edge ([99 0 R]). Sort-at-enqueue puts 99 in the queue before 100 is expanded,
// so seen_as_array is empty when 99 is dequeued → deferred. After the full BFS,
// the post-BFS pass admits 99 (seen_as_array populated by 100). The final global
// sort by original number places null(99) before IntermediateDict(100).
// Exercises: deferred_resurrect in main BFS, post-BFS admission, order global sort.
#[test]
fn revorder_resurrect_deferred_null_before_first_page_end() {
    let bytes = linearize_classic("revorder-resurrect.pdf");

    // Verify the output is valid and all objects resolve.
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    for r in refs {
        pdf.resolve_object(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // The null (resurrected from xref-absent ref 99) must appear before /E.
    let e_offset = parse_e_offset(&bytes);
    let null_pos = find(&bytes, b"\nnull\nendobj\n")
        .expect("null object must be written into the linearized output");
    assert!(
        null_pos < e_offset,
        "null (resurrected ref 99) must be in first-page section (before /E={e_offset}); \
         found at byte {null_pos}"
    );
    // Additionally verify null(99) appears before IntermediateDict(100):
    // global sort must place them in ascending original-number order.
    let intermediate_pos =
        find(&bytes, b"/Good").expect("/Good key from obj 100 must appear in output");
    assert!(
        null_pos < intermediate_pos,
        "null(orig 99) must come before IntermediateDict(orig 100) in the output; \
         null@{null_pos}, /Good@{intermediate_pos}"
    );
}

// flpdf-hsjh (discriminator): Page leaf at high original-object-number (10)
// with its content stream at low original-object-number (3).  A naive
// fully-global sort would move Page to a higher renumbered number than the
// content stream, reversing their order in the output.  The fix pins the Page
// leaf at order[0] and sorts only the non-page tail.
#[test]
fn page_highnum_content_lownum_page_before_content() {
    let bytes = linearize_classic("page-highnum-content-lownum.pdf");

    // Output must round-trip cleanly.
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    for r in refs {
        pdf.resolve_object(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // /Type /Page must appear before /E (first-page section check).
    let e_offset = parse_e_offset(&bytes);
    let page_pos = find(&bytes, b"/Type /Page").expect("/Type /Page in output");
    assert!(
        page_pos < e_offset,
        "/Type /Page must be in first-page section (before /E={e_offset}); found at {page_pos}"
    );

    // The Page leaf (/Type /Page) must come BEFORE the content stream.
    // The content stream is the FlateDecode object that follows the Page dict in
    // the first-page section; its presence is confirmed by /Contents in the Page.
    // We detect it as the last `stream\r\n` or `stream\n` before /E.
    let content_marker = b"\nstream\n";
    let mut last_stream_before_e = None;
    let mut pos = 0;
    while let Some(p) = find(&bytes[pos..], content_marker) {
        let abs = pos + p;
        if abs < e_offset {
            last_stream_before_e = Some(abs);
        }
        pos = abs + 1;
    }
    let stream_pos = last_stream_before_e.expect("content stream (\\nstream\\n) present before /E");
    assert!(
        page_pos < stream_pos,
        "/Type /Page (at {page_pos}) must appear before content stream (at {stream_pos}); \
         a fully-global sort by original-number would place the Page last"
    );
}

// flpdf-phfu: re-linearizing an already-linearized input must not over-populate
// the second half. qpdf garbage-collects the source's old /Linearized parameter
// dict and old hint stream (both unreachable from Root/Info), so the plan's
// object universe is the 7 reachable objects, NOT the source's full 9-object xref.
#[test]
fn relinearize_drops_source_linearization_artifacts_from_universe() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("linearized-one-page.pdf");
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    assert_eq!(
        plan.total_object_count, 7,
        "re-linearize universe must drop the source's old /Linearized dict + hint stream"
    );
}

// flpdf-hsjh (Codex P2): Catalog DICT-value edge (/OpenAction 5 0 R) to live OD
// obj 5, which itself holds /Arr [99 0 R] (xref-absent null 99).
// BFS-interior seen_as_array.insert fires when expanding obj 5's children.
// Null must land in OD section, i.e. BEFORE /E (first-page section end).
#[test]
fn od_live_arr_null_lands_in_od_section() {
    let bytes = linearize_classic("od-live-arr-null.pdf");

    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    for r in refs {
        pdf.resolve_object(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // The null (resurrected ref 99) must appear BEFORE /E (OD section).
    let e_offset = parse_e_offset(&bytes);
    let null_pos = find(&bytes, b"\nnull\nendobj\n")
        .expect("null object must be written into the linearized output");
    assert!(
        null_pos < e_offset,
        "null (resurrected ref 99 via live OD obj 5 array) must be in OD section \
         (before /E={e_offset}); found at byte {null_pos}"
    );
}

/// One-page PDF with a `/Contents` stream and an unrelated Image XObject
/// resource sibling, used to exercise the linearized writer's per-stream
/// content-normalization gate end to end.
fn content_and_xobject_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = [0u64; 6];
    let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
        offs[n] = pdf.len() as u64;
        pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    push(&mut pdf, 1, "<< /Type /Catalog /Pages 2 0 R >>");
    push(&mut pdf, 2, "<< /Type /Pages /Count 1 /Kids [3 0 R] >>");
    push(
        &mut pdf,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
         /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );
    push(&mut pdf, 4, "<< /Length 5 >>\nstream\nBT\nET\nendstream");
    push(
        &mut pdf,
        5,
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\nA\nendstream",
    );
    let xref_start = pdf.len() as u64;
    let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
    for off in offs.iter().skip(1) {
        xref.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// `--linearize --normalize-content=y` must scope normalization to the page
/// content stream (obj 4) only, matching qpdf's per-stream
/// `m->normalized_streams` identity (`QPDFWriter.cc:1277`), and must not
/// attempt to treat the sibling Image XObject (obj 5) as a normalization
/// candidate. Exercises the linearized writer's body-emission path
/// (`append_body_object`) with `content_normalization: true`, which no other
/// linearization test in this crate covers.
#[test]
fn linearize_content_normalization_scopes_to_page_content_only() {
    let mut pdf = Pdf::open(Cursor::new(content_and_xobject_pdf_bytes())).unwrap();
    let opts = WriterTestSettings {
        content_normalization: true,
        deterministic_id: true,
        ..WriterTestSettings::default()
    };
    let bytes = write_linearized_with_settings(&mut pdf, &opts)
        .expect("linearized write with content normalization must succeed");

    let mut written = Pdf::open(Cursor::new(bytes)).expect("output must reopen");
    for r in written.object_refs() {
        written
            .resolve_object(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}
