//! Byte-identity demonstration: flpdf plain rewrite == `qpdf --static-id`.
//!
//! This is the capstone of the qpdf byte-identical roadmap. It is gated on the
//! `qpdf-zlib-compat` feature because byte-identity requires flpdf's deflate
//! output to match qpdf's classic-libz output (the Pure-Rust miniz_oxide default
//! produces equivalent but not byte-identical compression). Three independent
//! pieces must all line up:
//!
//!   1. Stream-dictionary key order — `/Length` pulled out, `/Filter` last on
//!      re-filtered streams (matches `QPDFWriter::unparseObject`).
//!   2. Trailer on the `trailer ` line with keys sorted and `/ID` last.
//!   3. No newline before `endstream` ([`NewlineBeforeEndstream::Never`]) —
//!      qpdf's default output writes exactly `/Length` bytes then `endstream`.
//!
//! plus deflate parity (this feature) and the deterministic `--static-id` trailer
//! `/ID`. With all of these, flpdf's full rewrite is `cmp`-diff-0 against the
//! committed `qpdf --static-id` golden references.
//!
//! CAVEAT: byte-identity pins to the linked libz version (captured with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 / qpdf 11.9.0); a different libz may shift the deflate
//! bytes and require re-blessing the goldens.

#![cfg(feature = "qpdf-zlib-compat")]

mod common;
use common::PdfCanonicalTestExt;

use common::{write_with_settings, WriterTestSettings};
use flpdf::{load_xref_and_trailer, ObjectRef, ObjectStreamMode, Pdf, StreamDataMode, XrefEntry};
use std::io::Cursor;
use std::path::Path;

/// Full-rewrite `fixture` with the qpdf-matching option set and return the bytes.
fn rewrite_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode::Disable)
}

/// Full-rewrite `fixture` with an explicit object-stream mode and the
/// qpdf-matching option set.
fn rewrite_qpdf_equivalent_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        object_streams: mode,
        static_id: true,
        // qpdf's default output writes no newline before endstream.
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    // compress_streams defaults to Yes (decode + re-encode to single FlateDecode).

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("static-id.pdf");
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
    let actual = rewrite_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --static-id golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

fn assert_cmp_diff_zero_mode_named(fixture: &str, mode: ObjectStreamMode, stem: &str, name: &str) {
    let actual = rewrite_qpdf_equivalent_mode(fixture, mode);
    assert_cmp_diff_zero_named(&actual, stem, name);
}

fn assert_mode_cmp_diff_zero(fixture: &str, mode: ObjectStreamMode) {
    assert_cmp_diff_zero_mode_named(&format!("{fixture}.pdf"), mode, fixture, "static-id.pdf");
}

/// Full-rewrite `fixture` with an explicit object-stream `mode` + forced version
/// (qpdf-matching options).
fn rewrite_mode_force_qpdf_equivalent(
    fixture: &str,
    mode: ObjectStreamMode,
    force: &str,
) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        object_streams: mode,
        force_version: Some(force.to_string()),
        static_id: true,
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
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

/// Full-rewrite `fixture` in `--stream-data=preserve` mode with the qpdf-matching
/// option set (matches `qpdf --static-id --stream-data=preserve`).
fn rewrite_preserve_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        static_id: true,
        stream_data: Some(StreamDataMode::Preserve),
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
    out
}

/// Assert `actual` is byte-identical to the named golden under `references/<stem>/`.
fn assert_cmp_diff_zero_named(actual: &[u8], stem: &str, name: &str) {
    let expected = golden_named(stem, name);
    if let Some(off) = first_diff(actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{stem}/{name}: not byte-identical to qpdf golden \
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
fn force_below_1_5_downgrades_xref_stream_source_byte_identical_to_qpdf() {
    // The xref-stream -> classic-table DOWNGRADE path is new (ipc6 only ever did
    // table -> stream UPGRADES). Anchor it to qpdf: flpdf preserve+force1.4 on an
    // ObjStm/xref-stream source must be byte-identical to qpdf's classic-table
    // output (qpdf --object-streams=preserve --force-version=1.4 --static-id).
    let actual = rewrite_mode_force_qpdf_equivalent(
        "three-page-objstm.pdf",
        ObjectStreamMode::Preserve,
        "1.4",
    );
    let expected = golden_named("three-page-objstm", "downgrade-force14.pdf");
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "xref-stream downgrade not byte-identical to qpdf golden \
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
fn one_two_three_page_mode_matrix_is_byte_identical_to_qpdf() {
    for fixture in ["one-page", "two-page", "three-page"] {
        for mode in [ObjectStreamMode::Disable, ObjectStreamMode::Preserve] {
            assert_mode_cmp_diff_zero(fixture, mode);
        }
    }
}

#[test]
fn disable_xref_stream_source_downgrades_to_classic_table_byte_identical_to_qpdf() {
    assert_cmp_diff_zero_mode_named(
        "null-visible-matrix-objstm.pdf",
        ObjectStreamMode::Disable,
        "null-visible-matrix-objstm",
        "disable.pdf",
    );
}

#[test]
fn preserve_object_stream_mode_is_byte_identical_to_qpdf_static_id_matrix() {
    let cases = [
        ("three-page-objstm.pdf", "three-page-objstm", "preserve.pdf"),
        (
            "objstm-lin-od-indirect-length.pdf",
            "objstm-lin-od-indirect-length",
            "static-id.pdf",
        ),
        (
            "objstm-lin-od-indirect-length-flate.pdf",
            "objstm-lin-od-indirect-length-flate",
            "static-id.pdf",
        ),
        (
            "kept-indirect-length.pdf",
            "kept-indirect-length",
            "static-id.pdf",
        ),
    ];
    for (fixture, stem, name) in cases {
        assert_cmp_diff_zero_mode_named(fixture, ObjectStreamMode::Preserve, stem, name);
    }
}

#[test]
fn preserve_nonmonotonic_source_indices_match_qpdf_source_number_order() {
    let fixture = "nonmonotonic-objstm-index.pdf";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let source = std::fs::read(path).unwrap();
    let source_xref = load_xref_and_trailer(&mut Cursor::new(&source)).unwrap();
    assert_eq!(
        source_xref.entries.get(&ObjectRef::new(3, 0)),
        Some(&XrefEntry::Compressed {
            stream: 4,
            index: 0,
        })
    );
    assert_eq!(
        source_xref.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Compressed {
            stream: 4,
            index: 1,
        })
    );

    let actual = rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode::Preserve);
    assert_cmp_diff_zero_named(&actual, "nonmonotonic-objstm-index", "preserve.pdf");

    let output_xref = load_xref_and_trailer(&mut Cursor::new(&actual)).unwrap();
    assert_eq!(
        output_xref.entries.get(&ObjectRef::new(3, 0)),
        Some(&XrefEntry::Compressed {
            stream: 2,
            index: 0,
        })
    );
    assert_eq!(
        output_xref.entries.get(&ObjectRef::new(4, 0)),
        Some(&XrefEntry::Compressed {
            stream: 2,
            index: 1,
        })
    );

    let mut rewritten = Pdf::open(Cursor::new(actual)).unwrap();
    let catalog = rewritten
        .resolve_canonical_object(rewritten.root_ref().unwrap())
        .unwrap();
    assert_eq!(
        catalog.try_get_key(b"/Pages").unwrap().object_ref(),
        Some(ObjectRef::new(3, 0))
    );
    assert_eq!(
        catalog.try_get_key(b"/Extra").unwrap().object_ref(),
        Some(ObjectRef::new(4, 0))
    );
}

#[test]
fn lone_flate_l9_plain_rewrite_is_byte_identical_to_qpdf_static_id() {
    // A lone /FlateDecode source compressed at level 9: flpdf must preserve the
    // bytes verbatim (qpdf default), so re-encoding at level 6 would diverge.
    assert_cmp_diff_zero("lone-flate-l9.pdf", "lone-flate-l9");
}

#[test]
fn od_indirect_length_plain_rewrite_drops_orphan_holder_byte_identical_to_qpdf() {
    // The catalog's /OpenAction reaches a JavaScript stream (obj 6) with an
    // INDIRECT /Length (7 0 R); the holder (obj 7) is reachable ONLY through
    // that /Length edge. Once /Length is normalized to a direct integer the
    // holder orphans, and qpdf garbage-collects it. The plain full-rewrite path
    // must drop it too, shifting object numbers contiguously — not
    // emit it as a trailing integer object.
    assert_cmp_diff_zero(
        "objstm-lin-od-indirect-length.pdf",
        "objstm-lin-od-indirect-length",
    );
}

#[test]
fn od_indirect_length_flate_plain_rewrite_drops_orphan_holder_byte_identical_to_qpdf() {
    // Same orphan structure, but the JS stream is a lone /FlateDecode (the
    // writer's verbatim-preserve path): /Length is direct-ized to the
    // compressed byte count when the holder is dropped.
    assert_cmp_diff_zero(
        "objstm-lin-od-indirect-length-flate.pdf",
        "objstm-lin-od-indirect-length-flate",
    );
}

#[test]
fn od_indirect_length_preserve_drops_orphan_holder_byte_identical_to_qpdf() {
    // --stream-data=preserve keeps the stream bytes verbatim, but qpdf still
    // direct-izes every stream's /Length and GCs the orphaned holder.
    // The orphan-drop gate must therefore fire for preserve too, not only when
    // streams are recompressed.
    let actual = rewrite_preserve_qpdf_equivalent("objstm-lin-od-indirect-length.pdf");
    assert_cmp_diff_zero_named(&actual, "objstm-lin-od-indirect-length", "preserve.pdf");
}

#[test]
fn od_indirect_length_flate_preserve_drops_orphan_holder_byte_identical_to_qpdf() {
    // Same as above with a lone /FlateDecode JS stream: under preserve the
    // compressed bytes are kept verbatim and /Length is direct-ized to the
    // compressed byte count when the holder is dropped.
    let actual = rewrite_preserve_qpdf_equivalent("objstm-lin-od-indirect-length-flate.pdf");
    assert_cmp_diff_zero_named(
        &actual,
        "objstm-lin-od-indirect-length-flate",
        "preserve.pdf",
    );
}

#[test]
fn kept_indirect_length_preserve_directizes_kept_holder_byte_identical_to_qpdf() {
    // The dual of the orphan case: the image XObject's indirect /Length holder is
    // ALSO referenced by the catalog (/KeepHolder), so it stays live. qpdf still
    // direct-izes the stream's /Length even under preserve (it normalizes EVERY
    // stream's /Length to a direct integer), keeping the holder as a now
    // length-unreferenced live integer. flpdf must match.
    let actual = rewrite_preserve_qpdf_equivalent("kept-indirect-length.pdf");
    assert_cmp_diff_zero_named(&actual, "kept-indirect-length", "preserve.pdf");
}

#[test]
fn kept_indirect_length_plain_rewrite_directizes_length_keeps_holder_byte_identical_to_qpdf() {
    // Dual of the orphan case: an image XObject (obj 5) declares
    // /Filter /DCTDecode — which flpdf cannot decode, so it is passed through
    // verbatim — and carries an INDIRECT /Length (6 0 R) whose holder (obj 6) is
    // ALSO referenced by the catalog (/KeepHolder 6 0 R). qpdf direct-izes the
    // /Length to the raw byte count (every emitted stream gets a direct /Length)
    // while KEEPING the holder (it has another live reference). The decode-failure
    // passthrough path used to leak the renumbered indirect /Length; this pins it
    // byte-identical to qpdf.
    assert_cmp_diff_zero("kept-indirect-length.pdf", "kept-indirect-length");
}
