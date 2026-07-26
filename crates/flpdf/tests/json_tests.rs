use std::io;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use flpdf::json::Json;

struct MaxWriteSink {
    bytes: Vec<u8>,
    max_write: usize,
}

impl io::Write for MaxWriteSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.max_write {
            return Err(io::Error::other(format!(
                "write of {} bytes exceeds {} byte limit",
                bytes.len(),
                self.max_write
            )));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn incremental_writer_matches_qpdf_nested_bytes() {
    let mut out = Vec::new();
    let mut top_first = true;
    Json::write_dictionary_open(&mut out, &mut top_first, 0).unwrap();
    Json::write_dictionary_item(&mut out, &mut top_first, b"version", &Json::make_int(2), 1)
        .unwrap();
    Json::write_dictionary_key(&mut out, &mut top_first, b"items", 1).unwrap();
    let mut array_first = true;
    Json::write_array_open(&mut out, &mut array_first, 1).unwrap();
    Json::write_array_item(&mut out, &mut array_first, &Json::make_bool(true), 2).unwrap();
    Json::write_array_close(&mut out, array_first, 1).unwrap();
    Json::write_dictionary_close(&mut out, top_first, 0).unwrap();
    assert_eq!(
        out,
        b"{\n  \"version\": 2,\n  \"items\": [\n    true\n  ]\n}"
    );
}

#[test]
fn incremental_writer_keeps_empty_containers_compact() {
    let mut out = Vec::new();
    let mut first = false;
    Json::write_dictionary_open(&mut out, &mut first, 3).unwrap();
    assert!(first);
    Json::write_dictionary_close(&mut out, first, 3).unwrap();

    Json::write_array_open(&mut out, &mut first, 8).unwrap();
    assert!(first);
    Json::write_array_close(&mut out, first, 8).unwrap();

    assert_eq!(out, b"{}[]");
}

#[test]
fn incremental_writer_writes_dictionary_keys_that_are_already_encoded() {
    let mut out = Vec::new();
    let mut first = true;
    Json::write_dictionary_open(&mut out, &mut first, 0).unwrap();
    Json::write_dictionary_item(&mut out, &mut first, b"line\\n", &Json::make_int(1), 1).unwrap();
    Json::write_dictionary_close(&mut out, first, 0).unwrap();

    assert_eq!(out, b"{\n  \"line\\n\": 1\n}");
}

#[test]
fn make_blob_propagates_callback_io_errors() {
    let blob = Json::make_blob(|_| Err(io::Error::other("blob callback failure")));

    let error = blob.unparse().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.to_string(), "blob callback failure");
}

#[test]
fn encoded_member_order_is_independent_from_parsed_key_tracking() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"\x03", Json::make_int(1))
        .unwrap();
    dictionary
        .add_dictionary_member(b"Z", Json::make_int(2))
        .unwrap();

    assert!(!dictionary.check_dictionary_key_seen(b"\x03").unwrap());
    assert!(dictionary.check_dictionary_key_seen(b"\x03").unwrap());
    assert_eq!(
        dictionary.unparse().unwrap(),
        b"{\n  \"Z\": 2,\n  \"\\u0003\": 1\n}"
    );
}

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
fn qpdf_string_escape_bytes_are_exact() {
    let value = Json::make_string(b"<1>\xcf\x80<2>\xf0\x9f\xa5\x94\\\"<3>\x03\t\x08\r\n<4>");
    assert_eq!(
        value.unparse().unwrap(),
        b"\"<1>\xcf\x80<2>\xf0\x9f\xa5\x94\\\\\\\"<3>\\u0003\\t\\b\\r\\n<4>\""
    );
}

#[test]
fn qpdf_blob_uses_standard_base64_without_newlines() {
    let blob = Json::make_blob(|out| out.write_all(b"\x01\x02\x03\x04\x05\xff\xfe\xfd\xfc\xfb"));
    assert_eq!(blob.unparse().unwrap(), b"\"AQIDBAX//v38+w==\"");
}

#[test]
fn qpdf_blob_streams_base64_without_one_full_encoded_write() {
    let bytes = vec![0x5a; 4096];
    let payload = bytes.clone();
    let blob = Json::make_blob(move |out| out.write_all(&payload));
    let mut out = MaxWriteSink {
        bytes: Vec::new(),
        max_write: 2048,
    };

    blob.write(&mut out, 0).unwrap();

    let expected = format!("\"{}\"", STANDARD.encode(bytes)).into_bytes();
    assert_eq!(out.bytes, expected);
}

#[test]
#[allow(clippy::approx_constant)]
fn qpdf_real_uses_six_digit_trimmed_format() {
    assert_eq!(Json::make_real(3.14159).unparse().unwrap(), b"3.14159");
    assert_eq!(Json::make_real(3.1415927).unparse().unwrap(), b"3.141593");
    assert_eq!(Json::make_real(-0.0).unparse().unwrap(), b"-0");
}

#[test]
fn scalar_accessors_reject_other_types_without_mutating_output() {
    let value = Json::make_bool(true);
    assert_eq!(value.get_bool(), Some(true));
    assert_eq!(value.get_string(), None);
    assert_eq!(value.get_number(), None);
    assert!(value.get_dict_item(b"key").is_null());
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
fn dictionary_iteration_observes_future_mutations_without_revisiting_earlier_insertions() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    dictionary
        .add_dictionary_member(b"c", Json::make_int(3))
        .unwrap();
    let alias = dictionary.clone();

    let mut visited = Vec::new();
    assert!(dictionary.for_each_dict_item(|key, value| {
        visited.push((key.to_vec(), value.get_number().unwrap()));
        if key == b"a" {
            alias
                .add_dictionary_member(b"c", Json::make_int(30))
                .unwrap();
            alias
                .add_dictionary_member(b"b", Json::make_int(2))
                .unwrap();
            alias
                .add_dictionary_member(b"0", Json::make_int(0))
                .unwrap();
        }
    }));

    assert_eq!(
        visited,
        vec![
            (b"a".to_vec(), b"1".to_vec()),
            (b"b".to_vec(), b"2".to_vec()),
            (b"c".to_vec(), b"30".to_vec()),
        ]
    );
    assert_eq!(alias.get_dict_item(b"0").get_number(), Some(b"0".to_vec()));
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
