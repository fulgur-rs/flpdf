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

// The structural (layout) milestone goes green once the linearized writer emits
// qpdf-matching compressed cross-reference streams (flpdf-4z56). Ignored until
// the integration lands so the baseline commit (goldens + test) stays green.
#[test]
#[ignore = "compressed xref-stream layout pending flpdf-4z56 writer integration"]
fn two_page_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("two-page.pdf", "two-page");
}

#[test]
#[ignore = "compressed xref-stream layout pending flpdf-4z56 writer integration"]
fn three_page_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("three-page.pdf", "three-page");
}

// Strict (incl. /ID[1]) needs qpdf's pass-1 xref-stream reconstruction for the
// deterministic-/ID digest (flpdf-4z56 sub-step 2). Ignored until that lands.
#[test]
#[ignore = "deterministic /ID[1] byte-parity pending qpdf pass-1 xref reconstruction"]
fn two_page_objstm_byte_identical_to_qpdf() {
    assert_strict("two-page.pdf", "two-page");
}

#[test]
#[ignore = "deterministic /ID[1] byte-parity pending qpdf pass-1 xref reconstruction"]
fn three_page_objstm_byte_identical_to_qpdf() {
    assert_strict("three-page.pdf", "three-page");
}
