#[test]
fn qtest_parser_callbacks_use_the_resolving_abort_name_predicate() {
    let sources = [
        include_str!("../src/driver/test_34_41.rs").replace("\r\n", "\n"),
        include_str!("../src/driver/test_72_79.rs").replace("\r\n", "\n"),
    ];

    fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start = source.find(start).expect("source section start");
        let end = source[start..]
            .find(end)
            .map(|offset| start + offset)
            .expect("source section end");
        &source[start..end]
    }

    for source in sources {
        let callback = section(
            &source,
            "fn handle_object(",
            "fn handle_eof(&mut self) -> flpdf::Result<()>",
        );
        assert!(
            callback.contains("object.try_is_name_and_equals(b\"Abort\")?")
                && !callback.contains(".as_name()"),
            "qtest parser abort detection must use qpdf's resolving name predicate"
        );
    }
}
