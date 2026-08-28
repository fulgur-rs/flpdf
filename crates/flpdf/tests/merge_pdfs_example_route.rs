use std::fs;

#[test]
fn merge_pdfs_example_uses_canonical_object_handles() {
    let path = format!("{}/examples/merge_pdfs.rs", env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(path).expect("merge_pdfs example source");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "materialize(",
        "Object::",
        "Dictionary::",
        ".as_dict(",
        ".into_dict(",
        ".as_ref_id(",
    ] {
        assert!(
            !source.contains(forbidden),
            "merge_pdfs example retains legacy route marker: {forbidden}"
        );
    }

    for required in ["ObjectHandle", "get_object_handle(", "resolve(", "get_key("] {
        assert!(
            source.contains(required),
            "merge_pdfs example must use canonical route marker: {required}"
        );
    }
}
