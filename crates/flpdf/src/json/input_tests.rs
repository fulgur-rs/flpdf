//! qpdf correspondence: tests for the JSON input value and deferred stream provider boundaries.
//! (`libqpdf/QPDF_json.cc:65-231, 732-793`; `libqpdf/QUtil.cc:642-663`).

use super::input::{
    inline_stream_data_provider, json_value_to_handle, parse_indirect_reference, parse_object_key,
    IndirectReferenceParse, JsonReactor,
};
use super::{Json, Reactor};
use crate::json::parse_reader;
use crate::pipeline::test_support::NthWriteFailure;
use crate::pipeline::PipelineHandle;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, PdfOpenOptions, QPDFLogger};
use std::cell::{Cell, RefCell};
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

struct ToggleReader {
    cursor: Cursor<Vec<u8>>,
    fail: Rc<Cell<bool>>,
}

impl Read for ToggleReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.fail.get() {
            return Err(std::io::Error::other("instrumented source failure"));
        }
        self.cursor.read(buffer)
    }
}

impl Seek for ToggleReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(position)
    }
}

#[test]
fn qpdf_json_validators_accept_only_qpdf_reference_shapes() {
    assert_eq!(
        parse_indirect_reference(b"52  20  R"),
        IndirectReferenceParse::Reference(ObjectRef::new(52, 20))
    );
    assert_eq!(
        parse_indirect_reference(b"0 0 R"),
        IndirectReferenceParse::NoMatch
    );
    assert_eq!(
        parse_indirect_reference(b"52 20 R trailing"),
        IndirectReferenceParse::NoMatch
    );
    assert_eq!(
        parse_indirect_reference(b"52 20R"),
        IndirectReferenceParse::NoMatch
    );
    assert_eq!(
        parse_object_key(b"obj:12 13 R"),
        IndirectReferenceParse::Reference(ObjectRef::new(12, 13))
    );
    assert_eq!(
        parse_object_key(b"12 13 R"),
        IndirectReferenceParse::NoMatch
    );
}

#[test]
fn qpdf_json_validators_report_overflow_instead_of_no_match() {
    // Shape matches `N G R`, but the object number overflows qpdf's i32
    // `QUtil::string_to_int` narrowing stage (`QIntC.hh:87-109`). qpdf
    // throws an uncaught `std::range_error` here instead of returning
    // `false`, so this must not be conflated with "not a reference".
    match parse_indirect_reference(b"4294967296 0 R") {
        IndirectReferenceParse::Overflow(message) => {
            assert_eq!(
                message,
                "integer out of range converting 4294967296 from a 8-byte \
                 signed type to a 4-byte signed type"
            );
        }
        other => panic!("expected Overflow, got {other:?}"),
    }

    // Overflows qpdf's i64 `strtoll` stage itself (`QUtil.cc:373-386`).
    match parse_indirect_reference(b"99999999999999999999 0 R") {
        IndirectReferenceParse::Overflow(message) => {
            assert_eq!(
                message,
                "overflow/underflow converting 99999999999999999999 to 64-bit integer"
            );
        }
        other => panic!("expected Overflow, got {other:?}"),
    }

    // The generation number goes through the exact same
    // `QUtil::string_to_int` conversion as the object number
    // (`is_indirect_object`, `QPDF_json.cc:66-104` calls it for both), so
    // it must overflow the same way.
    match parse_indirect_reference(b"52 4294967296 R") {
        IndirectReferenceParse::Overflow(message) => {
            assert_eq!(
                message,
                "integer out of range converting 4294967296 from a 8-byte \
                 signed type to a 4-byte signed type"
            );
        }
        other => panic!("expected Overflow, got {other:?}"),
    }
}

#[test]
fn qpdf_json_validators_reject_a_generation_too_large_for_flpdf_object_ref() {
    // qpdf's generation is a bare `int` with no upper bound beyond i32
    // (`is_indirect_object` only checks `obj > 0`, never `gen`'s range).
    // flpdf's `ObjectRef` generation is `u16`, matching the PDF xref
    // table's actual generation width; a generation that fits i32 but not
    // u16 has no representable PDF object identity, so it is not a match here.
    assert_eq!(
        parse_indirect_reference(b"52 99999 R"),
        IndirectReferenceParse::NoMatch
    );
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
fn qpdf_json_value_factory_preserves_real_literals_and_never_rejects_non_finite_numbers() {
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

    // qpdf's real branch (`QPDF_json.cc:750-764`) only reformats scientific
    // notation through `std::stod`, and catches a `stod` failure --
    // including overflow to infinity -- keeping the original literal
    // unchanged. `QPDF_Real::create` (`QPDF_Real.cc:17-20`) never validates
    // the text numerically, so qpdf never rejects a syntactically valid
    // JSON number here, however large.
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("1e9999"))
            .expect("overflowing scientific real is preserved, not rejected")
            .as_real_literal(),
        Some((f64::INFINITY, b"1e9999".to_vec()))
    );
}

#[test]
fn qpdf_json_value_factory_rejects_non_utf8_number_bytes() {
    // `Json::make_number` accepts arbitrary bytes, bypassing the real JSON
    // tokenizer's number grammar (which only ever produces ASCII). This
    // exercises `json_number_to_handle`'s own UTF-8 guard, which real
    // parsing can never reach.
    let mut pdf = Pdf::empty().expect("empty PDF");
    let error =
        json_value_to_handle(&mut pdf, &Json::make_number(b"\xff\xfe")).expect_err("non-UTF-8");
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
fn qpdf_json_value_factory_preserves_unparseable_scientific_notation_literals() {
    let mut pdf = Pdf::empty().expect("empty PDF");

    // "1e+" is not a valid `f64` literal (`std::stod` would also fail on
    // it), but per the same oracle evidence as Finding C
    // (`QPDF_json.cc:750-764`, `QPDF_Real.cc:17-20`) a `stod` failure is
    // caught and the original text is kept unchanged: qpdf never rejects a
    // syntactically-present JSON number as a Real, however malformed its
    // scientific notation.
    let handle =
        json_value_to_handle(&mut pdf, &Json::make_number("1e+")).expect("preserved, not rejected");
    let (value, literal) = handle.as_real_literal().expect("real literal");
    assert!(value.is_nan());
    assert_eq!(literal, b"1e+");
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
    let provider = inline_stream_data_provider(Rc::clone(&source), &value, 0)
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
        let provider = inline_stream_data_provider(Rc::clone(&source), &value, 0)
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
    let provider = inline_stream_data_provider(source, &value, 0).expect("provider registration");
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
    let provider = inline_stream_data_provider(source, &value, 0).expect("provider registration");
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
        let error = inline_stream_data_provider(Rc::clone(&source), &value, 0)
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

    let error = stream
        .get_raw_stream_data()
        .expect_err("missing datafile must fail when the provider is piped");
    let Error::SystemBytes(message) = error else {
        panic!("expected a system error from the missing datafile");
    };
    assert_eq!(
        message,
        format!("open {}: No such file or directory", path.display()).into_bytes()
    );
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
                "obj:1 0 R": {
                    "value": {"/Type": "/Catalog", "/Pages": "2 0 R", "/Binary": "b:00ff"}
                },
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
        .windows(b"input.json, obj:1 0 R at offset".len())
        .any(|window| window == b"input.json, obj:1 0 R at offset"));
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
    assert_eq!(
        catalog_dict
            .get(b"/Binary".as_slice())
            .and_then(|value| value.as_string()),
        Some(vec![0, 0xff])
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
    // A brand-new stream (built by the `!is_stream` branch of the "stream"
    // key handler, matching `replace_object`'s "value" sibling) must carry
    // the same JSON-input description as any other freshly-set object,
    // not the generic "object N G" fallback used for a handle that never
    // received a description at all.
    assert!(stream
        .description()
        .windows(b"input.json, obj:3 0 R at offset".len())
        .any(|window| window == b"input.json, obj:3 0 R at offset"));

    let trailer = pdf.trailer();
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
fn json_reactor_treats_jsonversion_overflow_as_fatal_not_a_soft_error() {
    // qpdf's `jsonversion` handler calls `QUtil::string_to_int` directly on
    // the raw number text (`QPDF_json.cc:518-531`). Once the shape check
    // (`value.getNumber`) accepts the token, an overflow there is an
    // uncaught `std::range_error`, not qpdf's own "invalid JSON version"
    // soft warning -- the two must not be conflated.
    let json = br#"{
        "qpdf": [
            {"jsonversion": 99999999999999999999, "pdfversion": "1.3"},
            {}
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "input.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    let fatal = reactor
        .fatal_error()
        .expect("jsonversion overflow must be fatal");
    assert!(
        fatal.contains("overflow/underflow converting"),
        "unexpected fatal message: {fatal}"
    );
    assert!(!reactor.any_errors(), "must not also record a soft error");
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
fn json_reactor_treats_object_key_overflow_as_fatal_not_a_soft_error() {
    // "obj:4294967296 0 R" has the exact `obj:N G R` shape
    // (`is_obj_key`/`is_indirect_object`, `QPDF_json.cc:66-113`), but the
    // object number overflows qpdf's i32 `QUtil::string_to_int` narrowing
    // stage. qpdf's own validator throws an uncaught `std::range_error` at
    // that point instead of returning `false`, so this key must not be
    // treated as merely "not `trailer` or `obj:n n R`".
    let mut pdf = Pdf::empty().expect("empty PDF");
    let json = br#"{
        "qpdf": [
            {"jsonversion": 2},
            {"obj:4294967296 0 R": {"value": 1}}
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "update.json", false);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
    let fatal = reactor
        .fatal_error()
        .expect("object key overflow must be fatal");
    assert!(
        fatal.contains("integer out of range converting"),
        "unexpected fatal message: {fatal}"
    );
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

#[test]
fn json_reactor_reports_root_metadata_and_container_shape_errors() {
    let cases = [
        (br#"{}"#.as_slice(), "\"qpdf\" object was not seen"),
        (br#"{"qpdf":1}"#.as_slice(), "\"qpdf\" must be an array"),
        (
            br#"{"qpdf":[1,{}]}"#.as_slice(),
            "\"qpdf[0]\" must be a dictionary",
        ),
        (
            br#"{"qpdf":[{},1]}"#.as_slice(),
            "\"qpdf[1]\" must be a dictionary",
        ),
        (
            br#"{"qpdf":[{}, {}, {}]}"#.as_slice(),
            "\"qpdf\" must have two elements",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"obj:1 0 R":1,"trailer":1}]}"#
                .as_slice(),
            "\"obj:1 0 R\" must be a dictionary",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"obj:1 0 R":{"stream":1}}]}"#
                .as_slice(),
            "\"stream\" must be a dictionary",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"trailer":{}}]}"#.as_slice(),
            "\"trailer\" is missing \"value\"",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"trailer":{"stream":1}}]}"#
                .as_slice(),
            "the trailer may not be a stream",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.x"},{"trailer":{"value":{}}}]}"#
                .as_slice(),
            "invalid PDF version",
        ),
    ];

    for (json, expected) in cases {
        let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
        let mut pdf = Pdf::empty().expect("empty PDF");
        let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "shape.json", true);
        parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
        assert!(reactor.any_errors(), "expected an error for {json:?}");
        drop(reactor);
        assert!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "expected {expected:?} in diagnostics for {json:?}: {:?}",
            pdf.repair_diagnostics().entries()
        );
    }
}

#[test]
fn json_reactor_handles_flag_values_and_invalid_stream_members() {
    let json = br#"{
        "qpdf": [
            {
                "jsonversion": 2,
                "pdfversion": "1.3",
                "calledgetallpages": false,
                "pushedinheritedpageresources": false
            },
            {
                "obj:1 0 R": {
                    "stream": {
                        "dict": 1,
                        "data": 1,
                        "datafile": 1
                    }
                },
                "trailer": {"value": {}}
            }
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "shape.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(reactor.any_errors());
    drop(reactor);

    let invalid_flags = br#"{
        "qpdf": [
            {
                "jsonversion": 2,
                "pdfversion": "1.3",
                "calledgetallpages": "yes",
                "pushedinheritedpageresources": 1
            },
            {"trailer": {"value": {}}}
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(invalid_flags.to_vec())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "shape.json", false);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
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
        .any(|message| message.contains("calledgetallpages must be a boolean")));
    assert!(messages
        .iter()
        .any(|message| message.contains("pushedinheritedpageresources must be a boolean")));
    assert!(messages.iter().any(|message| message
        .contains("new \"stream\" must have exactly one of \"data\" or \"datafile\"")));
    assert!(messages
        .iter()
        .any(|message| message.contains("\"stream.data\" must be a string")));
    assert!(messages
        .iter()
        .any(|message| message.contains("\"stream.datafile\" must be a string")));
    assert!(messages
        .iter()
        .any(|message| message.contains("\"stream.dict\" must be a dictionary")));
}

fn excessive_page_tree_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let depth = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH + 1;
    let leaf_num = 2 + depth as u32;
    let mut offsets = Vec::with_capacity(1 + depth + 1);

    offsets.push(pdf.len() as u64);
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    for level in 0..depth {
        let this_num = 2 + level as u32;
        let next_ref = if level + 1 == depth {
            leaf_num
        } else {
            this_num + 1
        };
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "{this_num} 0 obj\n<< /Type /Pages /Kids [{next_ref} 0 R] /Count 1 >>\nendobj\n"
            )
            .as_bytes(),
        );
    }
    offsets.push(pdf.len() as u64);
    pdf.extend_from_slice(
        format!(
            "{leaf_num} 0 obj\n<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
            leaf_num - 1
        )
        .as_bytes(),
    );

    let total = offsets.len() + 1;
    let xref_start = pdf.len() as u64;
    pdf.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    pdf
}

fn malformed_object_pdf_bytes() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let object_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /A [\nendobj\n");
    let xref_offset = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn json_reactor_reports_a_lazy_source_object_resolution_error() {
    let json = br#"{
        "qpdf": [
            {"jsonversion": 2, "pdfversion": "1.3"},
            {
                "obj:1 0 R": {"value": 1},
                "trailer": {"value": {}}
            }
        ]
    }"#;
    let fail = Rc::new(Cell::new(false));
    let reader = ToggleReader {
        cursor: Cursor::new(malformed_object_pdf_bytes()),
        fail: Rc::clone(&fail),
    };
    let mut pdf = Pdf::open(reader).expect("PDF xref");
    fail.set(true);
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "broken.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(reactor.fatal_error().is_some());
}

#[test]
fn json_reactor_propagates_page_observation_failures() {
    for flag in ["calledgetallpages", "pushedinheritedpageresources"] {
        let mut pdf =
            Pdf::open(Cursor::new(excessive_page_tree_pdf_bytes())).expect("deep page-tree PDF");
        let json = format!("{{\"qpdf\":[{{\"jsonversion\":2,\"{flag}\":true}},{{}}]}}");
        let source = Rc::new(RefCell::new(Cursor::new(json.into_bytes())));
        let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "pages.json", false);
        parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON update");
        assert!(reactor
            .fatal_error()
            .is_some_and(|message| message.contains("page tree depth exceeds")));
    }
}

#[test]
fn json_reactor_handles_factory_and_dictionary_key_failures() {
    let cases = [
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"obj:1 0 R":{"value":"n:/A#"}}]}"#.as_slice(),
            "PDF name",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"obj:1 0 R":{"value":"4294967296 0 R"}}]}"#.as_slice(),
            "integer out of range",
        ),
        (
            br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"obj:1 0 R":{"value":{"n:/A#":1}}}]}"#.as_slice(),
            "invalid",
        ),
    ];

    for (json, _expected) in cases {
        let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
        let mut pdf = Pdf::empty().expect("empty PDF");
        let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "factory.json", true);
        parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
        assert!(reactor.any_errors() || reactor.fatal_error().is_some());
        assert!(
            reactor.fatal_error().is_some(),
            "factory failure must be fatal"
        );
        drop(reactor);
    }
}

#[test]
fn json_reactor_rejects_an_indirect_object_value() {
    let json = br#"{
        "qpdf": [
            {"jsonversion": 2, "pdfversion": "1.3"},
            {
                "obj:1 0 R": {"value": "12 0 R"},
                "trailer": {"value": {}}
            }
        ]
    }"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "indirect.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(reactor.any_errors());
    drop(reactor);
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic
            .message
            .contains("value of an object may not be an indirect object reference")));
}

#[test]
fn json_reactor_captures_callback_failures_and_uninitialized_callbacks() {
    let source = Rc::new(RefCell::new(Cursor::new(Vec::new())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "callbacks.json", false);
    assert!(Reactor::dictionary_item(
        &mut reactor,
        b"ignored",
        &Json::make_number("1")
    ));
    drop(reactor);

    let source = Rc::new(RefCell::new(Cursor::new(Vec::new())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "callbacks.json", false);
    assert!(Reactor::array_item(&mut reactor, &Json::make_number("1")));
    drop(reactor);

    let source = Rc::new(RefCell::new(Cursor::new(Vec::new())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "callbacks.json", false);
    Reactor::container_end(&mut reactor, &Json::default());
    drop(reactor);

    let source = Rc::new(RefCell::new(Cursor::new(Vec::new())));
    let mut pdf = Pdf::empty().expect("empty PDF");
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "callbacks.json", false);
    Reactor::top_level_scalar(&mut reactor);
    Reactor::array_start(&mut reactor);
    assert_eq!(
        reactor.fatal_error(),
        Some("QPDF JSON must be a dictionary")
    );
}

#[test]
fn json_reactor_propagates_warning_logger_failures() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(
        crate::pipeline::test_support::NthWriteFailure::new(1),
    )));
    let mut pdf = Pdf::empty().expect("empty PDF");
    pdf.set_logger(logger);
    let json = br#"{}"#;
    let source = Rc::new(RefCell::new(Cursor::new(json.to_vec())));
    let mut reactor = JsonReactor::new(&mut pdf, Rc::clone(&source), "failure.json", true);
    parse_reader(&mut *source.borrow_mut(), Some(&mut reactor)).expect("JSON input");
    assert!(reactor.any_errors());
    assert!(reactor
        .fatal_error()
        .is_some_and(|message| message.contains("sink write failure 1")));
}

#[test]
fn json_warning_route_preserves_qpdf_context_and_suppression() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(usize::MAX))));
    let mut pdf = Pdf::empty().expect("empty PDF");
    pdf.set_logger(logger);
    pdf.resolver
        .push_json_warning("", "", 5, "positive")
        .expect("warning delivery");
    pdf.resolver
        .push_json_warning("", "", 0, "zero")
        .expect("warning delivery");
    pdf.resolver
        .push_json_warning("", "obj:1 0 R", -1, "negative")
        .expect("warning delivery");
    pdf.resolver
        .push_json_warning("", "obj:1 0 R", 5, "object")
        .expect("warning delivery");
    pdf.set_suppress_warnings(true);
    pdf.resolver
        .push_json_warning("", "", 0, "suppressed")
        .expect("suppressed warning collection");
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message == "suppressed"));

    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(usize::MAX))));
    let options = PdfOpenOptions {
        logger: Some(logger),
        description: b"document.pdf".to_vec(),
        ..PdfOpenOptions::default()
    };
    let pdf_bytes = include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec();
    let pdf = Pdf::open_with_options(Cursor::new(pdf_bytes), options).expect("minimal PDF");
    pdf.resolver
        .push_json_warning("document.pdf", "", 5, "named positive")
        .expect("warning delivery");
    pdf.resolver
        .push_json_warning("document.pdf", "", 0, "named zero")
        .expect("warning delivery");
    pdf.resolver
        .push_json_warning("document.pdf", "obj:1 0 R", 5, "named object")
        .expect("warning delivery");
}

#[test]
fn json_reactor_handles_stream_dictionary_boundary_errors() {
    let stream = ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new()));
    let error = stream
        .replace_stream_dict(ObjectHandle::integer(1))
        .expect_err("non-dictionary replacement");
    assert!(error.to_string().contains("non-dictionary"));

    let scalar = ObjectHandle::integer(1);
    let error = scalar
        .replace_stream_dict(ObjectHandle::dictionary(Vec::new()))
        .expect_err("scalar replacement");
    assert!(error.to_string().contains("operation for stream"));
}
