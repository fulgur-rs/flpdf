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

#[test]
fn nonid_id0_linearized_is_byte_identical_to_qpdf() {
    // Non-16-byte (20-byte) source /ID[0] preserved verbatim on the linearized
    // path, byte-identical to qpdf --linearize --deterministic-id (flpdf-9hc.13.11).
    assert_linearize_byte_identical("nonid-id0.pdf", "nonid-id0");
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
