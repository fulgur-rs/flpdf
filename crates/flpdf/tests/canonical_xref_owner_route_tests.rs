//! Route contracts for the canonical-owner xref handoff.

use std::fs;
use std::path::PathBuf;

fn production_source(path: &str) -> String {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(path),
    )
    .unwrap_or_else(|error| panic!("unable to read {path}: {error}"));
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn canonical_open_does_not_keep_a_bootstrap_owner_or_rebind_handoff() {
    let engine = production_source("engine.rs");
    for forbidden in [
        "let bootstrap_cache = loaded_state.bootstrap_cache",
        "drop(bootstrap_cache)",
        "rebind_handle_value(",
    ] {
        assert!(
            !engine.contains(forbidden),
            "canonical Pdf::open retains bootstrap handoff {forbidden}"
        );
    }
    let reader = production_source("reader.rs");
    assert!(
        !reader.contains("rebind_handle_value("),
        "canonical parsed xref streams must already belong to ResolverHandle"
    );
}

#[test]
fn pdf_teardown_has_one_canonical_disconnect_owner() {
    let pdf = production_source("pdf.rs");
    assert_eq!(
        pdf.matches("self.resolver.disconnect_all()").count(),
        1,
        "Pdf teardown must use exactly one ResolverHandle disconnect walk"
    );
    let resolver = production_source("reader/resolver.rs");
    // The order check below uses the first occurrence of each marker, so it is
    // only meaningful while each appears exactly once in production code. Pin
    // that precondition explicitly: without it, a second `object_cache.values()`
    // added earlier in the file would silently satisfy `clear < walk` and the
    // ordering contract would stop being tested.
    assert_eq!(
        resolver.matches("core.source_xref_entries.clear()").count(),
        1,
        "the xref-table clear must stay a single production site"
    );
    assert_eq!(
        resolver.matches("core.object_cache.values()").count(),
        1,
        "the object-cache walk must stay a single production site"
    );
    let clear = resolver
        .find("core.source_xref_entries.clear()")
        .expect("teardown clears the canonical xref table");
    let walk = resolver
        .find("core.object_cache.values()")
        .expect("teardown walks the canonical object cache");
    assert!(
        clear < walk,
        "qpdf teardown clears xref_table before disconnecting obj_cache"
    );
}
