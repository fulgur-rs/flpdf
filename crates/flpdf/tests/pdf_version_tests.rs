use flpdf::{parse_pdf_version, parse_pdf_version_spec, PdfVersion};

#[test]
fn exposes_the_complete_qpdf_pdfversion_value_api() {
    let mut version = PdfVersion::default();
    assert_eq!(version.get_version(), ("0.0".to_string(), 0));

    version.update_if_greater(PdfVersion::new(1, 7, 3));
    assert_eq!(version.major(), 1);
    assert_eq!(version.minor(), 7);
    assert_eq!(version.extension_level(), 3);
    assert_eq!(version.get_version(), ("1.7".to_string(), 3));

    version.update_if_greater(PdfVersion::new(1, 7, 2));
    assert_eq!(version, PdfVersion::new(1, 7, 3));
    assert!(PdfVersion::new(1, 7, 2) < PdfVersion::new(1, 7, 3));
    assert!(PdfVersion::new(1, 6, 99) < PdfVersion::new(1, 7, 0));
}

#[test]
fn parses_only_existing_flpdf_major_minor_syntax() {
    assert_eq!(PdfVersion::parse("1.7"), Some(PdfVersion::new(1, 7, 0)));
    assert_eq!(PdfVersion::parse("1.10"), Some(PdfVersion::new(1, 10, 0)));
    assert_eq!(PdfVersion::parse("invalid"), None);
    assert_eq!(PdfVersion::parse("1.7.3"), None);
    assert_eq!(PdfVersion::parse("256.0"), None);
}

#[test]
fn public_parser_returns_the_value_type() {
    assert_eq!(parse_pdf_version("1.7"), Some(PdfVersion::new(1, 7, 0)));
}

#[test]
fn parses_qpdf_version_spec_into_base_version_and_extension_level() {
    assert_eq!(parse_pdf_version_spec("1.3"), Some(("1.3".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.7.1"), Some(("1.7".into(), 1)));
    assert_eq!(parse_pdf_version_spec("1.8.0"), Some(("1.8".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.8.5"), Some(("1.8".into(), 5)));
}

#[test]
fn rejects_version_specs_without_major_minor_or_with_extra_components() {
    for value in ["1", "1.", ".7", "1.7.", "1.7.1.2", "not-a-version"] {
        assert_eq!(parse_pdf_version_spec(value), None, "{value:?}");
    }
}
