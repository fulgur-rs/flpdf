//! Route contracts for the page/helper/job/JSON consumer cutover.

fn function_body<'a>(source: &'a str, signature: &str, end: &str) -> &'a str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once(end))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("route {signature:?} must exist"))
}

#[test]
fn default_appearance_tf_rewrite_uses_handle_callbacks() {
    let source = include_str!("../src/form_field_object_helper/rendering.rs").replace("\r\n", "\n");
    let route = function_body(
        &source,
        "fn substitute_da_tf_operand(",
        "/// Reproduce qpdf 11.9.0's `ValueSetter::writeAppearance`",
    );

    assert!(route.contains("ObjectHandleParserCallbacks"));
    assert!(route.contains("parse_content_stream_handles"));
    assert!(!route.contains("impl ParserCallbacks"));
    assert!(!route.contains("parse_content_stream_data"));
    assert!(!route.contains("Object::"));
}

#[test]
fn overlay_contents_rewrite_uses_the_document_stream_factory() {
    let source = include_str!("../src/job/overlay.rs").replace("\r\n", "\n");
    let route = function_body(
        &source,
        "fn apply_overlays_to_page_with_sources",
        "#[cfg(test)]\nfn apply_overlays_to_page",
    );

    assert!(route.contains("new_stream_with_data"));
    assert!(!route.contains("set_object("));
    assert!(!route.contains("Object::Stream"));
}

#[test]
fn json_attachments_project_through_filespec_helpers() {
    let source = include_str!("../src/job/json_sections.rs").replace("\r\n", "\n");
    let attachments = function_body(
        &source,
        "pub fn build_attachments_section<",
        "// ── build_encrypt_section",
    );

    assert!(source.contains("FileSpec"));
    assert!(source.contains("EmbeddedFileStream"));
    assert!(attachments.contains("filespec_handle_to_json"));
    assert!(source.contains("get_embedded_file_stream_entries"));
    assert!(!attachments.contains("filespec_dict_to_json"));
}
