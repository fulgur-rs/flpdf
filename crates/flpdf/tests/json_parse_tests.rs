use flpdf::json::{Json, Reactor};
use std::io::{self, Cursor, Read};

#[derive(Default)]
struct RecordingReactor {
    events: Vec<String>,
    item_offsets: Vec<(i64, i64)>,
}

impl Reactor for RecordingReactor {
    fn dictionary_start(&mut self) {
        self.events.push("dict-start".into());
    }

    fn array_start(&mut self) {
        self.events.push("array-start".into());
    }

    fn container_end(&mut self, value: &Json) {
        self.events.push(format!("end:{}", value.end()));
    }

    fn top_level_scalar(&mut self) {
        self.events.push("scalar".into());
    }

    fn dictionary_item(&mut self, key: &[u8], value: &Json) -> bool {
        self.item_offsets.push((value.start(), value.end()));
        self.events.push(format!(
            "dict-item:{}:{}",
            String::from_utf8_lossy(key),
            value
                .unparse()
                .map(|value| String::from_utf8_lossy(&value).into_owned())
                .unwrap()
        ));
        key != b"keep"
    }

    fn array_item(&mut self, value: &Json) -> bool {
        self.item_offsets.push((value.start(), value.end()));
        self.events.push(format!(
            "array-item:{}",
            String::from_utf8_lossy(&value.unparse().unwrap())
        ));
        true
    }
}

#[derive(Default)]
struct KeepingArrayReactor {
    events: Vec<String>,
}

impl Reactor for KeepingArrayReactor {
    fn dictionary_start(&mut self) {
        self.events.push("dict-start".into());
    }

    fn array_start(&mut self) {
        self.events.push("array-start".into());
    }

    fn container_end(&mut self, value: &Json) {
        self.events.push(format!("end:{}", value.end()));
    }

    fn top_level_scalar(&mut self) {
        self.events.push("scalar".into());
    }

    fn dictionary_item(&mut self, _: &[u8], _: &Json) -> bool {
        false
    }

    fn array_item(&mut self, value: &Json) -> bool {
        self.events.push(format!(
            "array-item:{}",
            String::from_utf8_lossy(&value.unparse().unwrap())
        ));
        false
    }
}

#[test]
fn reactor_parent_sees_empty_child_before_child_start() {
    let mut reader = Cursor::new(br#"{"drop":[1],"keep":2}"#.as_slice());
    let mut reactor = RecordingReactor::default();
    let value = Json::parse_reader(&mut reader, Some(&mut reactor)).unwrap();

    assert_eq!(
        reactor.events,
        [
            "dict-start",
            "dict-item:drop:[]",
            "array-start",
            "array-item:1",
            "end:11",
            "dict-item:keep:2",
            "end:21",
        ]
    );
    assert_eq!(reactor.item_offsets, [(8, 9), (9, 10), (19, 20)]);

    let mut members = Vec::new();
    assert!(value.for_each_dict_item(|key, value| {
        members.push((key.to_vec(), value.get_number()));
    }));
    assert_eq!(members, [(b"keep".to_vec(), Some(b"2".to_vec()))]);
}

#[test]
fn reactor_duplicate_detection_precedes_callback_for_consumed_items() {
    let mut reader = Cursor::new(br#"{"dup":1,"dup":2}"#.as_slice());
    let mut reactor = RecordingReactor::default();
    let error = Json::parse_reader(&mut reader, Some(&mut reactor)).unwrap_err();

    assert_eq!(
        error.to_string(),
        "JSON: offset 9: duplicated dictionary key"
    );
    assert_eq!(
        reactor.events,
        ["dict-start", "dict-item:dup:1"],
        "the duplicate value must not reach the callback"
    );
}

#[test]
fn reactor_reports_top_level_scalar_after_parsing() {
    let mut reader = Cursor::new(b"true".as_slice());
    let mut reactor = RecordingReactor::default();
    let value = Json::parse_reader(&mut reader, Some(&mut reactor)).unwrap();

    assert_eq!(value.get_bool(), Some(true));
    assert_eq!(reactor.events, ["scalar"]);
}

#[test]
fn reactor_array_item_false_keeps_the_item() {
    let mut reader = Cursor::new(b"[false]".as_slice());
    let mut reactor = KeepingArrayReactor::default();
    let value = Json::parse_reader(&mut reader, Some(&mut reactor)).unwrap();

    assert_eq!(reactor.events, ["array-start", "array-item:false", "end:7"]);
    let mut items = Vec::new();
    assert!(value.for_each_array_item(|value| items.push(value.get_bool())));
    assert_eq!(items, [Some(false)]);
}

#[test]
fn parser_preserves_number_token_and_offsets() {
    let value = Json::parse(b" \n-2.10E+05\t").unwrap();
    assert_eq!(value.get_number().as_deref(), Some(b"-2.10E+05".as_slice()));
    assert_eq!((value.start(), value.end()), (2, 11));
}

#[test]
fn parser_rejects_material_after_top_level_value() {
    for (input, expected) in [
        (
            b"null true".as_slice(),
            "JSON: offset 9: material follows end of object: true",
        ),
        (
            b"1{".as_slice(),
            "JSON: offset 2: material follows end of object: ",
        ),
        (
            b"1[".as_slice(),
            "JSON: offset 2: material follows end of object: ",
        ),
    ] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn parser_accepts_each_scalar_kind_and_decodes_string_escapes() {
    assert!(Json::parse(b"true").unwrap().get_bool().unwrap());
    assert!(!Json::parse(b"false").unwrap().get_bool().unwrap());
    assert!(Json::parse(b"null").unwrap().is_null());

    let string = Json::parse(br#""line\n\u03c0""#).unwrap();
    assert_eq!(string.get_string().as_deref(), Some("line\nπ".as_bytes()));
    assert_eq!((string.start(), string.end()), (0, 14));
}

#[test]
fn parser_reports_qpdf_scalar_lexical_errors() {
    for (input, expected) in [
        (b"".as_slice(), "JSON: premature end of input"),
        (b"\"".as_slice(), "JSON: offset 1: premature end of input"),
        (b"01".as_slice(), "JSON: offset 1: number with leading zero"),
        (b"1.".as_slice(), "JSON: premature end of input"),
        (b"1e+".as_slice(), "JSON: premature end of input"),
        (b"truth".as_slice(), "JSON: offset 5: invalid keyword truth"),
        (
            b"1x".as_slice(),
            "JSON: offset 1: numeric literal: unexpected character x",
        ),
        (b"+1".as_slice(), "JSON: offset 0: unexpected character +"),
        (b"\"unterminated".as_slice(), "JSON: premature end of input"),
    ] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn parser_matches_qpdf_scalar_lexer_error_corpus() {
    for (input, expected) in [
        (
            b"\0".as_slice(),
            "JSON: control or null character at offset 0",
        ),
        (
            b"3.14e5.6".as_slice(),
            "JSON: offset 6: numeric literal: decimal point after e",
        ),
        (
            b"3.14.159".as_slice(),
            "JSON: offset 4: numeric literal: decimal point already seen",
        ),
        (
            b"3e4e5".as_slice(),
            "JSON: offset 3: numeric literal: e already seen",
        ),
        (
            b"3+4".as_slice(),
            "JSON: offset 1: numeric literal: unexpected sign",
        ),
        (
            b"abc1".as_slice(),
            "JSON: offset 3: keyword: unexpected character 1",
        ),
        (
            br#""abc\yd""#.as_slice(),
            "JSON: offset 5: invalid character after backslash: y",
        ),
        (
            b"\"abc`\n".as_slice(),
            "JSON: offset 5: control character in string (missing \"?)",
        ),
        (
            b"- ".as_slice(),
            "JSON: offset 1: numeric literal: incomplete number",
        ),
        (
            b"123e ".as_slice(),
            "JSON: offset 4: numeric literal: incomplete number",
        ),
        (
            br#""a\u123x""#.as_slice(),
            "JSON: offset 3: \\u must be followed by four hex digits",
        ),
        (
            br#""\uDC00""#.as_slice(),
            "JSON: offset 1: UTF-16 low surrogate found not immediately after high surrogate",
        ),
        (
            br#""\uD800x\uDC00""#.as_slice(),
            "JSON: offset 8: UTF-16 low surrogate found not immediately after high surrogate",
        ),
        (
            br#""\uD800\uD800""#.as_slice(),
            "JSON: offset 7: UTF-16 high surrogate found after previous high surrogate at offset 1",
        ),
        (
            br#""\uD800""#.as_slice(),
            "JSON: offset 1: UTF-16 high surrogate not followed by low surrogate",
        ),
    ] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn parser_accepts_qpdf_number_forms_and_all_string_escape_forms() {
    for number in [b"0".as_slice(), b"-0", b"0e1", b"3.14", b"3E-4"] {
        assert_eq!(
            Json::parse(number).unwrap().get_number().as_deref(),
            Some(number)
        );
    }

    let escaped = Json::parse(br#"" ,:{}[]\\\"\/\b\f\n\r\t\u00Af\u00AF""#).unwrap();
    assert_eq!(
        escaped.get_string(),
        Some(vec![
            b' ', b',', b':', b'{', b'}', b'[', b']', b'\\', b'"', b'/', 8, 12, 10, 13, 9, 0xc2,
            0xaf, 0xc2, 0xaf,
        ])
    );

    assert_eq!(
        Json::parse(br#""\uD83D\uDE00""#).unwrap().get_string(),
        Some("😀".as_bytes().to_vec())
    );
}

#[test]
fn parser_accepts_qpdf_zero_sentinel_low_surrogate_at_offset_six() {
    let value = Json::parse(br#""aaaaa\uDC00""#).unwrap();
    assert_eq!(value.get_string(), Some("aaaaa𐀀".as_bytes().to_vec()));
    assert_eq!((value.start(), value.end()), (0, 13));
}

#[test]
fn parser_decodes_escapes_and_utf16_surrogate_pairs_in_dictionary_values() {
    let value = Json::parse(br#"{"x":"\u03c0 \ud83e\udd54"}"#).unwrap();
    let string = value.get_dict_item(b"x").get_string().unwrap();
    assert_eq!(string, "π 🥔".as_bytes());
}

#[test]
fn parser_builds_nested_container_tree_and_preserves_exclusive_offsets() {
    let value = Json::parse(br#"{"items":[true,{"line\n":null}],"number":-2}"#).unwrap();
    assert!(value.is_dictionary());
    assert_eq!((value.start(), value.end()), (0, 44));

    let items = value.get_dict_item(b"items");
    assert!(items.is_array());
    assert_eq!((items.start(), items.end()), (9, 31));

    let mut array = Vec::new();
    assert!(items.for_each_array_item(|item| array.push(item)));
    assert_eq!(array.len(), 2);
    assert_eq!(array[0].get_bool(), Some(true));
    let mut nested_keys = Vec::new();
    assert!(array[1].for_each_dict_item(|key, value| {
        nested_keys.push(key.to_vec());
        assert!(value.is_null());
    }));
    assert_eq!(nested_keys, [b"line\\n".to_vec()]);
    assert_eq!(
        value.get_dict_item(b"number").get_number(),
        Some(b"-2".to_vec())
    );
}

#[test]
fn parser_accepts_empty_root_containers_with_exclusive_offsets() {
    let dictionary = Json::parse(b"{}").unwrap();
    assert!(dictionary.is_dictionary());
    assert_eq!((dictionary.start(), dictionary.end()), (0, 2));

    let array = Json::parse(b"[]").unwrap();
    assert!(array.is_array());
    assert_eq!((array.start(), array.end()), (0, 2));
}

#[test]
fn parser_preserves_literal_high_bit_string_bytes_in_containers() {
    let value = Json::parse(b"{\"x\":\"\x80\"}").unwrap();
    assert_eq!(value.get_dict_item(b"x").get_string(), Some(vec![0x80]));
}

#[test]
fn parser_rejects_duplicate_key_even_when_spelling_uses_escape() {
    let error = Json::parse(br#"{"a":1,"\u0061":2}"#).unwrap_err();
    assert_eq!(
        error.to_string(),
        "JSON: offset 7: duplicated dictionary key"
    );
}

#[test]
fn parser_reports_qpdf_container_grammar_errors() {
    for (input, expected) in [
        (b":".as_slice(), "JSON: offset 1: unexpected colon"),
        (br#"{"x" "y"}"#.as_slice(), "JSON: offset 8: expected ':'"),
        (
            br#"{"x":3 "y"}"#.as_slice(),
            "JSON: offset 10: expected ',' or '}'",
        ),
        (
            br#"["x" "y"]"#.as_slice(),
            "JSON: offset 8: expected ',' or ']'",
        ),
        (
            br#"{5:5}"#.as_slice(),
            "JSON: offset 2: expect string as dictionary key",
        ),
        (
            br#"["a"}"#.as_slice(),
            "JSON: offset 5: unexpected dictionary end delimiter",
        ),
        (br#"[,]"#.as_slice(), "JSON: offset 2: unexpected comma"),
    ] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn parser_limits_nesting_to_qpdf_maximum_depth() {
    let mut input = vec![b'['; 501];
    input.extend(vec![b']'; 501]);
    assert_eq!(
        Json::parse(&input).unwrap_err().to_string(),
        "JSON: offset 501: maximum object depth exceeded"
    );
}

#[test]
fn parser_lexes_scalar_delimiters_inside_container_grammar() {
    for (input, expected) in [
        (
            b"[1,]".as_slice(),
            "JSON: offset 4: unexpected array end delimiter",
        ),
        (
            b"{\"x\":}".as_slice(),
            "JSON: offset 6: unexpected dictionary end delimiter",
        ),
    ] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            expected,
            "{input:?}"
        );
    }

    assert_eq!(Json::parse(b"[0.1]").unwrap().start(), 0);
}

#[test]
fn parser_reports_scalar_delimiter_and_numeric_errors_after_container_support() {
    for (input, expected) in [
        (
            b"-x".as_slice(),
            "JSON: offset 1: numeric literal: no digit after minus sign",
        ),
        (
            b"1.x".as_slice(),
            "JSON: offset 2: numeric literal: unexpected character x",
        ),
        (
            b"1ex".as_slice(),
            "JSON: offset 2: numeric literal: unexpected character x",
        ),
        (
            b"1e+x".as_slice(),
            "JSON: offset 3: numeric literal: unexpected character x",
        ),
    ] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            expected,
            "{input:?}"
        );
    }
}

struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("reader failure"))
    }
}

#[test]
fn parser_reads_scalars_from_readers_and_preserves_io_errors() {
    let mut reader = Cursor::new(b"  42\n".as_slice());
    let value = Json::parse_reader(&mut reader, None).unwrap();
    assert_eq!(value.get_number().as_deref(), Some(b"42".as_slice()));
    assert_eq!((value.start(), value.end()), (2, 4));

    let error = Json::parse_reader(&mut FailingReader, None).unwrap_err();
    assert_eq!(error.to_string(), "reader failure");
}
