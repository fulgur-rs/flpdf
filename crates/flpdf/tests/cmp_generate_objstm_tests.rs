//! Byte-identity: flpdf `--object-streams=generate` (NON-linearized) ==
//! `qpdf --object-streams=generate --static-id`.
//!
//! First parity coverage for the non-linearized generate path (flpdf-g6hb.1).
//! qpdf assigns object streams up front (`QPDF::getCompressibleObjGens` DFS +
//! `QPDFWriter::generateObjectStreams` even split), then numbers each ObjStm
//! container immediately before its members and serializes members in ascending
//! source-object order. The cross-reference is emitted as a *stream* (type-2
//! entries require it) and the header is floored to 1.5.
//!
//! Gated on `qpdf-zlib-compat` because byte-identity requires flpdf's deflate to
//! match qpdf's classic-libz output (the Pure-Rust miniz_oxide default produces
//! equivalent but not byte-identical compression). `--static-id` keeps the
//! trailer `/ID` byte-stable: `/ID[0]` is the preserved source identifier (or the
//! pi constant when the source has none) and `/ID[1]` is the pi constant — both
//! reproducible, so the gate is strict (no `/ID` masking).
//!
//! CAVEAT: byte-identity pins to the linked libz version (captured with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 / qpdf 11.9.0); a different libz may shift the deflate
//! bytes and require re-blessing the goldens (`tests/golden/regenerate.sh`).

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, ObjectStreamMode, Pdf, WriteOptions};
use std::path::Path;

/// Full-rewrite `fixture` with `--object-streams=generate --static-id` (the
/// qpdf-matching option set) and return the bytes.
fn generate_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.object_streams = ObjectStreamMode::Generate;
    opts.static_id = true;
    // qpdf's default output writes no newline before endstream.
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    // compress_streams defaults to Yes (decode + re-encode to single FlateDecode).

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("generate.pdf");
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

fn assert_cmp_diff_zero(fixture: &str, stem: &str) {
    let actual = generate_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --object-streams=generate \
             --static-id golden (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

// ── Step A: no-stream, single container ──────────────────────────────────────
// 5-page natural fixture: 7 eligible dicts (Catalog + Pages + 5 pages), no
// content streams => 1 ObjStm container, no plain-object emission. Isolates the
// container-first / members-ascending-source numbering.
#[test]
fn nostream_5_generate_is_byte_identical_to_qpdf() {
    assert_cmp_diff_zero("objstm-gen-nostream-5.pdf", "objstm-gen-nostream-5");
}

// ── Step B: no-stream, even split into multiple containers ───────────────────
// 130-page reverse fixture: 132 eligible => ceil(132/100)=2 containers of 66
// (even split, NOT greedy fill-100). /Kids descending so the
// getCompressibleObjGens DFS grouping differs from numeric order.
#[test]
fn nostream_130rev_generate_is_byte_identical_to_qpdf() {
    assert_cmp_diff_zero(
        "objstm-gen-nostream-130rev.pdf",
        "objstm-gen-nostream-130rev",
    );
}

// ── flpdf-ipc6: generate + forced sub-1.5 header suppresses object/xref streams ─
// A forced sub-1.5 header is a hard cap qpdf will not exceed, so object streams
// and cross-reference streams (both PDF 1.5 features) are suppressed: qpdf keeps
// the 1.4 header and writes a classic xref table (no `/ObjStm`, no `/Type /XRef`),
// i.e. generate+force1.4 == disable+force1.4. flpdf must be byte-identical.

/// Full-rewrite `fixture` with generate + a forced version (qpdf-matching options).
fn generate_force_qpdf_equivalent(fixture: &str, force: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.object_streams = ObjectStreamMode::Generate;
    opts.force_version = Some(force.to_string());
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
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
fn three_page_generate_force_version_1_4_suppressed_is_byte_identical_to_qpdf() {
    let actual = generate_force_qpdf_equivalent("three-page.pdf", "1.4");
    let expected = golden_named("three-page", "generate-force14.pdf");
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "three-page generate --force-version=1.4: not byte-identical to qpdf \
             suppressed golden (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

// ── Step C: content streams interleaved with the ObjStm container ────────────
// three-page fixture: the Catalog/Pages/page dicts pack into one container
// (objs 2-9), while the content streams and font remain plain objects numbered
// AFTER the members (objs 10-13). Validates the unified ascending-new-number
// emit (container body, then plain bodies) and the GenerateRenumber plain-object
// BFS ordering.
#[test]
fn three_page_generate_is_byte_identical_to_qpdf() {
    assert_cmp_diff_zero("three-page.pdf", "three-page");
}

// ── Step D: orphaned indirect /Length holder dropped (flpdf-sqkq) ─────────────
// The catalog's /OpenAction reaches a JavaScript stream (obj 6) with an INDIRECT
// /Length (7 0 R); the holder (obj 7) is reachable ONLY through that /Length
// edge. Once /Length is normalized to a direct integer the holder orphans, and
// qpdf garbage-collects it before renumbering. The generate path must drop it
// from the renumber universe too, shifting numbers contiguously.
#[test]
fn od_indirect_length_generate_drops_orphan_holder_byte_identical_to_qpdf() {
    assert_cmp_diff_zero(
        "objstm-lin-od-indirect-length.pdf",
        "objstm-lin-od-indirect-length",
    );
}

#[test]
fn od_indirect_length_flate_generate_drops_orphan_holder_byte_identical_to_qpdf() {
    // Same orphan structure, but the JS stream is a lone /FlateDecode source.
    assert_cmp_diff_zero(
        "objstm-lin-od-indirect-length-flate.pdf",
        "objstm-lin-od-indirect-length-flate",
    );
}
