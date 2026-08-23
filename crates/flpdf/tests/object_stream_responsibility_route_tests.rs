use std::fs;
use std::path::PathBuf;

fn writer_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("writer")
}

#[test]
fn object_streams_have_separate_qpdf_responsibility_modules() {
    let writer = writer_root();
    assert!(
        !writer.join("object_streams.rs").exists(),
        "the mixed object_streams.rs owner must be deleted"
    );

    for module in ["eligibility.rs", "planning.rs", "emission.rs"] {
        assert!(
            writer.join("object_streams").join(module).is_file(),
            "missing split ObjStm responsibility module: {module}"
        );
    }

    let module = fs::read_to_string(writer.join("object_streams/mod.rs"))
        .expect("object_streams/mod.rs must be readable");
    assert!(module.contains("mod eligibility;"));
    assert!(module.contains("mod planning;"));
    assert!(module.contains("mod emission;"));
    assert!(
        !module.contains("#![allow(dead_code)]"),
        "the split owner must not retain the monolithic dead-code scaffold"
    );
}
