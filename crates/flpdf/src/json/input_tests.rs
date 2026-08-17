//! qpdf correspondence: tests for the JSON input value and deferred stream provider boundaries.
//! (`libqpdf/QPDF_json.cc:65-231, 732-793`; `libqpdf/QUtil.cc:642-663`).

use super::input::{
    inline_stream_data_provider, json_value_to_handle, parse_indirect_reference, parse_object_key,
    JsonReactor,
};
use super::Json;
use crate::json::parse_reader;
use crate::{Error, ObjectHandle, ObjectRef, Pdf};
use std::cell::RefCell;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

struct CountingReader {
    cursor: Cursor<Vec<u8>>,
    read_calls: usize,
    seek_calls: usize,
    fail_read: bool,
    fail_seek: bool,
}

impl CountingReader {
    fn new(bytes: &[u8]) -> Self {
        Self {
            cursor: Cursor::new(bytes.to_vec()),
            read_calls: 0,
            seek_calls: 0,
            fail_read: false,
            fail_seek: false,
        }
    }
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.read_calls += 1;
        if self.fail_read {
            return Err(std::io::Error::other("instrumented read failure"));
        }
        self.cursor.read(buffer)
    }
}

impl Seek for CountingReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.seek_calls += 1;
        if self.fail_seek {
            return Err(std::io::Error::other("instrumented seek failure"));
        }
        self.cursor.seek(position)
    }
}

#[test]
fn qpdf_json_validators_accept_only_qpdf_reference_shapes() {
    assert_eq!(
        parse_indirect_reference(b"52  20  R"),
        Some(ObjectRef::new(52, 20))
    );
    assert_eq!(parse_indirect_reference(b"0 0 R"), None);
    assert_eq!(parse_indirect_reference(b"52 20 R trailing"), None);
    assert_eq!(parse_indirect_reference(b"52 20R"), None);
    assert_eq!(
        parse_object_key(b"obj:12 13 R"),
        Some(ObjectRef::new(12, 13))
    );
    assert_eq!(parse_object_key(b"12 13 R"), None);
}

#[test]
fn qpdf_json_value_factory_builds_scalars_and_pdf_string_forms() {
    let mut pdf = Pdf::empty().expect("empty PDF");

    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("42"))
            .expect("integer")
            .as_integer(),
        Some(42)
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_bool(true))
            .expect("boolean")
            .as_boolean(),
        Some(true)
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("/Name"))
            .expect("name")
            .as_name(),
        Some(b"Name".to_vec())
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("b:00ff"))
            .expect("binary string")
            .as_string(),
        Some(vec![0, 0xff])
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("u:hello"))
            .expect("unicode string")
            .as_string(),
        Some(crate::pdf_string::new_unicode_string(b"hello"))
    );
    assert!(json_value_to_handle(&mut pdf, &Json::make_string("b:0g"))
        .expect("invalid binary string")
        .is_null());
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("n:/Name"))
            .expect("encoded name")
            .as_name(),
        Some(b"Name".to_vec())
    );
    assert!(
        json_value_to_handle(&mut pdf, &Json::make_string("unknown"))
            .expect("unknown string")
            .is_null()
    );
}

#[test]
fn qpdf_json_value_factory_preserves_real_literals_and_rejects_non_finite_numbers() {
    let mut pdf = Pdf::empty().expect("empty PDF");

    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("1.5"))
            .expect("real")
            .as_real_literal(),
        Some((1.5, b"1.5".to_vec()))
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("1e+2"))
            .expect("scientific real")
            .as_real(),
        Some(100.0)
    );
    let error =
        json_value_to_handle(&mut pdf, &Json::make_number("1e9999")).expect_err("non-finite real");
    assert!(error.to_string().contains("invalid JSON number"));
}

#[test]
fn qpdf_json_value_factory_rejects_uninitialized_values_and_decodes_name_keys() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let error = json_value_to_handle(&mut pdf, &Json::default()).expect_err("uninitialized");
    assert!(error
        .to_string()
        .contains("JSON value has no initialized qpdf value kind"));

    let object = Json::parse(br#"{"n:/Name": 1}"#).expect("JSON");
    let dictionary = json_value_to_handle(&mut pdf, &object)
        .expect("dictionary")
        .as_dictionary()
        .expect("dictionary handle");
    assert_eq!(
        dictionary
            .get(b"/Name".as_slice())
            .and_then(|value| value.as_integer()),
        Some(1)
    );
}

#[test]
fn qpdf_json_reference_factory_reuses_canonical_identity() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let value = Json::make_string("12 0 R");
    let from_json = json_value_to_handle(&mut pdf, &value).expect("reference");
    let from_pdf = pdf.get_object_handle(ObjectRef::new(12, 0));

    assert!(from_json.is_same_object_as(&from_pdf));
    assert_eq!(from_json.object_ref(), Some(ObjectRef::new(12, 0)));
}

#[test]
fn qpdf_json_value_factory_builds_nested_canonical_handles() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let object = Json::parse(br#"{"/A": [1, "12 0 R"], "/B": null}"#).expect("JSON");
    let handle = json_value_to_handle(&mut pdf, &object).expect("dictionary");
    let dictionary = handle.as_dictionary().expect("dictionary handle");
    let array = dictionary
        .get(b"/A".as_slice())
        .expect("/A")
        .as_array()
        .expect("array");

    assert_eq!(array[0].as_integer(), Some(1));
    assert_eq!(array[1].object_ref(), Some(ObjectRef::new(12, 0)));
    assert!(dictionary.get(b"/B".as_slice()).expect("/B").is_null());
}

#[test]
fn qpdf_json_value_factory_rejects_non_qpdf_real_literals() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let error = json_value_to_handle(&mut pdf, &Json::make_number("1e+")).expect_err("real");

    assert!(error.to_string().contains("invalid JSON number"));
}

#[test]
fn inline_stream_provider_is_lazy_and_decodes_only_when_piped() {
    let source = Rc::new(RefCell::new(CountingReader::new(br#"{"data":"TWFu"}"#)));
    let value = {
        let mut reader = source.borrow_mut();
        Json::parse_reader(&mut *reader, None)
            .expect("JSON input")
            .get_dict_item("data")
    };
    assert_eq!((value.start(), value.end()), (8, 14));
    let parsed_reads = source.borrow().read_calls;
    let parsed_seeks = source.borrow().seek_calls;
    let provider = inline_stream_data_provider(Rc::clone(&source), &value)
        .expect("inline provider registration");

    assert_eq!(source.borrow().read_calls, parsed_reads);
    assert_eq!(source.borrow().seek_calls, parsed_seeks);

    let pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("new stream");
    stream
        .replace_stream_data_provider(provider, None, None)
        .expect("provider replacement");
    assert_eq!(source.borrow().read_calls, parsed_reads);
    assert_eq!(source.borrow().seek_calls, parsed_seeks);

    for _ in 0..2 {
        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("pipe inline provider")
                .as_slice(),
            b"Man"
        );
    }
    assert!(source.borrow().read_calls > parsed_reads);
    assert_eq!(source.borrow().seek_calls, parsed_seeks + 2);
}

#[test]
fn inline_stream_provider_reports_source_and_decode_failures_at_pipe_time() {
    for (fail_read, fail_seek) in [(true, false), (false, true)] {
        let source = Rc::new(RefCell::new(CountingReader::new(b"\"TWFu\"")));
        source.borrow_mut().fail_read = fail_read;
        source.borrow_mut().fail_seek = fail_seek;
        let value = Json::make_string("TWFu");
        value.set_start(0);
        value.set_end(6);
        let provider = inline_stream_data_provider(Rc::clone(&source), &value)
            .expect("inline provider registration");
        let pdf = Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("new stream");
        stream
            .replace_stream_data_provider(provider, None, None)
            .expect("provider replacement");

        assert!(matches!(stream.get_raw_stream_data(), Err(Error::Io(_))));
    }

    let source = Rc::new(RefCell::new(CountingReader::new(b"\"@@@@\"")));
    let value = Json::make_string("@@@@");
    value.set_start(0);
    value.set_end(6);
    let provider = inline_stream_data_provider(source, &value).expect("provider registration");
    let pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("new stream");
    stream
        .replace_stream_data_provider(provider, None, None)
        .expect("provider replacement");
    assert!(matches!(
        stream.get_raw_stream_data(),
        Err(Error::System(message)) if message.contains("base64 decode: invalid input")
    ));
}

#[test]
fn inline_stream_provider_finishes_when_source_ends_before_the_json_range() {
    let source = Rc::new(RefCell::new(CountingReader::new(b"\"TW")));
    let value = Json::make_string("TWFu");
    value.set_start(0);
    value.set_end(6);
    let provider = inline_stream_data_provider(source, &value).expect("provider registration");
    let pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("new stream");
    stream
        .replace_stream_data_provider(provider, None, None)
        .expect("provider replacement");

    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("pipe truncated inline provider")
            .as_slice(),
        b"M"
    );
}

#[test]
fn inline_stream_provider_rejects_an_invalid_json_string_range() {
    let source = Rc::new(RefCell::new(CountingReader::new(b"")));
    for (start, end, message) in [
        (2, 2, "JSON string length < 0"),
        (i64::MAX, i64::MAX, "JSON string start overflow"),
        (0, i64::MIN, "JSON string end underflow"),
        (i64::MIN, i64::MAX, "JSON string length is out of range"),
        (-2, 2, "JSON string start is negative"),
    ] {
        let value = Json::make_string("TWFu");
        value.set_start(start);
        value.set_end(end);
        let error = inline_stream_data_provider(Rc::clone(&source), &value)
            .expect_err("invalid range must fail before provider registration");
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn datafile_stream_provider_opens_only_when_piped() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("stream.bin");
    let provider = super::input::datafile_stream_data_provider(path.clone());
    let pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("new stream");
    stream
        .replace_stream_data_provider(provider, None, None)
        .expect("provider replacement");
    assert!(!path.exists(), "registration must not open datafile");

    fs::write(&path, b"external bytes").expect("create datafile after registration");
    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("pipe datafile provider")
            .as_slice(),
        b"external bytes"
    );
}

#[test]
fn datafile_stream_provider_reports_missing_file_at_pipe_time() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("missing.bin");
    let provider = super::input::datafile_stream_data_provider(path.clone());
    let pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("new stream");
    stream
        .replace_stream_data_provider(provider, None, None)
        .expect("provider replacement");

    assert!(matches!(
        stream.get_raw_stream_data(),
        Err(Error::System(message)) if message.starts_with("open ")
    ));
}

#[cfg(unix)]
#[test]
fn datafile_stream_provider_reports_read_failure_at_pipe_time() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let provider = super::input::datafile_stream_data_provider(directory.path());
    let pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("new stream");
    stream
        .replace_stream_data_provider(provider, None, None)
        .expect("provider replacement");

    assert!(matches!(
        stream.get_raw_stream_data(),
        Err(Error::System(message)) if message.starts_with("failure reading file ")
    ));
}

#[test]
fn json_reactor_builds_canonical_objects_trailer_and_deferred_stream() {
    let json = br#"{
        "qpdf": [
            {
                "jsonversion": 2,
                "pdfversion": "1.7",
                "unknown": {"ignored": true}
            },
            {
                "obj:1 0 R": {"value": {"/Type": "/Catalog", "/Pages": "2 0 R"}},
                "obj:2 0 R": {"value": [1, true]},
                "obj:3 0 R": {
                    "stream": {"dict": {"/Length": 3}, "data": "YWJj", "unknown": false}
                },
                "trailer": {"value": {"/Root": "1 0 R"}}
            }
        ],
        "unknown": "ignored"
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "input.json", true);

    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(!reactor.any_errors());
    drop(reactor);

    assert_eq!(pdf.version(), "1.7");

    let catalog = pdf.get_object_handle(ObjectRef::new(1, 0));
    catalog.try_dereference().expect("catalog resolves");
    assert!(catalog
        .description()
        .contains("input.json, obj:1 0 R at offset"));
    let catalog_dict = catalog.as_dictionary().expect("catalog dictionary");
    assert_eq!(
        catalog_dict
            .get(b"/Type".as_slice())
            .and_then(|value| value.as_name()),
        Some(b"Catalog".to_vec())
    );
    assert_eq!(
        catalog_dict
            .get(b"/Pages".as_slice())
            .and_then(ObjectHandle::object_ref),
        Some(ObjectRef::new(2, 0))
    );

    let array = pdf.get_object_handle(ObjectRef::new(2, 0));
    array.try_dereference().expect("array resolves");
    let values = array.as_array().expect("array value");
    assert_eq!(values[0].as_integer(), Some(1));
    assert_eq!(values[1].as_boolean(), Some(true));

    let stream = pdf.get_object_handle(ObjectRef::new(3, 0));
    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("stream data")
            .as_slice(),
        b"abc"
    );

    let trailer = pdf.trailer_handle();
    trailer.try_dereference().expect("trailer resolves");
    assert_eq!(
        trailer.get_key(b"/Root").object_ref(),
        Some(ObjectRef::new(1, 0))
    );
}

#[test]
fn json_reactor_normalizes_new_dangling_references_to_null() {
    let json = br#"{
        "qpdf": [
            {"jsonversion": 2, "pdfversion": "1.3"},
            {
                "obj:1 0 R": {"value": {"/Dangling": "9 0 R"}},
                "trailer": {"value": {"/Root": "1 0 R"}}
            }
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "input.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(!reactor.any_errors());
    drop(reactor);

    let object = pdf.get_object_handle(ObjectRef::new(1, 0));
    object.try_dereference().expect("object resolves");
    assert!(object.get_key(b"/Dangling").is_null());
    assert!(!pdf.get_object_handle(ObjectRef::new(9, 0)).is_reserved());
}

#[test]
fn json_reactor_records_qpdf_validation_errors_and_ignores_unknown_keys() {
    let json = br#"{
        "qpdf": [
            {
                "jsonversion": 3,
                "pdfversion": "bad",
                "future": {"nested": [true]}
            },
            {
                "obj:1 0 R": {"value": "plain", "future": {"ignored": false}}
            }
        ],
        "future": [1, 2, 3]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "input.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(reactor.any_errors());
    drop(reactor);

    let messages: Vec<_> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    assert!(messages
        .iter()
        .any(|message| message.contains("invalid JSON version")));
    assert!(messages
        .iter()
        .any(|message| message.contains("invalid PDF version")));
    assert!(messages
        .iter()
        .any(|message| message.contains("unrecognized string value")));
    assert!(messages
        .iter()
        .any(|message| message.contains("\"qpdf[1].trailer\" was not seen")));
    assert!(messages.iter().all(|message| !message.contains("future")));
}

#[test]
fn json_reactor_validates_object_and_stream_shapes() {
    let json = br#"{
        "qpdf": [
            {"jsonversion": 2, "pdfversion": "1.3"},
            {
                "obj:1 0 R": {"value": 1, "stream": {"dict": {}}},
                "obj:2 0 R": {"stream": {"dict": {}}},
                "obj:3 0 R": {"stream": {"data": "YQ=="}},
                "obj:4 0 R": {
                    "stream": {"dict": {}, "data": "YQ==", "datafile": "ignored.bin"}
                },
                "trailer": {"value": {}}
            }
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "input.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(reactor.any_errors());
    drop(reactor);

    let messages: Vec<_> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    assert!(messages
        .iter()
        .any(|message| message.contains("exactly one of \"value\" or \"stream\"")));
    assert!(messages
        .iter()
        .any(|message| message.contains("new \"stream\" must have exactly one")));
    assert!(messages
        .iter()
        .any(|message| message.contains("\"stream\" is missing \"dict\"")));
    assert!(messages.iter().any(|message| message
        .contains("new \"stream\" must have exactly one of \"data\" or \"datafile\"")));
}

#[test]
fn json_reactor_rejects_malformed_object_keys_and_preserves_omitted_objects() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let omitted = pdf.new_reserved().expect("reserved object");
    let object_ref = omitted.object_ref().expect("reserved identity");
    let json = br#"{
        "qpdf": [
            {"jsonversion": 2},
            {"not-an-object-key": {"value": 1}}
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "update.json", false);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
    assert!(reactor.any_errors());
    drop(reactor);

    assert!(pdf.get_object_handle(object_ref).is_reserved());
    let messages: Vec<_> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    assert!(messages
        .iter()
        .any(|message| { message.contains("object key should be \"trailer\" or \"obj:n n R\"") }));
}

#[test]
fn json_reactor_rejects_both_stream_data_sources_in_an_existing_stream() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("stream");
    let object_ref = stream.object_ref().expect("stream identity");
    stream.replace_stream_data(Rc::new(b"old".to_vec()), None, None);

    let json = format!(
        "{{\"qpdf\":[{{\"jsonversion\":2}},{{\"obj:{} {} R\":{{\"stream\":{{\"dict\":{{}},\"data\":\"YQ==\",\"datafile\":\"ignored.bin\"}}}}}}]}}",
        object_ref.number, object_ref.generation
    );
    let source = Rc::new(RefCell::new(Cursor::new(json.into_bytes())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "update.json", false);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
    assert!(reactor.any_errors());
    drop(reactor);

    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic
            .message
            .contains("existing \"stream\" may at most one of \"data\" or \"datafile\"")));
}

#[test]
fn json_reactor_updates_an_existing_stream_without_requiring_new_data() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let stream = pdf.new_stream().expect("stream");
    let object_ref = stream.object_ref().expect("stream identity");
    stream.replace_stream_data(Rc::new(b"old".to_vec()), None, None);

    let json = format!(
        "{{\"qpdf\":[{{\"jsonversion\":2}},{{\"obj:{} {} R\":{{\"stream\":{{\"dict\":{{\"/K\":7}}}}}}}}]}}",
        object_ref.number, object_ref.generation
    );
    let source = Rc::new(RefCell::new(Cursor::new(json.into_bytes())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "update.json", false);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
    assert!(!reactor.any_errors());
    drop(reactor);

    let updated = pdf.get_object_handle(object_ref);
    updated.try_dereference().expect("stream resolves");
    assert_eq!(
        updated
            .as_stream_dict()
            .expect("stream dictionary")
            .get_key(b"/K")
            .as_integer(),
        Some(7)
    );
    assert!(updated
        .as_stream_dict()
        .expect("stream dictionary")
        .get_key(b"/Length")
        .is_null());
    assert_eq!(
        updated
            .get_raw_stream_data()
            .expect("stream data")
            .as_slice(),
        b"old"
    );
}

#[test]
fn json_reactor_applies_update_page_observation_flags() {
    let pdf_bytes = include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec();
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("minimal PDF");
    let json = br#"{
        "qpdf": [
            {
                "jsonversion": 2,
                "calledgetallpages": true,
                "pushedinheritedpageresources": true
            },
            {}
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "update.json", false);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
    assert!(!reactor.any_errors());
    assert!(reactor.fatal_error().is_none());
    drop(reactor);
    assert!(pdf.ever_called_get_all_pages());
}

#[test]
fn json_reactor_rejects_top_level_scalar_and_array_as_runtime_errors() {
    for json in [b"true".as_slice(), b"[]".as_slice()] {
        let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
        let mut pdf = Pdf::empty().expect("empty PDF");
        let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "input.json", true);
        parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON parser");
        assert_eq!(
            reactor.fatal_error(),
            Some("QPDF JSON must be a dictionary")
        );
    }
}
