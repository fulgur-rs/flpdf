use std::fs;
use std::path::Path;

#[test]
fn rotate_fixture_mutations_use_live_handle_writeback() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let cases = [
        (
            "fn resolve_defaults_to_zero_for_a_parent_cycle",
            "fn resolve_defaults_to_zero_for_a_non_dictionary_parent",
        ),
        (
            "fn resolve_defaults_to_zero_for_a_non_dictionary_parent",
            "fn resolve_preserves_non_standard_value",
        ),
        (
            "fn resolve_reports_depth_limit_at_a_direct_page_tree_node",
            "fn resolve_rejects_a_non_integer_rotate_entry",
        ),
        (
            "fn resolve_rejects_a_non_integer_rotate_entry",
            "fn apply_rejects_a_non_multiple_angle_like_qpdf",
        ),
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

    for (start, end) in cases {
        let block = source
            .split_once(start)
            .expect("rotate mutation test must remain")
            .1
            .split_once(end)
            .expect("rotate mutation test boundary must remain")
            .0;
        for marker in [
            "resolve_object(",
            "resolve_borrowed(",
            "Object::",
            "set_object(",
            "materialize(",
        ] {
            assert!(
                !block.contains(marker),
                "rotate fixture setup must not keep raw route marker {marker:?}"
            );
        }
        assert!(
            block.contains("get_object_handle("),
            "rotate fixture setup must start from live object handles"
        );
        assert!(
            block.contains("replace_key("),
            "rotate fixture setup must use qpdf-shaped replace_key mutation"
        );
        assert!(
            block.contains("mark_object_handle_dirty("),
            "rotate fixture setup must mark live handle writeback dirty"
        );
    }
}
