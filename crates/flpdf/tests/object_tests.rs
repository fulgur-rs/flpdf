use flpdf::{Dictionary, Object, ObjectRef, Stream};

#[test]
fn object_ref_formats_as_pdf_reference() {
    let object_ref = ObjectRef::new(12, 3);
    assert_eq!(object_ref.to_string(), "12 3 R");
}

#[test]
fn dictionary_returns_required_references() {
    let mut dict = Dictionary::new();
    dict.insert("Root", Object::reference(ObjectRef::new(1, 0)));

    assert_eq!(dict.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(dict.get_ref("Info"), None);
}

#[test]
fn object_string_is_hex_encoded_when_non_printable() {
    let mut out = Vec::new();
    Object::String(vec![0x00, 0xff, 0x10, 0x20]).write_pdf(&mut out);
    assert_eq!(out, b"<00ff1020>");

    out.clear();
    Object::String(Vec::new()).write_pdf(&mut out);
    assert_eq!(out, b"()".to_vec());
}

#[test]
fn object_accessors_return_borrowed_variant_payloads() {
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::Name(b"Catalog".to_vec()));
    let stream = Stream::new(dict.clone(), b"payload".to_vec());
    let object_ref = ObjectRef::new(3, 0);

    assert_eq!(Object::Dictionary(dict.clone()).as_dict(), Some(&dict));
    assert_eq!(Object::Stream(stream.clone()).as_stream(), Some(&stream));
    assert_eq!(
        Object::Array(vec![Object::Integer(1)]).as_array(),
        Some([Object::Integer(1)].as_slice())
    );
    assert_eq!(
        Object::Name(b"Type".to_vec()).as_name(),
        Some(b"Type".as_slice())
    );
    assert_eq!(
        Object::String(b"value".to_vec()).as_string(),
        Some(b"value".as_slice())
    );
    assert_eq!(Object::Integer(42).as_integer(), Some(42));
    assert_eq!(Object::Real(1.5).as_real(), Some(1.5));
    assert_eq!(Object::Boolean(true).as_bool(), Some(true));
    assert_eq!(Object::Reference(object_ref).as_ref_id(), Some(object_ref));
    assert!(Object::Null.is_null());

    assert_eq!(Object::Integer(42).as_dict(), None);
    assert_eq!(Object::Integer(42).as_stream(), None);
    assert_eq!(Object::Null.as_array(), None);
    assert_eq!(Object::Null.as_name(), None);
    assert_eq!(Object::Null.as_string(), None);
    assert_eq!(Object::Null.as_integer(), None);
    assert_eq!(Object::Null.as_real(), None);
    assert_eq!(Object::Null.as_bool(), None);
    assert_eq!(Object::Null.as_ref_id(), None);
    assert!(!Object::Boolean(false).is_null());
}

#[test]
fn object_mut_accessors_allow_in_place_updates() {
    let mut object = Object::Dictionary(Dictionary::new());
    object
        .as_dict_mut()
        .expect("dictionary")
        .insert("Count", Object::Integer(1));
    assert_eq!(
        object.as_dict().and_then(|dict| dict.get("Count")),
        Some(&Object::Integer(1))
    );

    let mut object = Object::Stream(Stream::new(Dictionary::new(), b"old".to_vec()));
    object.as_stream_mut().expect("stream").data = b"new".to_vec();
    assert_eq!(
        object.as_stream().map(|stream| stream.data.as_slice()),
        Some(b"new".as_slice())
    );

    let mut object = Object::Array(vec![Object::Integer(1)]);
    object
        .as_array_mut()
        .expect("array")
        .push(Object::Integer(2));
    assert_eq!(
        object.as_array(),
        Some([Object::Integer(1), Object::Integer(2)].as_slice())
    );

    assert_eq!(Object::Null.as_dict_mut(), None);
    assert_eq!(Object::Null.as_stream_mut(), None);
    assert_eq!(Object::Null.as_array_mut(), None);
}

#[test]
fn object_into_accessors_consume_matching_variants() {
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::Name(b"Catalog".to_vec()));
    let stream = Stream::new(dict.clone(), b"payload".to_vec());
    let array = vec![Object::Integer(1), Object::Integer(2)];
    let name = b"Catalog".to_vec();
    let string = b"payload".to_vec();

    assert_eq!(Object::Dictionary(dict.clone()).into_dict(), Some(dict));
    assert_eq!(Object::Stream(stream.clone()).into_stream(), Some(stream));
    assert_eq!(Object::Array(array.clone()).into_array(), Some(array));
    assert_eq!(Object::Name(name.clone()).into_name(), Some(name));
    assert_eq!(Object::String(string.clone()).into_string(), Some(string));

    assert_eq!(Object::Null.into_dict(), None);
    assert_eq!(Object::Integer(1).into_stream(), None);
    assert_eq!(Object::String(b"x".to_vec()).into_array(), None);
    assert_eq!(Object::Null.into_name(), None);
    assert_eq!(Object::Null.into_string(), None);
}
