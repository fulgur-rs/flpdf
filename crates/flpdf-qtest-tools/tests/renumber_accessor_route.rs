#[test]
fn test_renumber_object_comparison_uses_resolving_accessors() {
    let source = include_str!("../src/renumber.rs").replace("\r\n", "\n");

    fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start = source.find(start).expect("source section start");
        let end = source[start..]
            .find(end)
            .map(|offset| start + offset)
            .expect("source section end");
        &source[start..end]
    }

    let values = section(&source, "fn compare_objects(", "fn compare_value<");
    assert!(
        values.contains("try_get_bool_value()?")
            && values.contains("try_get_int_value()?")
            && values.contains("try_get_real_value()?")
            && values.contains("try_get_string_value()?")
            && values.contains("try_get_name()?")
            && !values.contains(".as_boolean()")
            && !values.contains(".as_integer()")
            && !values.contains(".as_real()")
            && !values.contains(".as_string()")
            && !values.contains(".as_name()"),
        "test_renumber scalar comparison must use qpdf's resolving getters"
    );

    let arrays = section(&source, "fn compare_arrays(", "fn compare_dictionaries(");
    assert!(
        arrays.contains("try_get_array_n_items()?")
            && arrays.contains("try_get_array_item(")
            && !arrays.contains(".as_array()"),
        "test_renumber array comparison must use qpdf's resolving array accessors"
    );

    let dictionaries = section(
        &source,
        "fn compare_dictionaries(",
        "fn compare_xref_tables(",
    );
    assert!(
        dictionaries.contains("try_get_keys()?")
            && dictionaries.contains("try_get_key(")
            && !dictionaries.contains(".as_dictionary()"),
        "test_renumber dictionary comparison must use qpdf's resolving key accessors"
    );
}
