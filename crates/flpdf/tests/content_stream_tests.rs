//! Callback, operation-adapter, and normalization tests for content streams.

use flpdf::{
    parse_content_operations, parse_content_stream_data, Error, Object, ParseControl,
    ParserCallbacks,
};

#[derive(Default)]
struct RecordingCallbacks {
    size: Option<usize>,
    objects: Vec<(Object, usize, usize)>,
    diagnostics: Vec<(usize, String)>,
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

    fn handle_diagnostic(&mut self, offset: usize, message: &str) -> flpdf::Result<()> {
        self.diagnostics.push((offset, message.to_string()));
        Ok(())
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
fn operation_adapter_preserves_qpdf_recovered_null_operands() {
    let mut seen = Vec::new();
    parse_content_operations(b"1 } cm [1 } 2] cm", |operands, operator| {
        seen.push((operands.to_vec(), operator.to_vec()));
        Ok(ParseControl::Continue)
    })
    .unwrap();

    assert_eq!(
        seen,
        vec![
            (vec![Object::Integer(1), Object::Null], b"cm".to_vec(),),
            (
                vec![Object::Array(vec![
                    Object::Integer(1),
                    Object::Null,
                    Object::Integer(2),
                ])],
                b"cm".to_vec(),
            ),
        ]
    );
}

#[test]
fn callbacks_receive_qpdf_recovery_diagnostics_and_nulls() {
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(b"<< /A } /B 2 >> cm", &mut callbacks).unwrap();

    let mut expected = flpdf::Dictionary::new();
    expected.insert("A", Object::Null);
    expected.insert("B", Object::Integer(2));
    assert_eq!(callbacks.objects[0].0, Object::Dictionary(expected));
    assert_eq!(
        callbacks.diagnostics,
        vec![(6, "treating unexpected brace token as null".to_string())]
    );
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
    parse_content_stream_data(b"q <0g>", &mut callbacks)
        .expect("bad content token is a recoverable qpdf null");

    assert_eq!(
        callbacks.diagnostics,
        vec![
            (2, "invalid character (g) in hexstring".to_string()),
            (5, "EOF while reading token".to_string()),
        ]
    );
    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Operator(b"q".to_vec()), 0, 1),
            (Object::Null, 2, 3),
            (Object::Null, 5, 1),
        ]
    );
    assert!(callbacks.eof);
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
fn public_normalizer_reexport_preserves_qpdf_token_layout() {
    let result = flpdf::normalize_content_stream(b"% c\r\nBT  /N#61me Q");
    assert_eq!(result.as_bytes(), b"% c\nBT  /Name Q");
    assert!(!result.any_bad_tokens());
}
