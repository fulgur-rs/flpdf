//! Tokenizer tests for `flpdf::content_stream`.

use flpdf::content_stream::{ContentParseOptions, ContentStreamParser, ContentToken};
use flpdf::Object;

fn tokens(input: &[u8]) -> Vec<ContentToken> {
    ContentStreamParser::new(input)
        .collect::<flpdf::Result<Vec<_>>>()
        .expect("tokenize")
}

fn tokens_keep_comments(input: &[u8]) -> Vec<ContentToken> {
    ContentStreamParser::with_options(input, ContentParseOptions { keep_comments: true })
        .collect::<flpdf::Result<Vec<_>>>()
        .expect("tokenize")
}

fn op(operands: Vec<Object>, operator: &[u8]) -> ContentToken {
    ContentToken::Op {
        operands,
        operator: operator.to_vec(),
    }
}

#[test]
fn text_showing_block() {
    let toks = tokens(b"BT /F1 12 Tf (Hello World) Tj ET");
    assert_eq!(
        toks,
        vec![
            op(vec![], b"BT"),
            op(vec![Object::Name(b"F1".to_vec()), Object::Integer(12)], b"Tf"),
            op(vec![Object::String(b"Hello World".to_vec())], b"Tj"),
            op(vec![], b"ET"),
        ]
    );
}

#[test]
fn graphics_cm_q_q_rectangle_fill() {
    let toks = tokens(b"q 1 0 0 1 10.5 20 cm 0 0 100 200 re f Q");
    assert_eq!(
        toks,
        vec![
            op(vec![], b"q"),
            op(
                vec![
                    Object::Integer(1),
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(1),
                    Object::Real(10.5),
                    Object::Integer(20),
                ],
                b"cm"
            ),
            op(
                vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(100),
                    Object::Integer(200),
                ],
                b"re"
            ),
            op(vec![], b"f"),
            op(vec![], b"Q"),
        ]
    );
}

#[test]
fn path_with_starred_and_quote_operators() {
    // W*, f*, single-quote and double-quote operators must tokenize.
    let toks = tokens(b"10 20 m 30 40 l W* n (line) ' 1 2 (q) \"");
    assert_eq!(
        toks,
        vec![
            op(vec![Object::Integer(10), Object::Integer(20)], b"m"),
            op(vec![Object::Integer(30), Object::Integer(40)], b"l"),
            op(vec![], b"W*"),
            op(vec![], b"n"),
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
fn nested_array_and_dict_operands() {
    // TJ array operand, and a BDC with a properties dictionary operand.
    let toks = tokens(
        b"[(A) -120 (B) [(C) 5] ] TJ /OC << /Type /OCG /Nested [1 [2 3]] >> BDC",
    );
    assert_eq!(toks.len(), 2);
    match &toks[0] {
        ContentToken::Op { operands, operator } => {
            assert_eq!(operator, b"TJ");
            assert_eq!(operands.len(), 1);
            match &operands[0] {
                Object::Array(items) => {
                    assert_eq!(items.len(), 4);
                    assert_eq!(items[0], Object::String(b"A".to_vec()));
                    assert_eq!(items[1], Object::Integer(-120));
                    assert!(matches!(&items[3], Object::Array(_)));
                }
                other => panic!("expected array operand, got {other:?}"),
            }
        }
        other => panic!("expected TJ op, got {other:?}"),
    }
    match &toks[1] {
        ContentToken::Op { operands, operator } => {
            assert_eq!(operator, b"BDC");
            assert_eq!(operands.len(), 2);
            assert_eq!(operands[0], Object::Name(b"OC".to_vec()));
            assert!(matches!(&operands[1], Object::Dictionary(_)));
        }
        other => panic!("expected BDC op, got {other:?}"),
    }
}

#[test]
fn inline_image_preserves_raw_bytes() {
    // 6 raw bytes of "image data", including a byte sequence that contains
    // `EI` inside it to prove the boundary scan is robust.
    let raw: &[u8] = b"\x01EI\x02\x03\xff";
    let mut input = Vec::new();
    input.extend_from_slice(b"q BI /W 2 /H 3 /BPC 8 /CS /RGB ID ");
    input.extend_from_slice(raw);
    input.extend_from_slice(b" EI Q");

    let toks = tokens(&input);
    assert_eq!(toks.len(), 3);
    assert_eq!(toks[0], op(vec![], b"q"));
    match &toks[1] {
        ContentToken::InlineImage { dict, data } => {
            assert_eq!(data, raw, "inline image data must be byte-identical");
            assert_eq!(dict.get("W"), Some(&Object::Integer(2)));
            assert_eq!(dict.get("H"), Some(&Object::Integer(3)));
            assert_eq!(dict.get("BPC"), Some(&Object::Integer(8)));
            // Abbreviated /CS name kept as-is, not normalized.
            assert_eq!(dict.get("CS"), Some(&Object::Name(b"RGB".to_vec())));
        }
        other => panic!("expected inline image, got {other:?}"),
    }
    assert_eq!(toks[2], op(vec![], b"Q"));
}

#[test]
fn inline_image_with_binary_payload_and_crlf() {
    // ID followed by CRLF separator; data contains NUL and high bytes.
    let raw: &[u8] = b"\x00\x10\x20\x80\xfe";
    let mut input = Vec::new();
    input.extend_from_slice(b"BI /W 1 /H 1 /CS /G /BPC 8 /F /AHx ID\r\n");
    input.extend_from_slice(raw);
    input.extend_from_slice(b"\nEI");

    let toks = tokens(&input);
    assert_eq!(toks.len(), 1);
    match &toks[0] {
        ContentToken::InlineImage { dict, data } => {
            assert_eq!(data, raw);
            assert_eq!(dict.get("F"), Some(&Object::Name(b"AHx".to_vec())));
        }
        other => panic!("expected inline image, got {other:?}"),
    }
}

#[test]
fn comments_stripped_by_default() {
    let toks = tokens(b"% header comment\n1 0 0 1 0 0 cm % trailing\nBT ET");
    assert_eq!(
        toks,
        vec![
            op(
                vec![
                    Object::Integer(1),
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(1),
                    Object::Integer(0),
                    Object::Integer(0),
                ],
                b"cm"
            ),
            op(vec![], b"BT"),
            op(vec![], b"ET"),
        ]
    );
}

#[test]
fn comments_preserved_when_requested() {
    let toks = tokens_keep_comments(b"% lead\nq % inline\nQ");
    assert_eq!(
        toks,
        vec![
            ContentToken::Comment(b" lead".to_vec()),
            op(vec![], b"q"),
            ContentToken::Comment(b" inline".to_vec()),
            op(vec![], b"Q"),
        ]
    );
}

#[test]
fn empty_input_yields_no_tokens() {
    assert!(tokens(b"").is_empty());
    assert!(tokens(b"   \n\t  ").is_empty());
}

#[test]
fn dangling_operands_is_an_error() {
    let mut parser = ContentStreamParser::new(b"1 2 3");
    let last = parser.by_ref().last();
    assert!(matches!(last, Some(Err(_))));
    // Iterator fuses after an error.
    assert!(parser.next().is_none());
}

#[test]
fn boolean_and_null_operands() {
    let toks = tokens(b"true false null /Foo BDC");
    assert_eq!(
        toks,
        vec![op(
            vec![
                Object::Boolean(true),
                Object::Boolean(false),
                Object::Null,
                Object::Name(b"Foo".to_vec()),
            ],
            b"BDC"
        )]
    );
}

#[test]
fn hex_string_operand() {
    let toks = tokens(b"<48656c6c6f> Tj");
    assert_eq!(
        toks,
        vec![op(vec![Object::String(b"Hello".to_vec())], b"Tj")]
    );
}

#[test]
fn realistic_mixed_content_stream() {
    let stream = b"q
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
    let toks = tokens(stream);
    let operators: Vec<&[u8]> = toks
        .iter()
        .filter_map(|t| match t {
            ContentToken::Op { operator, .. } => Some(operator.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(
        operators,
        vec![
            b"q".as_slice(),
            b"rg",
            b"BT",
            b"Tf",
            b"Tm",
            b"Tj",
            b"ET",
            b"RG",
            b"w",
            b"m",
            b"l",
            b"S",
            b"Q",
        ]
    );
}
