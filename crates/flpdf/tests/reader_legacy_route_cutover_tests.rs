use std::fs;

fn production_region<'a>(source: &'a str, module: &str) -> &'a str {
    source
        .split_once("#[cfg(test)]\nmod tests")
        .map(|(production, _)| production)
        .unwrap_or_else(|| {
            assert_eq!(module, "pdf.rs", "only pdf.rs has no inline test module");
            source
        })
}

#[test]
fn reader_production_has_no_legacy_resolver_bridge() {
    let source = fs::read_to_string(format!("{}/src/reader.rs", env!("CARGO_MANIFEST_DIR")))
        .expect("reader source");
    let production = production_region(&source, "reader.rs");

    for forbidden in [
        "pub fn resolve_object(",
        "pub fn resolve_borrowed(",
        "fn resolve_to_cache(",
        "materialize_canonical_compatibility_value",
        "materialize_handle_for_legacy",
        "reconcile_legacy_materialized_memos",
        "legacy_materialized_memo",
        "legacy_materialized_replacement_refs",
    ] {
        assert!(
            !production.contains(forbidden),
            "reader.rs production retains legacy resolver marker {forbidden:?}"
        );
    }

    for required in [
        "pub fn resolve(",
        "get_object_handle(",
        "mark_object_handle_dirty(",
    ] {
        assert!(
            production.contains(required),
            "reader.rs production must retain canonical marker {required:?}"
        );
    }
}

#[test]
fn pdf_production_uses_live_handle_access_for_extension_level() {
    let source = fs::read_to_string(format!("{}/src/pdf.rs", env!("CARGO_MANIFEST_DIR")))
        .expect("pdf source");
    let production = production_region(&source, "pdf.rs");

    for forbidden in [
        "legacy_materialized_memo",
        "legacy_materialized_replacement_refs",
        "resolve_object(",
        "resolve_object_value(",
    ] {
        assert!(
            !production.contains(forbidden),
            "pdf.rs production retains legacy marker {forbidden:?}"
        );
    }
    assert!(
        production.contains("ObjectHandle"),
        "pdf.rs production must use ObjectHandle"
    );
    assert!(
        production.contains("adobe_extension_level"),
        "pdf.rs must retain the extension-level API"
    );
}

#[test]
fn reader_legacy_bridge_is_not_publicly_reexported() {
    let source = fs::read_to_string(format!("{}/src/lib.rs", env!("CARGO_MANIFEST_DIR")))
        .expect("lib source");
    assert!(
        !source.contains("resolve_borrowed"),
        "lib.rs must not re-export the removed raw resolver"
    );
}
