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

#[test]
fn production_renumber_route_has_only_the_canonical_handle_engine() {
    let source = fs::read_to_string(source_root().join("writer/rewrite_renumber.rs"))
        .expect("rewrite_renumber.rs must be readable");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("rewrite_renumber source has a production section");

    assert!(
        production.contains("CanonicalCatalogFirstRenumber"),
        "production renumbering must retain the canonical handle engine"
    );
    for forbidden in [
        "struct CatalogFirstRenumber",
        "impl CatalogFirstRenumber",
        "CatalogFirstRenumber::",
        "collect_qpdf_enqueue_refs",
    ] {
        assert!(
            !production.contains(forbidden),
            "production renumbering still contains obsolete raw engine token {forbidden:?}"
        );
    }
}
