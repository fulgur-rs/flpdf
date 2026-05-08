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
fn dictionary_type_is_exported_for_downstream_code() {
    let dict = Dictionary::new();
    assert_eq!(dict.iter().count(), 0);
}
