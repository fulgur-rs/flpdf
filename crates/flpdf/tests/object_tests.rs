use flpdf::{Dictionary, Object, ObjectRef};

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
