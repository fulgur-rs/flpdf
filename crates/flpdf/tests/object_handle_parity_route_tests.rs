use std::fs;
use std::path::Path;

#[test]
fn object_handle_parity_suite_is_canonical_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("tests/object_handle_parity_tests.rs"))
        .expect("object_handle_parity_tests.rs must be readable");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "resolve_to_terminal(",
        "materialize(",
        "Object::",
    ] {
        assert!(
            !source.contains(forbidden),
            "object_handle_parity_tests.rs still contains raw-route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            source.contains(required),
            "object_handle_parity_tests.rs must retain canonical marker {required:?}"
        );
    }
}
