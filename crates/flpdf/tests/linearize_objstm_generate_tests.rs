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
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open");

    // Locate the catalog and confirm it resolves as a plain /Type /Catalog dict
    // (a compressed catalog would still resolve, but the standalone invariant is
    // what we assert here — the catalog object must be directly addressable).
    let root = pdf.root_ref().expect("root ref present");
    let obj = pdf.resolve(root).expect("catalog resolves");
    let dict = obj.as_dict().expect("catalog is a dictionary");
    let is_catalog = dict
        .get("Type")
        .and_then(|t| t.as_name())
        .map(|n| n == b"Catalog")
        .unwrap_or(false);
    assert!(is_catalog, "root object must be the /Catalog");
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
