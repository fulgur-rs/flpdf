//! Route contracts for qpdf-backed inline-image CLI coverage.

#[test]
fn inline_image_transform_tests_invoke_the_pinned_qpdf_oracle() {
    let source = include_str!("cli_inline_images_transform.rs");
    assert!(
        source.contains("ProcessCommand::new(\"qpdf\")"),
        "inline-image transformation tests must execute qpdf 11.9.0"
    );
    assert!(
        source.contains("qpdf_pages_json(qpdf_output)"),
        "inline-image transformation tests must inspect qpdf's own output"
    );
    assert!(
        source.contains("qpdf_pages_json(flpdf_output)"),
        "the oracle must also read flpdf's output, so a writer and reader that \
         drift together cannot stay green"
    );
    assert!(
        source.contains("fn assert_qdf_bytes_match("),
        "metadata alone cannot see a transform that lands in a Form XObject; \
         the suite must also compare whole --qdf output bytes"
    );
}
