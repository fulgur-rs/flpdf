#[test]
fn tokenizer_helper_uses_resolving_dictionary_key_accessors() {
    let source = include_str!("../src/tokenizer_runner.rs").replace("\r\n", "\n");

    fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start = source.find(start).expect("source section start");
        let end = source[start..]
            .find(end)
            .map(|offset| start + offset)
            .expect("source section end");
        &source[start..end]
    }

    let page_contents = section(
        &source,
        "fn canonical_page_content_bytes",
        "fn collect_canonical_content_streams",
    );
    assert!(
        page_contents.contains("page.try_get_key(b\"/Contents\")?")
            && !page_contents.contains(".get_key("),
        "tokenizer page content lookup must use qpdf's resolving getKey route"
    );

    let object_stream_type = section(&source, "fn resolve_objstm_type(", "fn dump_tokens(");
    assert!(
        object_stream_type.contains("dict.try_get_key(b\"/Type\")")
            && !object_stream_type.contains(".get_key("),
        "tokenizer object-stream type lookup must use qpdf's resolving getKey route"
    );
}
