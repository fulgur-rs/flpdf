use std::fs;
use std::path::Path;

#[test]
fn inherited_rotate_results_use_the_live_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let tests = source
        .split_once("fn inherited_rotate_is_materialized_on_leaf")
        .expect("inherited rotate test must remain")
        .1
        .split_once("fn apply_to_empty_slice_is_noop")
        .expect("inherited rotate tests must remain ordered")
        .0;
    let inspection = tests
        .split_once("apply_rotate_to_pages(&mut pdf, &[page_ref], &op)")
        .expect("inherited rotate tests must apply their operation")
        .1;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
    ] {
        assert!(
            !inspection.contains(marker),
            "inherited rotate inspection must not keep raw route marker {marker:?}"
        );
    }
    assert!(
        inspection.contains("rotate_value("),
        "inherited rotate inspection must use the live rotate_value helper"
    );
}
