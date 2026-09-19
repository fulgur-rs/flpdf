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

fn matching_out_of_range_two_streams_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page_offset = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let first_stream_offset = bytes.len();
    bytes.extend_from_slice(
        b"5 65536 obj\n<< /Type /Metadata /Length 3 >>\nstream\nabc\nendstream\nendobj\n",
    );
    let second_stream_offset = bytes.len();
    bytes.extend_from_slice(b"6 65536 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{first_stream_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(format!("{second_stream_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_out_of_range_content_stream_with_parameters_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page_offset = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let content_offset = bytes.len();
    bytes.extend_from_slice(
        b"5 65536 obj\n<< /Filter /FlateDecode /DecodeParms 6 0 R /Length 21 >>\nstream\n",
    );
    bytes.extend_from_slice(&[
        0x78, 0x9c, 0x2b, 0x54, 0x30, 0x54, 0x30, 0x00, 0x42, 0x08, 0x99, 0x9c, 0xab, 0x10, 0xc8,
        0x05, 0x00, 0x25, 0x41, 0x03, 0xbf,
    ]);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let parameter_offset = bytes.len();
    bytes.extend_from_slice(b"6 0 obj\n<< /Columns 1 /Marker (parameter-only) >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{content_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(format!("{parameter_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_out_of_range_part8_shared_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n",
    );
    let mut page_offsets = Vec::new();
    for _ in 0..3 {
        page_offsets.push(bytes.len());
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
    }
    // Rewrite the page object headers in place so their xref rows keep the
    // explicit page identities 3, 4, and 5.
    for (index, offset) in page_offsets.iter().copied().enumerate() {
        let header = format!("{} 0 obj\n", index + 3);
        bytes[offset..offset + b"3 0 obj\n".len()].copy_from_slice(header.as_bytes());
    }
    let raw_shared_offset = bytes.len();
    bytes.extend_from_slice(b"7 65536 obj\n<< /Marker (raw-shared) >>\nendobj\n");
    let checked_shared_offset = bytes.len();
    bytes.extend_from_slice(b"20 0 obj\n<< /Marker (checked-shared) >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    for offset in page_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("7 1\n{raw_shared_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(format!("20 1\n{checked_shared_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 21 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn matching_out_of_range_outline_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /PageMode /UseOutlines >>\nendobj\n",
    );
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page_offset = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let outline_offset = bytes.len();
    bytes.extend_from_slice(
        b"20 65536 obj\n<< /Count 1 /First 6 0 R /Marker (raw-outline-root) /Type /Outlines >>\nendobj\n",
    );
    let outline_item_offset = bytes.len();
    bytes.extend_from_slice(
        b"6 0 obj\n<< /Dest [3 0 R /Fit] /Marker (raw-outline-child) /Title (item) >>\nendobj\n",
    );
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{outline_item_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("20 1\n{outline_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 21 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

#[test]
fn linearized_raw_part7_private_objects_stay_with_their_page_group() {
    let mut pdf =
        Pdf::open_mem_owned(matching_out_of_range_three_page_pdf()).expect("open raw Part-7 PDF");
    let raw_first = pdf.get_object_handle_by_raw_identity(8, 65_536);
    let raw_second = pdf.get_object_handle_by_raw_identity(9, 65_536);
    let checked_first = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(100))
        .expect("allocate checked page-private object");
    let checked_second = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(101))
        .expect("allocate second checked page-private object");
    let page_two = pdf.get_object_handle(flpdf::ObjectRef::new(4, 0));
    page_two
        .replace_key(b"/RawPrivate", raw_first)
        .expect("attach first raw page-private object");
    page_two
        .replace_key(b"/CheckedPrivate", checked_first)
        .expect("attach first checked page-private object");
    let page_three = pdf.get_object_handle(flpdf::ObjectRef::new(5, 0));
    page_three
        .replace_key(b"/RawPrivate", raw_second)
        .expect("attach second raw page-private object");
    page_three
        .replace_key(b"/CheckedPrivate", checked_second)
        .expect("attach second checked page-private object");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write raw Part-7 output");
    let output = writer.get_buffer().expect("read raw Part-7 output");
    let text = String::from_utf8_lossy(&output);

    let raw_first_offset = text.find("\n45\nendobj").expect("first raw body");
    let checked_first_offset = text.find("\n100\nendobj").expect("first checked body");
    assert!(
        raw_first_offset < checked_first_offset,
        "page 2's raw QPDFObjGen member must precede its checked member in Part 7: {text}"
    );
    let raw_second_offset = text.find("\n46\nendobj").expect("second raw body");
    let checked_second_offset = text.find("\n101\nendobj").expect("second checked body");
    assert!(
        raw_second_offset < checked_second_offset,
        "page 3's raw QPDFObjGen member must precede its checked member in Part 7: {text}"
    );
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
    let second_object_offset = bytes.len();
    bytes.extend_from_slice(b"6 65536 obj\n46\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page1_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{page2_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(format!("{second_object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn matching_out_of_range_three_page_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n",
    );
    let mut page_offsets = Vec::new();
    for _ in 0..3 {
        page_offsets.push(bytes.len());
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
    }
    // Rewrite the object headers for pages 4 and 5 in place while preserving
    // the explicit offsets used by the xref rows.
    for (page_index, &offset) in page_offsets.iter().enumerate() {
        let page_number = page_index as u32 + 3;
        let header = format!("{page_number} 0 obj\n");
        bytes[offset..offset + header.len()].copy_from_slice(header.as_bytes());
    }
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"8 65536 obj\n45\nendobj\n");
    let second_object_offset = bytes.len();
    bytes.extend_from_slice(b"9 65536 obj\n46\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 10\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    for offset in page_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(b"0000000000 00000 f \n0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(format!("{second_object_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 10 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
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
fn encrypted_linearized_raw_metadata_does_not_cleartext_other_raw_streams() {
    let mut pdf =
        Pdf::open_mem_owned(matching_out_of_range_two_streams_pdf()).expect("open raw stream PDF");
    let raw_metadata = pdf.get_object_handle_by_raw_identity(5, 65_536);
    assert_eq!(
        raw_metadata
            .type_code()
            .expect("resolve raw metadata stream"),
        10
    );
    assert!(raw_metadata
        .try_is_stream_of_type(b"Metadata", b"")
        .expect("inspect raw metadata type"));
    let root = pdf.root_handle().expect("resolve Catalog");
    root.replace_key(b"/Metadata", raw_metadata)
        .expect("attach raw metadata stream");
    let raw_stream = pdf.get_object_handle_by_raw_identity(6, 65_536);
    root.replace_key(b"/RawStream", raw_stream)
        .expect("attach non-metadata raw stream");
    assert!(pdf
        .get_object_handle_by_raw_identity(5, 65_536)
        .try_is_stream_of_type(b"Metadata", b"")
        .expect("reinspect attached raw metadata type"));

    let mut encryption = EncryptParams::v4_aes128(b"user", b"owner");
    encryption.encrypt_metadata = false;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.set_encryption_parameters(encryption);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write encrypted raw metadata linearization");
    let output = writer
        .get_buffer()
        .expect("read encrypted raw metadata linearization");
    let cleartext_stream = b"stream\nabcendstream";

    assert_eq!(
        output
            .windows(cleartext_stream.len())
            .filter(|window| *window == cleartext_stream)
            .count(),
        1,
        "only the /Metadata stream may remain cleartext: {}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn plain_normalizes_a_raw_page_content_and_drops_its_parameters() {
    // The plain live route gates content normalization on the raw
    // `normalized_streams` set, matching qpdf's `old_og`-keyed membership test
    // (`QPDFWriter.cc:1279`). A content stream whose generation does not project
    // to an `ObjectRef` stays in that set, so it must still be normalized -- the
    // `contents_seq` map alone would drop it. This is the plain counterpart of
    // `linearized_normalizes_a_raw_page_content_and_drops_its_parameters`.
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_content_stream_with_parameters_pdf())
        .expect("open raw content stream PDF");
    let raw_content = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.get_object_handle(flpdf::ObjectRef::new(3, 0))
        .replace_key(
            b"/Contents",
            ObjectHandle::array(vec![raw_content, ObjectHandle::integer(7)]),
        )
        .expect("attach raw content stream");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_content_normalization(true);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write normalized raw content output");
    let output = writer
        .get_buffer()
        .expect("read normalized raw content output");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("q 1 0 0 1 0 0 cm Q"),
        "raw page content must be normalized on the plain route: {text}"
    );
    assert!(
        !text.contains("/DecodeParms"),
        "normalized content must not retain the source parameter edge: {text}"
    );
}

#[test]
fn linearized_normalizes_a_raw_page_content_and_drops_its_parameters() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_content_stream_with_parameters_pdf())
        .expect("open raw content stream PDF");
    let raw_content = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.get_object_handle(flpdf::ObjectRef::new(3, 0))
        .replace_key(
            b"/Contents",
            ObjectHandle::array(vec![raw_content, ObjectHandle::integer(7)]),
        )
        .expect("attach raw content stream");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_content_normalization(true);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write normalized raw content linearization");
    let output = writer
        .get_buffer()
        .expect("read normalized raw content linearization");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("q 1 0 0 1 0 0 cm Q"),
        "raw page content must be normalized: {text}"
    );
    assert!(
        !text.contains("parameter-only"),
        "a parameter object removed by refiltering must not remain reachable: {text}"
    );
    assert!(
        !text.contains("/DecodeParms"),
        "normalized content must not retain the source parameter edge: {text}"
    );
}

#[test]
fn linearized_normalizes_a_raw_direct_page_content() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_content_stream_with_parameters_pdf())
        .expect("open raw content stream PDF");
    let raw_content = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.get_object_handle(flpdf::ObjectRef::new(3, 0))
        .replace_key(b"/Contents", raw_content)
        .expect("attach raw direct content stream");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_content_normalization(true);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer
        .write()
        .expect("write normalized raw direct content linearization");
    let output = writer
        .get_buffer()
        .expect("read normalized raw direct content linearization");
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("q 1 0 0 1 0 0 cm Q"),
        "raw direct page content must be normalized: {text}"
    );
    assert!(
        !text.contains("parameter-only"),
        "a parameter object removed by refiltering must not remain reachable: {text}"
    );
}

#[test]
fn linearized_raw_part8_shared_hint_order_matches_qpdf_source_order() {
    if std::process::Command::new("qpdf")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("qpdf is unavailable; skipping raw Part-8 shared hint differential");
        return;
    }

    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_part8_shared_pdf())
        .expect("open raw Part-8 shared PDF");
    let checked_shared = pdf.get_object_handle(flpdf::ObjectRef::new(20, 0));
    let raw_shared = pdf.get_object_handle_by_raw_identity(7, 65_536);
    for page_number in [4, 5] {
        let page = pdf.get_object_handle(flpdf::ObjectRef::new(page_number, 0));
        page.replace_key(b"/Shared", checked_shared.clone())
            .expect("attach checked shared object");
    }
    for page_number in [4, 5] {
        pdf.get_object_handle(flpdf::ObjectRef::new(page_number, 0))
            .replace_key(b"/RawShared", raw_shared.clone())
            .expect("attach raw Part-8 shared object");
    }

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write raw Part-8 shared output");
    let output = writer.get_buffer().expect("read raw Part-8 shared output");
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("raw-shared"),
        "raw Part-8 object was not emitted: {text}"
    );
    assert!(
        text.contains("/Type /ObjStm"),
        "checked Part-8 object must be packed into an ObjStm: {text}"
    );
    assert!(
        text.find("raw-shared").expect("raw marker") < text.find("/Type /ObjStm").expect("ObjStm"),
        "qpdf's raw Part-8 source order must place object 7 before object 20's ObjStm container: {text}"
    );

    let directory = tempfile::tempdir().expect("create qpdf check directory");
    let path = directory.path().join("raw-part8-shared.pdf");
    std::fs::write(&path, &output).expect("write raw Part-8 shared output for qpdf");
    let check = std::process::Command::new("qpdf")
        .arg("--check-linearization")
        .arg(&path)
        .output()
        .expect("run qpdf linearization checker");
    assert!(
        check.status.success(),
        "qpdf must accept raw Part-8 shared hints: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn linearized_raw_outlines_emit_an_outline_hint_table() {
    let mut pdf =
        Pdf::open_mem_owned(matching_out_of_range_outline_pdf()).expect("open raw outline PDF");
    let raw_outlines = pdf.get_object_handle_by_raw_identity(20, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/Outlines", raw_outlines)
        .expect("attach raw outlines");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write raw outline linearization");
    let output = writer.get_buffer().expect("read raw outline linearization");
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.find("raw-outline-root").expect("raw outline root")
            < text.find("raw-outline-child").expect("raw outline child"),
        "qpdf places the /Outlines root before its outline items: {text}"
    );
    let hint_dict_start = text.find(" /S ").expect("hint stream dictionary");
    let hint_dict_end = text[hint_dict_start..]
        .find(">>\nstream")
        .map(|offset| hint_dict_start + offset)
        .expect("hint stream dictionary end");
    assert!(
        text[hint_dict_start..hint_dict_end].contains(" /O "),
        "a raw /Outlines object must produce the hint stream's outline table offset: {text}"
    );
    if std::process::Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok()
    {
        let directory = tempfile::tempdir().expect("create raw outline check directory");
        let path = directory.path().join("raw-outline.pdf");
        std::fs::write(&path, &output).expect("write raw outline output for qpdf");
        let check = std::process::Command::new("qpdf")
            .arg("--check-linearization")
            .arg(&path)
            .output()
            .expect("run qpdf raw outline checker");
        assert!(
            check.status.success() && check.stderr.is_empty(),
            "qpdf must accept the raw outline hint table without warnings: stdout={} stderr={}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
    }
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
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
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
fn linearized_routes_a_raw_open_document_child_before_the_hint_stream() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/OpenAction", raw_child)
        .expect("attach raw open-document child");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write raw open-document output");
    let output = writer.get_buffer().expect("read raw open-document output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/OpenAction "));
    assert!(!text.contains("/OpenAction 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_routes_a_raw_outline_child_to_part9() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_one_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/Outlines", raw_child)
        .expect("attach raw outline child");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write raw outline output");
    let output = writer.get_buffer().expect("read raw outline output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/Outlines "));
    assert!(!text.contains("/Outlines 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_keeps_a_raw_generation_child_shared_by_pages() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_two_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    let second_raw_child = pdf.get_object_handle_by_raw_identity(6, 65_536);
    for page_number in [3, 4] {
        let page = pdf.get_object_handle(flpdf::ObjectRef::new(page_number, 0));
        page.replace_key(b"/RawChild", raw_child.clone())
            .expect("attach raw child to page");
        page.replace_key(b"/SecondRawChild", second_raw_child.clone())
            .expect("attach second raw child to page");
    }

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write shared raw-child output");
    let output = writer.get_buffer().expect("read shared raw-child output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_routes_a_raw_child_private_to_a_later_page() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_two_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(5, 65_536);
    pdf.get_object_handle(flpdf::ObjectRef::new(4, 0))
        .replace_key(b"/RawChild", raw_child)
        .expect("attach raw child to second page");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write later-page raw-child output");
    let output = writer
        .get_buffer()
        .expect("read later-page raw-child output");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("/RawChild "));
    assert!(!text.contains("/RawChild 45"));
    assert!(text.contains("\n45\nendobj"));
}

#[test]
fn linearized_routes_a_raw_child_shared_by_later_pages_to_part8() {
    let mut pdf = Pdf::open_mem_owned(matching_out_of_range_three_page_pdf()).expect("open PDF");
    let raw_child = pdf.get_object_handle_by_raw_identity(8, 65_536);
    let second_raw_child = pdf.get_object_handle_by_raw_identity(9, 65_536);
    let ordinary = pdf
        .make_indirect_from_object_handle(ObjectHandle::integer(99))
        .expect("allocate ordinary ObjStm member");
    for page_number in [4, 5] {
        let page = pdf.get_object_handle(flpdf::ObjectRef::new(page_number, 0));
        page.replace_key(b"/RawChild", raw_child.clone())
            .expect("attach raw child to later page");
        page.replace_key(b"/SecondRawChild", second_raw_child.clone())
            .expect("attach second raw child to later page");
        page.replace_key(b"/Ordinary", ordinary.clone())
            .expect("attach ordinary ObjStm member to later page");
    }

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_compress_streams(false);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write Part-8 raw-child output");
    let output = writer.get_buffer().expect("read Part-8 raw-child output");
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
