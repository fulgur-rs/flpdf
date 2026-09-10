use flpdf::Pdf;

fn matching_out_of_range_header_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n45\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
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
