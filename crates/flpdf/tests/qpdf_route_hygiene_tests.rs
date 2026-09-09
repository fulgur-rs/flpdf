use std::fs;
use std::path::Path;

fn source_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_source(path: impl AsRef<Path>) -> String {
    // Git's default autocrlf=true checkout converts source files to CRLF on
    // Windows; keep structural route guards independent of checkout EOLs.
    fs::read_to_string(source_root().join(path))
        .expect("read source")
        .replace("\r\n", "\n")
}

#[test]
fn dead_qpdf_routes_are_removed_and_canonical_owners_remain() {
    let keys = read_source("encryption/keys.rs");
    assert!(!keys.contains("fn per_object_key("));
    assert!(!keys.contains("#![allow(dead_code)]"));

    let standard = read_source("encryption/standard.rs");
    assert!(!standard.contains("keys::per_object_key"));
    let primitives = read_source("encryption/primitives.rs");
    assert!(primitives.contains("fn compute_data_key("));
    assert!(!read_source("encryption/state.rs").contains("fn compute_data_key("));
    assert!(!read_source("writer/encryption_state.rs").contains("fn compute_data_key("));

    let filters = read_source("filters.rs");
    assert!(!filters.contains("fn decode_stream_data_with_limits("));
    assert!(filters.contains("fn decode_stream_data_from_handle("));

    let reader = read_source("reader.rs");
    for dead in [
        "fn qtest_object_value_source_offset(",
        "fn qtest_array_item_source_offset(",
        "fn qtest_object_value_source_offsets(",
        "fn qtest_array_item_source_offsets(",
        "fn qtest_decode_parms_source_offset(",
        "fn source_stream_data_offset(",
    ] {
        assert!(
            !reader.contains(dead),
            "dead reader wrapper remains: {dead}"
        );
    }

    let tracked = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/qpdf-route-matrix/tracked-symbols.txt"),
    )
    .expect("read route symbol tracker");
    for dead in [
        "::per_object_key",
        "::decode_stream_data_with_limits",
        "::qtest_object_value_source_offset ",
        "::qtest_array_item_source_offset ",
        "::qtest_object_value_source_offsets",
        "::qtest_array_item_source_offsets",
        "::qtest_decode_parms_source_offset",
        "::read_window",
        "::resolution_fallbacks_remaining",
        "::MAX_RESOLUTION_FALLBACKS",
        "::parse_source_file_object_at",
    ] {
        assert!(
            !tracked.contains(dead),
            "dead route remains tracked: {dead}"
        );
    }
}

#[test]
fn canonical_xref_warnings_do_not_use_replay_or_deferred_bridges() {
    let engine = read_source("engine.rs");
    assert!(!engine.contains("replay_warnings("));
    assert!(!engine.contains("install_repair_diagnostics("));

    let resolver = read_source("reader/resolver.rs");
    assert!(!resolver.contains("replay_warnings("));
    assert!(!resolver.contains("defer_live_repair_diagnostics"));
    assert!(!resolver.contains("begin_deferred_repair_diagnostics"));

    let xref = read_source("xref.rs");
    assert!(!xref.contains("DeferredDiagnosticsGuard"));
}

#[test]
fn ownerless_xref_api_is_removed_in_favor_of_the_canonical_pdf_route() {
    let lib = read_source("lib.rs");
    assert!(!lib.contains("load_xref_and_trailer"));
    assert!(!lib.contains("LoadedXref"));

    let xref = read_source("xref.rs");
    assert!(!xref.contains("load_xref_and_trailer"));
    assert!(xref.contains("#[cfg(test)]\npub(crate) fn load_xref_state_with_options"));
    for dead in [
        "pub fn load_xref_and_trailer(",
        "pub fn load_xref_and_trailer_with_repair(",
        "pub fn load_xref_and_trailer_best_effort(",
        "pub struct LoadedXref",
    ] {
        assert!(
            !xref.contains(dead),
            "owner-less xref surface remains: {dead}"
        );
    }
}

#[test]
fn handle_only_helpers_do_not_carry_dead_pdf_parameters() {
    for path in [
        "filespec_helper/embedded_file_stream.rs",
        "nntree.rs",
        "page_annotation_flatten.rs",
        "page_object_helper.rs",
        "pages/repair.rs",
        "pages/tree_rebuild.rs",
        "resources.rs",
        // The CLI reaches the same handle-only routes, and the dirty bridge
        // left a dead parameter here too.
        "../../flpdf-cli/src/main.rs",
    ] {
        let source = read_source(path);
        assert!(
            !source.contains("_pdf: &mut Pdf"),
            "handle-only helper in {path} still carries a dead Pdf parameter"
        );
    }
}
