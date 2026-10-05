use flpdf::{
    parse_pdf_version, parse_pdf_version_spec, Pdf, PdfVersion, PdfWriter, WriterConfiguration,
};

fn write_with_configuration(configuration: WriterConfiguration) -> Vec<u8> {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let mut writer = PdfWriter::new(&mut pdf);
    configuration.apply_to(&mut writer);
    writer.set_output_memory().expect("configure memory output");
    writer.write().expect("write configured PDF");
    writer.get_buffer().expect("get configured PDF")
}

fn write_with_direct_version_setting(force: bool) -> Vec<u8> {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let mut writer = PdfWriter::new(&mut pdf);
    if force {
        writer.force_pdf_version("1.7");
    } else {
        writer.set_minimum_pdf_version("1.7");
    }
    writer.set_output_memory().expect("configure memory output");
    writer.write().expect("write configured PDF");
    writer.get_buffer().expect("get configured PDF")
}

fn assert_extension_level_defaults_to_zero(output: &[u8]) {
    assert!(output.starts_with(b"%PDF-1.7"));
    assert!(
        !output
            .windows(b"/ExtensionLevel".len())
            .any(|window| window == b"/ExtensionLevel"),
        "extension level 0 must not produce an /ExtensionLevel entry"
    );
}

#[test]
fn pdf_version_constructor_defaults_extension_level_to_zero() {
    assert_eq!(PdfVersion::new(1, 7).extension_level(), 0);
    assert_eq!(
        PdfVersion::new_with_extension_level(1, 7, 8).extension_level(),
        8
    );
}

#[test]
fn writer_version_setters_default_extension_level_to_zero() {
    for force in [false, true] {
        let mut configuration = WriterConfiguration::default();
        if force {
            configuration.force_pdf_version("1.7");
        } else {
            configuration.set_minimum_pdf_version("1.7");
        }
        assert_extension_level_defaults_to_zero(&write_with_configuration(configuration));
        assert_extension_level_defaults_to_zero(&write_with_direct_version_setting(force));
    }
}

#[test]
fn exposes_the_complete_qpdf_pdfversion_value_api() {
    let mut version = PdfVersion::default();
    assert_eq!(version.get_version(), ("0.0".to_string(), 0));

    version.update_if_greater(PdfVersion::new_with_extension_level(1, 7, 3));
    assert_eq!(version.major(), 1);
    assert_eq!(version.minor(), 7);
    assert_eq!(version.extension_level(), 3);
    assert_eq!(version.get_version(), ("1.7".to_string(), 3));

    version.update_if_greater(PdfVersion::new_with_extension_level(1, 7, 2));
    assert_eq!(version, PdfVersion::new_with_extension_level(1, 7, 3));
    assert!(
        PdfVersion::new_with_extension_level(1, 7, 2)
            < PdfVersion::new_with_extension_level(1, 7, 3)
    );
    assert!(PdfVersion::new_with_extension_level(1, 6, 99) < PdfVersion::new(1, 7));
}

#[test]
fn parses_only_existing_flpdf_major_minor_syntax() {
    assert_eq!(PdfVersion::parse("1.7"), Some(PdfVersion::new(1, 7)));
    assert_eq!(PdfVersion::parse("1.10"), Some(PdfVersion::new(1, 10)));
    assert_eq!(PdfVersion::parse("invalid"), None);
    assert_eq!(PdfVersion::parse("1.7.3"), None);
    assert_eq!(PdfVersion::parse("256.0"), None);
}

#[test]
fn public_parser_returns_the_value_type() {
    assert_eq!(parse_pdf_version("1.7"), Some(PdfVersion::new(1, 7)));
}

#[test]
fn parses_qpdf_version_spec_into_base_version_and_extension_level() {
    assert_eq!(parse_pdf_version_spec("1.3"), Some(("1.3".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.7.1"), Some(("1.7".into(), 1)));
    assert_eq!(parse_pdf_version_spec("1.8.0"), Some(("1.8".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.8.5"), Some(("1.8".into(), 5)));
}

#[test]
fn parses_qpdf_version_spec_with_raw_version_and_lenient_extension() {
    assert_eq!(parse_pdf_version_spec("1.7."), Some(("1.7.".into(), 0)));
    assert_eq!(parse_pdf_version_spec("1.7.1.2"), Some(("1.7".into(), 1)));
    assert_eq!(parse_pdf_version_spec("1.7.2x"), Some(("1.7".into(), 2)));
    assert_eq!(parse_pdf_version_spec("1.7.+2x"), Some(("1.7".into(), 2)));
    assert_eq!(parse_pdf_version_spec("abc"), Some(("abc".into(), 0)));
    assert_eq!(parse_pdf_version_spec(".7"), Some((".7".into(), 0)));
}

#[test]
fn rejects_version_specs_with_qpdf_integer_overflow() {
    for value in [
        "1.7.999999999999999999999",
        "2147483648.0",
        "1.2147483648",
        // No dot at all: qpdf's `QPDFWriter::parseVersion` still calls
        // `QUtil::string_to_int` on the whole value for the major component
        // (`QPDFWriter.cc:744-757`), so this range check applies here too.
        "2147483648",
    ] {
        assert_eq!(parse_pdf_version_spec(value), None, "{value:?}");
    }
}
