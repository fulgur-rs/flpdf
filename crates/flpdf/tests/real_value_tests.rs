use flpdf::ObjectHandle;

#[test]
fn double_factory_rounds_the_stored_value_like_qpdf() {
    let real = ObjectHandle::real(1.123456789);
    assert_eq!(real.try_get_real_value().unwrap(), b"1.123457");
    assert_eq!(real.try_get_numeric_value().unwrap(), 1.123457);
    assert_eq!(real.unparse_resolved().unwrap(), b"1.123457");
    assert_eq!(
        real.shallow_copy().unwrap().try_get_real_value().unwrap(),
        b"1.123457"
    );
}

#[test]
fn string_factory_keeps_the_literal_as_the_only_value() {
    let real = ObjectHandle::real_from_string(b"  +1.25tail");
    assert_eq!(real.try_get_numeric_value().unwrap(), 1.25);
    assert_eq!(real.unparse_resolved().unwrap(), b"  +1.25tail");
}

#[test]
fn real_numeric_access_matches_qpdf_atof_vectors() {
    for (text, bits) in [
        ("", 0x0000000000000000u64),
        ("not-a-real", 0x0000000000000000u64),
        ("  +1.25tail", 0x3ff4000000000000u64),
        ("1e+", 0x3ff0000000000000u64),
        ("-.4", 0xbfd999999999999au64),
        ("-0", 0x8000000000000000u64),
        ("+INFtail", 0x7ff0000000000000u64),
        ("-INFINITY", 0xfff0000000000000u64),
        ("-NaN", 0xfff8000000000000u64),
        ("nan(+123)", 0x7ff8000000000000u64),
        ("nan(184467440737095516160x)", 0x7ff8000000000000u64),
        ("nan(123)", 0x7ff800000000007bu64),
        ("nan(0x123)", 0x7ff8000000000123u64),
        ("nan(0123)", 0x7ff8000000000053u64),
        ("nan(18446744073709551616)", 0x7fffffffffffffffu64),
        ("0x1.8p2", 0x4018000000000000u64),
        ("0x1.8", 0x3ff8000000000000u64),
        ("0x.p1", 0x0000000000000000u64),
        ("0x1p+", 0x3ff0000000000000u64),
        ("0x0p999999", 0x0000000000000000u64),
        ("0x1p-1074", 0x0000000000000001u64),
        ("0x1p-1075", 0x0000000000000000u64),
        ("0x1.0000000000001p-1075", 0x0000000000000001u64),
        ("0x1.fffffffffffffp1023", 0x7fefffffffffffffu64),
        ("0x1.fffffffffffff8p1023", 0x7ff0000000000000u64),
        ("0x0.fffffffffffff8p-1022", 0x0010000000000000u64),
        ("1e9999", 0x7ff0000000000000u64),
        ("1e-9999", 0x0000000000000000u64),
    ] {
        let real = ObjectHandle::real_from_string(text);
        assert_eq!(
            real.try_get_numeric_value().unwrap().to_bits(),
            bits,
            "{text}"
        );
        assert_eq!(real.unparse_resolved().unwrap(), text.as_bytes());
    }
}

#[test]
fn real_precision_and_raw_json_follow_qpdf_text_rules() {
    assert_eq!(
        ObjectHandle::real_with_precision(1.25, 4, false)
            .try_get_real_value()
            .unwrap(),
        b"1.2500"
    );
    assert_eq!(
        ObjectHandle::real_with_precision(1.25, 4, true)
            .try_get_real_value()
            .unwrap(),
        b"1.25"
    );
    for (text, expected) in [
        ("", "0"),
        (".4", "0.4"),
        ("-.4", "-0.4"),
        ("nan", "nan"),
        ("not-a-real", "not-a-real"),
    ] {
        let real = ObjectHandle::real_from_string(text);
        let mut bytes = Vec::new();
        let mut sink = flpdf::pipeline::PlString::new("real", None, &mut bytes);
        real.write_json(2, &mut sink).unwrap();
        assert_eq!(bytes, expected.as_bytes());
    }
    assert!(matches!(
        ObjectHandle::real(f64::NAN).get_json(2),
        Err(flpdf::ObjectJsonError::Json(_))
    ));
}

#[test]
fn json_real_import_retains_text_when_stod_reports_range_error() {
    let exact_subnormal = format!("{:.1074}e0", f64::from_bits(1));
    for (literal, expected) in [
        (exact_subnormal.as_str(), "0"),
        ("1e2", "100"),
        (
            "1e-9999999999999999999999999",
            "1e-9999999999999999999999999",
        ),
        ("0e9999999999999999999999999", "0"),
        ("1e9999", "1e9999"),
        ("1e-9999", "1e-9999"),
        ("0e-9999", "0"),
        ("5e-324", "5e-324"),
        ("50e-325", "50e-325"),
        ("2.2250738585072013e-308", "0"),
        ("2.2250738585072012e-308", "2.2250738585072012e-308"),
    ] {
        let json = format!(
            r#"{{"qpdf":[{{"jsonversion":2,"pdfversion":"1.7"}},{{"obj:1 0 R":{{"value":{literal}}},"trailer":{{"value":{{}}}}}}]}}"#
        );
        let mut pdf =
            flpdf::Pdf::create_from_json(std::io::Cursor::new(json.into_bytes()), "real.json")
                .unwrap();
        let real = pdf.get_object_handle(flpdf::ObjectRef::new(1, 0));
        assert_eq!(
            real.try_get_real_value().unwrap(),
            expected.as_bytes(),
            "{literal}"
        );
    }
}

#[test]
fn real_string_numeric_conversion_uses_c_string_boundaries() {
    for (bytes, bits) in [
        (b"1.25\0ignored".as_slice(), 1.25f64.to_bits()),
        (b"1.25\xff".as_slice(), 1.25f64.to_bits()),
        (b"nan(\xff)".as_slice(), f64::NAN.to_bits()),
        (b"nan()".as_slice(), f64::NAN.to_bits()),
        (b"0XABC.p-1".as_slice(), 1374.0f64.to_bits()),
        (b"0x1p-9999999999999999999999".as_slice(), 0),
        (
            b"-0x1p9999999999999999999999".as_slice(),
            f64::NEG_INFINITY.to_bits(),
        ),
        (b"-0x0.p3".as_slice(), (-0.0f64).to_bits()),
        (b".bad".as_slice(), 0),
        (b"\t\r\n\x0b\x0c   ".as_slice(), 0),
    ] {
        let real = ObjectHandle::real_from_string(bytes);
        assert_eq!(real.try_get_numeric_value().unwrap().to_bits(), bits);
        assert_eq!(real.unparse_resolved().unwrap(), bytes);
    }
}
