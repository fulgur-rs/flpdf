use flpdf::{EncryptMethod, EncryptParams, Pdf, PdfWriter};

fn matching_out_of_range_header_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n45\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_out_of_range_stream_header_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn matching_out_of_range_object_header_is_not_damaged() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_header_pdf()).expect("open PDF");

    pdf.get_all_objects()
        .expect("qpdf reads the matching raw object header");

    assert!(
        pdf.repair_diagnostics().entries().is_empty(),
        "matching raw object identity must not trigger recovery diagnostics: {:?}",
        pdf.repair_diagnostics().entries()
    );
}

#[test]
fn qdf_renumber_keeps_a_raw_generation_child_as_a_reference() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_header_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to Catalog");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write QDF output");
    let output = writer.get_buffer().expect("read QDF output");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("/RawChild 3 0 R"),
        "renumber child emitter must use raw qpdf identity lookup: {text}"
    );
    assert!(
        text.contains("3 0 obj\n45\nendobj"),
        "the raw child must be emitted at its assigned output number: {text}"
    );
    assert!(
        text.contains("%% Original object ID: 5 65536\n"),
        "QDF provenance must retain the raw source identity: {text}"
    );
    assert!(
        !text.contains("/RawChild 45"),
        "the raw indirect child must not be inlined: {text}"
    );
}

#[test]
fn rewrite_renumber_keeps_a_raw_generation_child_as_a_reference_in_compact_mode() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_header_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to Catalog");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write compact output");
    let output = writer.get_buffer().expect("read compact output");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("/RawChild 3 0 R"),
        "dynamic compact emitter must use raw qpdf identity lookup: {text}"
    );
    assert!(
        text.contains("3 0 obj\n45\nendobj"),
        "the raw child must be emitted at its assigned output number: {text}"
    );
}

#[test]
fn qdf_renumber_keeps_a_raw_generation_stream_as_a_reference() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_stream_header_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw stream child");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write QDF output");
    let output = writer.get_buffer().expect("read QDF output");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("/RawChild 3 0 R"),
        "QDF stream child emitter must use raw qpdf identity lookup: {text}"
    );
    assert!(
        text.contains("3 0 obj") && text.contains("stream\n"),
        "the raw stream must be emitted at its assigned output number: {text}"
    );
}

#[test]
fn encrypted_qdf_renumber_keeps_a_raw_generation_stream_as_a_reference() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_stream_header_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw stream child");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_qdf_mode(true);
    writer.set_encryption_parameters(EncryptParams::rc4(
        EncryptMethod::V2Rc4128,
        b"user",
        b"owner",
    ));
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write encrypted QDF output");
    let output = writer.get_buffer().expect("read encrypted QDF output");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("/RawChild 3 0 R"),
        "encrypted QDF child emitter must use raw qpdf identity lookup: {text}"
    );
    assert!(
        text.contains("3 0 obj") && text.contains("stream\n"),
        "the raw stream must be emitted at its assigned output number: {text}"
    );
}
