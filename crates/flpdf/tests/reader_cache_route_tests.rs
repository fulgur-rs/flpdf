//! Route guards for the qpdf-shaped reader/cache cutover.

#[test]
fn resolved_cache_entries_are_handle_native() {
    let source = include_str!("../src/cache.rs");
    assert!(source.contains("Resolved(ObjectHandle)"));
    assert!(!source.contains("Resolved(Object)"));
}

#[test]
fn canonical_replacement_does_not_round_trip_through_raw_materialization() {
    let source = include_str!("../src/reader.rs");
    let replace_object = source
        .split("pub fn replace_object(")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn remove_object_handle").next())
        .expect("canonical replace_object exists");

    assert!(!replace_object.contains("promote_resolved_object_stream_members"));
    assert!(!replace_object.contains("materialize"));
    assert!(!replace_object.contains("lift_for_set_object"));
}

#[test]
fn json_inspection_keeps_top_level_resolution_handle_native() {
    let source = include_str!("../src/json_inspect.rs");
    let resolver = source
        .split("pub(crate) fn qpdf_resolve_top_level_object")
        .nth(1)
        .and_then(|tail| tail.split("pub fn qpdf_raw_stream_payload").next())
        .expect("qpdf JSON top-level resolver exists");

    assert!(resolver.contains("ObjectHandle"));
    assert!(resolver.contains("resolve_qpdf_json_handle"));
    assert!(!resolver.contains("resolve_qpdf_json_object"));
    assert!(!resolver.contains("lift_object_to_handle"));
    assert!(!resolver.contains("materialize"));
}

#[test]
fn json_stream_payload_uses_the_resolved_handle_directly() {
    let source = include_str!("../src/json_inspect.rs");
    let route = source
        .split("pub fn qpdf_raw_stream_payload")
        .nth(1)
        .and_then(|tail| tail.split("/// Convert a PDF object handle").next())
        .expect("qpdf raw stream payload route exists");

    assert!(route.contains("stream_payload_with_decode_status"));
    assert!(!route.contains("lift_object_to_handle"));
    assert!(!route.contains("materialize"));
}

#[test]
fn encryption_dictionary_reader_route_uses_handle_accessors() {
    let source = include_str!("../src/reader.rs");
    let route = source
        .split("fn encrypt_dictionary_handle")
        .nth(1)
        .and_then(|tail| tail.split("/// Snapshot `/Encrypt`").next())
        .expect("encryption dictionary route exists");

    assert!(route.contains("try_as_dictionary"));
    assert!(!route.contains("materialize"));
}

#[test]
fn encryption_state_production_route_uses_handle_values() {
    let source = include_str!("../src/encryption/state.rs");
    let route = source
        .split("pub(crate) fn parse_inspection_state")
        .nth(1)
        .and_then(|tail| {
            tail.split("#[cfg(test)]\npub(crate) fn first_file_id")
                .next()
        })
        .expect("encryption state production route exists");

    assert!(route.contains("&ObjectHandle"));
    assert!(!route.contains("&Dictionary"));
    assert!(!route.contains("Object::"));
    assert!(!route.contains("materialize"));

    for (start, end) in [
        (
            "fn standard_handler_inputs_from_handle",
            "fn standard_handler_r5_inputs_from_handle",
        ),
        (
            "fn standard_handler_r5_inputs_from_handle",
            "fn map_uo_length_to_bad_password",
        ),
        (
            "fn required_integer_from_handle",
            "/// qpdf's `/ID[0]` value",
        ),
        (
            "fn r6_perms_warning_from_handle",
            "/// qpdf's `/ID[0]` value",
        ),
    ] {
        let function = source
            .split(start)
            .nth(1)
            .and_then(|tail| tail.split(end).next())
            .expect("handle-native encryption helper exists");
        assert!(!function.contains("Object::"));
        assert!(!function.contains("materialize"));
    }
}
