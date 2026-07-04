//! Byte-parity: flpdf `--linearize --object-streams=generate --deterministic-id`
//! == qpdf 11.9.0, for the cross-reference *stream* path.
//!
//! Pins flpdf's linearized ObjStm output to the committed qpdf goldens at
//! `tests/golden/references/<stem>/linearize-objstm.pdf` (see
//! `tests/golden/regenerate.sh`). Gated on `qpdf-zlib-compat` because byte
//! identity requires flpdf's deflate to match qpdf's classic-zlib output.
//!
//! Two milestones:
//! * **structural** (`mask_id1`): everything except the changing `/ID[1]` digest.
//!   This is the layout milestone — object numbering, xref-stream encoding
//!   (`/Predictor 12`, `/W [1 2 1]`), hint stream, offsets, framing.
//! * **strict**: full byte identity including `/ID[1]`. This needs qpdf's pass-1
//!   xref-stream reconstruction for the deterministic `/ID` digest; until that
//!   lands the strict tests are `#[ignore]`d.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{NewlineBeforeEndstream, ObjectStreamMode, Pdf, WriteOptions};
use std::path::Path;

/// Linearize `fixture` with `--object-streams=generate` via the public API
/// (mirroring the CLI path) and return the complete back-patched bytes.
fn flpdf_linearized_objstm(fixture: &str) -> Vec<u8> {
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
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

fn golden(stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("linearize-objstm.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Linearize `fixture` with `--object-streams=preserve` via the public API and
/// return the complete back-patched bytes. Unlike `generate`, this keeps the
/// source document's ObjStm membership (qpdf's `preserveObjectStreams`).
fn flpdf_linearized_objstm_preserve(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let f1 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f1)).unwrap();
    // Match the production Preserve path: the CLI builds the plan with
    // `use_generate_objstm = (mode == Generate)` (flpdf-cli/src/main.rs), so
    // Preserve passes `false`. `true` would enable Generate-mode open-document
    // peeling, which the Preserve writer must not do.
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let renumber = RenumberMap::from_plan(&plan);
    let f2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(f2)).unwrap();
    let mut opts = WriteOptions::default();
    opts.object_streams = ObjectStreamMode::Preserve;
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

fn golden_preserve(stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("linearize-objstm-preserve.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    (0..common).find(|&i| a[i] != b[i]).or(Some(common))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Replace the 32 hex bytes of every `/ID [<id0><id1>]` array's *second* element
/// with ASCII `'0'`, leaving `/ID[0]` and all surrounding bytes intact. A
/// linearized ObjStm file carries `/ID` at both xref-stream dicts (obj7, obj5).
fn mask_id1(buf: &[u8]) -> Vec<u8> {
    let mut out = buf.to_vec();
    let needle = b" /ID [<";
    let mut i = 0usize;
    while let Some(rel) = find(&out[i..], needle) {
        let arr = i + rel + needle.len();
        let id0_end = arr + 32;
        if id0_end + 2 + 32 <= out.len() && out[id0_end] == b'>' && out[id0_end + 1] == b'<' {
            let id1 = id0_end + 2;
            for b in &mut out[id1..id1 + 32] {
                *b = b'0';
            }
            i = id1 + 32;
        } else {
            i = arr;
        }
    }
    out
}

fn report(fixture: &str, actual: &[u8], expected: &[u8], what: &str) {
    if let Some(off) = first_diff(actual, expected) {
        let lo = off.saturating_sub(24);
        panic!(
            "{fixture}: {what} diverged from qpdf golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
            String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
        );
    }
}

fn assert_structural(fixture: &str, stem: &str) {
    let actual = mask_id1(&flpdf_linearized_objstm(fixture));
    let expected = mask_id1(&golden(stem));
    report(
        fixture,
        &actual,
        &expected,
        "structural layout (ignoring /ID[1])",
    );
}

fn assert_strict(fixture: &str, stem: &str) {
    let actual = flpdf_linearized_objstm(fixture);
    let expected = golden(stem);
    report(fixture, &actual, &expected, "full bytes");
}

// Structural (layout) byte-identity: everything except the changing /ID[1].
#[test]
fn two_page_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("two-page.pdf", "two-page");
}

#[test]
fn three_page_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("three-page.pdf", "three-page");
}

#[test]
fn shared_stream_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("shared-stream-objstm.pdf", "shared-stream-objstm");
}

// Full byte-identity, including the deterministic /ID[1] (digested from qpdf's
// pass-1 xref-stream buffer; flpdf-9ntt).
#[test]
fn two_page_objstm_byte_identical_to_qpdf() {
    assert_strict("two-page.pdf", "two-page");
}

#[test]
fn three_page_objstm_byte_identical_to_qpdf() {
    assert_strict("three-page.pdf", "three-page");
}

// flpdf-zbf9: linearizing an ObjStm-bearing input (qpdf --object-streams=generate
// three-page.pdf). qpdf drops the source's stale /Type /ObjStm and /Type /XRef
// containers (rebuilding the xref and repacking ObjStm members into fresh
// containers), so the body carries no leaked structural objects. Note qpdf
// PRESERVES each stream's dict key order from the input, so this is NOT identical
// to the plain three-page golden (e.g. obj 11 is `/Filter /Length` here vs
// `/Length /Filter` there); the oracle is qpdf's own linearization of the
// ObjStm-bearing input. Before the fix the source containers leaked into the
// body, shifting every offset (qpdf --check-linearization rejected the output).
#[test]
fn objstm_bearing_input_structurally_byte_identical_to_qpdf() {
    assert_structural("three-page-objstm.pdf", "three-page-objstm");
}

#[test]
fn objstm_bearing_input_byte_identical_to_qpdf() {
    assert_strict("three-page-objstm.pdf", "three-page-objstm");
}

// flpdf-4vpi: a malformed input whose trailer references a missing indirect
// object (`/Info 99 0 R`, no xref entry). qpdf resolves the dangling ref to
// null, drops /Info, and linearizes the remaining objects; flpdf's generate
// planner must drop the unplanned `99 0 R` from ObjStm membership (rather than
// panic at place_objstm_members_per_half) and produce the same layout. This
// pins the post-fix output to qpdf's own oracle so the panic fix did not just
// stop crashing but stayed byte-parity.
#[test]
fn missing_trailer_info_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("missing-trailer-info.pdf", "missing-trailer-info");
}

#[test]
fn missing_trailer_info_objstm_byte_identical_to_qpdf() {
    assert_strict("missing-trailer-info.pdf", "missing-trailer-info");
}

// flpdf-4vpi / PR #421 Codex review: 100 missing `/Junk` trailer refs whose
// even-split positions would otherwise scatter the two real ObjStm members
// (the `/Info` dict and the `/Pages` tree) across separate containers. qpdf
// drops the missing refs before splitting, emitting ONE `/N 2` ObjStm; flpdf
// must match. Filtering the unplanned refs only AFTER the split produced two
// `/N 1` ObjStms (≈119 extra bytes) and broke byte-parity for this input class.
#[test]
fn split_boundary_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("objstm-lin-split-boundary.pdf", "objstm-lin-split-boundary");
}

#[test]
fn split_boundary_objstm_byte_identical_to_qpdf() {
    assert_strict("objstm-lin-split-boundary.pdf", "objstm-lin-split-boundary");
}

// flpdf-5apf: null-resolving body refs in the Catalog under --object-streams=
// generate. qpdf drops the null-valued dict keys and inlines `null` for the
// object-0 array element, then compresses the surviving objects; flpdf must
// match. (The missing-xref *array* resurrection case is out of scope here — see
// flpdf-0gyq — so this fixture deliberately uses only object-0 in the array.)
#[test]
fn dangling_body_refs_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("dangling-body-one-page.pdf", "dangling-body-one-page");
}

#[test]
fn dangling_body_refs_objstm_byte_identical_to_qpdf() {
    assert_strict("dangling-body-one-page.pdf", "dangling-body-one-page");
}

// flpdf-8891: the first-page private/shared split (Step 5) is object-stream-mode
// independent, so the Catalog /Ref2 edge that makes the Font lc_first_page_shared
// must reorder the generate-mode first-page ObjStm members too.
#[test]
fn catalog_firstpage_shared_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "catalog-firstpage-shared-one-page.pdf",
        "catalog-firstpage-shared-one-page",
    );
}

#[test]
fn catalog_firstpage_shared_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "catalog-firstpage-shared-one-page.pdf",
        "catalog-firstpage-shared-one-page",
    );
}

// flpdf-0gyq: under --object-streams=generate the resurrected null body object is
// compressed as the TRAILING ObjStm member (qpdf compresses it last). free and
// missing variants must both match.
#[test]
fn resurrect_free_array_ref_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("resurrect-free-one-page.pdf", "resurrect-free-one-page");
}

#[test]
fn resurrect_free_array_ref_objstm_byte_identical_to_qpdf() {
    assert_strict("resurrect-free-one-page.pdf", "resurrect-free-one-page");
}

#[test]
fn resurrect_missing_array_ref_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "resurrect-missing-one-page.pdf",
        "resurrect-missing-one-page",
    );
}

#[test]
fn resurrect_missing_array_ref_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "resurrect-missing-one-page.pdf",
        "resurrect-missing-one-page",
    );
}

// flpdf-o9im: when the FIRST-PAGE dict directly holds /Arr [<missing-ref> <live-ref>],
// the resurrected null must land in the first-page section in generate mode too.
#[test]
fn resurrect_missing_page_arr_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "resurrect-missing-page-arr.pdf",
        "resurrect-missing-page-arr",
    );
}

// flpdf-891f: when Page 1 holds /Bad 99 0 R (dict value) and Page 2 holds
// /Arr [99 0 R] (array element), the resurrected null must land in the
// second-half section (low object number) in generate mode too.
#[test]
fn resurrect_page2_arr_page1_dictval_not_in_first_page_section_objstm() {
    assert_strict(
        "resurrect-missing-page1-dictval-page2-arr.pdf",
        "resurrect-missing-page1-dictval-page2-arr",
    );
}

// flpdf-891f: both edges on same page — null must land in first-page section
// in generate mode too.
#[test]
fn resurrect_both_edges_same_page_null_in_first_page_section_objstm() {
    assert_strict(
        "resurrect-both-edges-same-page.pdf",
        "resurrect-both-edges-same-page",
    );
}

// flpdf-891f: cross-object case — null must land in first-page section in
// generate mode too (same original-number sort fix applies).
#[test]
fn resurrect_crossobj_arr_via_live_desc_null_in_first_page_section_objstm() {
    assert_strict(
        "resurrect-crossobj-arr-via-live-desc.pdf",
        "resurrect-crossobj-arr-via-live-desc",
    );
}

// flpdf-891f: else-branch ordering in generate mode — same number-order rule
// applies when ObjStm generation is enabled.
#[test]
fn else_branch_children_ordered_by_original_object_number_objstm() {
    assert_strict(
        "else-branch-obj-number-order.pdf",
        "else-branch-obj-number-order",
    );
}

// flpdf-hsjh: revorder case in generate mode — resurrectable ref (orig 99)
// lower-numbered than the live descendant (orig 100) holding the array edge.
// Deferred admission + sorted-tail insertion must place the null correctly in
// ObjStm output too.
#[test]
fn revorder_resurrect_null_in_first_page_section_objstm() {
    assert_strict("revorder-resurrect.pdf", "revorder-resurrect");
}

// flpdf-hsjh (discriminator): Page at high original number (10), content
// stream at low original number (3). Sorted-tail (non-page) ensures Page
// stays first in the closure even when its original number exceeds descendants.
#[test]
fn page_highnum_content_lownum_page_before_content_objstm() {
    assert_strict(
        "page-highnum-content-lownum.pdf",
        "page-highnum-content-lownum",
    );
}

// flpdf-hsjh (Codex P2): resurrectable null also reachable via Catalog
// dict-value (/OpenAction 99 0 R, dropped) AND first-page array (/Arr [99 0 R]).
// closure_from_seeds must skip Object::Null so the null stays lc_first_page.
#[test]
fn od_null_also_in_first_page_arr_byte_identical_to_qpdf_objstm() {
    assert_strict("od-null-page-arr.pdf", "od-null-page-arr");
}

// flpdf-hsjh (Codex P2): Catalog ARRAY edge (/OpenAction [99 0 R]) to
// xref-absent null — null must land in OD section (open_document_set), not
// first-page.  closure_from_seeds tracks array vs dict-value edges.
#[test]
fn od_catalog_arr_null_byte_identical_to_qpdf_objstm() {
    assert_strict("od-arr-null.pdf", "od-arr-null");
}

#[test]
fn shared_stream_objstm_byte_identical_to_qpdf() {
    assert_strict("shared-stream-objstm.pdf", "shared-stream-objstm");
}

// nonid-id0 (flpdf-9hc.13.11): a non-16-byte (20-byte / 40-hex) source /ID[0] on
// the ObjStm / xref-stream linearized path. This exercises the placeholder-then-
// patch route (patch_linearized_deterministic_id) at a non-16-byte id0 width: the
// 40-hex id0 must be carried verbatim at both /ID sites (first-page + main xref
// dicts) with a 16-byte (32-hex) id1, byte-identical to
// qpdf --linearize --object-streams=generate --deterministic-id.
#[test]
fn nonid_id0_linearized_objstm_is_byte_identical_to_qpdf() {
    assert_strict("nonid-id0.pdf", "nonid-id0");
}

// ---- Phase-2 (flpdf-g6hb.2): >cap global even-split + part routing ----------
//
// sharedfonts-100: 104 eligible first-page-shared dicts → 2 containers (50+51),
// BOTH in part6 (first half). Exercises the global even-split membership fix
// without second-half container numbering (finding-4): no part4 containers, so
// the existing per-half renumber suffices.
#[test]
fn sharedfonts100_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-sharedfonts-100.pdf",
        "objstm-lin-sharedfonts-100",
    );
}

#[test]
fn sharedfonts100_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-sharedfonts-100.pdf",
        "objstm-lin-sharedfonts-100",
    );
}

// cap-boundary-199: 199 first-page-shared font dicts + the /Pages tree = 200
// eligible first-page-shared members, an exact multiple of the 100 cap. This is
// the precise boundary the reverted ihb.3 fix targeted: the earlier per-part
// greedy chunker filled one 100-member container exactly, then re-chunked the
// /Info + /Pages extras into a stranded tiny first-half container, making the
// page-0 object count and the shared-object hint table disagree (qpdf
// --check-linearization not clean). qpdf's generateObjectStreams instead splits
// the 200 members EVENLY across 3 containers (66 + 68 + 66), all part6, so no
// container is stranded and the two hint tables stay in lockstep. flpdf's global
// even-split (objstm_batches_generate) reproduces this byte-for-byte; a
// regression to greedy chunks(cap) would re-split as 100 + 100 (2 containers)
// and is caught here (structural + strict).
#[test]
fn cap_boundary_199_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-cap-boundary-199.pdf",
        "objstm-lin-cap-boundary-199",
    );
}

#[test]
fn cap_boundary_199_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-cap-boundary-199.pdf",
        "objstm-lin-cap-boundary-199",
    );
}

// mixed-60-70: a part7 (other-page-private) ObjStm container. Exercises the
// second-half container numbering (finding-4), the page-private-font
// compression, and the per-page object-count / page-length container folds.
// Fully byte-identical to qpdf (structural + strict).
#[test]
fn mixed_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("objstm-lin-mixed-60-70.pdf", "objstm-lin-mixed-60-70");
}

#[test]
fn mixed_objstm_byte_identical_to_qpdf() {
    assert_strict("objstm-lin-mixed-60-70.pdf", "objstm-lin-mixed-60-70");
}

// threepage-2-120: a part8 (other-page-shared) ObjStm container holding fonts
// shared by pages 1 & 2 (not page 0). Exercises the shared-object hint table's
// first-page vs Part-8 split when a part4-shared object even-splits into the
// first-page container. Fully byte-identical (structural + strict).
#[test]
fn threepage_shared_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-threepage-2-120.pdf",
        "objstm-lin-threepage-2-120",
    );
}

#[test]
fn threepage_shared_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-threepage-2-120.pdf",
        "objstm-lin-threepage-2-120",
    );
}

// disc-2-250-2: a pure part7 ObjStm container coexisting with a part8 plain Form
// XObject AND a part8 container the even split filled with two pages' private
// fonts. Exercises the deepest second-half machinery: part-ordered container
// emission (the part7 container interleaved in its page's group), the
// shared-object hint table including a part8 container of page-private members,
// and per-page shared identifiers ordered by pre-renumber object number. Fully
// byte-identical to qpdf (structural + strict).
#[test]
fn disc_part7_part8_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("objstm-lin-disc-2-250-2.pdf", "objstm-lin-disc-2-250-2");
}

#[test]
fn disc_part7_part8_objstm_byte_identical_to_qpdf() {
    assert_strict("objstm-lin-disc-2-250-2.pdf", "objstm-lin-disc-2-250-2");
}

// openaction-80-80 (flpdf-1dmy, Stage A — in_open_document): the catalog's
// /OpenAction subtree (an action dict + 80 "od-only" font dicts reachable ONLY
// from /OpenAction) is qpdf's in_open_document category → lc_open_document →
// part4 (FIRST half, right after the Catalog, before the first page). The 80
// objects even-split into a container whose obj_user union is /OpenAction +
// /Pages, so qpdf routes the whole container to part4. flpdf's page-closure-only
// model used to drop it into part9 (second half), so /O (first_page_object) and
// the H/E offset cascade diverged. Exercises the OpenDocument container routing
// and the first-half part4-before-part6 numbering / emission.
#[test]
fn openaction_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-openaction-80-80.pdf",
        "objstm-lin-openaction-80-80",
    );
}

#[test]
fn openaction_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-openaction-80-80.pdf",
        "objstm-lin-openaction-80-80",
    );
}

// openaction-multi-od (flpdf-699x): TWO open-document ObjStm containers.
// The fixture arranges high-numbered OD objects (100..149) to be visited FIRST
// in DFS (/HighRef < /LowRef lexically), so even-split C0 has min-member 5
// (action) while C1 has min-member 2 (pages).  Discriminates between "DFS
// order" (correct) and "sort by ascending min-member" hypotheses: qpdf emits
// C0 before C1 (even-split/ObjGen order), so the physical byte order in the
// golden matches DFS order, not ascending-min order.
#[test]
fn openaction_multi_od_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-openaction-multi-od.pdf",
        "objstm-lin-openaction-multi-od",
    );
}

#[test]
fn openaction_multi_od_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-openaction-multi-od.pdf",
        "objstm-lin-openaction-multi-od",
    );
}

// acroform-widget-page0-5-10 (flpdf-sjgv): AcroForm widgets in both
// /AcroForm /Fields (in_open_document) and page 0 /Annots (in_first_page).
// qpdf's in_open_document > in_first_page precedence means widgets go to the
// open-document section (part4, first half, before /O). Without the fix,
// from_pdf Step 5 places them in part2, inflating page_hints[0].object_count
// and diverging hint tables. Exercises the from_pdf open_document_set peeling.
#[test]
fn acroform_widget_page0_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-acroform-widget-page0-5-10.pdf",
        "objstm-lin-acroform-widget-page0-5-10",
    );
}

#[test]
fn acroform_widget_page0_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-acroform-widget-page0-5-10.pdf",
        "objstm-lin-acroform-widget-page0-5-10",
    );
}

// outlines-80-80 (flpdf-rm09, Stage B — in_outlines, part9): the catalog's
// /Outlines subtree (an outline dict + 80 items reachable ONLY from /Outlines)
// is qpdf's in_outlines category. With no /PageMode /UseOutlines, qpdf places it
// in part9 (second half) via pushOutlinesToPart and emits the Outlines Hint Table
// (HGeneric) + the hint dict /O key. The body placement already coincides with
// flpdf's page-closure Rest path; this exercises the new outline hint table + /O.
// Two pages share fonts so a first-page (part6) container coexists.
#[test]
fn outlines_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("objstm-lin-outlines-80-80.pdf", "objstm-lin-outlines-80-80");
}

#[test]
fn outlines_objstm_byte_identical_to_qpdf() {
    assert_strict("objstm-lin-outlines-80-80.pdf", "objstm-lin-outlines-80-80");
}

// useoutlines-80-80 (flpdf-vvjr.1): /PageMode /UseOutlines causes outline
// objects (dict + 80 items) to route to part6 (first-page section) instead of
// part9. Their ObjStm container folds into page-0 nobjects (qpdf: 4, was 3).
// Two pages share fonts so a first-page (part6) container coexists.
// Regression: objstm-lin-outlines-80-80 (no /PageMode) must stay byte-identical.
#[test]
fn useoutlines_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}

#[test]
fn useoutlines_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}

// outlines-80-200 (flpdf-vvjr.3): outline tree with S=80 shared fonts and K=200
// items spans 3 ObjStm containers (281 eligible objects, even split:
// ceil(281/100)=3, containers of ~94 each). All three containers route to
// ContainerPart::Rest (outline priority applies). Verifies group_length
// consecutiveness in the multi-container case: nobjects=3, group_length covers
// all three consecutive containers.
//
// Previously #[ignore]d (flpdf-fmlf): second-half ObjStm containers were
// incorrectly included in the first-page section of the Shared Object Hint
// Table (nshared=3 vs qpdf's 2). Fixed by canonical_shared_hints skipping
// containers in second_half_container_nums when input_idx < first_page_input.
#[test]
fn outlines_multi_container_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-80-200.pdf",
        "objstm-lin-outlines-80-200",
    );
}

#[test]
fn outlines_multi_container_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-80-200.pdf",
        "objstm-lin-outlines-80-200",
    );
}

/// Parse the 16-byte Outlines Hint Table from a linearized PDF's hint stream.
///
/// Uses `/H [<hint_offset> ...]` from the linearization parameter dict to locate
/// the hint stream object, reads its `/O` key (byte offset into the decompressed
/// stream where the Outlines table starts), decompresses with ZlibDecoder, and
/// returns the 16 raw bytes (4 × u32 MSB-first: first_object, first_obj_offset,
/// nobjects, group_length).
fn parse_outline_hint_table(pdf: &[u8]) -> [u8; 16] {
    use std::io::Read;

    // Locate hint stream object via /H [<offset> ...] in the linearization dict
    let h_pos = pdf
        .windows(4)
        .position(|w| w == b"/H [")
        .expect("no /H key in linearization dict");
    let after_h_raw = &pdf[h_pos + 4..];
    // skip optional whitespace between '[' and the first digit
    let ws = after_h_raw
        .iter()
        .take_while(|&&b| b.is_ascii_whitespace())
        .count();
    let after_h = &after_h_raw[ws..];
    let n_len = after_h.iter().take_while(|&&b| b.is_ascii_digit()).count();
    let hint_offset: usize = std::str::from_utf8(&after_h[..n_len])
        .unwrap()
        .parse()
        .unwrap();

    let hint_area = &pdf[hint_offset..];

    // Extract /O value from the hint stream dict
    let o_rel = hint_area
        .windows(3)
        .position(|w| w == b"/O ")
        .expect("no /O key in hint stream dict");
    let after_o = &hint_area[o_rel + 3..];
    let n_len = after_o.iter().take_while(|&&b| b.is_ascii_digit()).count();
    let o_off: usize = std::str::from_utf8(&after_o[..n_len])
        .unwrap()
        .parse()
        .unwrap();

    // Locate "stream\n" inside the hint object
    let stream_rel = hint_area
        .windows(7)
        .position(|w| w == b"stream\n")
        .expect("no stream marker in hint object");
    let data_start = stream_rel + 7;

    // Locate "\nendstream"
    let end_rel = hint_area[data_start..]
        .windows(10)
        .position(|w| w == b"\nendstream")
        .expect("no endstream in hint object");
    let compressed = &hint_area[data_start..data_start + end_rel];

    // PDF FlateDecode = zlib (RFC 1950)
    let mut decoder = flate2::read::ZlibDecoder::new(compressed);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .expect("failed to decompress hint stream");

    assert!(
        decompressed.len() >= o_off + 16,
        "decompressed hint stream ({} bytes) too short for /O offset {}",
        decompressed.len(),
        o_off
    );

    let mut table = [0u8; 16];
    table.copy_from_slice(&decompressed[o_off..o_off + 16]);
    table
}

// Outlines Hint Table targeted assertion: verifies the 16-byte table at /O offset
// matches qpdf's golden exactly (first_object, first_obj_offset, nobjects=3,
// group_length). Complements the full-byte strict test above with a focused check
// on the Outlines Hint Table; the Shared Object table is now byte-identical too
// (the second-half ObjStm container miscount was fixed — see the note above).
#[test]
fn outlines_multi_container_hint_table_matches_qpdf() {
    let flpdf_bytes = flpdf_linearized_objstm("objstm-lin-outlines-80-200.pdf");
    let golden_bytes = golden("objstm-lin-outlines-80-200");

    let flpdf_table = parse_outline_hint_table(&flpdf_bytes);
    let golden_table = parse_outline_hint_table(&golden_bytes);

    assert_eq!(
        flpdf_table,
        golden_table,
        "Outlines Hint Table mismatch: flpdf vs qpdf golden\n\
         flpdf  first_object={} first_obj_off={} nobjects={} group_length={}\n\
         golden first_object={} first_obj_off={} nobjects={} group_length={}",
        u32::from_be_bytes(flpdf_table[0..4].try_into().unwrap()),
        u32::from_be_bytes(flpdf_table[4..8].try_into().unwrap()),
        u32::from_be_bytes(flpdf_table[8..12].try_into().unwrap()),
        u32::from_be_bytes(flpdf_table[12..16].try_into().unwrap()),
        u32::from_be_bytes(golden_table[0..4].try_into().unwrap()),
        u32::from_be_bytes(golden_table[4..8].try_into().unwrap()),
        u32::from_be_bytes(golden_table[8..12].try_into().unwrap()),
        u32::from_be_bytes(golden_table[12..16].try_into().unwrap()),
    );

    // Verify multi-container: nobjects must be >= 2
    let nobjects = u32::from_be_bytes(flpdf_table[8..12].try_into().unwrap());
    assert!(nobjects >= 2, "nobjects={nobjects}: not multi-container");
}

// outlines-shared-page-80-80 (flpdf-vvjr.4 scenario A): outline∩page object
// overlap — outline items reference page objects, which are already assigned to
// part-4/6/7. Verifies that the shared-page objects are not double-counted and
// are placed in the correct linearization part.
#[test]
fn outlines_shared_page_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-shared-page-80-80.pdf",
        "objstm-lin-outlines-shared-page-80-80",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_shared_page_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-shared-page-80-80.pdf",
        "objstm-lin-outlines-shared-page-80-80",
    );
}

// outlines-coloc-200-20 (flpdf-vvjr.4 scenario B): ObjStm co-location — outline
// items and page content share the same ObjStm containers (K=20 items spread
// over fewer containers alongside page objects). Verifies correct part assignment
// when outline objects co-locate with page objects in the same ObjStm.
#[test]
fn outlines_coloc_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-coloc-200-20.pdf",
        "objstm-lin-outlines-coloc-200-20",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_coloc_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-coloc-200-20.pdf",
        "objstm-lin-outlines-coloc-200-20",
    );
}

// outlines-otherpage-2-60-20 (flpdf-7aek): a single even-split ObjStm container
// mixes /Outlines items (in_outlines => part9 / Rest, no /PageMode /UseOutlines)
// with other-page-shared fonts (referenced by pages 1 AND 2, NOT page 0 => part8 /
// lc_other_page_shared). route_objstm_containers gives outline priority, so the
// mixed container is routed to part9; its part8 fonts must NOT be emitted as part8
// Shared Object Hint Table entries (canonical_shared_hints must guard part9
// containers in the input_idx >= first_page_input section too). Sized so the
// compressible set stays under the 100-object cap => one container, isolating this
// SOHT bug from the even-split page-dict-erasure boundary divergence (flpdf-g1eu).
#[test]
fn outlines_otherpage_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-otherpage-2-60-20.pdf",
        "objstm-lin-outlines-otherpage-2-60-20",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_otherpage_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-otherpage-2-60-20.pdf",
        "objstm-lin-outlines-otherpage-2-60-20",
    );
}

// outlines-otherpage-0-60-20 (flpdf-7aek, Codex P2): same as above but page 0 has
// NO ObjStm-eligible private member (P0=0), so the part9 container carries no
// first-page member. `part8_container_nums` (keyed on page reachability) would
// re-add it as a Part-8 entry in canonical_shared_hints' enumeration tail AND
// over-count it in SharedObjectHintTable::from_plan's `first_page_entries`. Both
// must exclude rest containers; this pins the empty-first-page case byte-identical.
#[test]
fn outlines_otherpage_empty_first_page_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-otherpage-0-60-20.pdf",
        "objstm-lin-outlines-otherpage-0-60-20",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_otherpage_empty_first_page_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-otherpage-0-60-20.pdf",
        "objstm-lin-outlines-otherpage-0-60-20",
    );
}

// outlines-otherpage-2-120-20 (flpdf-g1eu): the 2-container variant of the above.
// With G=120 the compressible set is 149 > the 100-per-stream cap, so qpdf's
// even split makes TWO containers: a part9 mixed container (DFS-early /Outlines
// items + page-0 fonts + 47 shared fonts) and a part8 pure other-page-shared
// container (the remaining 73 shared fonts). The even split yields the part9
// container FIRST, but qpdf emits the second half in strict part order (part8
// before part9), so the shared container is emitted first. The two containers'
// MEMBERSHIP is identical between flpdf and qpdf; only the layout order differs.
// Pins that objstm_batches_generate orders second-half containers by part rank,
// not even-split order (was: part9 emitted before part8 => byte divergence).
#[test]
fn outlines_otherpage_two_container_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-otherpage-2-120-20.pdf",
        "objstm-lin-outlines-otherpage-2-120-20",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_otherpage_two_container_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-otherpage-2-120-20.pdf",
        "objstm-lin-outlines-otherpage-2-120-20",
    );
}

// outlines-otherpage-0-120-20 (flpdf-g1eu, flpdf-7aek forward flag): the
// 2-container variant with P0=0, so the part9 mixed container holds NO first-page
// member (page 0 has no ObjStm-eligible font). This is the scenario flpdf-7aek's
// forward flag warned could reach canonical_shared_hints' enumeration tail. With
// the part-rank emission order (part8 shared container before the part9 outline
// container) AND flpdf-7aek's existing part9 guards, the output is byte-identical
// and qpdf --check is clean — so no extra part9 enumeration-tail guard is needed.
#[test]
fn outlines_otherpage_two_container_empty_first_page_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outlines-otherpage-0-120-20.pdf",
        "objstm-lin-outlines-otherpage-0-120-20",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn outlines_otherpage_two_container_empty_first_page_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outlines-otherpage-0-120-20.pdf",
        "objstm-lin-outlines-otherpage-0-120-20",
    );
}

// otherpage-others-48-50 (flpdf-pn7h): a two-container part7/part9 layout. Page 0
// is fontless (no first-page ObjStm member), page 1 has 48 private fonts, page 2
// has 50 private fonts. The even split yields C1 = {Pages tree node + the 48
// page-1 fonts} and C2 = {the 50 page-2 fonts}. C1's union has other_pages=={1}
// AND others>0 (the /Pages tree node is reached via ou_root_key "/Pages", which is
// not an open-document key nor /Outlines), so qpdf categorizes it lc_other (part9);
// C2 has other_pages=={2}, others==0, so it is lc_other_page_private (part7). qpdf
// emits/numbers the second half in strict part order — C2 (part7) before C1
// (part9). Before flpdf-pn7h, `route_objstm_containers` routed C1 to part7 by
// other_pages.len()==1 alone (ignoring `others`), AND `second_half_container_anchors`
// classified C1 as part7 because it holds a page-private member — both diverging
// from qpdf. This pins the part7/part9 container ordering AND numbering. Distinct
// from the outlines-otherpage fixtures above, which cover part8/part9 (shared) —
// this is the part7 (other-page-private) `others` gate.
#[test]
fn otherpage_others_two_container_objstm_structurally_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-otherpage-others-48-50.pdf",
        "objstm-lin-otherpage-others-48-50",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn otherpage_others_two_container_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-otherpage-others-48-50.pdf",
        "objstm-lin-otherpage-others-48-50",
    );
}

// outline-od-shared-stream: a /JS action stream reachable from BOTH the catalog's
// /OpenAction subtree (in_open_document) AND an outline item's /A (in_outlines).
// qpdf's canonical classification orders in_outlines ABOVE in_open_document
// (QPDF_linearization.cc lc_outlines before lc_open_document), so the shared
// object is an outline.  Being an Object::Stream it is ineligible for ObjStm, so
// qpdf emits it plain in part9 (second half) AFTER the outline container — NOT in
// the pre-/O open-document region.  Exercises from_pdf step-6b OD/outline
// precedence and the writer's second-half post-container plain ordering.
#[test]
fn outline_od_shared_stream_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-outline-od-shared-stream.pdf",
        "objstm-lin-outline-od-shared-stream",
    );
}

#[test]
fn outline_od_shared_stream_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-outline-od-shared-stream.pdf",
        "objstm-lin-outline-od-shared-stream",
    );
}

// useoutline-od-shared-stream (flpdf-q9o3): the UseOutlines sibling of
// outline-od-shared-stream. With /PageMode /UseOutlines the outline objects (and
// the ineligible OD+outline JS stream) route to the FIRST-page section (qpdf
// part6 / lc_outlines), BEFORE /E instead of part9. The ineligible outline stream
// is still emitted plain, and qpdf numbers it AFTER its part6 ObjStm container —
// so it is a first-half post-container plain object, the mirror of the second-half
// post-plain set. Exercises place_objstm_members_per_half's first-half
// post-container plain ordering.
#[test]
fn useoutline_od_shared_stream_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-useoutline-od-shared-stream.pdf",
        "objstm-lin-useoutline-od-shared-stream",
    );
}

#[test]
fn useoutline_od_shared_stream_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-useoutline-od-shared-stream.pdf",
        "objstm-lin-useoutline-od-shared-stream",
    );
}

// acroform-widget-ap-stream-page0 (PR #393 Fix 1 + Fix 3): AcroForm widget with
// an /AP /N Form XObject appearance stream (Object::Stream → ineligible for
// ObjStm packing). The Form XObject is in open_document_set (via
// Catalog → /AcroForm → widget → /AP) but cannot be an ObjStm member.
// qpdf emits it as a plain indirect object between the Catalog and the OD ObjStm
// containers (pre-/O region). flpdf routes it to `part4_open_document_plain` and
// emits it similarly. Exercises the eligibility check in from_pdf Step 6b and the
// pre-/O plain emission loop in writer.rs.
#[test]
fn acroform_widget_ap_stream_page0_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-acroform-widget-ap-stream-page0.pdf",
        "objstm-lin-acroform-widget-ap-stream-page0",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn acroform_widget_ap_stream_page0_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-acroform-widget-ap-stream-page0.pdf",
        "objstm-lin-acroform-widget-ap-stream-page0",
    );
}

// acroform-widget-page1-only (PR #393 Fix 4 — r3443001374): AcroForm widget
// exclusive to page 1 (not on page 0). Widget has page_reach==1 and is in
// open_document_set. Without the fix, the per_page_private_objects filter
// includes the widget (inflating page_hints[1].object_count) and the part7
// pre-pass places it in part4_other_pages_private, bypassing OD routing.
// With the fix, the widget flows to part4_rest (OD section) and
// page_hints[1].object_count==2 (page dict + contents only).
#[test]
fn acroform_widget_page1_only_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-acroform-widget-page1-only.pdf",
        "objstm-lin-acroform-widget-page1-only",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn acroform_widget_page1_only_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-acroform-widget-page1-only.pdf",
        "objstm-lin-acroform-widget-page1-only",
    );
}

// acroform-widget-page1-page2 (PR #393 Fix 5 — r3443001371): AcroForm widget
// shared by pages 1 AND 2 (page_reach==2, in open_document_set). OD routing
// sends the widget to part4_rest. Its OD ObjStm container spans pages {1,2}
// in all_referenced_pages, satisfying part8_container_nums' container_pages
// criterion. Without the fix, canonical_shared_hints appends the OD container
// as a spurious Part-8 SOHT entry (nshared_total > oracle). With the fix the
// open_document_container_nums filter skips it and nshared_total==2.
#[test]
fn acroform_widget_page1_page2_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-acroform-widget-page1-page2.pdf",
        "objstm-lin-acroform-widget-page1-page2",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn acroform_widget_page1_page2_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-acroform-widget-page1-page2.pdf",
        "objstm-lin-acroform-widget-page1-page2",
    );
}

// thumbnail-private-shared: a 4-page fixture where other pages carry /Thumb
// entries: page 1 has a private thumbnail (ou_thumb, lc_thumbnail_private → part9)
// and pages 2 & 3 share a thumbnail object (lc_thumbnail_shared → part9). Pins
// the compute_closure /Thumb skip that routes thumbnail objects to part4_rest
// rather than the per-page private/shared sections.
#[test]
fn thumbnail_private_shared_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-thumbnail-private-shared.pdf",
        "objstm-lin-thumbnail-private-shared",
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn thumbnail_private_shared_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-thumbnail-private-shared.pdf",
        "objstm-lin-thumbnail-private-shared",
    );
}

// cap-boundary-199-bearing (flpdf-ihb.4): PRESERVE mode (qpdf
// --object-streams=preserve) on an ObjStm-bearing input. qpdf's
// preserveObjectStreams keeps the SOURCE document's ObjStm grouping rather than
// repacking: 3 source containers 68/67/68, minus the erased /Catalog and /Page
// dicts (promoted to plain indirects), yields 66/66/68 (sum 200), and
// --check-linearization is clean. flpdf's preserve path currently re-chunks the
// first-half greedily into 100+100 (the bug), diverging from the golden. These
// tests pin the source-grouping behaviour (structural + strict).
#[test]
fn cap_boundary_199_bearing_preserve_structurally_byte_identical_to_qpdf() {
    let fixture = "objstm-lin-cap-boundary-199-bearing.pdf";
    let stem = "objstm-lin-cap-boundary-199-bearing";
    let actual = flpdf_linearized_objstm_preserve(fixture);
    let expected = golden_preserve(stem);
    report(
        fixture,
        &mask_id1(&actual),
        &mask_id1(&expected),
        "preserve structural",
    );
}

#[test]
fn cap_boundary_199_bearing_preserve_byte_identical_to_qpdf() {
    let fixture = "objstm-lin-cap-boundary-199-bearing.pdf";
    let stem = "objstm-lin-cap-boundary-199-bearing";
    let actual = flpdf_linearized_objstm_preserve(fixture);
    let expected = golden_preserve(stem);
    report(fixture, &actual, &expected, "preserve strict");
}

// od-indirect-length (flpdf-2vfg): an open-document stream (catalog
// /OpenAction → JavaScript action → /JS stream) whose /Length is an indirect
// reference, with the holder reachable ONLY via that /Length edge. Every writer
// normalizes a stream's /Length to a direct integer, so the holder becomes
// orphaned; qpdf drops it via reachability GC. flpdf historically emitted it as
// a plain second-half object (the bug). These pin flpdf's linearized output
// byte-identical to qpdf, i.e. the orphaned /Length holder is dropped — for both
// an uncompressed OD stream and a lone-/FlateDecode OD stream (the writer's
// verbatim-preserve path), guarding against over-dropping a still-referenced
// holder.
#[test]
fn od_indirect_length_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-od-indirect-length.pdf",
        "objstm-lin-od-indirect-length",
    );
}

#[test]
fn od_indirect_length_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-od-indirect-length.pdf",
        "objstm-lin-od-indirect-length",
    );
}

#[test]
fn od_indirect_length_flate_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-od-indirect-length-flate.pdf",
        "objstm-lin-od-indirect-length-flate",
    );
}

#[test]
fn od_indirect_length_flate_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-od-indirect-length-flate.pdf",
        "objstm-lin-od-indirect-length-flate",
    );
}

// page-contents-indirect-length (flpdf-2vfg, Codex review on PR #400): the PAGE
// /Contents stream (not an open-document stream) carries an indirect /Length
// whose holder is reachable only via that /Length edge. The holder enters the
// first-page closure because compute_closure follows the stream dict's /Length,
// so the Part-4 `all_refs` filter alone is not enough — the planner must drop it
// from the page closures too. These pin flpdf byte-identical to qpdf, i.e. the
// orphaned holder is dropped and not leaked into Part 2/3, for both an
// uncompressed and a lone-/FlateDecode content stream.
#[test]
fn page_contents_indirect_length_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-page-contents-indirect-length.pdf",
        "objstm-lin-page-contents-indirect-length",
    );
}

#[test]
fn page_contents_indirect_length_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-page-contents-indirect-length.pdf",
        "objstm-lin-page-contents-indirect-length",
    );
}

#[test]
fn page_contents_indirect_length_flate_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-page-contents-indirect-length-flate.pdf",
        "objstm-lin-page-contents-indirect-length-flate",
    );
}

#[test]
fn page_contents_indirect_length_flate_objstm_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-page-contents-indirect-length-flate.pdf",
        "objstm-lin-page-contents-indirect-length-flate",
    );
}

// kept-indirect-length (flpdf-hwx0): a page /Resources image XObject (/DCTDecode
// passthrough) carries an indirect /Length whose holder is ALSO referenced by the
// catalog (/KeepHolder), so it stays live after /Length is directized. Two qpdf
// parities are pinned here: (1) the kept holder must NOT be page-reachable — qpdf
// directizes /Length before computing obj_user, so the holder + its ObjStm
// container (with the /Pages tree) land in the second half (part9), not the
// first-page section; and (2) the first-page section streams (content + image)
// must be numbered in ascending source-object order, not /Resources-DFS order.
// Both diverged before flpdf-hwx0 (object-numbering at byte 16; ordering later).
#[test]
fn kept_indirect_length_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("kept-indirect-length.pdf", "kept-indirect-length");
}

#[test]
fn kept_indirect_length_objstm_byte_identical_to_qpdf() {
    assert_strict("kept-indirect-length.pdf", "kept-indirect-length");
}

// ── flpdf-ipc6: forced sub-1.5 header suppresses object/xref-stream generation ──
// on the linearize path too. The output is a CLASSIC linearized PDF at header
// 1.4 (no `/ObjStm`, no `/Type /XRef`), identical to the disable path. Unlike the
// xref-stream objstm goldens (whose strict /ID[1] is `#[ignore]`d pending pass-1
// reconstruction), the suppressed output is classic, so the deterministic `/ID`
// is fully reproducible and the comparison is strict (full bytes).

/// Linearize `fixture` with an explicit `mode` + forced version (qpdf-matching
/// options). `use_generate` is derived from `mode`, mirroring the CLI.
fn flpdf_linearized_objstm_mode_force(
    fixture: &str,
    mode: ObjectStreamMode,
    force: &str,
) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let f1 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(f1)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, mode == ObjectStreamMode::Generate).unwrap();
    let renumber = RenumberMap::from_plan(&plan);
    let f2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(f2)).unwrap();
    let mut opts = WriteOptions::default();
    opts.object_streams = mode;
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    opts.force_version = Some(force.to_string());
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    doc.bytes
}

/// Linearize `fixture` with generate + a forced version (qpdf-matching options).
fn flpdf_linearized_objstm_force(fixture: &str, force: &str) -> Vec<u8> {
    flpdf_linearized_objstm_mode_force(fixture, ObjectStreamMode::Generate, force)
}

/// Read a named golden under `references/<stem>/`.
fn golden_named(stem: &str, name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

#[test]
fn three_page_linearize_generate_force_version_1_4_suppressed_is_byte_identical_to_qpdf() {
    let actual = flpdf_linearized_objstm_force("three-page.pdf", "1.4");
    let expected = golden_named("three-page", "linearize-objstm-force14.pdf");
    report(
        "three-page.pdf (linearize generate --force-version=1.4)",
        &actual,
        &expected,
        "full bytes (classic linearized, suppressed)",
    );
}

// flpdf-w35w: a forced sub-1.5 header downgrades an inherited xref-stream/ObjStm
// SOURCE to a classic linearized output. The linearize renumbering is distinct
// from the non-linearized rewrite, so anchor it to qpdf separately: flpdf
// preserve+force1.4 on an ObjStm source == qpdf's classic linearized output.
#[test]
fn linearize_preserve_force_version_1_4_downgrades_objstm_source_byte_identical_to_qpdf() {
    let actual = flpdf_linearized_objstm_mode_force(
        "three-page-objstm.pdf",
        ObjectStreamMode::Preserve,
        "1.4",
    );
    let expected = golden_named("three-page-objstm", "linearize-downgrade-force14.pdf");
    report(
        "three-page-objstm.pdf (linearize preserve --force-version=1.4)",
        &actual,
        &expected,
        "full bytes (classic linearized, inherited ObjStm downgraded)",
    );
}
