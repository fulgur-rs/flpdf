//! Route contracts for qpdf-backed inline-image CLI coverage.

#[test]
fn inline_image_transform_tests_invoke_the_pinned_qpdf_oracle() {
    let source = include_str!("cli_inline_images_transform.rs");
    assert!(
        source.contains("ProcessCommand::new(\"qpdf\")"),
        "inline-image transformation tests must execute qpdf 11.9.0"
    );
    assert!(
        source.contains("qpdf_pages_json("),
        "inline-image transformation tests must inspect qpdf output"
    );
}
