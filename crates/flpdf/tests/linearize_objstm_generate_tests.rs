//! Generate-mode linearized ObjStm packing: exercises the first-half (Part-3)
//! ObjStm container layout that matches qpdf 11.9.0's linearized member set
//! ({first-page `/Font` dict, `/Font`, `/Info`, `/Pages` tree} in one first-half
//! container, `/Catalog` standalone).
//!
//! These tests drive the public `write_linearized` API with
//! `ObjectStreamMode::Generate` and assert structural properties of the
//! back-patched bytes WITHOUT requiring qpdf at test time (the offsets/markers
//! are parsed directly), so they run on every build and cover the
//! generate-multipage writer / plan / renumber / hint-reconciliation paths.

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{ObjectStreamMode, Pdf, WriteOptions};
use std::io::Cursor;
use std::path::Path;

/// Linearize `fixture` with `--object-streams=generate` via the public API and
/// return the complete back-patched bytes.
fn linearize_generate(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);

    let f1 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f1)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
    let renumber = RenumberMap::from_plan(&plan);

    let f2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(f2)).unwrap();

    let mut opts = WriteOptions::default();
    opts.object_streams = ObjectStreamMode::Generate;
    opts.deterministic_id = true;

    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

/// Parse the `/E` (end-of-first-page) value from the linearization parameter
/// dictionary in the leading bytes of a linearized file.
fn parse_e_offset(bytes: &[u8]) -> usize {
    let needle = b"/E ";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("param dict /E key present");
    let mut i = pos + needle.len();
    let mut val = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        val = val * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    val
}

/// Count `/Type /ObjStm` container markers in the file body.
fn count_objstm_markers(bytes: &[u8]) -> usize {
    let needle = b"/Type /ObjStm";
    bytes.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Byte offset of the first `/Type /ObjStm` container marker.
fn first_objstm_marker_offset(bytes: &[u8]) -> Option<usize> {
    let needle = b"/Type /ObjStm";
    bytes.windows(needle.len()).position(|w| w == needle)
}

/// The first-half (Part-3) ObjStm container holding the first-page shared dicts
/// (plus the `/Pages` tree and `/Info`) must be emitted BEFORE `/E`, and the
/// document round-trips (every object, including compressed members, resolves).
#[test]
fn three_page_generate_packs_first_half_container_before_e() {
    let bytes = linearize_generate("three-page.pdf");

    // Round-trip: every object resolves (compressed members reachable via the
    // type-2 xref entries that the per-half layout emits).
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // Exactly one ObjStm container (qpdf's single first-half container).
    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 1,
        "three-page generate must emit exactly one ObjStm container, found {n_objstm}"
    );

    // The container marker must be physically before /E (first-page section).
    let e_off = parse_e_offset(&bytes);
    let marker = first_objstm_marker_offset(&bytes).expect("ObjStm marker present");
    assert!(
        marker < e_off,
        "the first-half ObjStm container (marker at {marker}) must be before /E ({e_off})"
    );
}

/// The `/Catalog` must stay a standalone (uncompressed) indirect object — never
/// folded into the first-half container — matching qpdf's linearized layout.
#[test]
fn three_page_generate_keeps_catalog_standalone() {
    let bytes = linearize_generate("three-page.pdf");
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("Pdf::open");

    // The root must resolve to a /Type /Catalog dict ...
    let root = pdf.root_ref().expect("root ref present");
    let obj = pdf.resolve(root).expect("catalog resolves");
    let dict = obj.as_dict().expect("catalog is a dictionary");
    let is_catalog = dict
        .get("Type")
        .and_then(|t| t.as_name())
        .map(|n| n == b"Catalog")
        .unwrap_or(false);
    assert!(is_catalog, "root object must be the /Catalog");

    // ... and it must be UNCOMPRESSED: a standalone indirect object is emitted
    // as a top-level `<num> 0 obj` marker in the file body, whereas a compressed
    // ObjStm member has no such marker (it lives inside the container's stream).
    // qpdf keeps the linearized /Catalog standalone; assert the marker exists.
    let marker = format!("\n{} 0 obj", root.number);
    let present = bytes.windows(marker.len()).any(|w| w == marker.as_bytes());
    assert!(
        present,
        "the /Catalog ({} 0 obj) must be a standalone (uncompressed) indirect \
         object — no `{} 0 obj` marker means it was compressed into an ObjStm",
        root.number, root.number
    );
}

/// Two-page generate also packs a single first-half container before /E
/// (the layout generalises across multi-page fixtures).
#[test]
fn two_page_generate_packs_first_half_container_before_e() {
    let bytes = linearize_generate("two-page.pdf");

    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 1,
        "two-page generate must emit exactly one ObjStm container, found {n_objstm}"
    );
    let e_off = parse_e_offset(&bytes);
    let marker = first_objstm_marker_offset(&bytes).expect("ObjStm marker present");
    assert!(
        marker < e_off,
        "the first-half ObjStm container (marker at {marker}) must be before /E ({e_off})"
    );
}

/// A `>cap` fixture that produces BOTH a first-half (part6) and a second-half
/// (part7) ObjStm container. Exercises the second-half container path: the
/// page-private-font compression, the per-page object-count fold, and the
/// per-page byte-length fold (a page-1 private object compressed into the part7
/// container contributes the container's bytes, not its own). Runs on every
/// build (no qpdf-zlib-compat needed — only structure is asserted).
#[test]
fn mixed_generate_emits_part6_and_part7_containers_and_round_trips() {
    let bytes = linearize_generate("objstm-lin-mixed-60-70.pdf");

    // Two ObjStm containers: one before /E (part6, first-page shared) and one
    // after /E (part7, page-1 private fonts).
    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 2,
        "mixed generate must emit two ObjStm containers (part6 + part7), found {n_objstm}"
    );
    let e_off = parse_e_offset(&bytes);
    let first_marker = first_objstm_marker_offset(&bytes).expect("ObjStm marker present");
    assert!(
        first_marker < e_off,
        "the first-half (part6) ObjStm container (marker at {first_marker}) must be before /E ({e_off})"
    );

    // Round-trip: every object resolves, including both containers' compressed
    // members (the part7 container's members are page-1 private fonts).
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}

/// A fixture whose fonts are shared by pages 1 & 2 (not page 0), with the first
/// chunk even-split into the FIRST-PAGE (part6) container and the rest into a
/// part8 (other-page-shared) container. Exercises the shared-object hint table's
/// Part-8 split — including the branch that skips a part4-shared object folded
/// into a first-page container. Structure-only (no qpdf-zlib-compat).
#[test]
fn threepage_shared_generate_emits_part6_and_part8_containers_and_round_trips() {
    let bytes = linearize_generate("objstm-lin-threepage-2-120.pdf");

    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 2,
        "threepage-shared generate must emit two ObjStm containers (part6 + part8), found {n_objstm}"
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}

/// A fixture with a PLAIN (uncompressed, stream) part8 object — a Form XObject
/// shared by pages 1 & 2 — alongside a part7 container. Exercises the
/// shared-object hint table's plain-Part-8 branch (`first_object_number` taken
/// from a non-compressed shared object). Structure-only round-trip; this fixture
/// is not yet byte-identical to qpdf (it needs part-ordered second-half
/// containers), so no golden comparison is made here.
#[test]
fn disc_part7_part8_generate_round_trips() {
    let bytes = linearize_generate("objstm-lin-disc-2-250-2.pdf");

    // A part7 container (page-1 private fonts) plus a part8 container (page-2
    // private fonts even-split with page-1's tail) — at least two ObjStm
    // containers, and the shared Form XObject stays a plain stream.
    let n_objstm = count_objstm_markers(&bytes);
    assert!(
        n_objstm >= 2,
        "disc generate must emit at least two ObjStm containers, found {n_objstm}"
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}
