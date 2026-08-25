use std::fs;
use std::path::Path;

#[test]
fn apply_rotate_direct_results_use_the_live_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let direct_tests = source
        .split_once("fn assign_replaces_existing_rotate")
        .expect("assign rotate test must remain")
        .1
        .split_once("fn add_clamps_an_out_of_i32_range_existing_rotate_like_qpdf")
        .expect("direct rotate tests must precede clamp tests")
        .0;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
        "materialize(",
    ] {
        assert!(
            !direct_tests.contains(marker),
            "direct rotate tests must not keep raw route marker {marker:?}"
        );
    }
    assert!(
        direct_tests.contains("rotate_value("),
        "direct rotate tests must inspect /Rotate through the live handle helper"
    );
}
