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
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
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

/// Parse an integer-valued key (e.g. `/O `, `/E `) from the linearization
/// parameter dictionary in the leading bytes of a linearized file.
fn parse_param_int(bytes: &[u8], key: &[u8]) -> usize {
    let pos = bytes
        .windows(key.len())
        .position(|w| w == key)
        .unwrap_or_else(|| panic!("param dict {} key present", String::from_utf8_lossy(key)));
    let mut i = pos + key.len();
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

/// A fixture whose catalog `/OpenAction` reaches an action dict + 80 "od-only"
/// font dicts that no page references. qpdf categorizes them `in_open_document`
/// (lc_open_document → part4, the FIRST half right after the Catalog and before
/// the first page), so they even-split into an open-document container numbered
/// ahead of the first-page section. The first page therefore keeps a LOW object
/// number (`/O`), while a part6 (first-page-shared) container holds the page
/// fonts. Pins the open-document routing + first-half part4-before-part6
/// numbering (deflate-independent — object numbers do not depend on the
/// compression backend, so this runs without qpdf-zlib-compat).
#[test]
fn openaction_generate_routes_open_document_container_to_first_half() {
    let bytes = linearize_generate("objstm-lin-openaction-80-80.pdf");

    // Two ObjStm containers: the open-document container (part4) and the
    // first-page-shared container (part6). Both are first-half (before /E).
    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 2,
        "openaction generate must emit two ObjStm containers (open-document + \
         first-page), found {n_objstm}"
    );
    let e_off = parse_e_offset(&bytes);
    let first_marker = first_objstm_marker_offset(&bytes).expect("ObjStm marker present");
    assert!(
        first_marker < e_off,
        "the open-document ObjStm container (marker at {first_marker}) must be in \
         the first half, before /E ({e_off})"
    );

    // /O (first_page_object): the open-document objects are numbered in part4
    // (first half, before the first page), so the first page object keeps qpdf's
    // low number 9 — NOT the high number it had when the OpenAction subtree was
    // mis-routed into the second half.
    let first_page_object = parse_param_int(&bytes, b"/O ");
    assert_eq!(
        first_page_object, 9,
        "first page object number (/O) must be 9 (open-document objects numbered \
         in part4 ahead of the first page); got {first_page_object}"
    );

    // Round-trip: every object resolves, including both containers' compressed
    // members (the open-document container's 82 members + the first-page
    // container's 80 shared fonts).
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}

/// A fixture whose catalog `/Outlines` reaches an outline dict + 80 items that no
/// page references (and no `/PageMode /UseOutlines`). qpdf categorizes them
/// `in_outlines` and emits an Outlines Hint Table (qpdf's `HGeneric`) plus the
/// hint-stream dict `/O` key. Pins the new outline hint table emission +
/// round-trip (deflate-independent: the decoded `nobjects` / `first_object` do
/// not depend on the compression backend, so this runs without qpdf-zlib-compat).
#[test]
fn outlines_generate_emits_outline_hint_table_and_o_key() {
    let bytes = linearize_generate("objstm-lin-outlines-80-80.pdf");

    // The hint-stream dictionary must carry the `/O` (outlines hint table) key.
    let hint_dict_start = bytes
        .windows(b"/Filter /FlateDecode /S ".len())
        .position(|w| w == b"/Filter /FlateDecode /S ")
        .expect("hint stream dict present");
    let dict_end = hint_dict_start
        + bytes[hint_dict_start..]
            .windows(2)
            .position(|w| w == b">>")
            .expect("hint dict close");
    let hint_dict = &bytes[hint_dict_start..dict_end];
    assert!(
        hint_dict.windows(4).any(|w| w == b" /O "),
        "hint stream dict must carry the /O key when outlines exist: {:?}",
        String::from_utf8_lossy(hint_dict)
    );

    // Decode the linearization data and assert the Outlines Hint Table is present
    // with one output unit (the single ObjStm container holding all 81 outline
    // objects). first_object = 3 is deflate-independent (object numbering).
    let dump = flpdf::linearization::show_linearization_bytes(&bytes, "outlines.pdf")
        .expect("show-linearization decode");
    assert!(
        dump.contains("Outlines Hint Table"),
        "decoded linearization data must include the Outlines Hint Table:\n{dump}"
    );
    // `first_object: <n>` is emitted only by the outline (generic) table dump;
    // object 3 is the single ObjStm container holding all 81 outline objects
    // (deflate-independent — object numbering does not depend on the backend).
    assert!(
        dump.contains("first_object: 3"),
        "outline hint table first_object must be the outline container (3):\n{dump}"
    );

    // Round-trip: every object resolves (the outline container's members included).
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}

/// A fixture with TWO open-document ObjStm containers (flpdf-699x).
///
/// The catalog `/OpenAction` action dict carries `/HighRef` (50 high-numbered OD
/// fonts, 100..149) BEFORE `/LowRef` (50 low-numbered OD fonts, 6..55) because
/// H < L lexically in DFS.  The 107 eligible objects split into two containers:
///   C0: [action(5), 100..149 (50), 6..8 (3)] — N=54, all OD, min=5
///   C1: [9..55 (47), pages(2), 56..60 (5)] — N=53, has OD, min=2
///
/// qpdf emits C0 before C1 (ObjGen / even-split order), NOT ascending-min-member
/// order.  This verifies that flpdf's DFS order for open_document_batches is
/// correct: if it erroneously sorted by ascending min-member it would place C1
/// (min=2) before C0 (min=5), diverging from qpdf.
#[test]
fn openaction_multi_od_generates_two_od_containers_in_dfs_order() {
    let bytes = linearize_generate("objstm-lin-openaction-multi-od.pdf");

    // Two ObjStm containers: both open-document (part4), both in the first half.
    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 2,
        "openaction-multi-od must emit exactly two ObjStm containers (both OD), \
         found {n_objstm}"
    );
    let e_off = parse_e_offset(&bytes);
    let first_marker = first_objstm_marker_offset(&bytes).expect("ObjStm marker present");
    assert!(
        first_marker < e_off,
        "the first OD ObjStm container (marker at {first_marker}) must be in the first \
         half, before /E ({e_off})"
    );

    // The two OD containers are both in the first half; verify the second one
    // also appears before /E.
    let second = bytes[first_marker + 1..]
        .windows(b"/Type /ObjStm".len())
        .position(|w| w == b"/Type /ObjStm")
        .map(|p| first_marker + 1 + p)
        .expect("second OD ObjStm container present");
    assert!(
        second < e_off,
        "the second OD ObjStm container (marker at {second}) must also be in the \
         first half, before /E ({e_off})"
    );

    // /O (first_page_object) = 10: open-document objects occupy 2 containers
    // (part4) before the first page, which therefore receives object number 10.
    let first_page_object = parse_param_int(&bytes, b"/O ");
    assert_eq!(
        first_page_object, 10,
        "first page object (/O) must be 10 with two OD containers in the first half; \
         got {first_page_object}"
    );

    // Round-trip: all objects (including both OD containers' compressed members)
    // must resolve in the back-patched output.
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

// flpdf-zbf9: linearizing an ObjStm-bearing input must NOT leak the source's
// /Type /ObjStm and /Type /XRef containers into the body. After the fix the
// output carries exactly one freshly-generated ObjStm container and the two
// regenerated linearization XRef streams (first-page + main) — the same clean
// rebuild qpdf produces (verified against the golden marker counts). A leaked
// source container would push either count up by one. (Default features:
// structural marker count, no qpdf/zlib needed.)
#[test]
fn objstm_bearing_input_drops_source_structural_containers() {
    let bytes = linearize_generate("three-page-objstm.pdf");

    let n_objstm = count_objstm_markers(&bytes);
    assert_eq!(
        n_objstm, 1,
        "expected exactly one (freshly generated) ObjStm container; a stale source \
         container would push this to 2, found {n_objstm}"
    );

    // Linearized output has two XRef streams: the first-page xref and the main
    // xref. A leaked source XRef stream would make three.
    let xref_needle = b"/Type /XRef";
    let n_xref = bytes
        .windows(xref_needle.len())
        .filter(|w| *w == xref_needle)
        .count();
    assert_eq!(
        n_xref, 2,
        "expected the two regenerated linearization XRef streams; a leaked source \
         XRef stream would push this to 3, found {n_xref}"
    );

    // The drop must not strand any reference: every object still resolves.
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("Pdf::open round-trip");
    for r in pdf.object_refs() {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve after drop: {e}"));
    }
}

// flpdf-vvjr.1: /PageMode /UseOutlines causes outline containers to route to
// FirstPage (part6). Exercises route_objstm_containers FirstPage arm and
// page-0 nobjects fold without qpdf-zlib-compat. Byte-parity is gated on
// qpdf-zlib-compat in cmp_linearize_objstm_tests.rs.
#[test]
fn useoutlines_generate_routes_outlines_to_first_page_and_round_trips() {
    let bytes = linearize_generate("objstm-lin-useoutlines-80-80.pdf");

    // The output must parse as a valid linearized PDF.
    let mut pdf = Pdf::open(Cursor::new(&bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }

    // The hint-stream dict must carry /O (outline objects present in part6).
    let hint_dict_start = bytes
        .windows(b"/Filter /FlateDecode /S ".len())
        .position(|w| w == b"/Filter /FlateDecode /S ")
        .expect("hint stream dict present");
    let dict_end = hint_dict_start
        + bytes[hint_dict_start..]
            .windows(2)
            .position(|w| w == b">>")
            .expect("hint dict close");
    let hint_dict = &bytes[hint_dict_start..dict_end];
    assert!(
        hint_dict.windows(4).any(|w| w == b" /O "),
        "hint stream dict must carry /O key when /PageMode /UseOutlines: {:?}",
        String::from_utf8_lossy(hint_dict)
    );

    // The linearization data must show page-0 nobjects = 4: the page object
    // (part2), its content stream (part2), the first-page shared-dicts
    // container (part3), and the outline container (now in part6 = page-0 section).
    let dump = flpdf::linearization::show_linearization_bytes(&bytes, "useoutlines.pdf")
        .expect("show-linearization decode");
    assert!(
        dump.contains("nobjects: 4"),
        "page-0 nobjects must be 4 when outlines route to first-page section:\n{dump}"
    );
}

/// A fixture whose AcroForm widgets appear in BOTH /AcroForm /Fields (open-document)
/// AND page 0 /Annots (first-page closure). qpdf's in_open_document > in_first_page
/// precedence means widgets must NOT be in part2/part3 (first-page section) — they
/// should be left in Part 4 so route_objstm_containers puts their ObjStm container
/// in the OpenDocument slot (first half, before /O). Pins the from_pdf Step 5 fix
/// (flpdf-sjgv): before the fix, widgets land in part2 and inflate
/// page_hints[0].object_count beyond what qpdf computes.
///
/// Fixture layout (W=5, S=10):
///   obj 1: Catalog (/AcroForm 5, /Pages 2)
///   obj 2: Pages
///   obj 3: Page0 (/Annots [6..10], /Resources inline /Font -> 11..20, /Contents 21)
///   obj 4: Page1 (/Resources inline /Font -> 11..20, /Contents 22)
///   obj 5: AcroForm (/Fields [6..10])
///   obj 6-10: Widget annotations (in both /AcroForm /Fields AND page0 /Annots)
///   obj 11-20: Shared fonts (page0 and page1 both reference them → Part 3)
///   obj 21: Content stream for page0
///   obj 22: Content stream for page1
#[test]
fn acroform_widget_page0_peeled_from_first_page_section() {
    use flpdf::ObjectRef;
    use std::collections::BTreeSet;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("objstm-lin-acroform-widget-page0-5-10.pdf");

    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();

    // Widgets: objects 6..=10 (W=5). They are in open_document_set (via /AcroForm
    // /Fields) AND first_page_closure (via page0 /Annots). qpdf's in_open_document
    // precedence means they must be ABSENT from part2 and part3.
    let widget_refs: Vec<ObjectRef> = (6u32..=10)
        .map(|n| ObjectRef {
            number: n,
            generation: 0,
        })
        .collect();

    let part2_set: BTreeSet<_> = plan.part2_objects.iter().copied().collect();
    let part3_set: BTreeSet<_> = plan.part3_objects.iter().copied().collect();

    for r in &widget_refs {
        assert!(
            !part2_set.contains(r),
            "widget {r} must not be in part2 (in_open_document > in_first_page)"
        );
        assert!(
            !part3_set.contains(r),
            "widget {r} must not be in part3 (in_open_document > in_first_page)"
        );
    }

    // Widgets must end up in Part 4 (awaiting OpenDocument container routing).
    let part4: BTreeSet<_> = plan.part4_objects().into_iter().collect();
    for r in &widget_refs {
        assert!(
            part4.contains(r),
            "widget {r} must be in part4 (left for route_objstm_containers OpenDocument routing)"
        );
    }

    // page_hints[0].object_count must reflect only the true first-page section:
    //   part2 = {page0(3), c0(21)} = 2 objects
    //   part3 = {shared fonts 11..=20} = 10 objects
    //   total = 12
    // Without the fix widgets(5) are in part2, giving 7+10 = 17.
    assert_eq!(
        plan.page_hints[0].object_count, 12,
        "page 0 object_count must be 12 (page + content + 10 shared fonts, widgets peeled)"
    );
}

/// Ineligible OD objects (streams) are routed to `part4_open_document_plain` and
/// written as plain indirect objects in the pre-/O region, NOT packed into an ObjStm.
///
/// Fixture layout (obj 8 = `/AP /N` Form XObject appearance stream):
///   obj 1: Catalog  (eligible OD — dict)
///   obj 5: AcroForm (eligible OD — dict)
///   obj 6: Widget   (eligible OD — dict)
///   obj 7: AP dict  (eligible OD — dict)
///   obj 8: Form XObject (ineligible OD — stream)
///
/// After `from_pdf`, obj 8 must be in `part4_open_document_plain` and the eligible
/// dicts in `part4_rest` (to be packed into an OD ObjStm).  Writing must succeed
/// and the plain Form XObject must appear in the output bytes before the ObjStm.
#[test]
fn ineligible_od_stream_routes_to_part4_open_document_plain() {
    use flpdf::ObjectRef;
    use std::collections::BTreeSet;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("objstm-lin-acroform-widget-ap-stream-page0.pdf");

    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();

    // Only obj 8 (the Form XObject stream) must be in part4_open_document_plain.
    let ap_stream = ObjectRef {
        number: 8,
        generation: 0,
    };
    assert_eq!(
        plan.part4_open_document_plain,
        vec![ap_stream],
        "only the ineligible AP stream (obj 8) must be in part4_open_document_plain"
    );

    // The eligible OD dicts (5=AcroForm, 6=Widget, 7=AP dict) must NOT be in
    // part4_open_document_plain; they should appear in part4_objects() (via part4_rest).
    let plain_set: BTreeSet<_> = plan.part4_open_document_plain.iter().copied().collect();
    for n in [5u32, 6, 7] {
        let r = ObjectRef {
            number: n,
            generation: 0,
        };
        assert!(
            !plain_set.contains(&r),
            "eligible OD dict obj {n} must NOT be in part4_open_document_plain"
        );
    }

    // Exercise the writer path: write_linearized must succeed and the output
    // must contain the plain Form XObject before the OD ObjStm container.
    let renumber = RenumberMap::from_plan(&plan);
    let f2 = std::fs::File::open(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join("objstm-lin-acroform-widget-ap-stream-page0.pdf"),
    )
    .unwrap();
    let mut pdf2 = Pdf::open(std::io::BufReader::new(f2)).unwrap();
    let mut opts = WriteOptions::default();
    opts.object_streams = ObjectStreamMode::Generate;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();

    // The new number assigned to the Form XObject is immediately after the Catalog
    // in the renumber map.  Verify it appears as a plain "N 0 obj" before any ObjStm.
    let new_ref = renumber
        .new_for_original(ap_stream)
        .expect("AP stream must have a new number");
    let plain_marker = format!("{} 0 obj", new_ref.number).into_bytes();
    let bytes = &doc.bytes;
    let plain_pos = bytes
        .windows(plain_marker.len())
        .position(|w| w == plain_marker.as_slice())
        .unwrap_or_else(|| {
            panic!(
                "plain marker '{} 0 obj' not found in output",
                new_ref.number
            )
        });

    // ObjStm appears in the output; find it after the Catalog section.
    let objstm_marker = b" /Type /ObjStm";
    let objstm_pos = bytes
        .windows(objstm_marker.len())
        .position(|w| w == objstm_marker)
        .expect("/Type /ObjStm must appear in output");

    assert!(
        plain_pos < objstm_pos,
        "plain Form XObject (byte {plain_pos}) must precede the OD ObjStm (byte {objstm_pos})"
    );
}

/// Regression test: in non-generate mode the OD peeling must NOT apply.
///
/// Oracle: `qpdf --linearize --object-streams=disable` on the AcroForm fixture
/// shows Page 0 with nobjects=12 and nshared=0, meaning all first-page-closure
/// objects (including the 5 widgets) stay in the first-page section.  When
/// `use_generate_objstm=false`, Part 2 must contain the widgets and
/// `page_hints[0].object_count` must equal 17 (7 Part-2 + 10 Part-3 fonts).
#[test]
fn acroform_widget_stays_in_first_page_section_in_disable_mode() {
    use flpdf::ObjectRef;
    use std::collections::BTreeSet;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("objstm-lin-acroform-widget-page0-5-10.pdf");

    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = flpdf::Pdf::open(std::io::BufReader::new(f)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

    let widget_refs: Vec<ObjectRef> = (6u32..=10)
        .map(|n| ObjectRef {
            number: n,
            generation: 0,
        })
        .collect();

    let part2_set: BTreeSet<_> = plan.part2_objects.iter().copied().collect();
    let part4_set: BTreeSet<_> = plan.part4_objects().into_iter().collect();

    for r in &widget_refs {
        assert!(
            part2_set.contains(r),
            "widget {r} must be in part2 in disable mode (no OD peeling)"
        );
        assert!(
            !part4_set.contains(r),
            "widget {r} must NOT be in part4 in disable mode (disjoint partition)"
        );
    }

    // page0 dict(3) + content0(21) + widgets(6-10, =5) = 7 Part-2 objects,
    // plus shared fonts 11-20 (=10) in Part-3 → total 17.
    assert_eq!(
        plan.page_hints[0].object_count, 17,
        "page 0 object_count must be 17 in disable mode (widgets not peeled)"
    );
}

/// OD routing for a widget exclusive to page 1 (r3443001374).
///
/// Fixture: 2-page PDF, AcroForm widget (obj 6) in page 1 /Annots ONLY — not
/// on page 0.  Widget has page_reach==1 (page 1) and is in open_document_set.
///
/// Oracle: qpdf places the widget in the pre-/O OD section, NOT in the
/// second-half page-1 group.  Page 1 has nobjects==2 (page dict + contents).
///
/// Bug path without the fix:
///   per_page_private_objects[1] includes widget (reach=1, not filtered by OD)
///   → page_hints[1].object_count over-counts (would be 3 instead of 2)
///   → part7 pre-pass puts widget in part4_other_pages_private
///   → OD routing in the part8/part9 loop is bypassed for widget.
///
/// With the fix (open_document_set exclusion in per_page_private_objects filter):
///   widget is absent from per_page_private_objects[1]
///   → page_hints[1].object_count == 2 ✓
///   → part7 pre-pass never sees widget
///   → widget reaches OD routing → lands in part4_rest ✓
#[test]
fn acroform_widget_page1_only_routes_to_od_not_part7() {
    use flpdf::ObjectRef;
    use std::collections::BTreeSet;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("objstm-lin-acroform-widget-page1-only.pdf");

    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = flpdf::Pdf::open(std::io::BufReader::new(f)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();

    let widget = ObjectRef {
        number: 6,
        generation: 0,
    };

    // Widget must NOT be in part7 (other-pages-private).
    let private_set: BTreeSet<_> = plan.part4_other_pages_private.iter().copied().collect();
    assert!(
        !private_set.contains(&widget),
        "widget (obj 6) must NOT be in part4_other_pages_private after the OD filter fix"
    );

    // Widget must be in part4_objects() (via OD routing → part4_rest).
    let part4_set: BTreeSet<_> = plan.part4_objects().into_iter().collect();
    assert!(
        part4_set.contains(&widget),
        "widget (obj 6) must appear in part4_objects() after OD routing to part4_rest"
    );

    // page_hints[1].object_count must be 2, not 3.
    // Oracle: qpdf --show-linearization on the qpdf-processed fixture reports
    // Page 1: nobjects=2 (page dict + contents stream; widget is in pre-/O OD section).
    assert_eq!(
        plan.page_hints[1].object_count, 2,
        "page 1 object_count must be 2 (page dict + contents); widget in OD section not counted"
    );
}

/// OD ObjStm container NOT added as Part-8 SOHT entry when its members span
/// multiple later pages (r3443001371).
///
/// Fixture: 3-page PDF, AcroForm widget (obj 6) in BOTH page 1 AND page 2
/// /Annots.  Widget has page_reach==2 and is in open_document_set.  OD routing
/// sends it to part4_rest.  Its container's all_referenced_pages spans {1, 2},
/// which satisfies part8_container_nums' `container_pages.len()>=2` criterion.
///
/// Bug path without the fix:
///   canonical_shared_hints line 1242 appends the OD container as a Part-8 SOHT
///   entry → nshared_total > nshared_first_page (diverges from oracle).
///
/// With the fix (open_document_container_nums filter in the Part-8 append loop):
///   OD container is skipped → nshared_total == nshared_first_page == 2 ✓
#[test]
fn acroform_widget_page1_page2_od_container_excluded_from_part8_soht() {
    let bytes = linearize_generate("objstm-lin-acroform-widget-page1-page2.pdf");

    // Decode the linearization dump and verify nshared_total matches oracle.
    let dump = flpdf::linearization::show_linearization_bytes(&bytes, "widget-page1-page2")
        .expect("show-linearization decode");

    // Oracle: nshared_first_page=2, nshared_total=2 (no Part-8 OD container entry).
    assert!(
        dump.contains("nshared_first_page: 2"),
        "SOHT must have 2 first-page entries:\n{dump}"
    );
    assert!(
        dump.contains("nshared_total: 2"),
        "SOHT nshared_total must be 2 (no spurious Part-8 OD container entry):\n{dump}"
    );

    // Round-trip sanity.
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).expect("Pdf::open round-trip");
    let refs = pdf.object_refs();
    assert!(!refs.is_empty(), "round-tripped doc must expose objects");
    for r in refs {
        pdf.resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve: {e}"));
    }
}
