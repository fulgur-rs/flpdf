//! Integration test: qpdf --json=2 vs flpdf --json=2 schema-diff over a curated
//! fixture corpus (beads flpdf-9hc.11.14).

#[allow(dead_code, unused_imports)]
mod support;

#[test]
fn json_schema_diff_corpus_smoke() {
    // Real corpus test added in Task 7. This placeholder verifies the module
    // wiring compiles.
    let _ = std::any::type_name::<support::json_diff::Divergence>();
}
