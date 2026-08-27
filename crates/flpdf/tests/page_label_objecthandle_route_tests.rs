use std::fs;
use std::path::Path;

#[test]
fn page_label_helper_has_no_test_only_raw_object_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_label_document_helper.rs"))
        .expect("page_label_document_helper.rs must be readable");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "set_object(",
        "Object::",
    ] {
        assert!(
            !source.contains(forbidden),
            "page label helper still contains raw-route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "Pdf::resolve",
        "make_indirect_from_object_handle",
    ] {
        assert!(
            source.contains(required),
            "page label helper must retain canonical marker {required:?}"
        );
    }
}
