use flpdf::{parse_object, Dictionary, Object, ObjectRef};

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
    let object = parse_object(b"[0 0 595.28 841.89 -0.5 .75 1.  +.25 -1.5 1e3]").unwrap();

    let Object::Array(values) = object else {
        panic!("expected array")
    };

    assert_eq!(values[0], Object::Integer(0));
    assert_eq!(values[1], Object::Integer(0));
    assert_eq!(values[2], Object::Real(595.28));
    assert_eq!(values[3], Object::Real(841.89));
    assert_eq!(values[4], Object::Real(-0.5));
    assert_eq!(values[5], Object::Real(0.75));
    assert_eq!(values[6], Object::Real(1.0));
    assert_eq!(values[7], Object::Real(0.25));
    assert_eq!(values[8], Object::Real(-1.5));
    assert_eq!(values[9], Object::Real(1000.0));
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
