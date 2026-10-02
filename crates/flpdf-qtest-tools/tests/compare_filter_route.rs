#[test]
fn compare_filter_detection_uses_resolving_name_and_array_accessors() {
    let source = include_str!("../src/compare.rs").replace("\r\n", "\n");
    let start = source
        .find("fn stream_uses_flatedecode")
        .expect("stream filter helper source start");
    let end = source[start..]
        .find("fn resolve_compare_children<")
        .map(|offset| start + offset)
        .expect("stream filter helper source end");
    let section = &source[start..end];

    assert!(
        section.contains("stream_dict.try_get_key(b\"/Filter\")?")
            && section.contains("try_is_name_and_equals(b\"FlateDecode\")")
            && section.contains("try_is_array()?")
            && section.contains("try_get_array_n_items()?")
            && section.contains("try_get_array_item(")
            && !section.contains("pdf.resolve(")
            && !section.contains(".as_name()")
            && !section.contains(".as_array()"),
        "compare filter detection must use qpdf's resolving predicates and array accessors"
    );
}
