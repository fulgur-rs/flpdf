use std::fs;
use std::path::Path;

#[test]
fn flatten_rotation_rotate_assertions_use_the_live_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");

    let cases = [
        (
            "fn flatten_90_swaps_mediabox_zeroes_rotate_and_wraps_content",
            "fn flatten_is_noop_when_rotate_zero",
        ),
        (
            "fn flatten_180_keeps_mediabox_dims_and_zeroes_rotate",
            "fn flatten_transforms_cropbox_present_on_leaf",
        ),
        ("fn flatten_does_not_materialize_inherited_rotate", "\n}"),
    ];

    for (start, end) in cases {
        let block = source
            .split_once(start)
            .expect("flatten rotation test must remain")
            .1
            .split_once(end)
            .expect("flatten rotation test boundary must remain")
            .0;
        for marker in [
            "resolve_object(",
            "resolve_borrowed(",
            "Object::",
            "set_object(",
        ] {
            assert!(
                !block.contains(marker),
                "flatten rotation inspection must not keep raw route marker {marker:?}"
            );
        }
        assert!(
            block.contains("rotate_value("),
            "flatten rotation inspection must use the live rotate_value helper"
        );
    }
}
