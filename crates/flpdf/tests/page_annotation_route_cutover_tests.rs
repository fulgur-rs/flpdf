use flpdf::{AcroFormDocumentHelper, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf};
use std::io::Cursor;
use std::path::Path;

#[test]
fn legacy_annotation_aggregate_route_is_removed_after_consumer_cutover() {
    let module_name = ["page", "annotation", "enum"].join("_");
    let function_name = ["enumerate", "page", "annotations"].join("_");
    let type_name = ["Enumerated", "Annotation"].concat();
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(format!("{module_name}.rs"));
    assert!(
        !source_path.exists(),
        "the flpdf-specific annotation aggregate source must be deleted"
    );

    for (label, source) in [
        ("flpdf::lib", include_str!("../src/lib.rs")),
        (
            "page_annotation_flatten",
            include_str!("../src/page_annotation_flatten.rs"),
        ),
        (
            "flpdf-cli::main",
            include_str!("../../flpdf-cli/src/main.rs"),
        ),
    ] {
        assert!(
            !source.contains(&module_name)
                && !source.contains(&function_name)
                && !source.contains(&type_name),
            "{label} still references the deleted annotation aggregate route"
        );
    }
}

#[test]
fn canonical_helpers_preserve_grouped_widget_field_association() {
    let bytes = include_bytes!("../../../tests/fixtures/compat/form-fields-and-annotations.pdf");
    let mut pdf = Pdf::open(Cursor::new(bytes.as_slice())).unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let widgets = PageObjectHelper::new(page_ref, &mut pdf)
        .get_annotations_filtered(Some(b"/Widget"))
        .unwrap();
    assert_eq!(widgets.len(), 5);

    let mut acroform = AcroFormDocumentHelper::new(&mut pdf);
    let annotation_to_field = acroform.annotation_to_field_map().unwrap();
    let top_level_fields: Vec<ObjectRef> = widgets
        .into_iter()
        .map(|widget| {
            let annotation_ref = widget.object_ref().unwrap();
            acroform
                .get_top_level_field(annotation_to_field[&annotation_ref])
                .unwrap()
        })
        .collect();

    assert_eq!(
        top_level_fields
            .iter()
            .filter(|field_ref| **field_ref == ObjectRef::new(5, 0))
            .count(),
        3,
        "the three grouped widgets must resolve through the canonical qpdf helper composition"
    );
}
