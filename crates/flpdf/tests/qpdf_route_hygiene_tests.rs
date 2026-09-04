use std::fs;
use std::path::Path;

fn source_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_source(path: impl AsRef<Path>) -> String {
    fs::read_to_string(source_root().join(path)).expect("read source")
}

#[test]
fn dead_qpdf_routes_are_removed_and_canonical_owners_remain() {
    let keys = read_source("encryption/keys.rs");
    assert!(
        !keys.contains("fn per_object_key("),
        "the dead per-object-key implementation remains"
    );
    assert!(
        !keys.contains("#![allow(dead_code)]"),
        "keys.rs still hides dead code"
    );

    let standard = read_source("encryption/standard.rs");
    assert!(
        !standard.contains("keys::per_object_key"),
        "standard encryption docs still point at the removed route"
    );
    assert!(
        read_source("encryption/state.rs").contains("fn compute_data_key("),
        "the canonical encryption-state key owner is missing"
    );

    let filters = read_source("filters.rs");
    assert!(
        !filters.contains("fn decode_stream_data_with_limits("),
        "the dead whole-buffer limits wrapper remains"
    );
    assert!(
        filters.contains("fn decode_stream_data_from_handle("),
        "the canonical ObjectHandle decode route is missing"
    );

    let reader = read_source("reader.rs");
    for dead in [
        "fn qtest_object_value_source_offset(",
        "fn qtest_array_item_source_offset(",
    ] {
        assert!(
            !reader.contains(dead),
            "dead reader wrapper remains: {dead}"
        );
    }
    assert!(
        reader.contains("fn qtest_object_value_source_offsets("),
        "the canonical batched object-offset route is missing"
    );
    assert!(
        reader.contains("fn qtest_array_item_source_offsets("),
        "the canonical batched array-offset route is missing"
    );

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
    ] {
        assert!(
            !tracked.contains(dead),
            "dead route remains tracked: {dead}"
        );
    }
}
