use flpdf::json::{Json, SchemaFlags};

fn parsed(input: &[u8]) -> Json {
    Json::parse(input).unwrap()
}

#[test]
fn optional_flag_allows_missing_but_not_extra_keys() {
    let schema = parsed(br#"{"a":"value","b":"value"}"#);
    let value = parsed(br#"{"a":1}"#);
    let mut errors = Vec::new();
    assert!(value.check_schema_with_flags(
        &schema,
        SchemaFlags::NONE | SchemaFlags::OPTIONAL,
        &mut errors,
    ));

    let extra = parsed(br#"{"a":1,"x":2}"#);
    assert!(!extra.check_schema_with_flags(&schema, SchemaFlags::OPTIONAL, &mut errors));
    assert_eq!(
        errors.last().unwrap(),
        "top-level object: key \"x\" is not present in schema but appears in object"
    );
}

#[test]
fn pattern_key_validates_every_dictionary_value() {
    let schema = parsed(br#"{"<objid>":{"n":"number"}}"#);
    let value = parsed(br#"{"one":{"a":1},"two":{"x":2}}"#);
    let mut errors = Vec::new();
    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(
        errors,
        [
            "json key \".one\": key \"n\" is present in schema but missing in object",
            "json key \".one\": key \"a\" is not present in schema but appears in object",
            "json key \".two\": key \"n\" is present in schema but missing in object",
            "json key \".two\": key \"x\" is not present in schema but appears in object",
        ]
    );
}

#[test]
fn dictionary_failures_accumulate_with_qpdf_paths() {
    let schema = parsed(br#"{"a":{"n":"number"},"b":"value"}"#);
    let value = parsed(br#"{"a":{"x":1},"extra":2}"#);
    let mut errors = Vec::new();

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(
        errors,
        [
            "json key \".a\": key \"n\" is present in schema but missing in object",
            "json key \".a\": key \"x\" is not present in schema but appears in object",
            "top-level object: key \"b\" is present in schema but missing in object",
            "top-level object: key \"extra\" is not present in schema but appears in object",
        ]
    );
}

#[test]
fn array_schemas_accept_single_item_or_validate_each_array_element() {
    let schema = parsed(br#"[{"n":"number"}]"#);
    let scalar = parsed(br#"{"n":1}"#);
    let array = parsed(br#"[{"n":1},{"x":2}]"#);
    let mut errors = Vec::new();

    assert!(scalar.check_schema(&schema, &mut errors));
    assert!(!array.check_schema(&schema, &mut errors));
    assert_eq!(
        errors,
        [
            "json key \".1\": key \"n\" is present in schema but missing in object",
            "json key \".1\": key \"x\" is not present in schema but appears in object",
        ]
    );
}

#[test]
fn fixed_length_array_reports_length_and_invalid_schema_types() {
    let fixed_schema = parsed(br#"["first","second"]"#);
    let short_value = parsed(br#"[1]"#);
    let mut errors = Vec::new();

    assert!(!short_value.check_schema(&fixed_schema, &mut errors));
    assert_eq!(
        errors,
        ["top-level object is supposed to be an array of length 2"]
    );

    let same_length_schema = parsed(br#"[{"a":"value"},{"b":"value"}]"#);
    let same_length_value = parsed(br#"[{"a":1},{"x":2}]"#);
    let mut errors = Vec::new();
    assert!(!same_length_value.check_schema(&same_length_schema, &mut errors));
    assert_eq!(
        errors,
        [
            "json key \".1\": key \"b\" is present in schema but missing in object",
            "json key \".1\": key \"x\" is not present in schema but appears in object",
        ]
    );

    let invalid_schema = parsed(b"true");
    let value = parsed(b"null");
    let mut errors = Vec::new();
    assert!(!value.check_schema(&invalid_schema, &mut errors));
    assert_eq!(
        errors,
        ["top-level object schema value is not dictionary, array, or string"]
    );
}

#[test]
fn dictionary_schema_rejects_a_non_dictionary_checked_value() {
    let schema = parsed(br#"{"a":"value"}"#);
    let value = parsed(br#"[]"#);
    let mut errors = Vec::new();

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(errors, ["top-level object is supposed to be a dictionary"]);
}

#[test]
fn schema_strings_are_wildcards_and_prior_errors_affect_return_value() {
    let schema = parsed(br#""description""#);
    let value = parsed(br#"{"any":[true,2]}"#);
    let mut errors = vec!["previous error".to_owned()];

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(errors, ["previous error"]);
}
