//! Route contracts for qpdf-backed inline-image CLI coverage.

/// The oracle comparisons must be reached from `#[test]` bodies, not merely
/// defined.
///
/// Scanning the whole file would let the helper definitions satisfy every
/// required substring, so deleting every call from the test bodies would keep
/// this contract green while the suite stopped comparing anything. Slice the
/// source at the first `#[test]` and search only what follows.
#[test]
fn inline_image_transform_tests_invoke_the_pinned_qpdf_oracle() {
    let source = include_str!("cli_inline_images_transform.rs");
    let (helpers, bodies) = source
        .split_once("#[test]")
        .expect("the suite has at least one test");

    assert!(
        helpers.contains("ProcessCommand::new(\"qpdf\")"),
        "inline-image transformation tests must execute qpdf 11.9.0"
    );
    for (call, why) in [
        (
            "assert_page_images_match(",
            "the test bodies must compare page-image metadata against qpdf",
        ),
        (
            "assert_qdf_bytes_match(",
            "metadata alone cannot see a transform that lands in a Form XObject; \
             the test bodies must also compare whole --qdf output bytes",
        ),
        (
            "assert_page_selection_qdf_bytes_match(",
            "the page-selection route must be compared through its own \
             invocation shape, not only through image metadata",
        ),
    ] {
        assert!(bodies.contains(call), "{why}");
    }
    assert!(
        helpers.contains("qpdf_pages_json(flpdf_output)"),
        "the oracle must also read flpdf's output, so a writer and reader that \
         drift together cannot stay green"
    );
}
