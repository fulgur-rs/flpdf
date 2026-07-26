use flpdf::json::Json;

#[test]
fn default_handle_writes_null_but_is_not_initialized_null() {
    let value = Json::default();
    assert_eq!(value.unparse().unwrap(), b"null");
    assert!(!value.is_null());
    assert_eq!(value.start(), 0);
    assert_eq!(value.end(), 0);
}

#[test]
fn encoded_number_is_not_normalized() {
    let value = Json::make_number(b"2.1e5");
    assert_eq!(value.get_number().as_deref(), Some(b"2.1e5".as_slice()));
    assert_eq!(value.unparse().unwrap(), b"2.1e5");
}

#[test]
fn real_special_values_match_qpdf_classic_locale_bytes() {
    assert_eq!(Json::make_real(f64::NAN).unparse().unwrap(), b"nan");
    assert_eq!(
        Json::make_real(f64::from_bits(0xfff8_0000_0000_0000))
            .unparse()
            .unwrap(),
        b"-nan"
    );
    assert_eq!(Json::make_real(f64::INFINITY).unparse().unwrap(), b"inf");
    assert_eq!(
        Json::make_real(f64::NEG_INFINITY).unparse().unwrap(),
        b"-inf"
    );
    assert_eq!(Json::make_real(-0.0).unparse().unwrap(), b"-0");
}

#[test]
fn scalar_accessors_reject_other_types_without_mutating_output() {
    let value = Json::make_bool(true);
    assert_eq!(value.get_bool(), Some(true));
    assert_eq!(value.get_string(), None);
    assert_eq!(value.get_number(), None);
}
