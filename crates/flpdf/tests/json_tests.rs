use flpdf::json::Json;

#[test]
fn default_handle_writes_null_but_is_not_initialized_null() {
    let value = Json::default();
    assert_eq!(value.unparse().unwrap(), b"null");
    assert!(!value.is_null());
    assert_eq!(value.start(), 0);
    assert_eq!(value.end(), 0);
}

#[test]
fn encoded_number_is_not_normalized() {
    let value = Json::make_number(b"2.1e5");
    assert_eq!(value.get_number().as_deref(), Some(b"2.1e5".as_slice()));
    assert_eq!(value.unparse().unwrap(), b"2.1e5");
}

#[test]
fn real_special_values_match_qpdf_classic_locale_bytes() {
    assert_eq!(Json::make_real(f64::NAN).unparse().unwrap(), b"nan");
    assert_eq!(
        Json::make_real(f64::from_bits(0xfff8_0000_0000_0000))
            .unparse()
            .unwrap(),
        b"-nan"
    );
    assert_eq!(Json::make_real(f64::INFINITY).unparse().unwrap(), b"inf");
    assert_eq!(
        Json::make_real(f64::NEG_INFINITY).unparse().unwrap(),
        b"-inf"
    );
    assert_eq!(Json::make_real(-0.0).unparse().unwrap(), b"-0");
}

#[test]
fn scalar_accessors_reject_other_types_without_mutating_output() {
    let value = Json::make_bool(true);
    assert_eq!(value.get_bool(), Some(true));
    assert_eq!(value.get_string(), None);
    assert_eq!(value.get_number(), None);
}

#[test]
fn cloned_dictionary_handles_share_mutation_and_sort_encoded_keys() {
    let dictionary = Json::make_dictionary();
    let alias = dictionary.clone();
    dictionary
        .add_dictionary_member(b"b", Json::make_int(2))
        .unwrap();
    alias
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    assert_eq!(
        dictionary.unparse().unwrap(),
        b"{\n  \"a\": 1,\n  \"b\": 2\n}"
    );
}

#[test]
fn uninitialized_children_become_initialized_null() {
    let array = Json::make_array();
    let dictionary = Json::make_dictionary();
    assert!(array.add_array_element(Json::default()).unwrap().is_null());
    assert!(dictionary
        .add_dictionary_member(b"key", Json::default())
        .unwrap()
        .is_null());
}

#[test]
fn parser_key_seen_set_is_separate_from_encoded_members() {
    let dictionary = Json::make_dictionary();
    assert!(!dictionary.check_dictionary_key_seen(b"a\n").unwrap());
    assert!(dictionary.check_dictionary_key_seen(b"a\n").unwrap());
}

#[test]
fn container_accessors_expose_encoded_dictionary_keys_and_array_values() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"line\n", Json::make_int(3))
        .unwrap();
    let array = Json::make_array();
    array.add_array_element(Json::make_bool(true)).unwrap();

    assert!(dictionary.is_dictionary());
    assert!(!dictionary.is_array());
    assert!(array.is_array());
    assert!(!array.is_dictionary());
    assert_eq!(
        dictionary.get_dict_item(b"line\\n").get_number(),
        Some(b"3".to_vec())
    );
    assert!(dictionary.get_dict_item(b"line\n").is_null());

    let mut members = Vec::new();
    assert!(dictionary.for_each_dict_item(|key, value| {
        members.push((key.to_vec(), value.get_number()));
    }));
    assert_eq!(members, vec![(b"line\\n".to_vec(), Some(b"3".to_vec()))]);

    let mut values = Vec::new();
    assert!(array.for_each_array_item(|value| values.push(value.get_bool())));
    assert_eq!(values, vec![Some(true)]);
}

#[test]
fn container_mutations_reject_wrong_type_with_qpdf_messages() {
    let scalar = Json::make_null();
    assert_eq!(
        scalar
            .add_dictionary_member(b"key", Json::make_int(1))
            .unwrap_err()
            .to_string(),
        "JSON::addDictionaryMember called on non-dictionary"
    );
    assert_eq!(
        scalar
            .add_array_element(Json::make_int(1))
            .unwrap_err()
            .to_string(),
        "JSON::addArrayElement called on non-array"
    );
    assert_eq!(
        scalar
            .check_dictionary_key_seen(b"key")
            .unwrap_err()
            .to_string(),
        "JSON::checkDictionaryKey called on non-dictionary"
    );
    assert!(!scalar.for_each_dict_item(|_, _| unreachable!()));
    assert!(!scalar.for_each_array_item(|_| unreachable!()));
}

#[test]
fn uninitialized_handles_reject_mutation_and_return_empty_access_results() {
    let value = Json::default();
    assert_eq!(
        value
            .add_dictionary_member(b"key", Json::make_int(1))
            .unwrap_err()
            .to_string(),
        "JSON::addDictionaryMember called on non-dictionary"
    );
    assert_eq!(
        value
            .add_array_element(Json::make_int(1))
            .unwrap_err()
            .to_string(),
        "JSON::addArrayElement called on non-array"
    );
    assert_eq!(
        value
            .check_dictionary_key_seen(b"key")
            .unwrap_err()
            .to_string(),
        "JSON::checkDictionaryKey called on non-dictionary"
    );
    assert!(value.get_dict_item(b"key").is_null());
    assert!(!value.for_each_dict_item(|_, _| unreachable!()));
    assert!(!value.for_each_array_item(|_| unreachable!()));
}
