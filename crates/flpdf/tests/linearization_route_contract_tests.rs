//! Architectural guards for the qpdf-shaped linearization writer route.
//!
//! These assertions are migration contracts, not a replacement for the qpdf
//! differential tests.  The latter prove observable output; this target proves
//! that the migrated QPDFWriter body/stream responsibility is actually routed
//! through the live ObjectHandle writer boundary instead of a legacy bridge.

const WRITER_SOURCE: &str = include_str!("../src/linearization/writer.rs");
const RENUMBER_SOURCE: &str = include_str!("../src/writer/rewrite_renumber.rs");

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

fn catalog_resolution_production_slices() -> (&'static str, &'static str) {
    let outline_start = WRITER_SOURCE
        .find("fn compute_outline_hint_info")
        .expect("outline-hint resolver marker must remain present");
    let outline_end = WRITER_SOURCE
        .find("fn build_outline_hint_table")
        .expect("outline-hint table marker must remain present");
    let adbe_start = WRITER_SOURCE
        .find("fn resolve_catalog_adbe_status")
        .expect("Catalog ADBE resolver marker must remain present");
    let adbe_end = WRITER_SOURCE
        .find("/// Write a complete linearized PDF")
        .expect("linearized writer marker must remain present");
    (
        &WRITER_SOURCE[outline_start..outline_end],
        &WRITER_SOURCE[adbe_start..adbe_end],
    )
}

fn reachable_production_slice() -> &'static str {
    let start_marker = "pub(crate) fn reachable_object_set_with_stream_parameters";
    let end_marker = "/// Indirect references that qpdf";
    let start = RENUMBER_SOURCE
        .find(start_marker)
        .expect("reachable-object marker must remain present");
    let end = RENUMBER_SOURCE
        .find(end_marker)
        .expect("null-resurrection marker must remain present");
    &RENUMBER_SOURCE[start..end]
}

fn resurrectable_production_slice() -> &'static str {
    let start_marker = "pub(crate) fn resurrectable_null_refs_excluding";
    let end_marker = "/// Drop-aware handle walk for";
    let start = RENUMBER_SOURCE
        .find(start_marker)
        .expect("resurrectable marker must remain present");
    let end = RENUMBER_SOURCE
        .find(end_marker)
        .expect("resurrectable walk marker must remain present");
    &RENUMBER_SOURCE[start..end]
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
        "pdf.resolve_object(orig)",
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

#[test]
fn linearization_catalog_resolution_uses_live_handles() {
    let (outline, adbe) = catalog_resolution_production_slices();
    assert!(
        outline.contains("get_object_handle"),
        "outline hint resolution must start from Pdf's canonical handle registry"
    );
    assert!(
        outline.contains("try_get_key"),
        "outline hint resolution must use the qpdf-shaped key accessor"
    );
    assert!(
        adbe.contains("get_object_handle"),
        "ADBE resolution must start from Pdf's canonical handle registry"
    );
    assert!(
        adbe.contains("try_get_key"),
        "ADBE resolution must use the qpdf-shaped key accessor"
    );
    assert!(
        adbe.contains("try_as_dictionary"),
        "ADBE resolution must use typed dictionary inspection"
    );
    for (name, production) in [("outline", outline), ("ADBE", adbe)] {
        for forbidden in [
            "resolve_borrowed",
            "Object::Dictionary",
            "Object::Reference",
            ".as_dict()",
        ] {
            assert!(
                !production.contains(forbidden),
                "{name} Catalog resolution still contains raw token {forbidden:?}"
            );
        }
    }
}

#[test]
fn linearization_reachability_route_uses_live_handles() {
    let production = reachable_production_slice();
    assert!(
        production.contains("get_object_handle"),
        "linearization reachability must resolve objects through the canonical handle registry"
    );
    for forbidden in [
        "resolve_object(",
        "Object::",
        "qpdf_null::snapshot_entries",
        "collect_qpdf_enqueue_refs",
    ] {
        assert!(
            !production.contains(forbidden),
            "linearization reachability route still contains raw token {forbidden:?}"
        );
    }
}

#[test]
fn linearization_resurrectable_route_uses_live_handles() {
    let production = resurrectable_production_slice();
    assert!(
        production.contains("get_object_handle"),
        "resurrectable null references must use the canonical handle registry"
    );
    for forbidden in [
        "resolve_object(",
        "Object::",
        "qpdf_null::value_is_null",
        "walk_surviving",
    ] {
        assert!(
            !production.contains(forbidden),
            "resurrectable route still contains raw token {forbidden:?}"
        );
    }
}
