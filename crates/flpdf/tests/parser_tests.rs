use flpdf::{parse_object, Dictionary, Error, Object, ObjectRef};

fn parsed_stream_data(input: &[u8]) -> Vec<u8> {
    parse_object(input)
        .expect("stream must parse")
        .into_stream()
        .expect("expected stream")
        .data
}

#[test]
fn public_parser_recovers_indirect_stream_length() {
    assert_eq!(
        parsed_stream_data(b"<< /Length 9 0 R >>\nstream\nabc\nendstream"),
        b"abc"
    );
}

#[test]
fn public_parser_accepts_a_comment_before_the_stream_keyword() {
    assert_eq!(
        parsed_stream_data(b"<< /Length 3 >> % comment\r\nstream\nabcendstream"),
        b"abc"
    );
}

#[test]
fn public_parser_ignores_inline_endstream_tokens_during_recovery() {
    assert_eq!(
        parsed_stream_data(b"<< /Length 9 0 R >>\nstream\nabc endstream def\nendstream"),
        b"abc endstream def"
    );
}

#[test]
fn public_parser_rejects_truncated_stream_with_indirect_length() {
    let err = parse_object(b"<< /Length 9 0 R >>\nstream\nabc")
        .expect_err("truncated stream with an indirect length must error");

    assert!(
        matches!(err, Error::Parse { .. }),
        "expected Error::Parse, got {err:?}"
    );
}

#[test]
fn public_parser_recovers_missing_stream_length() {
    assert_eq!(parsed_stream_data(b"<< >>\nstream\nabc\nendstream"), b"abc");
}

#[test]
fn public_parser_recovers_non_integer_stream_length() {
    assert_eq!(
        parsed_stream_data(b"<< /Length /Bad >>\nstream\nabc\nendstream"),
        b"abc"
    );
}

#[test]
fn public_parser_rejects_mismatched_usable_direct_stream_length() {
    for input in [
        &b"<< /Length 1 >>\nstream\nabc\nendstream"[..],
        &b"<< /Length 5 >>\nstream\nabc\nendstream"[..],
    ] {
        let err =
            parse_object(input).expect_err("a usable direct /Length must define the boundary");
        assert!(
            matches!(err, Error::Parse { .. }),
            "expected Error::Parse, got {err:?}"
        );
    }
}

#[test]
fn parses_dictionary_with_reference() {
    let object = parse_object(b"<< /Type /Catalog /Pages 2 0 R >>").unwrap();

    let Object::Dictionary(dict) = object else {
        panic!("expected dictionary")
    };

    assert_eq!(dict.get("Type"), Some(&Object::Name(b"Catalog".to_vec())));
    assert_eq!(dict.get_ref("Pages"), Some(ObjectRef::new(2, 0)));
}

#[test]
fn parses_array_and_strings() {
    let object = parse_object(b"[1 true false null (hello)]").unwrap();
    assert_eq!(
        object,
        Object::Array(vec![
            Object::Integer(1),
            Object::Boolean(true),
            Object::Boolean(false),
            Object::Null,
            Object::String(b"hello".to_vec()),
        ])
    );
}

#[test]
fn parses_real_numbers() {
    let object = parse_object(b"[0 0 595.28 841.89 -0.5 .75 1.  +.25 -1.5]").unwrap();

    let Object::Array(values) = object else {
        panic!("expected array")
    };

    // Non-canonical source literals (leading dot, trailing dot, explicit `+`,
    // explicit signs) round-trip through [`Object::RealLiteral`] which
    // preserves the source bytes verbatim; canonical forms (`595.28`,
    // `841.89`, `-0.5`, `-1.5`) stay as [`Object::Real`].
    let lit = |v: f64, s: &[u8]| Object::RealLiteral {
        value: v,
        literal: s.to_vec(),
    };
    assert_eq!(values[0], Object::Integer(0));
    assert_eq!(values[1], Object::Integer(0));
    assert_eq!(values[2], Object::Real(595.28));
    assert_eq!(values[3], Object::Real(841.89));
    assert_eq!(values[4], Object::Real(-0.5));
    assert_eq!(values[5], lit(0.75, b".75"));
    assert_eq!(values[6], lit(1.0, b"1."));
    assert_eq!(values[7], lit(0.25, b"+.25"));
    assert_eq!(values[8], Object::Real(-1.5));
}

#[test]
fn parses_qpdf_good13_shaped_nesting_strings_and_names() {
    let object = parse_object(
        br#"<<
          /strings [(one) ($\242) () (()) (\() (\)) (a\f\b\t\r\nb) (A\000B)]
          /hex#20strings [<506F7461746f> <010 203 0004056> <41
42>]
          /n#65sting <<
            /a [1 2 << /x (y) >> [(z)]]
            /b <</a [1 2] / (legal)>>
          >>
          /names [/n#65sting /hex#20strings /text#2fplain]
        >>"#,
    )
    .unwrap();
    let dict = object.as_dict().unwrap();
    assert_eq!(
        dict.get("hex strings"),
        Some(&Object::Array(vec![
            Object::String(b"Potato".to_vec()),
            Object::String(b"\x01\x02\x03\x00\x04\x05\x60".to_vec()),
            Object::String(b"AB".to_vec()),
        ]))
    );
    assert_eq!(
        dict.get("names"),
        Some(&Object::Array(vec![
            Object::Name(b"nesting".to_vec()),
            Object::Name(b"hex strings".to_vec()),
            Object::Name(b"text/plain".to_vec()),
        ]))
    );
    let nesting = dict.get("nesting").unwrap().as_dict().unwrap();
    let b_dict = nesting.get("b").unwrap().as_dict().unwrap();
    assert_eq!(b_dict.get(""), Some(&Object::String(b"legal".to_vec())));
}

#[test]
fn qpdf_empty_name_is_valid() {
    assert_eq!(parse_object(b"/").unwrap(), Object::Name(Vec::new()));
}

#[test]
fn qpdf_literal_string_normalizes_unescaped_line_endings() {
    assert_eq!(
        parse_object(b"(a\rb\r\nc)").unwrap(),
        Object::String(b"a\nb\nc".to_vec())
    );
}

#[test]
fn qpdf_exponent_looking_token_is_not_a_real() {
    assert!(parse_object(b"1e3").is_err());
}

#[test]
fn unterminated_array_reports_an_error() {
    assert!(parse_object(b"[1").is_err());
}

#[test]
fn dictionary_type_is_exported_for_downstream_code() {
    let dict = Dictionary::new();
    assert_eq!(dict.iter().count(), 0);
}

#[test]
fn parses_dictionary_with_stream() {
    let object = parse_object(b"<< /Type /XRef /Length 5 >>\nstream\nhello\nendstream").unwrap();

    let Object::Stream(stream) = object else {
        panic!("expected stream")
    };

    assert_eq!(
        stream.dict.get("Type"),
        Some(&Object::Name(b"XRef".to_vec()))
    );
    assert_eq!(stream.data, b"hello".to_vec());
}

#[test]
fn parses_hex_string() {
    let object = parse_object(b"<48656C6C6F>").unwrap();
    assert_eq!(object, Object::String(b"Hello".to_vec()));
}

#[test]
fn parses_escaped_name() {
    let object = parse_object(b"/Hello#20World").unwrap();
    assert_eq!(object, Object::Name(b"Hello World".to_vec()));
}

#[test]
fn parses_literal_string_escapes_and_octal() {
    let object = parse_object(b"(Hello\\nWorld\\041)").unwrap();
    assert_eq!(object, Object::String(b"Hello\nWorld!".to_vec()));
}

#[test]
fn parses_literal_string_newline_continuation() {
    let object = parse_object(b"(a\\\nb)").unwrap();
    assert_eq!(object, Object::String(b"ab".to_vec()));
}

#[test]
fn parses_dictionary_with_odd_whitespace() {
    let object = parse_object(b"[ 1\t0\r\n2\t3 ]").unwrap();
    assert_eq!(
        object,
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(0),
            Object::Integer(2),
            Object::Integer(3)
        ])
    );
}

#[test]
fn parses_true_false_null_boundaries() {
    assert_eq!(parse_object(b"true").unwrap(), Object::Boolean(true));
    assert_eq!(parse_object(b"false").unwrap(), Object::Boolean(false));
    assert_eq!(parse_object(b"null").unwrap(), Object::Null);
    assert!(parse_object(b"truex").is_err());
    assert!(parse_object(b"null?").is_err());
}

#[test]
fn skips_comments_between_tokens() {
    let object = parse_object(b"[1%comment\n2% again\n 3]").unwrap();
    assert_eq!(
        object,
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(2),
            Object::Integer(3)
        ])
    );
}

// Nesting-depth limit (qpdf CVE-2018-9918 class). The recursive descent parser
// (`object` -> `dictionary`/`array` -> `object`) would otherwise overflow the
// stack and abort — an uncatchable failure — on adversarially deep input. The
// limit (`MAX_PARSE_DEPTH`, 500, matching qpdf's `parser_max_nesting` region)
// must reject such input with a catchable `Error::Parse`.

/// An array nested exactly at the depth limit still parses. Guards against an
/// off-by-one that would reject legitimate (if deep) documents.
#[test]
fn accepts_array_nesting_at_limit() {
    let depth = 500;
    let mut input = vec![b'['; depth];
    input.extend(std::iter::repeat_n(b']', depth));

    assert!(
        parse_object(&input).is_ok(),
        "array nested {depth} deep (at the limit) must parse"
    );
}

/// An array nested one level past the limit is rejected with a catchable
/// `Error::Parse`, not a panic or abort.
#[test]
fn rejects_array_nesting_over_limit() {
    let depth = 501;
    let mut input = vec![b'['; depth];
    input.extend(std::iter::repeat_n(b']', depth));

    let err = parse_object(&input).expect_err("over-limit array nesting must error");
    assert!(
        matches!(err, Error::Parse { .. }),
        "expected Error::Parse, got {err:?}"
    );
}

/// Pathologically deep array nesting returns an error instead of overflowing
/// the stack (which would abort the process, uncatchable by callers).
#[test]
fn rejects_deeply_nested_arrays_without_stack_overflow() {
    let depth = 100_000;
    let mut input = vec![b'['; depth];
    input.extend(std::iter::repeat_n(b']', depth));

    assert!(
        parse_object(&input).is_err(),
        "deeply nested arrays must error, not abort"
    );
}

/// A dictionary nested well within the depth limit parses normally, confirming
/// the limit does not regress ordinary nested documents.
#[test]
fn accepts_dictionary_nesting_within_limit() {
    let depth = 100;
    let mut input = b"<</K ".repeat(depth);
    input.extend_from_slice(b"0");
    input.extend(b">>".repeat(depth));

    assert!(
        parse_object(&input).is_ok(),
        "dictionary nested {depth} deep (within the limit) must parse"
    );
}

/// Pathologically deep dictionary nesting returns an error instead of
/// overflowing the stack. `dictionary` is a distinct recursion site from
/// `array`, so it needs its own deep-nesting guard.
#[test]
fn rejects_deeply_nested_dictionaries_without_stack_overflow() {
    let depth = 100_000;
    let mut input = b"<</K ".repeat(depth);
    input.extend_from_slice(b"0");
    input.extend(b">>".repeat(depth));

    assert!(
        parse_object(&input).is_err(),
        "deeply nested dictionaries must error, not abort"
    );
}
