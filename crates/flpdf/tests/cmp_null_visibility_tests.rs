//! Byte-identity coverage for qpdf's null-aware dictionary visibility.

#![cfg(feature = "qpdf-zlib-compat")]

mod common;
use common::PdfCanonicalTestExt;

use common::{write_linearized_with_settings, write_with_settings, WriterTestSettings};
use flpdf::{
    CompressStreams, NewlineBeforeEndstream, ObjectHandle, ObjectRef, ObjectStreamMode, Pdf,
    PdfOpenOptions, StreamDataMode,
};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::sync::Arc;

fn append_xref_entry(entries: &mut Vec<u8>, kind: u8, field1: u32, field2: u16) {
    entries.push(kind);
    entries.extend_from_slice(&field1.to_be_bytes());
    entries.extend_from_slice(&field2.to_be_bytes());
}

fn single_member_objstm_fixture(member: &[u8], trailer_extras: &str) -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Held 5 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let objstm_offset = bytes.len();
    let mut body = b"5 0 ".to_vec();
    body.extend_from_slice(member);
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
            body.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    let mut xref = Vec::new();
    append_xref_entry(&mut xref, 0, 0, u16::MAX);
    append_xref_entry(&mut xref, 1, catalog_offset as u32, 0);
    append_xref_entry(&mut xref, 1, pages_offset as u32, 0);
    append_xref_entry(&mut xref, 0, 0, 0);
    append_xref_entry(&mut xref, 1, objstm_offset as u32, 0);
    append_xref_entry(&mut xref, 2, 4, 0);
    append_xref_entry(&mut xref, 1, xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R {trailer_extras}\
             /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

fn preserve_fixture(
    fixture: &[u8],
    configure: impl FnOnce(&mut WriterTestSettings),
) -> flpdf::Result<Vec<u8>> {
    let mut pdf = Pdf::open_mem(Arc::from(fixture))?;
    let mut settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Preserve,
        static_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    configure(&mut settings);
    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &settings)?;
    Ok(out)
}

fn rewrite_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    rewrite_mode_with_policy(fixture, mode, None, CompressStreams::Yes)
}

fn rewrite_mode_with_policy(
    fixture: &str,
    mode: ObjectStreamMode,
    stream_data: Option<StreamDataMode>,
    compress_streams: CompressStreams,
) -> Vec<u8> {
    rewrite_mode_with_policy_and_id(fixture, mode, stream_data, compress_streams, false)
}

fn rewrite_mode_with_policy_and_id(
    fixture: &str,
    mode: ObjectStreamMode,
    stream_data: Option<StreamDataMode>,
    compress_streams: CompressStreams,
    deterministic_id: bool,
) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let settings = WriterTestSettings {
        object_streams: mode,
        stream_data,
        compress_streams,
        static_id: !deterministic_id,
        deterministic_id,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &settings).unwrap();
    out
}

fn linearize_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    linearize_mode_with_stream_data(fixture, mode, None)
}

fn linearize_mode_with_stream_data(
    fixture: &str,
    mode: ObjectStreamMode,
    stream_data: Option<StreamDataMode>,
) -> Vec<u8> {
    linearize_mode_result(fixture, mode, stream_data).expect("linearized write")
}

fn linearize_mode_result(
    fixture: &str,
    mode: ObjectStreamMode,
    stream_data: Option<StreamDataMode>,
) -> flpdf::Result<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);

    let mut pdf = Pdf::open(BufReader::new(File::open(&path).unwrap()))?;
    let settings = WriterTestSettings {
        object_streams: mode,
        stream_data,
        deterministic_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut pdf, &settings)
}

fn linearize_encrypted_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let open_options = PdfOpenOptions::default();
    let mut pdf = Pdf::open_with_options(BufReader::new(File::open(&path).unwrap()), open_options)
        .expect("encrypted source must authenticate");
    let settings = WriterTestSettings {
        object_streams: mode,
        stream_data: Some(StreamDataMode::Preserve),
        deterministic_id: true,
        preserve_encryption: false,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut pdf, &settings).expect("linearized write")
}

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

fn assert_golden(actual: &[u8], golden_name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(golden_name);
    let expected =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read golden {path:?}: {error}"));
    if let Some(offset) = first_diff(actual, &expected) {
        let start = offset.saturating_sub(16);
        panic!(
            "{golden_name}: not byte-identical to qpdf golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {offset})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[start..(offset + 16).min(actual.len())],
            &expected[start..(offset + 16).min(expected.len())],
        );
    }
}

#[test]
fn disable_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix.pdf", ObjectStreamMode::Disable),
        "null-visible-matrix/disable.pdf",
    );
}

#[test]
fn generate_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix.pdf", ObjectStreamMode::Generate),
        "null-visible-matrix/generate.pdf",
    );
}

#[test]
fn generate_null_visibility_split_boundary_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode(
            "null-visible-split-boundary.pdf",
            ObjectStreamMode::Generate,
        ),
        "null-visible-split-boundary/generate.pdf",
    );
}

#[test]
fn generate_stale_generation_does_not_hide_current_generation() {
    assert_golden(
        &rewrite_mode(
            "null-visible-stale-generation.pdf",
            ObjectStreamMode::Generate,
        ),
        "null-visible-stale-generation/generate.pdf",
    );
}

#[test]
fn generate_direct_trailer_array_rewrites_removed_generation_to_null() {
    let source = include_bytes!("../../../tests/fixtures/compat/null-visible-stale-generation.pdf");
    let trailer = b"<< /Size 5 /Root 1 0 R >>";
    let trailer_offset = source
        .windows(trailer.len())
        .position(|window| window == trailer)
        .expect("fixture trailer");
    let mut fixture = source[..trailer_offset].to_vec();
    fixture.extend_from_slice(b"<< /Size 5 /Root 1 0 R /Extra [4 0 R 4 1 R] >>");
    fixture.extend_from_slice(&source[trailer_offset + trailer.len()..]);

    let mut pdf = Pdf::open(Cursor::new(fixture)).expect("fixture must open");
    let settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Generate,
        static_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)
        .expect("generate rewrite must remap direct trailer values");
    let mut rewritten = Pdf::open(Cursor::new(output)).expect("generated output must reopen");

    let extra = rewritten
        .trailer()
        .try_get_key(b"/Extra")
        .expect("rewritten trailer must expose /Extra");
    let values = extra.as_array().expect("/Extra must remain an array");
    assert_eq!(values.len(), 2);
    assert!(values[0].is_null(), "removed generation must become null");
    assert!(
        values[1].object_ref().is_some(),
        "current generation must remain an indirect reference"
    );
}

#[test]
fn disable_keeps_stale_generation_identity_like_standard_qpdf_enqueue() {
    assert_golden(
        &rewrite_mode(
            "null-visible-stale-generation.pdf",
            ObjectStreamMode::Disable,
        ),
        "null-visible-stale-generation/disable.pdf",
    );
}

#[test]
fn preserve_without_source_objstm_keeps_stale_generation_identity() {
    assert_golden(
        &rewrite_mode(
            "null-visible-stale-generation.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-stale-generation/preserve.pdf",
    );
}

#[test]
fn source_objstm_preserve_removes_only_stale_generation() {
    assert_golden(
        &rewrite_mode(
            "null-visible-stale-generation-objstm.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-stale-generation-objstm/preserve.pdf",
    );
}

#[test]
fn generate_structural_streams_follow_effective_stream_policy() {
    let cases = [
        (
            Some(StreamDataMode::Preserve),
            CompressStreams::Yes,
            "stream-preserve.pdf",
        ),
        (
            Some(StreamDataMode::Uncompress),
            CompressStreams::Yes,
            "stream-uncompress.pdf",
        ),
        (
            Some(StreamDataMode::Compress),
            CompressStreams::Yes,
            "stream-compress.pdf",
        ),
        (None, CompressStreams::No, "compress-streams-n.pdf"),
    ];
    for (stream_data, compress_streams, golden) in cases {
        assert_golden(
            &rewrite_mode_with_policy(
                "null-visible-stale-generation.pdf",
                ObjectStreamMode::Generate,
                stream_data,
                compress_streams,
            ),
            &format!("null-visible-stale-generation/{golden}"),
        );
    }
}

#[test]
fn source_objstm_preserve_structural_streams_follow_effective_stream_policy() {
    let cases = [
        (
            Some(StreamDataMode::Preserve),
            CompressStreams::Yes,
            "stream-preserve.pdf",
        ),
        (
            Some(StreamDataMode::Uncompress),
            CompressStreams::Yes,
            "stream-uncompress.pdf",
        ),
        (
            Some(StreamDataMode::Compress),
            CompressStreams::Yes,
            "stream-compress.pdf",
        ),
        (None, CompressStreams::No, "compress-streams-n.pdf"),
    ];
    for (stream_data, compress_streams, golden) in cases {
        assert_golden(
            &rewrite_mode_with_policy(
                "null-visible-preserve-signature-null-fields.pdf",
                ObjectStreamMode::Preserve,
                stream_data,
                compress_streams,
            ),
            &format!("null-visible-preserve-signature-null-fields/{golden}"),
        );
    }
}

#[test]
fn generate_keeps_signatures_with_null_fields_compressed_and_hides_fields() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-signature-null-fields.pdf",
            ObjectStreamMode::Generate,
        ),
        "null-visible-preserve-signature-null-fields/generate.pdf",
    );
}

#[test]
fn preserve_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix-objstm.pdf", ObjectStreamMode::Preserve),
        "null-visible-matrix-objstm/preserve.pdf",
    );
}

#[test]
fn linearize_disable_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &linearize_mode("null-visible-matrix.pdf", ObjectStreamMode::Disable),
        "null-visible-matrix/linearize.pdf",
    );
}

#[test]
fn linearize_generate_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &linearize_mode("null-visible-matrix.pdf", ObjectStreamMode::Generate),
        "null-visible-matrix/linearize-objstm.pdf",
    );
}

#[test]
fn linearize_preserve_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &linearize_mode("null-visible-matrix-objstm.pdf", ObjectStreamMode::Preserve),
        "null-visible-matrix-objstm/linearize-objstm-preserve.pdf",
    );
}

#[test]
fn linearize_generate_real_null_thumb_first_edge_is_byte_identical_to_qpdf() {
    assert_golden(
        &linearize_mode(
            "null-visible-thumb-first-edge.pdf",
            ObjectStreamMode::Generate,
        ),
        "null-visible-thumb-first-edge/linearize-objstm.pdf",
    );
}

#[test]
fn linearize_preserve_real_null_thumb_first_edge_is_byte_identical_to_qpdf() {
    assert_golden(
        &linearize_mode(
            "null-visible-thumb-first-edge-bearing.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-thumb-first-edge-bearing/linearize-objstm-preserve.pdf",
    );
}

#[test]
fn linearize_generate_stream_data_preserve_structural_streams_match_qpdf() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-thumb-first-edge.pdf",
            ObjectStreamMode::Generate,
            Some(StreamDataMode::Preserve),
        ),
        "null-visible-thumb-first-edge/linearize-objstm-stream-preserve.pdf",
    );
}

#[test]
fn linearize_generate_stream_data_uncompress_structural_streams_match_qpdf() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-thumb-first-edge.pdf",
            ObjectStreamMode::Generate,
            Some(StreamDataMode::Uncompress),
        ),
        "null-visible-thumb-first-edge/linearize-objstm-stream-uncompress.pdf",
    );
}

#[test]
fn linearize_generate_stream_data_compress_structural_streams_match_qpdf() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-thumb-first-edge.pdf",
            ObjectStreamMode::Generate,
            Some(StreamDataMode::Compress),
        ),
        "null-visible-thumb-first-edge/linearize-objstm.pdf",
    );
}

#[test]
fn linearize_preserve_stream_data_preserve_structural_streams_match_qpdf() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-thumb-first-edge-bearing.pdf",
            ObjectStreamMode::Preserve,
            Some(StreamDataMode::Preserve),
        ),
        "null-visible-thumb-first-edge-bearing/linearize-objstm-stream-preserve.pdf",
    );
}

#[test]
fn linearize_preserve_stream_data_uncompress_structural_streams_match_qpdf() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-thumb-first-edge-bearing.pdf",
            ObjectStreamMode::Preserve,
            Some(StreamDataMode::Uncompress),
        ),
        "null-visible-thumb-first-edge-bearing/linearize-objstm-stream-uncompress.pdf",
    );
}

#[test]
fn linearize_preserve_stream_data_compress_structural_streams_match_qpdf() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-thumb-first-edge-bearing.pdf",
            ObjectStreamMode::Preserve,
            Some(StreamDataMode::Compress),
        ),
        "null-visible-thumb-first-edge-bearing/linearize-objstm-preserve.pdf",
    );
}

#[test]
fn linearize_generate_stale_generation_inlines_null_without_body() {
    assert_golden(
        &linearize_mode(
            "null-visible-stale-generation.pdf",
            ObjectStreamMode::Generate,
        ),
        "null-visible-stale-generation/linearize-objstm.pdf",
    );
}

#[test]
fn linearize_generate_handles_replaced_objstm_source() {
    let source = include_bytes!("../../../tests/fixtures/compat/null-visible-stale-generation.pdf");
    let mut pdf = Pdf::open(Cursor::new(source.as_slice())).expect("fixture must open");
    pdf.replace_object(ObjectRef::new(4, 0), ObjectHandle::dictionary(Vec::new()))
        .expect("replacement object must be accepted");

    let settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Generate,
        deterministic_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut pdf, &settings)
        .expect("stale live generation must be directized before duplicate-generation rejection");
}

#[test]
fn linearize_standard_modes_reject_multiple_live_generations_like_qpdf() {
    const QPDF_ERROR: &str = "cannot currently linearize files that contain multiple objects \
        with the same object ID and different generations";
    for mode in [ObjectStreamMode::Disable, ObjectStreamMode::Preserve] {
        let error = linearize_mode_result("null-visible-stale-generation.pdf", mode, None)
            .expect_err("standard qpdf linearization must reject duplicate generations");
        assert!(
            error.to_string().contains(QPDF_ERROR),
            "{mode:?}: unexpected error: {error}"
        );
    }
}

#[test]
fn linearize_preserve_source_objstm_removes_only_stale_generation() {
    assert_golden(
        &linearize_mode(
            "null-visible-stale-generation-objstm.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-stale-generation-objstm/linearize-objstm-preserve.pdf",
    );
}

#[test]
fn linearize_encrypted_recovered_length_matches_qpdf() {
    for (mode, golden) in [
        (
            ObjectStreamMode::Disable,
            "encrypted-recovered-eol/linearize-disable.pdf",
        ),
        (
            ObjectStreamMode::Generate,
            "encrypted-recovered-eol/linearize-objstm.pdf",
        ),
    ] {
        let actual = linearize_encrypted_mode("encrypted-recovered-eol.pdf", mode);
        assert_golden(&actual, golden);
    }
}

#[test]
fn linearize_preserve_stream_data_directizes_null_length() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-visible-stream-null-length.pdf",
            ObjectStreamMode::Disable,
            Some(StreamDataMode::Preserve),
        ),
        "null-visible-stream-null-length/linearize-preserve.pdf",
    );
}

#[test]
fn linearize_preserve_restores_exact_null_length_framing() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-length-framing-matrix.pdf",
            ObjectStreamMode::Disable,
            Some(StreamDataMode::Preserve),
        ),
        "null-length-framing-matrix/linearize-preserve.pdf",
    );
}

#[test]
fn linearize_uncompress_restores_exact_null_length_framing() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-length-framing-matrix.pdf",
            ObjectStreamMode::Disable,
            Some(StreamDataMode::Uncompress),
        ),
        "null-length-framing-matrix/linearize-uncompress.pdf",
    );
}

#[test]
fn linearize_compress_restores_exact_null_length_framing() {
    assert_golden(
        &linearize_mode_with_stream_data(
            "null-length-framing-matrix.pdf",
            ObjectStreamMode::Disable,
            Some(StreamDataMode::Compress),
        ),
        "null-length-framing-matrix/linearize-compress.pdf",
    );
}

#[test]
fn plain_rewrite_restores_all_length_fallback_framing() {
    for (mode, name) in [
        (StreamDataMode::Preserve, "preserve"),
        (StreamDataMode::Uncompress, "uncompress"),
        (StreamDataMode::Compress, "compress"),
    ] {
        assert_golden(
            &rewrite_mode_with_policy(
                "null-length-framing-matrix.pdf",
                ObjectStreamMode::Disable,
                Some(mode),
                CompressStreams::Yes,
            ),
            &format!("null-length-framing-matrix/plain-{name}.pdf"),
        );
    }
}

#[test]
fn generate_restores_all_length_fallback_framing() {
    for (mode, name) in [
        (StreamDataMode::Preserve, "preserve"),
        (StreamDataMode::Uncompress, "uncompress"),
        (StreamDataMode::Compress, "compress"),
    ] {
        assert_golden(
            &rewrite_mode_with_policy(
                "null-length-framing-matrix.pdf",
                ObjectStreamMode::Generate,
                Some(mode),
                CompressStreams::Yes,
            ),
            &format!("null-length-framing-matrix/generate-{name}.pdf"),
        );
    }
}

#[test]
fn source_objstm_preserve_restores_all_length_fallback_framing() {
    for (mode, name) in [
        (StreamDataMode::Preserve, "preserve"),
        (StreamDataMode::Uncompress, "uncompress"),
        (StreamDataMode::Compress, "compress"),
    ] {
        assert_golden(
            &rewrite_mode_with_policy(
                "null-length-framing-matrix-objstm.pdf",
                ObjectStreamMode::Preserve,
                Some(mode),
                CompressStreams::Yes,
            ),
            &format!("null-length-framing-matrix-objstm/preserve-{name}.pdf"),
        );
    }
}

#[test]
fn linearized_preserve_uses_resolver_aware_signature_eligibility() {
    assert_golden(
        &linearize_mode(
            "null-visible-preserve-signature.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-signature/linearize-objstm-preserve.pdf",
    );
    assert_golden(
        &linearize_mode(
            "null-visible-preserve-signature-null-fields.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-signature-null-fields/linearize-objstm-preserve.pdf",
    );
}

#[test]
fn disable_null_visibility_cycle_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-cycle.pdf", ObjectStreamMode::Disable),
        "null-visible-cycle/disable.pdf",
    );
}

#[test]
fn preserve_filters_unreachable_sibling_from_source_container() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-mixed.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-mixed/preserve.pdf",
    );
}

#[test]
fn preserve_drops_fully_unreachable_source_container() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-unreachable.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-unreachable/preserve.pdf",
    );
}

#[test]
fn preserve_empty_source_batches_keeps_generation_removals() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-empty-removed.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-empty-removed/preserve.pdf",
    );
}

#[test]
fn preserve_empty_source_batches_keeps_deterministic_id_parity() {
    assert_golden(
        &rewrite_mode_with_policy_and_id(
            "null-visible-preserve-empty-removed.pdf",
            ObjectStreamMode::Preserve,
            None,
            CompressStreams::Yes,
            true,
        ),
        "null-visible-preserve-empty-removed/deterministic-id.pdf",
    );
}

#[test]
fn preserve_keeps_single_source_container_over_100_members() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-over-100.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-over-100/preserve.pdf",
    );
}

#[test]
fn preserve_emits_reachable_signature_dictionary_plain() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-signature.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-signature/preserve.pdf",
    );
}

#[test]
fn preserve_keeps_signatures_with_null_fields_compressed_and_hides_fields() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-signature-null-fields.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-signature-null-fields/preserve.pdf",
    );
}

#[test]
fn preserve_empty_qpdf_plan_does_not_repack_signature() {
    let fixture =
        single_member_objstm_fixture(b"<< /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >>", "");
    let output = preserve_fixture(&fixture, |_| {}).unwrap();
    assert!(
        output.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n")
            && output
                .windows(b"\ntrailer ".len())
                .any(|w| w == b"\ntrailer "),
        "an empty Preserve plan must fall back to a classic xref table and trailer"
    );
    assert!(
        !output
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "an empty Preserve plan must not emit an xref stream"
    );
    let mut reopened = Pdf::open(Cursor::new(output.clone())).unwrap();

    assert!(
        reopened.object_refs().into_iter().all(|object_ref| {
            let object = reopened.resolve_canonical_object(object_ref).unwrap();
            !object.as_stream_dict().is_some_and(|stream_dict| {
                stream_dict
                    .try_get_key(b"/Type")
                    .ok()
                    .and_then(|type_name| type_name.as_name())
                    .as_deref()
                    == Some(b"ObjStm".as_slice())
            })
        }),
        "an empty qpdf Preserve plan is authoritative; the writer must not repack /Sig"
    );
    assert!(
        reopened.object_refs().into_iter().any(|object_ref| {
            let object = reopened.resolve_canonical_object(object_ref).unwrap();
            object
                .try_get_key(b"/Type")
                .ok()
                .and_then(|type_name| type_name.as_name())
                .as_deref()
                == Some(b"Sig".as_slice())
        }),
        "the reachable signature dictionary must be emitted as a plain object"
    );
}

#[test]
fn preserve_fast_path_uses_static_id_when_both_id_modes_are_set() {
    let fixture = single_member_objstm_fixture(b"<< /Kind /Ordinary >>", "");
    let both = preserve_fixture(&fixture, |options| options.deterministic_id = true).unwrap();
    let static_only = preserve_fixture(&fixture, |_| {}).unwrap();
    assert_eq!(
        both, static_only,
        "the Preserve fast path must use static ID precedence"
    );
}

#[test]
fn preserve_empty_qpdf_plan_supports_deterministic_id() {
    let fixture =
        single_member_objstm_fixture(b"<< /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >>", "");
    let write = || {
        preserve_fixture(&fixture, |options| {
            options.static_id = false;
            options.deterministic_id = true;
        })
        .unwrap()
    };
    let first = write();
    let second = write();

    assert_eq!(
        first, second,
        "empty-plan Preserve output must honor deterministic ID"
    );
    assert!(
        first.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"),
        "deterministic ID must retain the empty-plan classic xref form"
    );
}

#[test]
fn preserve_fast_path_retains_direct_trailer_extras() {
    let fixture = single_member_objstm_fixture(
        b"<< /Kind /Ordinary >>",
        "/Foo << /Held 5 0 R >> /Info << /Producer (direct-info) >> ",
    );
    let output = preserve_fixture(&fixture, |_| {}).unwrap();
    let mut reopened = Pdf::open(Cursor::new(output.clone())).unwrap();

    let trailer = reopened.trailer();
    let foo = trailer
        .try_get_key(b"/Foo")
        .expect("the direct /Foo trailer dictionary must survive");
    let held = foo
        .try_get_key(b"/Held")
        .expect("direct /Foo must retain /Held")
        .object_ref()
        .expect("direct /Foo /Held must remain an indirect reference");
    assert_ne!(
        held,
        ObjectRef::new(5, 0),
        "nested trailer refs must be rewritten from their source number"
    );
    assert!(
        {
            let object = reopened.resolve_canonical_object(held).unwrap();
            object.try_get_key(b"/Kind").unwrap().as_name().as_deref()
                == Some(b"Ordinary".as_slice())
        },
        "the remapped /Foo /Held reference must resolve to the original member object"
    );
    assert!(
        trailer
            .try_get_key(b"/Info")
            .unwrap()
            .try_get_key(b"/Producer")
            .unwrap()
            .as_string()
            .as_deref()
            == Some(b"direct-info".as_slice()),
        "a direct /Info trailer dictionary must survive"
    );

    let position = |token: &[u8]| {
        output
            .windows(token.len())
            .position(|window| window == token)
            .unwrap_or_else(|| panic!("missing trailer token {:?}", String::from_utf8_lossy(token)))
    };
    let positions = [
        position(b" /Foo "),
        position(b" /Info "),
        position(b" /Root "),
        position(b" /Size "),
        position(b" /ID "),
    ];
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "trimmed trailer entries and generated keys must follow qpdf writeTrailer order"
    );
}

#[test]
fn preserve_explicit_structural_null_replacement_keeps_source_container_over_100_members() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/null-visible-preserve-over-100.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();

    // The source xref stream is structural and unreachable from the document
    // graph, so replacing it with qpdf's public null value is behavior-neutral.
    // The shared Preserve pipeline still keeps the 104 reachable source ObjStm
    // members.
    pdf.replace_object(ObjectRef::new(107, 0), ObjectHandle::null())
        .expect("replace the structural xref stream with null");

    let settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Preserve,
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &settings).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let mut member_counts = Vec::new();
    for object_ref in reopened.object_refs() {
        let object = reopened.resolve_canonical_object(object_ref).unwrap();
        if let Some(stream_dict) = object.as_stream_dict() {
            if stream_dict
                .try_get_key(b"/Type")
                .unwrap()
                .as_name()
                .as_deref()
                == Some(b"ObjStm".as_slice())
            {
                let count = stream_dict
                    .try_get_key(b"/N")
                    .unwrap()
                    .as_integer()
                    .expect("ObjStm must carry an integer /N");
                member_counts.push(count);
            }
        }
    }
    member_counts.sort_unstable();
    assert_eq!(
        member_counts,
        vec![104],
        "Preserve must retain the source container without Generate's 100-member cap"
    );
}
