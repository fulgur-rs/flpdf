//! Callback, operation-adapter, and normalization tests for content streams.

use flpdf::content_stream::normalize_content_stream;
use flpdf::{
    parse_content_operations, parse_content_stream_data, Error, Object, ParseControl,
    ParserCallbacks,
};

#[derive(Default)]
struct RecordingCallbacks {
    size: Option<usize>,
    objects: Vec<(Object, usize, usize)>,
    eof: bool,
    stop_after: Option<usize>,
}

impl ParserCallbacks for RecordingCallbacks {
    fn content_size(&mut self, size: usize) -> flpdf::Result<()> {
        self.size = Some(size);
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        length: usize,
    ) -> flpdf::Result<ParseControl> {
        self.objects.push((object, offset, length));
        Ok(if self.stop_after == Some(self.objects.len()) {
            ParseControl::Stop
        } else {
            ParseControl::Continue
        })
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        self.eof = true;
        Ok(())
    }
}

#[derive(Default)]
struct DefaultContentSizeCallbacks {
    eof: bool,
}

impl ParserCallbacks for DefaultContentSizeCallbacks {
    fn handle_object(
        &mut self,
        _object: Object,
        _offset: usize,
        _length: usize,
    ) -> flpdf::Result<ParseControl> {
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        self.eof = true;
        Ok(())
    }
}

#[test]
fn content_module_has_no_independent_lexer_helpers() {
    let source = include_str!("../src/content_stream.rs");
    for forbidden in [
        "skip_ws_collect_comment",
        "read_keyword",
        "at_operand_start",
        "starts_number_token",
        "fn parse_inline_image",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy lexical helper remains: {forbidden}"
        );
    }
}

#[test]
fn operation_adapter_groups_objects_without_lexing_bytes() {
    let mut seen = Vec::new();
    parse_content_operations(b"1 2 cm q", |operands, operator| {
        seen.push((operands.to_vec(), operator.to_vec()));
        Ok(ParseControl::Continue)
    })
    .unwrap();

    assert_eq!(
        seen,
        vec![
            (vec![Object::Integer(1), Object::Integer(2)], b"cm".to_vec()),
            (vec![], b"q".to_vec()),
        ]
    );
}

#[test]
fn operation_adapter_recovers_at_parser_token_boundaries() {
    let mut operators = Vec::new();
    parse_content_operations(b"1 2 add } 3 4 sub", |_, operator| {
        operators.push(operator.to_vec());
        Ok(ParseControl::Continue)
    })
    .unwrap();

    assert_eq!(operators, vec![b"add".to_vec(), b"sub".to_vec()]);
}

#[test]
fn operation_adapter_ignores_inline_image_payload_events() {
    let mut seen = Vec::new();
    parse_content_operations(b"BI /CS /RGB ID payload EI q", |operands, operator| {
        seen.push((operands.to_vec(), operator.to_vec()));
        Ok(ParseControl::Continue)
    })
    .unwrap();

    assert_eq!(
        seen,
        vec![
            (vec![], b"BI".to_vec()),
            (
                vec![Object::Name(b"CS".to_vec()), Object::Name(b"RGB".to_vec())],
                b"ID".to_vec()
            ),
            (vec![], b"EI".to_vec()),
            (vec![], b"q".to_vec()),
        ]
    );
}

#[test]
fn default_content_size_callback_is_optional_and_empty_input_reports_eof() {
    let mut callbacks = DefaultContentSizeCallbacks::default();
    parse_content_stream_data(b"", &mut callbacks).unwrap();
    assert!(callbacks.eof);
}

#[test]
fn callbacks_receive_qpdf_object_offsets_lengths_and_eof() {
    let input = b"  1 2 cm\n";
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).unwrap();

    assert_eq!(callbacks.size, Some(input.len()));
    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Integer(1), 2, 1),
            (Object::Integer(2), 4, 1),
            (Object::Operator(b"cm".to_vec()), 6, 2),
        ]
    );
    assert!(callbacks.eof);
}

#[test]
fn callbacks_receive_nested_objects_at_the_probe_start_with_consumed_length() {
    let input = b"[1] <</K 2>> q";
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).unwrap();

    let mut dictionary = flpdf::Dictionary::new();
    dictionary.insert(b"K", Object::Integer(2));
    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Array(vec![Object::Integer(1)]), 0, 3),
            (Object::Dictionary(dictionary), 4, 8),
            (Object::Operator(b"q".to_vec()), 13, 1),
        ]
    );
}

#[test]
fn early_stop_skips_handle_eof_like_qpdf() {
    let mut callbacks = RecordingCallbacks {
        stop_after: Some(1),
        ..RecordingCallbacks::default()
    };
    parse_content_stream_data(b"1 2 cm", &mut callbacks).unwrap();

    assert_eq!(callbacks.objects.len(), 1);
    assert!(!callbacks.eof);
}

#[test]
fn callbacks_report_inline_image_as_a_separate_qpdf_object_event() {
    let input = b"BI /W 1 /H 1 ID x EI Q";
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).unwrap();

    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Operator(b"BI".to_vec()), 0, 2),
            (Object::Name(b"W".to_vec()), 3, 2),
            (Object::Integer(1), 6, 1),
            (Object::Name(b"H".to_vec()), 8, 2),
            (Object::Integer(1), 11, 1),
            (Object::Operator(b"ID".to_vec()), 13, 2),
            (Object::InlineImage(b"x ".to_vec()), 16, 2),
            (Object::Operator(b"EI".to_vec()), 18, 2),
            (Object::Operator(b"Q".to_vec()), 21, 1),
        ]
    );
    assert!(callbacks.eof);
}

#[test]
fn stopping_on_inline_image_skips_ei_and_eof_callbacks() {
    let mut callbacks = RecordingCallbacks {
        stop_after: Some(3),
        ..RecordingCallbacks::default()
    };
    parse_content_stream_data(b"BI ID x EI", &mut callbacks).unwrap();

    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Operator(b"BI".to_vec()), 0, 2),
            (Object::Operator(b"ID".to_vec()), 3, 2),
            (Object::InlineImage(b"x ".to_vec()), 6, 2),
        ]
    );
    assert!(!callbacks.eof);
}

#[test]
fn inline_image_protocol_discards_exactly_one_byte_after_id() {
    let input = b"BI ID\r\nx EI";
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).unwrap();

    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Operator(b"BI".to_vec()), 0, 2),
            (Object::Operator(b"ID".to_vec()), 3, 2),
            (Object::InlineImage(b"\nx ".to_vec()), 6, 3),
            (Object::Operator(b"EI".to_vec()), 9, 2),
        ]
    );
}

#[test]
fn inline_image_protocol_requires_a_byte_after_id() {
    let mut callbacks = RecordingCallbacks::default();
    let error = parse_content_stream_data(b"ID", &mut callbacks)
        .expect_err("ID at EOF must not synthesize a separator");

    let Error::Parse { offset, message } = error else {
        panic!("expected parse error");
    };
    assert_eq!(offset, 2);
    assert!(message.contains("separator after ID"));
    assert!(!callbacks.eof);
}

#[test]
fn unterminated_inline_image_reports_the_qpdf_diagnostic_at_data_start() {
    let mut callbacks = RecordingCallbacks::default();
    let error = parse_content_stream_data(b"ID x", &mut callbacks)
        .expect_err("inline image without EI must fail");

    assert_eq!(
        error.to_string(),
        "parse error at byte 3: EOF found while reading inline image"
    );
    assert!(!callbacks.eof);
}

#[test]
fn bad_content_token_preserves_its_qpdf_offset_and_message() {
    let mut callbacks = RecordingCallbacks::default();
    let error = parse_content_stream_data(b"q <0g>", &mut callbacks)
        .expect_err("bad content token must fail");

    assert_eq!(
        error.to_string(),
        "parse error at byte 2: invalid character (g) in hexstring"
    );
    assert_eq!(
        callbacks.objects,
        vec![(Object::Operator(b"q".to_vec()), 0, 1)]
    );
    assert!(!callbacks.eof);
}

fn operations(input: &[u8]) -> Vec<(Vec<Object>, Vec<u8>)> {
    let mut seen = Vec::new();
    parse_content_operations(input, |operands, operator| {
        seen.push((operands.to_vec(), operator.to_vec()));
        Ok(ParseControl::Continue)
    })
    .expect("parse content operations");
    seen
}

fn content_objects(input: &[u8]) -> Vec<Object> {
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).expect("parse content objects");
    callbacks
        .objects
        .into_iter()
        .map(|(object, _, _)| object)
        .collect()
}

fn op(operands: Vec<Object>, operator: &[u8]) -> (Vec<Object>, Vec<u8>) {
    (operands, operator.to_vec())
}

#[test]
fn operation_adapter_preserves_text_graphics_and_nested_operands() {
    assert_eq!(
        operations(b"BT /F1 12 Tf (Hello World) Tj ET"),
        vec![
            op(vec![], b"BT"),
            op(
                vec![Object::Name(b"F1".to_vec()), Object::Integer(12)],
                b"Tf"
            ),
            op(vec![Object::String(b"Hello World".to_vec())], b"Tj"),
            op(vec![], b"ET"),
        ]
    );

    let seen = operations(b"q 1 0 0 1 10.5 20 cm [(A) -120 (B)] TJ /OC << /Type /OCG >> BDC Q");
    assert_eq!(seen[1].1, b"cm");
    assert_eq!(seen[1].0[4], Object::Real(10.5));
    assert!(matches!(&seen[2].0[0], Object::Array(_)));
    assert!(matches!(&seen[3].0[1], Object::Dictionary(_)));
}

#[test]
fn operation_adapter_preserves_keyword_and_quote_operator_boundaries() {
    assert_eq!(
        operations(b"5 1e3 12abc nullop trueColor falseStart"),
        vec![
            op(vec![Object::Integer(5)], b"1e3"),
            op(vec![], b"12abc"),
            op(vec![], b"nullop"),
            op(vec![], b"trueColor"),
            op(vec![], b"falseStart"),
        ]
    );
    assert_eq!(
        operations(b"10 20 m W* (line) ' 1 2 (q) \""),
        vec![
            op(vec![Object::Integer(10), Object::Integer(20)], b"m"),
            op(vec![], b"W*"),
            op(vec![Object::String(b"line".to_vec())], b"'"),
            op(
                vec![
                    Object::Integer(1),
                    Object::Integer(2),
                    Object::String(b"q".to_vec()),
                ],
                b"\""
            ),
        ]
    );
}

#[test]
fn callback_pipeline_preserves_inline_image_payload_events() {
    let raw: &[u8] = b"\x01}EI \x02/EI \xff";
    let mut input = Vec::new();
    input.extend_from_slice(b"q BI /W 2 /H 1 /BPC 8 /CS /G ID ");
    input.extend_from_slice(raw);
    input.extend_from_slice(b" EI Q");

    let objects = content_objects(&input);
    assert_eq!(objects.first(), Some(&Object::Operator(b"q".to_vec())));
    assert_eq!(
        objects
            .iter()
            .find_map(Object::as_inline_image)
            .expect("inline image event"),
        [raw, b" "].concat()
    );
    assert_eq!(objects.last(), Some(&Object::Operator(b"Q".to_vec())));
}

#[test]
fn callback_pipeline_skips_comments_and_preserves_scalar_operands() {
    assert_eq!(
        operations(b"% lead\ntrue false null /Foo <48656c6c6f> BDC % tail\nQ"),
        vec![
            op(
                vec![
                    Object::Boolean(true),
                    Object::Boolean(false),
                    Object::Null,
                    Object::Name(b"Foo".to_vec()),
                    Object::String(b"Hello".to_vec()),
                ],
                b"BDC"
            ),
            op(vec![], b"Q"),
        ]
    );
    assert!(operations(b" \n% only a comment\r\n").is_empty());
}

// ============================================================
// normalize_content_stream tests
// ============================================================

fn operator_sequence(input: &[u8]) -> Vec<Vec<u8>> {
    operations(input)
        .into_iter()
        .map(|(_, operator)| operator)
        .collect()
}

/// Round-trip property: normalize produces the same operator sequence as the
/// original, and the result is idempotent (normalize(normalize(x)) == normalize(x)).
#[test]
fn normalize_round_trip_operator_sequence() {
    let original = b"q
0 0 0 rg
BT
/F1 24 Tf
1 0 0 1 72 720 Tm
(qpdf test) Tj
ET
0 0 1 RG
2 w
72 700 m
540 700 l
S
Q";
    let normalized = normalize_content_stream(original).expect("normalize");
    // Same operator sequence as original.
    assert_eq!(operator_sequence(&normalized), operator_sequence(original));
    // Idempotent: a second normalize produces byte-identical output.
    let normalized2 = normalize_content_stream(&normalized).expect("normalize again");
    assert_eq!(normalized, normalized2, "normalize is not idempotent");
}

/// Exactly one operator per line; lines are newline-terminated; operands are
/// space-separated on the same line as the operator.
#[test]
fn normalize_one_operator_per_line() {
    let input = b"BT /F1 12 Tf (Hello) Tj ET";
    let out = normalize_content_stream(input).expect("normalize");
    let text = std::str::from_utf8(&out).expect("utf8");
    let lines: Vec<&str> = text.lines().collect();
    // Expected: "BT", "/F1 12 Tf", "(Hello) Tj", "ET"
    assert_eq!(lines.len(), 4, "lines: {lines:?}");
    assert_eq!(lines[0], "BT");
    assert_eq!(lines[1], "/F1 12 Tf");
    assert_eq!(lines[2], "(Hello) Tj");
    assert_eq!(lines[3], "ET");
}

/// Operand values are preserved: names, integers, reals (observable semantics).
#[test]
fn normalize_operand_values_preserved() {
    let input = b"1 0 0 1 10.5 20.0 cm";
    let out = normalize_content_stream(input).expect("normalize");
    let seen = operations(&out);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, b"cm");
    assert_eq!(seen[0].0.len(), 6);
    assert_eq!(seen[0].0[0], Object::Integer(1));
    assert_eq!(seen[0].0[4], Object::Real(10.5));
}

/// Nested array operand (TJ) is preserved after round-trip.
#[test]
fn normalize_nested_array_operand() {
    let input = b"[(A) -120 (B)] TJ";
    let out = normalize_content_stream(input).expect("normalize");
    let seen = operations(&out);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, b"TJ");
    let Object::Array(items) = &seen[0].0[0] else {
        panic!("expected array operand");
    };
    assert_eq!(
        items,
        &vec![
            Object::String(b"A".to_vec()),
            Object::Integer(-120),
            Object::String(b"B".to_vec())
        ]
    );
}

/// Dictionary operand (BDC) is preserved after round-trip.
#[test]
fn normalize_dict_operand() {
    let input = b"/OC << /Type /OCG >> BDC";
    let out = normalize_content_stream(input).expect("normalize");
    let seen = operations(&out);
    assert_eq!(seen[0].1, b"BDC");
    assert_eq!(seen[0].0[0], Object::Name(b"OC".to_vec()));
    let Object::Dictionary(dictionary) = &seen[0].0[1] else {
        panic!("expected dictionary operand");
    };
    assert_eq!(dictionary.get("Type"), Some(&Object::Name(b"OCG".to_vec())));
}

/// Inline image round-trip: BI/ID/EI structure is preserved; data bytes are
/// byte-identical; dict entries survive re-parse.
#[test]
fn normalize_inline_image_round_trip() {
    let raw: &[u8] = b"\x01\x02\x03\xff";
    let mut input = Vec::new();
    input.extend_from_slice(b"q BI /W 2 /H 2 /BPC 8 /CS /RGB ID ");
    input.extend_from_slice(raw);
    input.extend_from_slice(b" EI Q");

    let out = normalize_content_stream(&input).expect("normalize");
    let mut expected = b"q\nBI\n /BPC 8\n /CS /RGB\n /H 2\n /W 2\nID\n".to_vec();
    expected.extend_from_slice(raw);
    expected.extend_from_slice(b"\nEI\nQ\n");
    assert_eq!(
        out, expected,
        "temporary bridge must preserve the established CLI bytes"
    );
    assert_eq!(
        content_objects(&out)
            .into_iter()
            .find_map(|object| object.as_inline_image().map(<[u8]>::to_vec))
            .expect("inline image event"),
        [raw, b"\n"].concat()
    );
}

/// Inline image with binary payload (contains high bytes and EI-like sequence).
#[test]
fn normalize_inline_image_binary_payload() {
    let raw: &[u8] = b"\x00EI\x10\x20\x80\xfe";
    let mut input = Vec::new();
    input.extend_from_slice(b"BI /W 1 /H 1 /CS /G /BPC 8 ID ");
    input.extend_from_slice(raw);
    input.extend_from_slice(b" EI");

    let out = normalize_content_stream(&input).expect("normalize");
    let payload = content_objects(&out)
        .into_iter()
        .find_map(|object| object.as_inline_image().map(<[u8]>::to_vec))
        .expect("inline image event");
    assert_eq!(payload, [raw, b"\n"].concat());
}

#[test]
fn normalize_inline_image_crlf_separators_preserve_existing_cli_bytes() {
    let input = b"BI /W 1 ID\r\nraw\r\nEI";
    assert_eq!(
        normalize_content_stream(input).expect("normalize"),
        b"BI\n /W 1\nID\nraw\nEI\n"
    );
}

#[test]
fn inline_image_events_preserve_qpdf_payload_boundaries_and_offsets() {
    for (input, expected_payload, expected_offset, expected_length) in [
        (
            b"BI /W 1 ID \nraw EI".as_slice(),
            b"\nraw ".as_slice(),
            11,
            5,
        ),
        (
            b"BI /W 1 ID\r\nraw\r\nEI".as_slice(),
            b"\nraw\r\n".as_slice(),
            11,
            6,
        ),
        (
            b"BI /W 1 ID payload}EI Q".as_slice(),
            b"payload}".as_slice(),
            11,
            8,
        ),
        (
            b"BI /W 1 ID raw\xff EI".as_slice(),
            b"raw\xff ".as_slice(),
            11,
            5,
        ),
        (b"BI /W 1 ID  EI".as_slice(), b" ".as_slice(), 11, 1),
        (
            b"BI /W 1 ID one EI A1 two EI Q".as_slice(),
            b"one EI A1 two ".as_slice(),
            11,
            14,
        ),
    ] {
        let mut callbacks = RecordingCallbacks::default();
        parse_content_stream_data(input, &mut callbacks).expect("parse inline image");
        let event = callbacks
            .objects
            .iter()
            .find(|(object, _, _)| object.as_inline_image().is_some())
            .expect("inline image event");
        assert_eq!(
            event,
            &(
                Object::InlineImage(expected_payload.to_vec()),
                expected_offset,
                expected_length,
            ),
            "input {input:?}"
        );
    }
}

#[test]
fn normalize_preserves_payload_lf_after_space_separator() {
    assert_eq!(
        normalize_content_stream(b"BI /W 1 ID \nraw EI").expect("normalize"),
        b"BI\n /W 1\nID\n\nraw\nEI\n"
    );
}

#[test]
fn normalize_preserves_non_whitespace_payload_terminal_bytes() {
    for (input, expected) in [
        (
            b"BI /W 1 ID payload}EI Q".as_slice(),
            b"BI\n /W 1\nID\npayload}\nEI\nQ\n".as_slice(),
        ),
        (
            b"BI /W 1 ID raw\xff EI".as_slice(),
            b"BI\n /W 1\nID\nraw\xff\nEI\n".as_slice(),
        ),
    ] {
        assert_eq!(
            normalize_content_stream(input).expect("normalize"),
            expected,
            "input {input:?}"
        );
    }
}

#[test]
fn normalize_strips_only_actual_inline_image_separator_whitespace() {
    for (input, expected) in [
        (
            b"BI /W 1 ID\nraw\nEI".as_slice(),
            b"BI\n /W 1\nID\nraw\nEI\n".as_slice(),
        ),
        (
            b"BI /W 1 ID\r\nraw\r\nEI".as_slice(),
            b"BI\n /W 1\nID\nraw\nEI\n".as_slice(),
        ),
        (
            b"BI /W 1 ID  EI".as_slice(),
            b"BI\n /W 1\nID\n\nEI\n".as_slice(),
        ),
        (
            b"BI /W 1 ID one EI A1 two EI Q".as_slice(),
            b"BI\n /W 1\nID\none EI A1 two\nEI\nQ\n".as_slice(),
        ),
    ] {
        assert_eq!(
            normalize_content_stream(input).expect("normalize"),
            expected,
            "input {input:?}"
        );
    }
}

#[test]
fn normalize_rejects_malformed_event_sequences() {
    for (input, expected) in [
        (
            b"1 BI ID x EI".as_slice(),
            "inline image operator BI cannot have operands",
        ),
        (
            b"BI 1 2 ID x EI".as_slice(),
            "inline image key is not a name",
        ),
        (b"BI /W ID x EI".as_slice(), "inline image key has no value"),
        (
            b"BI q".as_slice(),
            "unexpected operator q in inline image header",
        ),
        (b"ID x EI".as_slice(), "inline image found outside BI/ID"),
        (
            b"1 2".as_slice(),
            "content stream ended with dangling operands",
        ),
        (b"BI /W 1".as_slice(), "inline image missing ID"),
    ] {
        let error = normalize_content_stream(input).expect_err("normalization must fail");
        assert!(
            error.to_string().contains(expected),
            "input {input:?}: expected {expected:?}, got {error}"
        );
    }
}

/// Comments are stripped by normalize (keep_comments=false semantics).
#[test]
fn normalize_strips_comments() {
    let input = b"% header\nq % inline comment\nQ";
    let out = normalize_content_stream(input).expect("normalize");
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(!text.contains('%'), "comments must be stripped: {text:?}");
    let ops: Vec<_> = operator_sequence(&out);
    assert_eq!(ops, vec![b"q".to_vec(), b"Q".to_vec()]);
}

/// Idempotence on a stream that already uses the normalized form.
#[test]
fn normalize_idempotent_already_normal() {
    let input = b"BT\n/F1 12 Tf\n(hello) Tj\nET\n";
    let out = normalize_content_stream(input).expect("normalize");
    let out2 = normalize_content_stream(&out).expect("normalize again");
    assert_eq!(out, out2);
}
