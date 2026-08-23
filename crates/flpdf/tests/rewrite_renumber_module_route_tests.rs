use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn rewrite_renumber_is_owned_by_the_writer_module() {
    let source = source_root();
    assert!(
        !source.join("rewrite_renumber.rs").exists(),
        "the crate-level rewrite_renumber route must be removed"
    );
    assert!(
        source.join("writer/rewrite_renumber.rs").is_file(),
        "rewrite_renumber must live under the writer module"
    );

    let lib = fs::read_to_string(source.join("lib.rs")).expect("lib.rs must be readable");
    assert!(
        !lib.contains("mod rewrite_renumber;"),
        "lib.rs must not declare the old crate-level module"
    );

    let writer = fs::read_to_string(source.join("writer.rs")).expect("writer.rs must be readable");
    assert!(
        writer.contains("mod rewrite_renumber;"),
        "writer.rs must declare the writer-owned module"
    );
}
