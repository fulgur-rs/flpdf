use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn page_rotation_uses_separate_job_route_without_the_legacy_module() {
    let src = source_root();
    assert!(
        !src.join("page_rotate.rs").exists(),
        "the mixed page_rotate.rs route must be deleted"
    );
    assert!(
        src.join("job/rotate.rs").is_file(),
        "job rotation orchestration must have its own qpdf-shaped module"
    );

    let lib = fs::read_to_string(src.join("lib.rs")).expect("lib.rs must be readable");
    assert!(!lib.contains("pub mod page_rotate;"));
    assert!(!lib.contains("pub use page_rotate::"));

    let job = fs::read_to_string(src.join("job/mod.rs")).expect("job/mod.rs must be readable");
    assert!(job.contains("mod rotate;"));
    assert!(job.contains("pub use rotate::"));

    let rotate_spec =
        fs::read_to_string(src.join("job/rotate_spec.rs")).expect("rotate_spec.rs must exist");
    assert!(!rotate_spec.contains("crate::page_rotate"));

    let page_helper = fs::read_to_string(src.join("page_object_helper.rs"))
        .expect("page_object_helper.rs must be readable");
    assert!(!page_helper.contains("crate::page_rotate"));
}

#[test]
fn dead_batch_rotate_helper_and_types_are_removed_after_cli_cutover() {
    let rotate = fs::read_to_string(source_root().join("job/rotate.rs"))
        .expect("rotate.rs must be readable")
        .replace("\r\n", "\n");
    for forbidden in [
        "apply_rotate_to_pages",
        "pub enum RotateMode",
        "pub struct RotateOp",
    ] {
        assert!(
            !rotate.contains(forbidden),
            "dead rotate batch surface remains in rotate.rs: {forbidden}"
        );
    }

    let job = fs::read_to_string(source_root().join("job/mod.rs"))
        .expect("job/mod.rs must be readable")
        .replace("\r\n", "\n");
    assert!(!job.contains("apply_rotate_to_pages"));
    assert!(!job.contains("RotateMode"));
    assert!(!job.contains("RotateOp"));

    let lib = fs::read_to_string(source_root().join("lib.rs"))
        .expect("lib.rs must be readable")
        .replace("\r\n", "\n");
    assert!(!lib.contains("RotateMode"));
    assert!(!lib.contains("RotateOp"));

    let page_helper = fs::read_to_string(source_root().join("page_object_helper.rs"))
        .expect("page_object_helper.rs must be readable")
        .replace("\r\n", "\n");
    assert!(!page_helper.contains("crate::job::apply_rotate_to_pages"));
}
