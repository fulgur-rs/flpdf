//! Final source contracts for the qpdf-shaped ObjectHandle resolution routes.

use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn pdf_resolution_facades_are_removed_after_consumer_cutover() {
    let reader = fs::read_to_string(source_root().join("reader.rs"))
        .expect("reader.rs must be readable")
        .replace("\r\n", "\n");

    let qtest_resolve =
        "#[cfg(feature = \"qtest-driver\")]\n    #[doc(hidden)]\n    pub fn resolve(";
    assert!(
        reader.contains(qtest_resolve),
        "Pdf::resolve must be isolated to the qtest-driver exception feature"
    );
    assert!(
        !reader.contains("pub(crate) fn resolve_handle("),
        "Pdf::resolve_handle must not remain as a test facade"
    );
    assert!(
        !reader.contains("resolve_handle_ref"),
        "Pdf::resolve_handle_ref must remain absent"
    );
    assert!(
        !reader.contains("resolve_qpdf_json_handle"),
        "the removed JSON resolver facade must remain absent"
    );
}

#[test]
fn panic_key_facades_are_only_available_to_the_qtest_driver_feature() {
    let object_handle = fs::read_to_string(source_root().join("object_handle.rs"))
        .expect("object_handle.rs must be readable")
        .replace("\r\n", "\n");

    for method in ["get_key", "has_key"] {
        let marker =
            format!("#[cfg(feature = \"qtest-driver\")]\n    #[doc(hidden)]\n    pub fn {method}(");
        assert!(
            object_handle.contains(&marker),
            "ObjectHandle::{method} must be isolated to the qtest-driver exception feature"
        );
    }
}

#[test]
fn merge_example_does_not_turn_fallible_key_lookups_into_panics() {
    let example = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/merge_pdfs.rs"),
    )
    .expect("merge_pdfs example must be readable")
    .replace("\r\n", "\n");

    for forbidden in [
        "try_get_key(b\"/Resources\").unwrap()",
        "try_get_key(b\"/Font\").unwrap()",
        "try_get_key(b\"/F1\").unwrap().object_ref()",
    ] {
        assert!(
            !example.contains(forbidden),
            "merge_pdfs must keep fallible lookup errors on its Option boundary: {forbidden}"
        );
    }
}
