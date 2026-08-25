use std::fs;
use std::path::Path;

#[test]
fn flatten_annotation_rect_tests_use_qpdf_annotation_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let cases = [
        (
            "fn flatten_transforms_annotation_rect",
            "fn flatten_transforms_direct_and_indirect_annotation_rects",
        ),
        (
            "fn flatten_transforms_annotation_rect_via_indirect_annots",
            "fn flatten_rejects_non_leaf_target_even_when_rotate_zero",
        ),
    ];

    for (start, end) in cases {
        let block = source
            .split_once(start)
            .expect("flatten annotation test must remain")
            .1
            .split_once(end)
            .expect("flatten annotation test boundary must remain")
            .0;
        for marker in [
            "resolve_object(",
            "resolve_borrowed(",
            "Object::",
            "set_object(",
        ] {
            assert!(
                !block.contains(marker),
                "flatten annotation inspection must not keep raw route marker {marker:?}"
            );
        }
        assert!(
            block.contains("get_annotation_handles("),
            "flatten annotation inspection must use qpdf-shaped annotation handles"
        );
        assert!(
            block.contains("handle_to_pagebox("),
            "flatten annotation inspection must read rectangles through handles"
        );
    }
}
