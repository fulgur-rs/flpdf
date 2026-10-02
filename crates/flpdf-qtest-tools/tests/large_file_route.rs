#[test]
fn large_file_helper_uses_resolving_dictionary_key_accessors() {
    let source = include_str!("../src/large_file.rs").replace("\r\n", "\n");

    fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start = source.find(start).expect("source section start");
        let end = source[start..]
            .find(end)
            .map(|offset| start + offset)
            .expect("source section end");
        &source[start..end]
    }

    let page_contents = section(&source, "fn check_page_contents(", "fn check_image(");
    assert!(
        page_contents.contains(".try_get_key(b\"/Contents\")?")
            && !page_contents.contains(".get_key("),
        "large-file page content lookup must use qpdf's resolving getKey accessor"
    );

    let image = section(&source, "fn check_image(", "fn check_pdf(");
    assert!(
        image.contains(".try_get_key(b\"/Resources\")?")
            && image.contains("try_get_key(b\"/XObject\")?")
            && image.contains("try_get_key(b\"/Im1\")?")
            && !image.contains(".get_key("),
        "large-file image lookups must use qpdf's resolving getKey accessor"
    );
}
