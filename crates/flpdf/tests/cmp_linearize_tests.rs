//! Byte-identity demonstration: flpdf linearized output ==
//! `qpdf --linearize --deterministic-id`.
//!
//! This pins flpdf's linearized + deterministic-`/ID` writer to the committed
//! qpdf golden references at `tests/golden/references/<stem>/linearize.pdf`
//! (produced by qpdf 11.9.0; see `tests/golden/regenerate.sh`). It is gated on
//! the `qpdf-zlib-compat` feature because byte-identity requires flpdf's deflate
//! output to match qpdf's classic-libz output (the Pure-Rust miniz_oxide default
//! produces equivalent but not byte-identical compression).
//!
//! The public-API sequence mirrors the CLI's `--linearize` path: build the
//! [`LinearizationPlan`] and [`RenumberMap`] from one handle, then re-open the
//! file so [`write_linearized`] can seek/read objects independently, write with
//! `deterministic_id` set, and `back_patch` the param-dict placeholders, `/Prev`,
//! and `/ID`.
//!
//! CAVEAT: byte-identity pins to the linked libz version (captured with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 / qpdf 11.9.0); a different libz may shift the deflate
//! bytes and require re-blessing the goldens.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

/// Linearize `fixture` via the public API (mirroring the CLI `--linearize`
/// path) and return the complete back-patched bytes.
fn flpdf_linearized(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);

    // Build the plan + renumber map from one handle.
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let renumber = RenumberMap::from_plan(&plan);

    // Re-open so `write_linearized` can seek/read objects independently.
    let file2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(file2)).unwrap();

    let mut opts = WriteOptions::default();
    // Linearization is implied by calling `write_linearized`; this only opts in
    // to the qpdf-matching deterministic trailer `/ID`.
    opts.deterministic_id = true;
    // qpdf's default output writes no newline before endstream; the linearized
    // body content streams honour this option (see the plain-path sibling in
    // `cmp_diff_zero_tests`).
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    // Back-patches the param-dict placeholders (/L, /H, /O, /E, /T, /N), /Prev,
    // and /ID with their final values.
    doc.back_patch().unwrap();
    doc.bytes
}

fn golden(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("linearize.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn golden_classic(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("linearize-classic.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Report the first differing byte offset for a readable failure message.
fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_linearize_byte_identical(fixture: &str, stem: &str) {
    let actual = flpdf_linearized(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --linearize --deterministic-id golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

fn assert_classic_byte_identical(fixture: &str, stem: &str) {
    let actual = flpdf_linearized(fixture);
    let expected = golden_classic(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --linearize --deterministic-id golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

fn assert_classic_structurally_byte_identical(fixture: &str, stem: &str) {
    let actual = mask_id1(&flpdf_linearized(fixture));
    let expected = mask_id1(&golden_classic(stem));
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: structural layout diverged from qpdf classic golden (ignoring /ID[1]) \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn one_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("one-page.pdf", "one-page");
}

#[test]
fn two_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("two-page.pdf", "two-page");
}

#[test]
fn three_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("three-page.pdf", "three-page");
}

// flpdf-05jt: a degenerate first page — the 3-object catalog/pages/page shape
// with NO /Contents and NO /Resources (no content stream, no inheritable
// resources to push down). The one/two/three-page corpus above all carry a page
// content stream, so this pins the no-stream shape that was previously
// uncovered. The primary hint stream's *uncompressed* bytes are already
// byte-identical to qpdf's for this shape (61 bytes, same content) — the encoder
// does not over-emit; the only delta on a default (miniz_oxide) build is the
// FlateDecode-*compressed* hint-stream size (qpdf 32 vs miniz 38 bytes, which
// shifts /L /E /T by 6), the sole sanctioned DEFLATE-backend deviation. Under
// qpdf-zlib-compat the compressed hint stream — and thus the whole file — is
// byte-identical to qpdf --linearize --deterministic-id, which this pins.
#[test]
fn no_stream_one_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("no-stream-one-page.pdf", "no-stream-one-page");
}

#[test]
fn nonid_id0_linearized_is_byte_identical_to_qpdf() {
    // Non-16-byte (20-byte) source /ID[0] preserved verbatim on the linearized
    // path, byte-identical to qpdf --linearize --deterministic-id (flpdf-9hc.13.11).
    assert_linearize_byte_identical("nonid-id0.pdf", "nonid-id0");
}

// flpdf-8wo1: the /Pages node holds a DIRECT /Resources dict (not a
// reference) and the /Page leaf has no local /Resources, so linearization
// must push the inherited /Resources down to the leaf (minting a fresh
// indirect object for the copy) and strip it from the now-interior /Pages
// node. This exercises the fix on `write_linearized`'s own `Pdf` handle (a
// separate handle from the one `LinearizationPlan::from_pdf` planned with,
// exactly as the CLI and this file's `flpdf_linearized` helper both do).
// Confirmed (by temporarily reverting the fix) that without it, this fixture
// diverges from qpdf: the interior /Pages node keeps its /Resources dict
// unstripped, the /Page leaf never gains a /Resources key at all (the mutation
// simply never happened on this handle), and the minted object that should
// hold the pushed-down copy resolves to `Object::Null` on this (unpushed)
// handle and ends up dropped from the output entirely — one fewer object than
// qpdf's golden, with every later object number, offset, and hint value
// (`/E`, `/L`) shifted as a result.
#[test]
fn inherited_resources_one_page_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "inherited-resources-one-page.pdf",
        "inherited-resources-one-page",
    );
}

// A `/Page` leaf shared by two `/Pages` parents that carry different inherited
// `/Rotate` values (A: 90, B: 180). qpdf's `pushInheritedAttributesToPage`
// begins with `(void)cache()`, whose `getAllPagesInternal` clones the 2nd
// occurrence of the shared leaf into a distinct page object (QPDF_pages.cc:202).
// The original leaf then inherits parent A's `/Rotate 90` and the clone inherits
// parent B's `/Rotate 180`; the clone keeps the original leaf's `/Parent` (the
// clone arm never flattens) and both share the `/Contents` stream. The param
// dict must report `/N 2` and the root `/Count` stays 2 (flpdf-52md).
#[test]
fn shared_page_two_parents_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("shared-page-two-parents.pdf", "shared-page-two-parents");
}

// As above, but parent A also carries a DIRECT (non-scalar) `/Resources` that the
// inherited-attribute push mints into a fresh indirect object. This pins the
// relative object numbers of the clone (minted first, in the cache()-equivalent
// pass) and the push-minted `/Resources` (minted after), which must match qpdf's
// cache()-then-push order (flpdf-52md).
#[test]
fn shared_page_two_parents_pushmint_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "shared-page-two-parents-pushmint.pdf",
        "shared-page-two-parents-pushmint",
    );
}

/// An interior /Pages node whose /Type is not /Pages and a leaf whose /Type is not
/// /Page. qpdf 11.9.0's getAllPagesInternal overrides both /Type keys
/// (QPDF_pages.cc:89-92, 131-134); the corrected interior node then has its inherited
/// /Rotate pushed down to the leaf (flpdf-nd38 repair 2).
#[test]
fn mistyped_page_tree_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("mistyped-page-tree.pdf", "mistyped-page-tree");
}

/// A /Page leaf with no /MediaBox and no ancestor /MediaBox. qpdf 11.9.0's
/// getAllPagesInternal defaults it to letter/ANSI A [0 0 612 792]
/// (QPDF_pages.cc:104-112) (flpdf-nd38 repair 3).
#[test]
fn missing_mediabox_leaf_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("missing-mediabox-leaf.pdf", "missing-mediabox-leaf");
}

/// A /Page leaf shared by two /Pages parents where ONLY parent A carries a
/// /MediaBox [0 0 200 300]. This pins qpdf 11.9.0's MediaBox-default-BEFORE-clone
/// ordering (QPDF_pages.cc:104-112 runs before the duplicate-clone at :119-130):
/// the shared original is first visited via A (media_box=true, default
/// suppressed), then via B (media_box=false), where the default [0 0 612 792] is
/// applied to the shared ORIGINAL and the clone is then copied from it — so BOTH
/// the A page (the original) and the B page (the clone) end up /MediaBox
/// [0 0 612 792] with /Parent -> A (verified from the qpdf 11.9.0 golden). Were
/// the order reversed (clone before default), the A page would keep A's inherited
/// [0 0 200 300] and only the clone would be [0 0 612 792] — a divergence this
/// guards (flpdf-nd38 repair 3).
#[test]
fn shared_leaf_mediabox_default_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "shared-leaf-mediabox-default.pdf",
        "shared-leaf-mediabox-default",
    );
}

/// A /Page leaf whose /MediaBox is a direct array with an indirect-reference
/// element ([0 0 612 4 0 R], obj 4 = 792). qpdf 11.9.0's isRectangle()
/// dereferences each element via isNumber(), so the box is a valid rectangle and
/// is kept, NOT overwritten with the [0 0 612 792] default. is_rectangle must
/// resolve each element before defaulting (flpdf-nd38 repair 3; codex review
/// r3522482671 on PR #453).
#[test]
fn indirect_mediabox_element_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("indirect-mediabox-element.pdf", "indirect-mediabox-element");
}

/// A /Pages node whose single /Kids entry is a DIRECT (inline) /Page dictionary
/// rather than an indirect reference. qpdf 11.9.0's getAllPagesInternal converts
/// it to an indirect object (QPDF_pages.cc:113-118) (flpdf-nd38 repair 1).
#[test]
fn direct_leaf_kid_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("direct-leaf-kid.pdf", "direct-leaf-kid");
}

/// A catalog whose /Pages points INTO the page tree (at the first page) instead
/// of at the true root. qpdf 11.9.0's getAllPages walks /Parent up to the real
/// root and rewrites the catalog /Pages (QPDF_pages.cc:50-67) (flpdf-nd38 repair 6).
#[test]
fn root_pages_points_into_tree_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "root-pages-points-into-tree.pdf",
        "root-pages-points-into-tree",
    );
}

#[test]
fn relinearize_one_page_is_byte_identical_to_qpdf() {
    // Re-linearizing an already-linearized input: the source's old /Linearized
    // param dict and hint stream must be reachability-GC'd, not leaked into the
    // second half, to stay byte-identical to qpdf --linearize (flpdf-phfu).
    assert_linearize_byte_identical("linearized-one-page.pdf", "linearized-one-page");
}

// flpdf-5apf: a live body object (the Catalog) carrying null-resolving indirect
// refs. qpdf drops the null-valued dict keys (/Bad 0 0 R, /Junk 99 0 R, the
// nested /Inner 99 0 R) and inlines `null` for the object-0 array element
// (/ArrZero [0 0 R 2 0 R] -> [null <font>]); flpdf must reproduce that byte for
// byte rather than exit 2 or keep the dead refs.
#[test]
fn dangling_body_refs_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("dangling-body-one-page.pdf", "dangling-body-one-page");
}

// flpdf-0gyq: a null-resolving ARRAY ref (/Arr [<null-ref> 8 0 R]) is resurrected
// as an indirect null body object the array points at. The FREE variant (obj 9, a
// free xref row) already worked; the MISSING variant (obj 99, no row) must produce
// the identical layout — qpdf treats the two the same.
#[test]
fn resurrect_free_array_ref_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("resurrect-free-one-page.pdf", "resurrect-free-one-page");
}

#[test]
fn resurrect_missing_array_ref_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "resurrect-missing-one-page.pdf",
        "resurrect-missing-one-page",
    );
}

// flpdf-8891: a document-level (Catalog) reference to a first-page object marks
// it lc_first_page_shared, so qpdf orders the first-page section private-then-
// shared (Page, Resources, Content, Font) rather than by source number alone.
// Without the document-`others` signal flpdf left the Font in part2 (private)
// and emitted Font before Content (first divergence at byte 353).
#[test]
fn catalog_firstpage_shared_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "catalog-firstpage-shared-one-page.pdf",
        "catalog-firstpage-shared-one-page",
    );
}

// flpdf-8891 (non-degenerate shape): two pages share font 6 (other_pages) while
// page 1's private font 7 is referenced by the Catalog /Ref2 (others). Both
// sharing signals land in part6's shared group; qpdf orders the first-page
// section Page, Content, Font6, Font7 (private before shared, shared by source
// number). On `main` this diverges at byte 56; the fix makes it byte-identical.
// (Generate mode for this multi-page shape has a separate, pre-existing ObjStm
// layout divergence, so only the classic layout is pinned here.)
#[test]
fn catalog_firstpage_shared_two_page_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "catalog-firstpage-shared-two-page.pdf",
        "catalog-firstpage-shared-two-page",
    );
}

// flpdf-zda0: a NON-first-page object (page-2 font, obj 7) reached by exactly one
// other page AND by a document-level `others` reference (Catalog /Ref2) is qpdf
// `lc_other` (part9), not `lc_other_page_private` (part7): part7 requires
// others==0 (QPDF_linearization.cc:1128). The demoted object takes a part9 object
// number (after the pages tree) and page 1's part7 `object_count` hint excludes
// it (oracle: page-1 nobjects==2). On `main` flpdf routes it to part7, shifting
// object numbers and the hint; the fix makes it byte-identical. (Generate mode
// for this shape is tracked in flpdf-pn7h, so only the classic layout is pinned.)
#[test]
fn catalog_otherpage_other_two_page_classic_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "catalog-otherpage-other-two-page.pdf",
        "catalog-otherpage-other-two-page",
    );
}

// flpdf-8891 (page-tree custom key): a custom extension key on an interior
// /Pages node references the first-page Font. qpdf keeps non-inheritable custom
// keys on /Pages nodes (only the inheritable /Resources,/MediaBox,/CropBox,
// /Rotate are stripped), so ou_root_key("/Pages") still reaches the Font ->
// first-page shared. The Font's source number (2) is below the Content's (5), so
// only the private-before-shared rule yields qpdf's order (Page, Content, Font).
#[test]
fn pages_ext_firstpage_shared_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "pages-ext-firstpage-shared-one-page.pdf",
        "pages-ext-firstpage-shared-one-page",
    );
}

// flpdf-o9im: when the FIRST-PAGE dict directly holds /Arr [<missing-ref> <live-ref>],
// the resurrected null must be classified in the first-page section (Part 2) and
// receive a HIGH object number — not land in part4_rest with a LOW number.
// Oracle: qpdf 11.9.0 assigns obj 9 = null, /Size 10 for this fixture.
#[test]
fn resurrect_missing_page_arr_classic_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "resurrect-missing-page-arr.pdf",
        "resurrect-missing-page-arr",
    );
}

// flpdf-891f: when Page 1 holds /Bad 99 0 R (dict value → dropped by writer)
// and Page 2 holds /Arr [99 0 R] (array element → resurrected null), the null
// must land in the SECOND-HALF section (low object number) — NOT in Part 2.
// Oracle: qpdf 11.9.0 assigns obj 2 = null in the second-half for this fixture.
#[test]
fn resurrect_page2_arr_page1_dictval_not_in_first_page_section() {
    assert_linearize_byte_identical(
        "resurrect-missing-page1-dictval-page2-arr.pdf",
        "resurrect-missing-page1-dictval-page2-arr",
    );
}

// flpdf-891f: when Page 1 holds BOTH /Bad 99 0 R (dict value, dropped by
// writer) AND /Good [99 0 R] (array element, resurrected null), the null must
// land in the FIRST-PAGE section — the array edge wins even though the
// dict-value edge is enqueued first (alphabetical key order).
#[test]
fn resurrect_both_edges_same_page_null_in_first_page_section() {
    assert_linearize_byte_identical(
        "resurrect-both-edges-same-page.pdf",
        "resurrect-both-edges-same-page",
    );
}

// flpdf-891f: cross-object case — Page 1 references resurrectable ref 99 via a
// dict-value edge (/Bad 99 0 R) AND via an array element in a live descendant
// (/Other 4 0 R where obj 4 = << /Good [99 0 R] >>). The dict-value tuple is
// dequeued before the descendant is expanded, but sorting page-dict refs by
// original object number ensures obj 4 (live) is expanded before obj 99
// (resurrectable) is dequeued, so seen_as_array already contains 99 and the
// null is admitted into the first-page section.
#[test]
fn resurrect_crossobj_arr_via_live_desc_null_in_first_page_section() {
    assert_linearize_byte_identical(
        "resurrect-crossobj-arr-via-live-desc.pdf",
        "resurrect-crossobj-arr-via-live-desc",
    );
}

// flpdf-891f: else-branch ordering — a live non-page object's children must be
// enqueued in ascending original-object-number order, not dict-key (alphabetical)
// order. The fixture has an intermediate dict with /AA→orig6 and /ZZ→orig5;
// alphabetical ordering would emit orig6 before orig5, but qpdf emits orig5
// first (number order).
#[test]
fn else_branch_children_ordered_by_original_object_number() {
    assert_linearize_byte_identical(
        "else-branch-obj-number-order.pdf",
        "else-branch-obj-number-order",
    );
}

// flpdf-hsjh: revorder case — resurrectable ref (orig 99) has a LOWER original
// number than the live descendant (orig 100) that holds the array edge
// ([99 0 R]). Sort-at-enqueue puts 99 in the queue before 100 is expanded,
// so seen_as_array is empty when 99 is dequeued → deferred. After the full
// BFS, 100 has populated seen_as_array with 99, so the post-BFS pass admits
// it and inserts it at the correct position in the sorted non-page tail.
#[test]
fn revorder_resurrect_null_in_first_page_section() {
    assert_linearize_byte_identical("revorder-resurrect.pdf", "revorder-resurrect");
}

// flpdf-hsjh (discriminator): Page leaf at a HIGH original-object-number (10)
// with its content stream at a LOWER original-object-number (3). A naive
// fully-global sort by original number would misplace the Page (renumber it
// higher than its content stream), but qpdf keeps the Page first in its
// closure. flpdf must pin the Page at order[0] and sort only order[1..].
#[test]
fn page_highnum_content_lownum_page_before_content() {
    assert_linearize_byte_identical(
        "page-highnum-content-lownum.pdf",
        "page-highnum-content-lownum",
    );
}

// flpdf-hsjh (Codex P2): resurrectable null (orig 99) is reachable via BOTH
// a Catalog dict-value edge (/OpenAction 99 0 R, dropped by writer) and a
// first-page array edge (/Arr [99 0 R], produces a null body object). Before
// this fix, closure_from_seeds admitted the null-resolving ref into
// open_document_set, causing the null to be misrouted to the open-document
// section (Part 4) with a LOW renumbered number, diverging from qpdf which
// classifies the null as lc_first_page (Part 2, last in the first-page
// section with /O=5, /E=900). The fix skips Object::Null in closure_from_seeds.
#[test]
fn od_null_also_in_first_page_arr_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("od-null-page-arr.pdf", "od-null-page-arr");
}

// flpdf-hsjh (Codex P2): resurrectable null (orig 99) reached via a Catalog
// ARRAY edge (/OpenAction [99 0 R]) — qpdf classifies this as open_document
// (lc_open_document) because the null body IS emitted for the surviving array
// slot.  The null must land in the OD section (pre-/O, before the hint stream)
// not in the first-page section.  The fix tracks array vs dict-value edge type
// in closure_from_seeds via collect_direct_refs_with_context so that
// array-reached xref-absent nulls are admitted to open_document_set while
// dict-value-only nulls are excluded.
#[test]
fn od_catalog_arr_null_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("od-arr-null.pdf", "od-arr-null");
}

// --------------------------------------------------------------------------
// Structural byte-parity (flpdf-9hc.13.10): the full-file byte-identity tests
// above now subsume these — flpdf reproduces qpdf's deterministic `/ID[1]` by
// digesting a byte-identical reconstruction of qpdf's *first* write pass (empty
// parameter dict, no hint stream, unresolved first-page xref; see
// `QPDFWriter::writeLinearized` → `computeDeterministicIDData`). These tests are
// kept as a narrower diagnostic: they zero the `/ID[1]` hex run on both sides
// before comparing, so a regression in the structural layout (object numbering,
// physical order, trailers, framing) is isolated from a regression in the
// `/ID[1]` digest, giving a more targeted failure than the strict tests.
// --------------------------------------------------------------------------

/// Replace the 32 hex bytes of every `/ID [<id0><id1>]` array's *second*
/// element with ASCII `'0'`, leaving the permanent identifier `/ID[0]` and all
/// surrounding bytes intact. Used to compare structural layout independently of
/// the deterministic changing identifier.
fn mask_id1(buf: &[u8]) -> Vec<u8> {
    let mut out = buf.to_vec();
    let needle = b" /ID [<";
    let mut i = 0usize;
    while let Some(rel) = find(&out[i..], needle) {
        let arr = i + rel + needle.len();
        // arr -> first hex digit of id0; id0 is 32 hex then '>', then '<', then id1.
        let id0_end = arr + 32;
        // Expect '>' then '<' then 32 id1 hex.
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

fn assert_linearize_structurally_byte_identical(fixture: &str, stem: &str) {
    let actual = mask_id1(&flpdf_linearized(fixture));
    let expected = mask_id1(&golden(stem));
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: structural layout diverged from qpdf golden (ignoring /ID[1])              (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn one_page_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("one-page.pdf", "one-page");
}

#[test]
fn two_page_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("two-page.pdf", "two-page");
}

#[test]
fn three_page_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("three-page.pdf", "three-page");
}

#[test]
fn lone_flate_l9_linearized_structurally_byte_identical_to_qpdf() {
    // Lone /FlateDecode (level 9) preserved verbatim on the linearized path. The
    // structural comparison includes the stream bytes (only /ID[1] is masked), so
    // it verifies preservation; /ID[1] for a new fixture is a separate divergence.
    assert_linearize_structurally_byte_identical("lone-flate-l9.pdf", "lone-flate-l9");
}

// --------------------------------------------------------------------------
// Body content-stream recompression parity (flpdf-9hc.13.10, divergence A).
//
// The linearized writer used to clone each body stream's dict + raw data
// verbatim, preserving e.g. an `[/ASCII85Decode /FlateDecode]` source chain,
// whereas qpdf (and flpdf's plain rewrite) decode the chain and re-encode to a
// single `/FlateDecode`. These tests pin the recompression independently of
// object NUMBERING and physical PLACEMENT (separate divergences), so they pass
// even while the full-file byte-parity tests above are still red.
// --------------------------------------------------------------------------

/// Full-rewrite (non-linearized) one-page.pdf under qpdf-equivalent options:
/// the plain path that already byte-matches `qpdf --static-id`.
fn plain_rewrite_one_page() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("one-page.pdf");
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.deterministic_id = true;
    // Same framing the linearized path uses, so the content-stream bytes line up.
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

/// Extract the page content-stream object body — the single-`/FlateDecode`
/// stream whose dict has neither `/Type` (excludes xref/ObjStm/metadata) nor
/// `/S` (excludes the linearization hint stream) — returning
/// `(dict_bytes, payload_bytes)`. The payload is the `/Length` bytes between
/// the `stream` EOL and `endstream`.
fn content_stream_object(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let needle = b"stream\n";
    let mut found: Option<(Vec<u8>, Vec<u8>)> = None;
    let mut i = 0usize;
    while let Some(rel) = find(&data[i..], needle) {
        let stream_kw = i + rel; // position of "stream\n"
                                 // Walk back to the dict opener "<<" on a preceding `N G obj\n` line.
        let dict_open = rfind(&data[..stream_kw], b"<<").expect("stream must follow a dict");
        let dict_close = stream_kw - 1; // the byte before "stream\n" is '\n' after ">>"
        let dict = &data[dict_open..dict_close];
        let payload_start = stream_kw + needle.len();
        let es =
            find(&data[payload_start..], b"endstream").expect("endstream must follow a stream");
        let payload_end = payload_start + es;
        let payload = &data[payload_start..payload_end];
        let is_lone_flate = find(dict, b"/Filter /FlateDecode").is_some();
        let has_type = find(dict, b"/Type").is_some();
        let is_hint = find(dict, b"/S ").is_some();
        if is_lone_flate && !has_type && !is_hint {
            assert!(
                found.is_none(),
                "expected exactly one page content stream in one-page output"
            );
            found = Some((dict.to_vec(), payload.to_vec()));
        }
        i = payload_end + b"endstream".len();
    }
    found.expect("a single-FlateDecode page content stream must be present")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

#[test]
fn one_page_linearized_content_stream_is_recompressed_single_flate() {
    let lin = flpdf_linearized("one-page.pdf");
    let (dict, payload) = content_stream_object(&lin);

    // Recompressed: a single `/FlateDecode`, NOT the source `[/ASCII85Decode
    // /FlateDecode]` chain, and serialized in qpdf's stream-dict key order
    // (`/Length` then a regenerated `/Filter`).
    assert_eq!(
        dict,
        b"<< /Length 82 /Filter /FlateDecode >>",
        "linearized page content stream must be re-encoded to a single \
         /FlateDecode in qpdf key order; got {:?}",
        String::from_utf8_lossy(&dict)
    );
    assert!(
        find(&dict, b"ASCII85").is_none(),
        "the ASCII85 source filter must be dropped on recompression"
    );
    assert_eq!(payload.len(), 82, "/Length must equal the on-disk payload");
}

#[test]
fn one_page_linearized_content_stream_equals_plain_and_qpdf_golden() {
    let lin = flpdf_linearized("one-page.pdf");
    let plain = plain_rewrite_one_page();
    let qpdf = golden("one-page");

    let (lin_dict, lin_payload) = content_stream_object(&lin);
    let (plain_dict, plain_payload) = content_stream_object(&plain);
    let (qpdf_dict, qpdf_payload) = content_stream_object(&qpdf);

    // The recompressed linearized content-stream object must be byte-identical
    // to the plain-path object and to qpdf's golden obj9 — dict AND payload.
    assert_eq!(
        (&lin_dict, &lin_payload),
        (&plain_dict, &plain_payload),
        "linearized content stream must equal the plain-path content stream"
    );
    assert_eq!(
        (&lin_dict, &lin_payload),
        (&qpdf_dict, &qpdf_payload),
        "linearized content stream must equal qpdf golden obj9"
    );
}

// --------------------------------------------------------------------------
// Classic (non-ObjStm) linearize: outline object section routing (flpdf-vvjr.2).
//
// outlines-80-80 (!UseOutlines): catalog /Outlines -> outline dict + 80 items.
// qpdf routes these to part9 (second-half, after /E). Regression: the
// useoutlines case (below) must also stay byte-identical.
//
// useoutlines-80-80 (UseOutlines): /PageMode /UseOutlines causes outline
// objects (dict + 80 items) to route to part6 (first-page section, before /E).
// Their plain bytes count toward page-0 length in the page hint table header.
// --------------------------------------------------------------------------

#[test]
fn outlines_classic_structurally_byte_identical_to_qpdf() {
    assert_classic_structurally_byte_identical(
        "objstm-lin-outlines-80-80.pdf",
        "objstm-lin-outlines-80-80",
    );
}

#[test]
fn outlines_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical("objstm-lin-outlines-80-80.pdf", "objstm-lin-outlines-80-80");
}

#[test]
fn useoutlines_classic_structurally_byte_identical_to_qpdf() {
    assert_classic_structurally_byte_identical(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}

#[test]
fn useoutlines_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-useoutlines-80-80.pdf",
        "objstm-lin-useoutlines-80-80",
    );
}

// outlines-shared-page-80-80 (flpdf-q2zw): one font (the highest-numbered outline
// object) is referenced by BOTH pages AND an outline item via /Extra. qpdf's
// categorization (QPDF_linearization.cc:1120) ranks in_outlines above in_first_page,
// so that font is lc_outlines (part9, second half) while the 79 page-only fonts stay
// lc_first_page_shared (part6, first half). The classic path previously kept it in
// the first-page section (param dict 85 vs qpdf 86); this pins the outline >
// first-page precedence on the non-ObjStm path. pushOutlinesToPart emits outlines
// as [root] ++ ascending source number, so the font (max number) is the last object.
#[test]
fn outlines_shared_page_classic_structurally_byte_identical_to_qpdf() {
    assert_classic_structurally_byte_identical(
        "objstm-lin-outlines-shared-page-80-80.pdf",
        "objstm-lin-outlines-shared-page-80-80",
    );
}

#[test]
fn outlines_shared_page_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-outlines-shared-page-80-80.pdf",
        "objstm-lin-outlines-shared-page-80-80",
    );
}

// --------------------------------------------------------------------------
// Open-document closure (flpdf-lubb): objects reachable from the catalog
// open-document keys (/OpenAction, /AcroForm, /PageMode, /Threads,
// /ViewerPreferences) are placed in part4 (first half, before /O) by qpdf in
// ALL object-stream modes. These fixtures carry no source ObjStm, so the
// preserve/disable classic golden pins that partition byte-for-byte.
// --------------------------------------------------------------------------

#[test]
fn od_indirect_length_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-od-indirect-length.pdf",
        "objstm-lin-od-indirect-length",
    );
}

#[test]
fn openaction_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-openaction-80-80.pdf",
        "objstm-lin-openaction-80-80",
    );
}

#[test]
fn acroform_widget_page1_page2_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-acroform-widget-page1-page2.pdf",
        "objstm-lin-acroform-widget-page1-page2",
    );
}

#[test]
fn acroform_widget_page1_only_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-acroform-widget-page1-only.pdf",
        "objstm-lin-acroform-widget-page1-only",
    );
}

#[test]
fn acroform_widget_page0_5_10_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-acroform-widget-page0-5-10.pdf",
        "objstm-lin-acroform-widget-page0-5-10",
    );
}

#[test]
fn acroform_widget_ap_stream_page0_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-acroform-widget-ap-stream-page0.pdf",
        "objstm-lin-acroform-widget-ap-stream-page0",
    );
}

#[test]
fn useoutline_od_shared_stream_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-useoutline-od-shared-stream.pdf",
        "objstm-lin-useoutline-od-shared-stream",
    );
}

#[test]
fn od_indirect_length_flate_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-od-indirect-length-flate.pdf",
        "objstm-lin-od-indirect-length-flate",
    );
}

/// Exercises the `part4_open_document_plain` source-number sort with MORE THAN
/// ONE open-document object (multiple /OpenAction destinations), so the ordering
/// is pinned against the qpdf oracle rather than only reasoned about.
#[test]
fn openaction_multi_od_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-openaction-multi-od.pdf",
        "objstm-lin-openaction-multi-od",
    );
}

#[test]
fn outline_od_shared_stream_classic_byte_identical_to_qpdf() {
    assert_classic_byte_identical(
        "objstm-lin-outline-od-shared-stream.pdf",
        "objstm-lin-outline-od-shared-stream",
    );
}
