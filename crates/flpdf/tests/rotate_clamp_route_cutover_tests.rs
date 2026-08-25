use std::fs;
use std::path::Path;

#[test]
fn rotate_clamp_results_use_the_live_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let boundaries = [
        (
            "fn add_clamps_an_out_of_i32_range_existing_rotate_like_qpdf",
            "fn add_clamps_a_below_i32_range_existing_rotate_like_qpdf",
        ),
        (
            "fn add_clamps_a_below_i32_range_existing_rotate_like_qpdf",
            "fn add_clamps_a_near_i64_max_existing_rotate_without_overflow",
        ),
        (
            "fn add_clamps_a_near_i64_max_existing_rotate_without_overflow",
            "fn inherited_rotate_is_materialized_on_leaf",
        ),
    ];

    for (start, end) in boundaries {
        let test = source
            .split_once(start)
            .expect("clamp test must remain")
            .1
            .split_once(end)
            .expect("clamp tests must remain ordered")
            .0;
        let inspection = test
            .split_once("apply_rotate_to_pages(&mut pdf, &[page_ref], &op)")
            .expect("clamp test must apply its operation")
            .1;
        for marker in [
            "resolve_object(",
            "resolve_borrowed(",
            "Object::",
            "set_object(",
        ] {
            assert!(
                !inspection.contains(marker),
                "clamp result inspection must not keep raw route marker {marker:?}"
            );
        }
        assert!(
            inspection.contains("rotate_value("),
            "clamp result inspection must use the live rotate_value helper"
        );
    }
}
