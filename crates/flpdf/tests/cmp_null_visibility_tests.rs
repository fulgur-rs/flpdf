//! Byte-identity coverage for qpdf's null-aware dictionary visibility.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{
    write_pdf_with_options, CompressStreams, NewlineBeforeEndstream, Object, ObjectRef,
    ObjectStreamMode, Pdf, PdfOpenOptions, StreamDataMode, WriteOptions,
};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;

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
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = mode;
    options.stream_data = stream_data;
    options.compress_streams = compress_streams;
    options.static_id = !deterministic_id;
    options.deterministic_id = deterministic_id;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();
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
    let plan = LinearizationPlan::from_pdf_with_object_stream_mode(&mut pdf, mode)?;
    let renumber = RenumberMap::from_plan(&plan);

    let mut pdf = Pdf::open(BufReader::new(File::open(&path).unwrap()))?;
    let mut options = WriteOptions::default();
    options.object_streams = mode;
    options.stream_data = stream_data;
    options.deterministic_id = true;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut document = write_linearized(&plan, &renumber, &mut pdf, &options)?;
    document.back_patch()?;
    Ok(document.bytes)
}

fn linearize_encrypted_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let open_options = PdfOpenOptions::default();
    let mut pdf = Pdf::open_with_options(
        BufReader::new(File::open(&path).unwrap()),
        open_options.clone(),
    )
    .expect("encrypted source must authenticate");
    let plan = LinearizationPlan::from_pdf_with_object_stream_mode(&mut pdf, mode).expect("plan");
    let renumber = RenumberMap::from_plan(&plan);

    let mut pdf = Pdf::open_with_options(BufReader::new(File::open(&path).unwrap()), open_options)
        .expect("encrypted source must authenticate");
    let mut options = WriteOptions::default();
    options.object_streams = mode;
    options.stream_data = Some(StreamDataMode::Preserve);
    options.deterministic_id = true;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut document =
        write_linearized(&plan, &renumber, &mut pdf, &options).expect("linearized write");
    document.back_patch().expect("linearized back-patch");
    document.bytes
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
fn linearize_encrypted_recovered_eol_is_not_appended_to_plaintext() {
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
fn legacy_preserve_fallback_splits_source_container_over_100_members() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/null-visible-preserve-over-100.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();

    // The source xref stream is structural and unreachable from the document
    // graph, so deleting it is a behavior-neutral routing sentinel. It makes
    // `deleted_object_refs` non-empty, bypassing the dedicated plain qpdf
    // Preserve writer and exercising the shared legacy planner without
    // changing the 104 reachable source ObjStm members.
    pdf.delete_object(ObjectRef::new(107, 0));

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Preserve;
    options.static_id = true;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let mut member_counts = Vec::new();
    for object_ref in reopened.object_refs() {
        if let Object::Stream(stream) = reopened.resolve(object_ref).unwrap() {
            if matches!(
                stream.dict.get("Type"),
                Some(Object::Name(name)) if name.as_slice() == b"ObjStm"
            ) {
                let Some(Object::Integer(count)) = stream.dict.get("N") else {
                    panic!("ObjStm must carry an integer /N");
                };
                member_counts.push(*count);
            }
        }
    }
    member_counts.sort_unstable();
    assert_eq!(
        member_counts,
        vec![4, 100],
        "legacy Preserve must retain its pre-Task-2 100-member cap"
    );
}
