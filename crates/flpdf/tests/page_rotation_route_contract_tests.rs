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
fn apply_rotate_to_pages_uses_canonical_live_handle_accessors() {
    let source = fs::read_to_string(source_root().join("job/rotate.rs"))
        .expect("rotate.rs must be readable");
    let start = source
        .find("pub fn apply_rotate_to_pages")
        .expect("apply_rotate_to_pages must exist");
    let body = &source[start..];
    let end = body
        .find("// ---------------------------------------------------------------------------\n// Public API")
        .expect("flattening API must follow page rotation");
    let body = &body[..end];

    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !body.contains(forbidden),
            "apply_rotate_to_pages retains non-canonical route {forbidden}"
        );
    }
    assert!(body.contains("page.try_dereference()?"));
    assert!(body.contains("page.try_as_dictionary()?"));
    assert!(body.contains("page.try_get_key(b\"/Type\")?"));
}
