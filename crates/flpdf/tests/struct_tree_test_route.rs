//! Contract tests for the structure-tree page-reference test handle cutover.

fn test_region(path: &str) -> String {
    let source = std::fs::read_to_string(path).expect("test module must be readable");
    source
        .split_once("#[cfg(test)]")
        .expect("test module must have a cfg(test) region")
        .1
        .to_owned()
}

#[test]
fn structure_tree_page_tests_use_canonical_handles() {
    let tests = test_region(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/struct_tree_pg.rs"
    ));

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        "Dictionary",
        "set_object(",
    ] {
        assert!(
            !tests.contains(forbidden),
            "struct_tree_pg tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "pdf.resolve(",
        ".get_key(",
    ] {
        assert!(
            tests.contains(required),
            "struct_tree_pg tests must retain canonical marker {required:?}"
        );
    }
}

#[test]
fn objr_annotation_page_tests_use_canonical_handles() {
    let tests = test_region(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/objr_obj_annot_p.rs"
    ));

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        "Dictionary",
        "set_object(",
    ] {
        assert!(
            !tests.contains(forbidden),
            "objr_obj_annot_p tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "pdf.resolve(",
        ".get_key(",
    ] {
        assert!(
            tests.contains(required),
            "objr_obj_annot_p tests must retain canonical marker {required:?}"
        );
    }
}
