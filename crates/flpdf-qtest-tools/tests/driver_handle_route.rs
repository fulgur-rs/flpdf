#[test]
fn qtest_handle_serializer_uses_resolving_container_and_null_accessors() {
    let source = include_str!("../src/driver/handle.rs").replace("\r\n", "\n");
    let start = source
        .find("fn write_qpdf_handle_into<")
        .expect("qpdf handle serializer start");
    let end = source[start..]
        .find("#[cfg(test)]")
        .map(|offset| start + offset)
        .expect("qpdf handle serializer end");
    let serializer = &source[start..end];

    assert!(serializer.contains("value.try_is_array()?"));
    assert!(serializer.contains("value.try_get_array_as_vector()?"));
    assert!(serializer.contains("value.try_is_dictionary()?"));
    assert!(serializer.contains("value.try_get_dict_as_map()?"));
    assert!(serializer.contains("child.try_is_null()?"));
    assert!(!serializer.contains(".as_array()"));
    assert!(!serializer.contains(".as_dictionary()"));
    assert!(!serializer.contains(".is_null()"));
}
