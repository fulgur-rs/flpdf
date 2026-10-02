//! Route contracts for qpdf-shaped outline object identity.

#[test]
fn outline_json_identity_stays_on_the_raw_object_handle() {
    let outline_item = include_str!("../src/outline_object_helper.rs");
    let outline_builder = include_str!("../src/outline_document_helper.rs");
    let json_sections = include_str!("../src/job/json_sections.rs");

    assert!(
        !outline_item.contains("source_ref: Option<ObjectRef>"),
        "OutlineItem must not expose a checked ObjectRef projection"
    );
    assert!(
        !outline_builder.contains("cursor.object_ref()"),
        "outline construction must retain raw identity from its canonical handle"
    );
    assert!(
        !json_sections.contains("item.source_ref"),
        "outline JSON must not bypass the canonical handle's raw identity"
    );
    assert_eq!(
        json_sections
            .matches("pdf_object_to_json_with_version(&item.object, version)?")
            .count(),
        2,
        "both qpdf JSON outline consumers must serialize the retained handle"
    );
}
