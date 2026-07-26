use flpdf::json::Json;
use std::io::{self, Cursor, Read};

#[test]
fn parser_preserves_number_token_and_offsets() {
    let value = Json::parse(b" \n-2.10E+05\t").unwrap();
    assert_eq!(value.get_number().as_deref(), Some(b"-2.10E+05".as_slice()));
    assert_eq!((value.start(), value.end()), (2, 11));
}

#[test]
fn parser_rejects_material_after_top_level_value() {
    let error = Json::parse(b"null true").unwrap_err();
    assert_eq!(
        error.to_string(),
        "JSON: offset 9: material follows end of object: true"
    );
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
fn parser_defers_container_grammar_to_task_six() {
    for input in [b"{".as_slice(), b"}".as_slice(), b"[", b"]", b":", b","] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            "JSON: offset 1: premature end of input",
            "{input:?}"
        );
    }
}

#[test]
fn parser_lexes_scalar_delimiters_before_task_six_handles_them() {
    for input in [b"1,".as_slice(), b"1:", b"1{", b"1}", b"1[", b"1]"] {
        assert_eq!(
            Json::parse(input).unwrap_err().to_string(),
            "JSON: offset 2: material follows end of object: ",
            "{input:?}"
        );
    }

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

    assert_eq!(
        Json::parse(b"0.1").unwrap().get_number(),
        Some(b"0.1".to_vec())
    );
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
