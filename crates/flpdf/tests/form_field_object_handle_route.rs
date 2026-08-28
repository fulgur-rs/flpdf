use std::fs;

#[test]
fn form_field_tests_use_canonical_object_handle_routes() {
    let path = format!(
        "{}/tests/form_field_object_helper_tests.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    let source = fs::read_to_string(path).expect("form-field test source");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "resolve_to_cache(",
        "materialize(",
        "set_object(",
        "Object::",
        "Dictionary::",
        ".as_dict(",
        ".into_dict(",
    ] {
        assert!(
            !source.contains(forbidden),
            "form-field tests retain legacy route marker: {forbidden}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(",
        "get_key(",
        "as_dictionary(",
    ] {
        assert!(
            source.contains(required),
            "form-field tests must use canonical route marker: {required}"
        );
    }
}
