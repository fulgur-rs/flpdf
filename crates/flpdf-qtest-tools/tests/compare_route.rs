#[test]
fn compare_xref_stream_type_uses_the_resolving_name_predicate() {
    let source = include_str!("../src/compare.rs").replace("\r\n", "\n");
    let start = source
        .find("fn stream_is_xref(")
        .expect("stream_is_xref source section start");
    let end = source[start..]
        .find("fn stream_uses_flatedecode(")
        .map(|offset| start + offset)
        .expect("stream_is_xref source section end");
    let section = &source[start..end];

    assert!(
        section.contains("stream_dict.try_get_key(b\"/Type\")?")
            && section.contains("try_is_name_and_equals(b\"XRef\")")
            && !section.contains("pdf.resolve(")
            && !section.contains(".as_name()"),
        "XRef stream classification must use qpdf's resolving key and name predicate"
    );
}

#[test]
fn compare_recursive_containers_use_resolving_accessors() {
    let source = include_str!("../src/compare.rs").replace("\r\n", "\n");
    let start = source
        .find("fn resolve_compare_children<")
        .expect("recursive comparison helper start");
    let end = source[start..]
        .find("#[cfg(test)]")
        .map(|offset| start + offset)
        .expect("recursive comparison helper end");
    let helper = &source[start..end];

    assert!(helper.contains("handle.try_is_array()?"));
    assert!(helper.contains("handle.try_get_array_as_vector()?"));
    assert!(helper.contains("handle.try_is_dictionary()?"));
    assert!(helper.contains("handle.try_get_dict_as_map()?"));
    assert!(helper.contains("terminal.try_is_dictionary()?"));
    assert!(helper.contains("terminal.try_is_array()?"));
    assert!(!helper.contains(".as_array()"));
    assert!(!helper.contains(".as_dictionary()"));
}
