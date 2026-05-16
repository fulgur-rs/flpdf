//! Tokenizer tests for `flpdf::content_stream`.

use flpdf::content_stream::{
    normalize_content_stream, ContentParseOptions, ContentStreamParser, ContentToken,
};
use flpdf::Object;

fn tokens(input: &[u8]) -> Vec<ContentToken> {
    ContentStreamParser::new(input)
        .collect::<flpdf::Result<Vec<_>>>()
        .expect("tokenize")
}

fn tokens_keep_comments(input: &[u8]) -> Vec<ContentToken> {
    ContentStreamParser::with_options(
        input,
        ContentParseOptions {
            keep_comments: true,
        },
    )
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
            op(
                vec![Object::Name(b"F1".to_vec()), Object::Integer(12)],
                b"Tf"
            ),
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
    let toks = tokens(b"[(A) -120 (B) [(C) 5] ] TJ /OC << /Type /OCG /Nested [1 [2 3]] >> BDC");
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
fn inline_image_header_comment_with_keep_comments_does_not_break() {
    // A comment inside the inline-image header must be consumed (not
    // emitted, not fatal) even when keep_comments=true.
    let toks = tokens_keep_comments(b"BI %hdr comment\n/W 1 /H 1 /BPC 8 /CS /G ID x EI");
    assert_eq!(toks.len(), 1, "inline image must parse with header comment");
    match &toks[0] {
        ContentToken::InlineImage { dict, data } => {
            assert_eq!(data, b"x");
            assert_eq!(dict.get("W"), Some(&Object::Integer(1)));
        }
        other => panic!("expected inline image, got {other:?}"),
    }
    // Same input with default options must behave identically.
    let toks = tokens(b"BI %hdr comment\n/W 1 /H 1 /BPC 8 /CS /G ID x EI");
    assert_eq!(toks.len(), 1);
}

#[test]
fn inline_image_data_with_delimiter_before_ei_not_truncated() {
    // Image data contains `}EI ` and `/EI ` — a delimiter immediately
    // before `EI` followed by whitespace. Per ISO 32000-1 §7.8.2 the real
    // terminating `EI` must be preceded by whitespace, so these embedded
    // sequences must NOT end the image.
    let raw: &[u8] = b"\x01}EI \x02/EI \xff";
    let mut input = Vec::new();
    input.extend_from_slice(b"BI /W 2 /H 1 /BPC 8 /CS /G ID ");
    input.extend_from_slice(raw);
    input.extend_from_slice(b" EI");

    let toks = tokens(&input);
    assert_eq!(toks.len(), 1, "delimiter+EI inside data must not terminate");
    match &toks[0] {
        ContentToken::InlineImage { data, .. } => {
            assert_eq!(data, raw, "image data must be preserved byte-identical");
        }
        other => panic!("expected inline image, got {other:?}"),
    }
}

#[test]
fn keyword_prefixed_operators_are_not_split_as_bool_null_operands() {
    // Extension/unknown operators sharing a true/false/null prefix must
    // tokenize as a single operator, not operand + shorter operator.
    let toks = tokens(b"nullop trueColor falseStart");
    assert_eq!(
        toks,
        vec![
            op(vec![], b"nullop"),
            op(vec![], b"trueColor"),
            op(vec![], b"falseStart"),
        ]
    );
    // Genuine keyword operands still parse (token-bounded).
    let toks = tokens(b"true false null do");
    assert_eq!(
        toks,
        vec![op(
            vec![Object::Boolean(true), Object::Boolean(false), Object::Null,],
            b"do"
        )]
    );
}

#[test]
fn operands_before_bi_are_a_parse_error() {
    // `BI` takes no operands; stray operands before it mean the content
    // stream is malformed and must not be silently discarded.
    let mut p = ContentStreamParser::new(b"1 2 BI /W 1 /H 1 ID x EI");
    match p.next() {
        Some(Err(_)) => {}
        other => panic!("expected parse error for operands before BI, got {other:?}"),
    }
    // Iterator fuses after an error.
    assert!(p.next().is_none(), "iterator must fuse after error");
}

#[test]
fn inline_image_crlf_before_ei_strips_full_separator() {
    // `ID` separator is CRLF and the `EI` is also preceded by CRLF. Both
    // separators must be stripped wholly: a single-byte strip before `EI`
    // would leave a stray `\r` at the end of `data`, changing the payload.
    let raw: &[u8] = b"\x00\x10\xff\x7f";
    let mut input = Vec::new();
    input.extend_from_slice(b"BI /W 2 /H 1 /BPC 8 /CS /G ID\r\n");
    input.extend_from_slice(raw);
    input.extend_from_slice(b"\r\nEI");

    let toks = tokens(&input);
    assert_eq!(toks.len(), 1);
    match &toks[0] {
        ContentToken::InlineImage { data, .. } => {
            assert_eq!(
                data, raw,
                "CRLF before EI must be stripped fully (no trailing \\r)"
            );
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

// ============================================================
// normalize_content_stream tests
// ============================================================

/// Helper: collect operator names from a byte slice via ContentStreamParser.
fn operator_sequence(input: &[u8]) -> Vec<Vec<u8>> {
    ContentStreamParser::new(input)
        .collect::<flpdf::Result<Vec<_>>>()
        .expect("parse")
        .into_iter()
        .filter_map(|tok| match tok {
            ContentToken::Op { operator, .. } => Some(operator),
            _ => None,
        })
        .collect()
}

/// Helper: collect all tokens (including InlineImage) from a byte slice.
fn all_tokens(input: &[u8]) -> Vec<ContentToken> {
    ContentStreamParser::new(input)
        .collect::<flpdf::Result<Vec<_>>>()
        .expect("parse")
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
    let toks = all_tokens(&out);
    assert_eq!(toks.len(), 1);
    match &toks[0] {
        ContentToken::Op { operands, operator } => {
            assert_eq!(operator, b"cm");
            assert_eq!(operands.len(), 6);
            assert_eq!(operands[0], Object::Integer(1));
            assert_eq!(operands[1], Object::Integer(0));
            assert_eq!(operands[2], Object::Integer(0));
            assert_eq!(operands[3], Object::Integer(1));
            // 10.5 is preserved as Real; 20.0 is serialized as "20" which
            // re-parses as Integer(20) — this is a documented behaviour.
            assert_eq!(operands[4], Object::Real(10.5));
        }
        other => panic!("unexpected token: {other:?}"),
    }
}

/// Nested array operand (TJ) is preserved after round-trip.
#[test]
fn normalize_nested_array_operand() {
    let input = b"[(A) -120 (B)] TJ";
    let out = normalize_content_stream(input).expect("normalize");
    let toks = all_tokens(&out);
    assert_eq!(toks.len(), 1);
    match &toks[0] {
        ContentToken::Op { operands, operator } => {
            assert_eq!(operator, b"TJ");
            assert_eq!(operands.len(), 1);
            match &operands[0] {
                Object::Array(items) => {
                    assert_eq!(items.len(), 3);
                    assert_eq!(items[0], Object::String(b"A".to_vec()));
                    assert_eq!(items[1], Object::Integer(-120));
                    assert_eq!(items[2], Object::String(b"B".to_vec()));
                }
                other => panic!("expected array operand, got {other:?}"),
            }
        }
        other => panic!("unexpected token: {other:?}"),
    }
}

/// Dictionary operand (BDC) is preserved after round-trip.
#[test]
fn normalize_dict_operand() {
    let input = b"/OC << /Type /OCG >> BDC";
    let out = normalize_content_stream(input).expect("normalize");
    let toks = all_tokens(&out);
    assert_eq!(toks.len(), 1);
    match &toks[0] {
        ContentToken::Op { operands, operator } => {
            assert_eq!(operator, b"BDC");
            assert_eq!(operands.len(), 2);
            assert_eq!(operands[0], Object::Name(b"OC".to_vec()));
            match &operands[1] {
                Object::Dictionary(d) => {
                    assert_eq!(d.get("Type"), Some(&Object::Name(b"OCG".to_vec())));
                }
                other => panic!("expected dict, got {other:?}"),
            }
        }
        other => panic!("unexpected token: {other:?}"),
    }
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
    let toks = all_tokens(&out);
    assert_eq!(toks.len(), 3, "tokens: {toks:?}");
    assert_eq!(toks[0], op(vec![], b"q"));
    match &toks[1] {
        ContentToken::InlineImage { dict, data } => {
            assert_eq!(data, raw, "inline image data must be byte-identical");
            assert_eq!(dict.get("W"), Some(&Object::Integer(2)));
            assert_eq!(dict.get("H"), Some(&Object::Integer(2)));
            assert_eq!(dict.get("BPC"), Some(&Object::Integer(8)));
            assert_eq!(dict.get("CS"), Some(&Object::Name(b"RGB".to_vec())));
        }
        other => panic!("expected inline image, got {other:?}"),
    }
    assert_eq!(toks[2], op(vec![], b"Q"));
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
    let toks = all_tokens(&out);
    assert_eq!(toks.len(), 1);
    match &toks[0] {
        ContentToken::InlineImage { data, .. } => {
            assert_eq!(data, raw, "binary payload must survive normalize");
        }
        other => panic!("expected inline image, got {other:?}"),
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
