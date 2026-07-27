use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use flpdf::json::Json;

struct MaxWriteSink {
    bytes: Vec<u8>,
    max_write: usize,
    write_sizes: Vec<usize>,
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
        self.write_sizes.push(bytes.len());
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CallbackSink<F> {
    bytes: Vec<u8>,
    callback: F,
}

impl<F: FnMut(&[u8])> io::Write for CallbackSink<F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        (self.callback)(&self.bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FlushSink {
    bytes: Vec<u8>,
    flushes: Rc<Cell<usize>>,
}

impl io::Write for FlushSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes.set(self.flushes.get() + 1);
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
fn qpdf_blob_batches_base64_into_bounded_writes() {
    let bytes = vec![0x5a; 4096];
    let payload = bytes.clone();
    let blob = Json::make_blob(move |out| out.write_all(&payload));
    let mut out = MaxWriteSink {
        bytes: Vec::new(),
        max_write: 8192,
        write_sizes: Vec::new(),
    };

    blob.write(&mut out, 0).unwrap();

    let expected = format!("\"{}\"", STANDARD.encode(bytes)).into_bytes();
    assert_eq!(out.bytes, expected);
    assert!(
        out.write_sizes.len() <= 4,
        "bounded Base64 batching should need at most opening quote, complete groups, tail, and closing quote; got {:?}",
        out.write_sizes
    );
}

#[test]
fn blob_callback_flushes_its_underlying_sink() {
    let flushes = Rc::new(Cell::new(0));
    let blob = Json::make_blob(|out| out.flush());
    let mut out = FlushSink {
        bytes: Vec::new(),
        flushes: flushes.clone(),
    };

    blob.write(&mut out, 0).unwrap();

    assert_eq!(out.bytes, b"\"\"");
    assert_eq!(flushes.get(), 1);
}

#[test]
fn blob_base64_preserves_pending_bytes_across_split_writes() {
    let one_plus_one = Json::make_blob(|out| {
        out.write_all(b"x")?;
        out.write_all(b"y")
    });
    assert_eq!(one_plus_one.unparse().unwrap(), b"\"eHk=\"");

    let two_plus_empty = Json::make_blob(|out| {
        out.write_all(b"xy")?;
        assert_eq!(out.write(&[])?, 0);
        Ok(())
    });
    assert_eq!(two_plus_empty.unparse().unwrap(), b"\"eHk=\"");

    let one_plus_one_plus_one = Json::make_blob(|out| {
        out.write_all(b"x")?;
        out.write_all(b"y")?;
        out.write_all(b"z")
    });
    assert_eq!(one_plus_one_plus_one.unparse().unwrap(), b"\"eHl6\"");
}

#[test]
fn blob_callback_can_reenter_the_same_callback() {
    let holder = Rc::new(RefCell::new(None::<Json>));
    let weak_holder = Rc::downgrade(&holder);
    let nested = Rc::new(Cell::new(false));
    let blob = Json::make_blob({
        let nested = nested.clone();
        move |out| {
            if nested.replace(true) {
                out.write_all(b"x")?;
            } else {
                let blob = weak_holder
                    .upgrade()
                    .expect("holder is alive")
                    .borrow()
                    .as_ref()
                    .expect("blob is installed")
                    .clone();
                blob.write(out, 0)?;
            }
            Ok(())
        }
    });
    *holder.borrow_mut() = Some(blob.clone());

    assert_eq!(blob.unparse().unwrap(), b"\"ImVBPT0i\"");
    holder.borrow_mut().take();
}

#[test]
fn blob_error_does_not_finalize_a_partial_base64_group() {
    for (raw, expected) in [
        (b"x".as_slice(), b"\"".as_slice()),
        (b"abcd".as_slice(), b"\"YWJj".as_slice()),
    ] {
        let raw = raw.to_vec();
        let blob = Json::make_blob(move |out| {
            out.write_all(&raw)?;
            Err(io::Error::other("producer failed"))
        });
        let mut bytes = Vec::new();

        let error = blob.write(&mut bytes, 0).unwrap_err();

        assert_eq!(error.to_string(), "producer failed");
        assert_eq!(bytes, expected);
    }
}

#[test]
fn dictionary_writer_rereads_value_after_key_output() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    let alias = dictionary.clone();
    let replaced = Rc::new(Cell::new(false));
    let mut sink = CallbackSink {
        bytes: Vec::new(),
        callback: {
            let replaced = replaced.clone();
            move |bytes: &[u8]| {
                if !replaced.get() && bytes.ends_with(b"\"a\": ") {
                    replaced.set(true);
                    alias
                        .add_dictionary_member(b"a", Json::make_int(99))
                        .unwrap();
                }
            }
        },
    };

    dictionary.write(&mut sink, 0).unwrap();

    assert!(replaced.get());
    assert_eq!(sink.bytes, b"{\n  \"a\": 99\n}");
}

#[test]
fn dictionary_writer_starts_iteration_after_opening_brace() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    let alias = dictionary.clone();
    let inserted = Rc::new(Cell::new(false));
    let mut sink = CallbackSink {
        bytes: Vec::new(),
        callback: {
            let inserted = inserted.clone();
            move |bytes: &[u8]| {
                if !inserted.get() && bytes == b"{" {
                    inserted.set(true);
                    alias
                        .add_dictionary_member(b"b", Json::make_int(2))
                        .unwrap();
                }
            }
        },
    };

    dictionary.write(&mut sink, 0).unwrap();

    assert_eq!(sink.bytes, b"{\n  \"a\": 1,\n  \"b\": 2\n}");
}

#[test]
fn array_writer_snapshots_elements_after_opening_bracket() {
    let array = Json::make_array();
    array.add_array_element(Json::make_int(1)).unwrap();
    let alias = array.clone();
    let inserted = Rc::new(Cell::new(false));
    let mut sink = CallbackSink {
        bytes: Vec::new(),
        callback: {
            let inserted = inserted.clone();
            move |bytes: &[u8]| {
                if !inserted.get() && bytes == b"[" {
                    inserted.set(true);
                    alias.add_array_element(Json::make_int(2)).unwrap();
                }
            }
        },
    };

    array.write(&mut sink, 0).unwrap();

    assert_eq!(sink.bytes, b"[\n  1,\n  2\n]");
}

#[test]
fn dictionary_writer_observes_live_mutations_from_blob_callback() {
    let dictionary = Json::make_dictionary();
    let owner = Rc::new(RefCell::new(Some(dictionary.clone())));
    let weak_owner = Rc::downgrade(&owner);
    dictionary
        .add_dictionary_member(
            b"a",
            Json::make_blob(move |_| {
                let owner = weak_owner
                    .upgrade()
                    .expect("dictionary owner must be alive");
                let owner = owner.borrow();
                let dictionary = owner.as_ref().expect("dictionary must remain installed");
                dictionary
                    .add_dictionary_member(b"b", Json::make_int(2))
                    .unwrap();
                dictionary
                    .add_dictionary_member(b"c", Json::make_int(30))
                    .unwrap();
                dictionary
                    .add_dictionary_member(b"0", Json::make_int(0))
                    .unwrap();
                Ok(())
            }),
        )
        .unwrap();
    dictionary
        .add_dictionary_member(b"c", Json::make_int(3))
        .unwrap();

    assert_eq!(
        dictionary.unparse().unwrap(),
        b"{\n  \"a\": \"\",\n  \"b\": 2,\n  \"c\": 30\n}"
    );
    assert_eq!(
        dictionary.get_dict_item(b"0").get_number(),
        Some(b"0".to_vec())
    );
}

#[test]
fn dictionary_writer_stops_after_blob_io_error() {
    let later_visits = Rc::new(Cell::new(0));
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(
            b"a",
            Json::make_blob(|_| Err(io::Error::other("first blob failed"))),
        )
        .unwrap();
    dictionary
        .add_dictionary_member(
            b"b",
            Json::make_blob({
                let later_visits = later_visits.clone();
                move |_| {
                    later_visits.set(later_visits.get() + 1);
                    Ok(())
                }
            }),
        )
        .unwrap();
    let mut out = Vec::new();

    let error = dictionary.write(&mut out, 0).unwrap_err();

    assert_eq!(error.to_string(), "first blob failed");
    assert_eq!(out, b"{\n  \"a\": \"");
    assert_eq!(later_visits.get(), 0);
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
fn array_iteration_releases_borrow_and_uses_initial_elements() {
    let array = Json::make_array();
    array.add_array_element(Json::make_int(1)).unwrap();
    array.add_array_element(Json::make_int(2)).unwrap();
    let mut first_pass = Vec::new();

    assert!(array.for_each_array_item(|item| {
        first_pass.push(item.get_number().unwrap());
        if first_pass.len() == 1 {
            array.set_start(7);
            array.set_end(9);
            array.add_array_element(Json::make_int(3)).unwrap();
        }
    }));

    assert_eq!(first_pass, [b"1".to_vec(), b"2".to_vec()]);
    assert_eq!((array.start(), array.end()), (7, 9));
    let mut second_pass = Vec::new();
    assert!(array.for_each_array_item(|item| {
        second_pass.push(item.get_number().unwrap());
    }));
    assert_eq!(second_pass, [b"1".to_vec(), b"2".to_vec(), b"3".to_vec()]);
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
