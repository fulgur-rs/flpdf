use flpdf::{EncryptMethod, EncryptParams, ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};

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

fn matching_out_of_range_one_page_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page_offset = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n45\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_out_of_range_one_page_stream_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page_offset = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_out_of_range_two_page_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n");
    let page1_offset = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let page2_offset = bytes.len();
    bytes.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n45\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page1_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page2_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn matching_out_of_range_object_header_is_not_damaged() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_header_pdf()).expect("open PDF");

    let objects = pdf
        .get_all_objects()
        .expect("qpdf reads the matching raw object header");

    assert!(objects
        .iter()
        .any(|object| object.unparse() == b"5 65536 R"));

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
fn linearized_renumber_keeps_a_raw_generation_child_as_a_reference() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to Catalog");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write linearized output");
    let output = writer.get_buffer().expect("read linearized output");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("/RawChild "),
        "linearized Catalog must retain the raw child key: {text}"
    );
    assert!(
        !text.contains("/RawChild 45"),
        "the raw indirect child must not be inlined: {text}"
    );
    assert!(
        text.contains("\n45\nendobj"),
        "the raw child must be emitted as a linearized object: {text}"
    );
}

#[test]
fn linearized_generate_keeps_a_raw_generation_child_as_a_reference() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to Catalog");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write generated linearized output");
    let output = writer
        .get_buffer()
        .expect("read generated linearized output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_keeps_a_raw_generation_stream_as_a_reference() {
    let mut pdf =
        Pdf::open_mem_owned(matching_out_of_range_one_page_stream_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw stream child");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write linearized stream output");
    let output = writer.get_buffer().expect("read linearized stream output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild <<"));
    assert!(text.contains("stream\nabc"));
}

#[test]
fn encrypted_linearized_write_accepts_a_raw_generation_child() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to Catalog");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_encryption_parameters(EncryptParams::rc4(
        EncryptMethod::V2Rc4128,
        b"user",
        b"owner",
    ));
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write encrypted linearized output");
    assert!(!writer
        .get_buffer()
        .expect("read encrypted linearized output")
        .is_empty());
}

#[test]
fn encrypted_linearized_write_accepts_a_raw_generation_stream() {
    let mut pdf =
        Pdf::open_mem_owned(matching_out_of_range_one_page_stream_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw stream child");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_encryption_parameters(EncryptParams::rc4(
        EncryptMethod::V2Rc4128,
        b"user",
        b"owner",
    ));
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write encrypted linearized stream output");
    assert!(!writer
        .get_buffer()
        .expect("read encrypted linearized stream output")
        .is_empty());
}

#[test]
fn linearized_keeps_a_raw_generation_page_child_in_the_first_page_section() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.get_object_handle(flpdf::ObjectRef::new(3, 0))
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to first page");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write page raw-child output");
    let output = writer.get_buffer().expect("read page raw-child output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_generate_preserves_a_raw_child_when_another_object_uses_objstm() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    let ordinary = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(99))
        .expect("allocate ordinary object");
    let catalog = pdf.root_handle().expect("resolve Catalog");
    catalog
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to Catalog");
    catalog
        .replace_key(b"/Ordinary", ordinary)
        .expect("attach ordinary child to Catalog");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write generated ObjStm linearized output");
    let output = writer
        .get_buffer()
        .expect("read generated ObjStm linearized output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_keeps_a_raw_generation_child_shared_by_pages() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_two_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    for page_number in [3, 4] {
        pdf.get_object_handle(flpdf::ObjectRef::new(page_number, 0))
            .replace_key(b"/RawChild", raw_child.clone())
            .expect("attach raw child to page");
    }

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write shared raw-child output");
    let output = writer.get_buffer().expect("read shared raw-child output");
    std::fs::write("/tmp/flpdf-474u8-linearized-shared.pdf", &output)
        .expect("save shared raw-child output for qpdf check");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_generate_keeps_a_raw_page_child_alongside_objstm_members() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    let ordinary = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(99))
        .expect("allocate ordinary object");
    let page = pdf.get_object_handle(flpdf::ObjectRef::new(3, 0));
    page.replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to first page");
    page.replace_key(b"/Ordinary", ordinary)
        .expect("attach ordinary child to first page");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write generated page ObjStm linearized output");
    let output = writer
        .get_buffer()
        .expect("read generated page ObjStm linearized output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
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
