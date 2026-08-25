use std::fs;
use std::path::Path;

#[test]
fn print_and_screen_annotation_assertions_use_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let cases = [
        (
            "fn print_mode_only_prints_print_bit_annotations",
            "fn screen_mode_only_flattens_non_print_annotations",
        ),
        (
            "fn screen_mode_only_flattens_non_print_annotations",
            "fn hidden_annotation_skipped_in_all_mode",
        ),
    ];

    for (start, end) in cases {
        let block = source
            .split_once(start)
            .expect("annotation mode test must remain")
            .1
            .split_once(end)
            .expect("annotation mode test boundary must remain")
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
                "annotation mode inspection must not keep raw route marker {marker:?}"
            );
        }
        assert!(
            block.contains("get_annotation_handles("),
            "annotation mode inspection must use qpdf-shaped annotation handles"
        );
        assert!(
            block.contains("object_ref()"),
            "annotation mode inspection must preserve indirect annotation identity"
        );
    }
}
