//! Contract test for the Filespec helper test handle cutover.

#[test]
fn filespec_helper_tests_use_canonical_handles() {
    let source = include_str!("../src/filespec_helper/mod.rs");
    let tests = source
        .split_once("#[cfg(test)]")
        .expect("filespec_helper must have a test module")
        .1;

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        "Dictionary::",
        "set_object(",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !tests.contains(forbidden),
            "Filespec helper tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "pdf.resolve(",
        "set_object_handle(",
    ] {
        assert!(
            tests.contains(required),
            "Filespec helper tests must retain canonical marker {required:?}"
        );
    }
}
