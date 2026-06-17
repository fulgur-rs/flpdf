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
    let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
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

#[test]
fn shared_stream_objstm_byte_identical_to_qpdf() {
    assert_strict("shared-stream-objstm.pdf", "shared-stream-objstm");
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

// threepage-2-120 (part8 other-page-shared container) and disc-2-250-2 (a part7
// container coexisting with a part8 plain Form XObject) are not yet
// byte-identical: threepage's residual is the shared-object hint table for a
// part8 container; disc additionally needs second-half containers ordered by
// PART (part7→part8→part9) rather than even-split order (they pass the part-order
// check only coincidentally today). Tracked in the Phase-2 design doc.
#[test]
#[ignore = "part8 shared-object hint table (threepage); part-ordered second-half containers (disc)"]
fn threepage_shared_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural(
        "objstm-lin-threepage-2-120.pdf",
        "objstm-lin-threepage-2-120",
    );
}

#[test]
#[ignore = "finding-4: second-half container numbering (Stage B)"]
fn disc_part7_part8_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("objstm-lin-disc-2-250-2.pdf", "objstm-lin-disc-2-250-2");
}
