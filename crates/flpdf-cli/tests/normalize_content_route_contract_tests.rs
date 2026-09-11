fn apply_normalize_content_source() -> String {
    let source = include_str!("../src/main.rs").replace("\r\n", "\n");
    let start = source
        .find("fn apply_normalize_content")
        .expect("apply_normalize_content must exist");
    let body = &source[start..];
    let end = body
        .find("\nfn normalize_and_store_stream_handle")
        .expect("normalize stream helper must follow apply_normalize_content");
    body[..end].to_owned()
}

#[test]
fn normalize_content_uses_canonical_handle_accessors() {
    let source = apply_normalize_content_source();
    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
    ] {
        assert!(
            !source.contains(forbidden),
            "apply_normalize_content retains non-canonical route {forbidden}"
        );
    }
    assert!(source.contains(".try_get_key("));
    assert!(source.contains(".try_is_stream_of_type(b\"\", b\"\")?"));
    let array_guard = source
        .find(".try_is_array()?")
        .expect("normalize-content must resolve the Contents array predicate");
    let array_view = source
        .find(".as_array()")
        .expect("normalize-content must retain a child view after the array predicate");
    assert!(
        array_guard < array_view,
        "the non-resolving array view must follow the resolving array predicate"
    );
}
