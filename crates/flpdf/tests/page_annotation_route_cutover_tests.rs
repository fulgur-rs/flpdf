use flpdf::{
    AcroFormDocumentHelper, FormFieldObjectHelper, ObjectRef, PageDocumentHelper, PageObjectHelper,
    Pdf,
};
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

    let annotation_to_field = {
        let mut acroform = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        acroform.annotation_to_field_map().unwrap()
    };
    let top_level_fields: Vec<ObjectRef> = widgets
        .into_iter()
        .map(|widget| {
            let annotation_ref = widget.object_ref().unwrap();
            FormFieldObjectHelper::new(annotation_to_field[&annotation_ref], &mut pdf)
                .get_top_level_field()
                .unwrap()
                .0
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

#[test]
fn page_annotation_flatten_production_has_no_legacy_object_route() {
    let source = include_str!("../src/page_annotation_flatten.rs").replace("\r\n", "\n");
    let production = source
        .split_once("#[cfg(test)]\nmod tests")
        .expect("page_annotation_flatten test module marker")
        .0;

    for forbidden in [
        "Object::",
        "resolve_borrowed",
        "resolve_ref_chain",
        "Pdf::set_object",
        "qpdf-deviation",
    ] {
        assert!(
            !production.contains(forbidden),
            "page_annotation_flatten production still uses legacy {forbidden} route"
        );
    }
}
