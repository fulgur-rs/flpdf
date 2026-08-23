use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn annotation_object_helper_uses_the_qpdf_object_helper_module_path() {
    let src = source_root();
    let new_path = src.join("annotation_object_helper.rs");
    let old_path = src.join("annotation_helper.rs");

    assert!(
        new_path.is_file(),
        "QPDFAnnotationObjectHelper implementation must live at {}",
        new_path.display()
    );
    assert!(
        !old_path.exists(),
        "the old annotation_helper.rs route must be deleted"
    );

    let lib = fs::read_to_string(src.join("lib.rs")).expect("lib.rs must be readable");
    assert!(lib.contains("pub mod annotation_object_helper;"));
    assert!(!lib.contains("pub mod annotation_helper;"));

    let module_index = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/qpdf-module-doc-index.md"),
    )
    .expect("qpdf module index must be readable");
    assert!(module_index.contains("crates/flpdf/src/annotation_object_helper.rs"));
    assert!(!module_index.contains("crates/flpdf/src/annotation_helper.rs"));
}
