use flpdf::{
    AcroFormDocumentHelper, FormFieldObjectHelper, ObjectRef, PageDocumentHelper, PageObjectHelper,
    Pdf,
};
use std::io::Cursor;

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
