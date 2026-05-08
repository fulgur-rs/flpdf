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
