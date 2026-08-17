use super::input::{json_value_to_handle, parse_indirect_reference, parse_object_key};
use super::Json;
use crate::{ObjectRef, Pdf};

#[test]
fn qpdf_json_validators_accept_only_qpdf_reference_shapes() {
    assert_eq!(
        parse_indirect_reference(b"52  20  R"),
        Some(ObjectRef::new(52, 20))
    );
    assert_eq!(parse_indirect_reference(b"0 0 R"), None);
    assert_eq!(parse_indirect_reference(b"52 20 R trailing"), None);
    assert_eq!(parse_indirect_reference(b"52 20R"), None);
    assert_eq!(
        parse_object_key(b"obj:12 13 R"),
        Some(ObjectRef::new(12, 13))
    );
    assert_eq!(parse_object_key(b"12 13 R"), None);
}

#[test]
fn qpdf_json_value_factory_builds_scalars_and_pdf_string_forms() {
    let mut pdf = Pdf::empty().expect("empty PDF");

    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("42"))
            .expect("integer")
            .as_integer(),
        Some(42)
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_bool(true))
            .expect("boolean")
            .as_boolean(),
        Some(true)
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("/Name"))
            .expect("name")
            .as_name(),
        Some(b"Name".to_vec())
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("b:00ff"))
            .expect("binary string")
            .as_string(),
        Some(vec![0, 0xff])
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("u:hello"))
            .expect("unicode string")
            .as_string(),
        Some(crate::pdf_string::new_unicode_string(b"hello"))
    );
    assert!(json_value_to_handle(&mut pdf, &Json::make_string("b:0g"))
        .expect("invalid binary string")
        .is_null());
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_string("n:/Name"))
            .expect("encoded name")
            .as_name(),
        Some(b"Name".to_vec())
    );
    assert!(
        json_value_to_handle(&mut pdf, &Json::make_string("unknown"))
            .expect("unknown string")
            .is_null()
    );
}

#[test]
fn qpdf_json_value_factory_preserves_real_literals_and_rejects_non_finite_numbers() {
    let mut pdf = Pdf::empty().expect("empty PDF");

    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("1.5"))
            .expect("real")
            .as_real_literal(),
        Some((1.5, b"1.5".to_vec()))
    );
    assert_eq!(
        json_value_to_handle(&mut pdf, &Json::make_number("1e+2"))
            .expect("scientific real")
            .as_real(),
        Some(100.0)
    );
    let error =
        json_value_to_handle(&mut pdf, &Json::make_number("1e9999")).expect_err("non-finite real");
    assert!(error.to_string().contains("invalid JSON number"));
}

#[test]
fn qpdf_json_value_factory_rejects_uninitialized_values_and_decodes_name_keys() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let error = json_value_to_handle(&mut pdf, &Json::default()).expect_err("uninitialized");
    assert!(error
        .to_string()
        .contains("JSON value has no initialized qpdf value kind"));

    let object = Json::parse(br#"{"n:/Name": 1}"#).expect("JSON");
    let dictionary = json_value_to_handle(&mut pdf, &object)
        .expect("dictionary")
        .as_dictionary()
        .expect("dictionary handle");
    assert_eq!(
        dictionary
            .get(b"/Name".as_slice())
            .and_then(|value| value.as_integer()),
        Some(1)
    );
}

#[test]
fn qpdf_json_reference_factory_reuses_canonical_identity() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let value = Json::make_string("12 0 R");
    let from_json = json_value_to_handle(&mut pdf, &value).expect("reference");
    let from_pdf = pdf.get_object_handle(ObjectRef::new(12, 0));

    assert!(from_json.is_same_object_as(&from_pdf));
    assert_eq!(from_json.object_ref(), Some(ObjectRef::new(12, 0)));
}

#[test]
fn qpdf_json_value_factory_builds_nested_canonical_handles() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let object = Json::parse(br#"{"/A": [1, "12 0 R"], "/B": null}"#).expect("JSON");
    let handle = json_value_to_handle(&mut pdf, &object).expect("dictionary");
    let dictionary = handle.as_dictionary().expect("dictionary handle");
    let array = dictionary
        .get(b"/A".as_slice())
        .expect("/A")
        .as_array()
        .expect("array");

    assert_eq!(array[0].as_integer(), Some(1));
    assert_eq!(array[1].object_ref(), Some(ObjectRef::new(12, 0)));
    assert!(dictionary.get(b"/B".as_slice()).expect("/B").is_null());
}

#[test]
fn qpdf_json_value_factory_rejects_non_qpdf_real_literals() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let error = json_value_to_handle(&mut pdf, &Json::make_number("1e+")).expect_err("real");

    assert!(error.to_string().contains("invalid JSON number"));
}
