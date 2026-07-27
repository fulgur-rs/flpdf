use flpdf::json::{Json, SchemaFlags};

fn parsed(input: &[u8]) -> Json {
    Json::parse(input).unwrap()
}

fn error_bytes(errors: &[flpdf::json::JsonMessage]) -> Vec<&[u8]> {
    errors.iter().map(|error| error.as_bytes()).collect()
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
        errors.last().unwrap().as_bytes(),
        b"top-level object: key \"x\" is not present in schema but appears in object"
    );
}

#[test]
fn pattern_key_validates_every_dictionary_value() {
    let schema = parsed(br#"{"<objid>":{"n":"number"}}"#);
    let value = parsed(br#"{"one":{"a":1},"two":{"x":2}}"#);
    let mut errors = Vec::new();
    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(
        error_bytes(&errors),
        [
            b"json key \".one\": key \"n\" is present in schema but missing in object".as_slice(),
            b"json key \".one\": key \"a\" is not present in schema but appears in object"
                .as_slice(),
            b"json key \".two\": key \"n\" is present in schema but missing in object".as_slice(),
            b"json key \".two\": key \"x\" is not present in schema but appears in object"
                .as_slice(),
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
        error_bytes(&errors),
        [
            b"json key \".a\": key \"n\" is present in schema but missing in object".as_slice(),
            b"json key \".a\": key \"x\" is not present in schema but appears in object".as_slice(),
            b"top-level object: key \"b\" is present in schema but missing in object".as_slice(),
            b"top-level object: key \"extra\" is not present in schema but appears in object"
                .as_slice(),
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
        error_bytes(&errors),
        [
            b"json key \".1\": key \"n\" is present in schema but missing in object".as_slice(),
            b"json key \".1\": key \"x\" is not present in schema but appears in object".as_slice(),
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
        error_bytes(&errors),
        [b"top-level object is supposed to be an array of length 2".as_slice()]
    );

    let same_length_schema = parsed(br#"[{"a":"value"},{"b":"value"}]"#);
    let same_length_value = parsed(br#"[{"a":1},{"x":2}]"#);
    let mut errors = Vec::new();
    assert!(!same_length_value.check_schema(&same_length_schema, &mut errors));
    assert_eq!(
        error_bytes(&errors),
        [
            b"json key \".1\": key \"b\" is present in schema but missing in object".as_slice(),
            b"json key \".1\": key \"x\" is not present in schema but appears in object".as_slice(),
        ]
    );

    let invalid_schema = parsed(b"true");
    let value = parsed(b"null");
    let mut errors = Vec::new();
    assert!(!value.check_schema(&invalid_schema, &mut errors));
    assert_eq!(
        error_bytes(&errors),
        [b"top-level object schema value is not dictionary, array, or string".as_slice()]
    );
}

#[test]
fn dictionary_schema_rejects_a_non_dictionary_checked_value() {
    let schema = parsed(br#"{"a":"value"}"#);
    let value = parsed(br#"[]"#);
    let mut errors = Vec::new();

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(
        error_bytes(&errors),
        [b"top-level object is supposed to be a dictionary".as_slice()]
    );
}

#[test]
fn schema_strings_are_wildcards_and_prior_errors_affect_return_value() {
    let schema = parsed(br#""description""#);
    let value = parsed(br#"{"any":[true,2]}"#);
    let mut errors = vec![flpdf::json::JsonMessage::from("previous error")];

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(error_bytes(&errors), [b"previous error".as_slice()]);
}

#[test]
fn uninitialized_checked_handle_fails_without_adding_an_error() {
    let mut errors = Vec::new();

    assert!(!Json::default().check_schema(&parsed(br#""value""#), &mut errors));
    assert!(errors.is_empty());
}

#[test]
fn fixed_length_array_accepts_each_matching_position() {
    let schema = parsed(br#"["first","second"]"#);
    let value = parsed(br#"[1,true]"#);
    let mut errors = Vec::new();

    assert!(value.check_schema(&schema, &mut errors));
    assert!(errors.is_empty());
}

#[test]
fn schema_errors_preserve_non_utf8_keys_in_paths_and_messages() {
    let schema = Json::make_dictionary();
    schema
        .add_dictionary_member(b"\xff", Json::make_string(b"value"))
        .unwrap();
    let value = Json::make_dictionary();
    let mut errors = Vec::new();

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(
        errors[0].as_bytes(),
        b"top-level object: key \"\xff\" is present in schema but missing in object"
    );

    let pattern = Json::make_dictionary();
    pattern.add_dictionary_member(b"<item>", schema).unwrap();
    let nested = Json::make_dictionary();
    nested
        .add_dictionary_member(b"\x80", Json::make_dictionary())
        .unwrap();
    let mut errors = Vec::new();
    assert!(!nested.check_schema(&pattern, &mut errors));
    assert_eq!(
        errors[0].as_bytes(),
        b"json key \".\x80\": key \"\xff\" is present in schema but missing in object"
    );
}
