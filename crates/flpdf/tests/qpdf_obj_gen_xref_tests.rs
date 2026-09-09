use flpdf::Pdf;

fn classic_xref_with_free_generation(generation: u32) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 {generation:05} f \n{object_offset:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn classic_free_row_generation_65536_is_accepted_without_recovery() {
    let mut pdf = Pdf::open_mem_owned(classic_xref_with_free_generation(65_536))
        .expect("qpdf accepts the five-digit free-row generation");
    let root = pdf
        .trailer()
        .try_get_key(b"/Root")
        .expect("trailer has /Root");
    pdf.resolve(&root).expect("catalog resolves");

    assert!(
        pdf.repair_diagnostics().entries().is_empty(),
        "qpdf does not recover this valid xref table: {:?}",
        pdf.repair_diagnostics().entries()
    );
}
