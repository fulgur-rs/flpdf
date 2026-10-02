#[test]
fn compare_xref_stream_type_uses_the_resolving_name_predicate() {
    let source = include_str!("../src/compare.rs").replace("\r\n", "\n");
    let start = source
        .find("fn stream_is_xref(")
        .expect("stream_is_xref source section start");
    let end = source[start..]
        .find("fn stream_uses_flatedecode<")
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
