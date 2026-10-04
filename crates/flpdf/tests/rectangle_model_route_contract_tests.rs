//! The Rust page/annotation geometry surface uses qpdf's single Rectangle model.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn qpdf_rectangle_has_no_duplicate_page_box_model() {
    let page_helper = include_str!("../src/page_object_helper.rs");
    let annotation = production_source(include_str!("../src/annotation_object_helper.rs"));
    let rendering = production_source(include_str!("../src/form_field_object_helper/rendering.rs"));
    let overlay = production_source(include_str!("../src/job/overlay.rs"));
    let rotate = production_source(include_str!("../src/job/rotate.rs"));
    let lib = include_str!("../src/lib.rs");

    for (module, source) in [
        ("page_object_helper.rs", page_helper),
        ("annotation_object_helper.rs", annotation),
        ("form_field_object_helper/rendering.rs", rendering),
        ("job/overlay.rs", overlay),
        ("job/rotate.rs", rotate),
        ("lib.rs", lib),
    ] {
        assert!(
            !source.contains("PageBox"),
            "{module} retains the duplicate qpdf Rectangle model"
        );
    }

    assert!(
        annotation.contains("pub fn get_rect(&mut self) -> Result<Rectangle>"),
        "AnnotationObjectHelper::get_rect must return qpdf's Rectangle type"
    );
    assert!(lib.contains("pub use matrix::{Matrix, Rectangle};"));
}
