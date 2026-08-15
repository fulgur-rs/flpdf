//! Architectural guards for the qpdf-shaped linearization writer route.
//!
//! These assertions are migration contracts, not a replacement for the qpdf
//! differential tests.  The latter prove observable output; this target proves
//! that the migrated QPDFWriter body/stream responsibility is actually routed
//! through the live ObjectHandle writer boundary instead of a legacy bridge.

const WRITER_SOURCE: &str = include_str!("../src/linearization/writer.rs");

fn body_and_stream_production_slice() -> &'static str {
    let start_marker = "fn append_object";
    let end_marker = "fn write_part1_xref_and_trailer";
    let start = WRITER_SOURCE
        .find(start_marker)
        .expect("linearization writer body marker must remain present");
    let end = WRITER_SOURCE
        .find(end_marker)
        .expect("linearization writer public result marker must remain present");
    &WRITER_SOURCE[start..end]
}

fn objstm_production_slice() -> &'static str {
    let start_marker = "fn append_objstm_container_object";
    // Match the section label without depending on the checkout's line ending
    // convention (Windows may materialize this source as CRLF).
    let end_marker = "// Public result types";
    let start = WRITER_SOURCE
        .find(start_marker)
        .expect("linearization ObjStm marker must remain present");
    let end = WRITER_SOURCE
        .find(end_marker)
        .expect("linearization public result marker must remain present");
    &WRITER_SOURCE[start..end]
}

fn hint_production_slice() -> &'static str {
    let start_marker = "fn append_hint_stream_object";
    let end_marker = "struct OutlineHintInfo";
    let start = WRITER_SOURCE
        .find(start_marker)
        .expect("linearization hint-stream marker must remain present");
    let end = WRITER_SOURCE
        .find(end_marker)
        .expect("linearization outline-hint marker must remain present");
    &WRITER_SOURCE[start..end]
}

#[test]
fn linearization_body_stream_route_has_no_legacy_object_bridge() {
    let production = body_and_stream_production_slice();
    for forbidden in [
        "Object::",
        "resolve_borrowed",
        "decode_stream_data",
        "encode_stream_data",
    ] {
        assert!(
            !production.contains(forbidden),
            "canonical linearization body/stream route still contains legacy token {forbidden:?}"
        );
    }
}

#[test]
fn linearization_objstm_route_uses_live_handles() {
    let production = objstm_production_slice();
    assert!(
        production.contains("emit_objstm_body_from_handles_with_writer"),
        "ObjStm members must be emitted through the canonical ObjectHandle writer"
    );
    assert!(
        production.contains("get_object_handle"),
        "ObjStm members must be resolved from Pdf's canonical handle registry"
    );
    for forbidden in [
        "Vec<(ObjectRef, Object)>",
        "pdf.resolve(orig)",
        "emit_objstm_body_from_resolved",
        "renumber_object_with_removed",
    ] {
        assert!(
            !production.contains(forbidden),
            "canonical linearization ObjStm route still contains legacy token {forbidden:?}"
        );
    }
}

#[test]
fn linearization_hint_route_uses_canonical_pipeline() {
    let production = hint_production_slice();
    assert!(
        production.contains("write_stream_payload_with_pipeline"),
        "hint payloads must use the canonical writer pipeline"
    );
    for forbidden in [
        "encrypt_stream_payload_for_writer",
        "encrypt_stream_payload_with_iv",
        "crate::Stream::new",
    ] {
        assert!(
            !production.contains(forbidden),
            "canonical linearization hint route still contains legacy token {forbidden:?}"
        );
    }
}
