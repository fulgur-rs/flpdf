fn normalize_source(source: &str) -> String {
    source.replace("\r\n", "\n")
}

#[test]
fn qpdf_less_page_label_display_renderer_is_not_public() {
    let helper = normalize_source(include_str!("../src/page_label_document_helper.rs"));
    let crate_root = normalize_source(include_str!("../src/lib.rs"));

    assert!(!helper.contains("pub fn format(&self, value: i64)"));
    assert!(!helper.contains("pub fn label_string_for_page"));
    assert!(!helper.contains("MAX_RENDERABLE_LABEL_VALUE"));
    assert!(!helper.contains("fn to_roman"));
    assert!(!helper.contains("fn to_alpha"));
    assert!(!helper.contains("qpdf-deviation-start: page-label rendering"));
    assert!(!crate_root.contains("label_string_for_page"));

    for raw_api in [
        "pub fn get_label_for_page",
        "pub fn get_labels_for_page_range",
        "pub fn page_label_dict",
    ] {
        assert!(
            helper.contains(raw_api),
            "raw page-label API must remain: {raw_api}"
        );
    }
}
