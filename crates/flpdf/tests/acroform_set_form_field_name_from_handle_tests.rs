use std::collections::BTreeSet;

use flpdf::{AcroFormDocumentHelper, ObjectHandle, Pdf};

fn text_field(name: &[u8]) -> ObjectHandle {
    ObjectHandle::dictionary(vec![
        (b"/FT".to_vec(), ObjectHandle::name(b"Tx".to_vec())),
        (b"/T".to_vec(), ObjectHandle::string(name.to_vec())),
    ])
}

fn partial_name(field: &ObjectHandle) -> Vec<u8> {
    field
        .try_get_key(b"/T")
        .expect("read field /T")
        .try_get_value_as_utf8()
        .expect("read UTF-8 field /T")
        .expect("field /T should be a string")
}

#[test]
fn handle_and_object_ref_routes_set_unicode_names_and_refresh_the_cache() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let by_handle = pdf
        .make_indirect_object_handle(text_field(b"BeforeHandle"))
        .expect("allocate handle field");
    let by_ref = pdf
        .make_indirect_object_handle(text_field(b"BeforeRef"))
        .expect("allocate ref field");
    let by_ref_id = by_ref.object_ref().expect("indirect field");
    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze empty form");

    helper
        .set_form_field_name_from_handle(by_handle.clone(), "AfterHandle")
        .expect("set name through a live handle");
    helper
        .set_form_field_name(by_ref_id, "AfterRef")
        .expect("set name through the ObjectRef convenience route");

    assert_eq!(partial_name(&by_handle), b"AfterHandle");
    assert_eq!(partial_name(&by_ref), b"AfterRef");
    assert_eq!(
        helper
            .get_fields_with_qualified_name("AfterHandle")
            .expect("query updated handle-name cache"),
        BTreeSet::from([by_handle.object_ref().expect("indirect field")])
    );
    assert_eq!(
        helper
            .get_fields_with_qualified_name("AfterRef")
            .expect("query updated ObjectRef-name cache"),
        BTreeSet::from([by_ref_id])
    );
}

#[test]
fn direct_field_handle_mutates_then_remains_ignored_by_the_cache() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let direct = text_field(b"BeforeDirect");
    assert!(direct.object_ref().is_none());
    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze empty form");

    helper
        .set_form_field_name_from_handle(direct.clone(), "AfterDirect")
        .expect("mutate direct field handle before cache traversal");

    assert_eq!(partial_name(&direct), b"AfterDirect");
    assert!(helper
        .get_fields_with_qualified_name("AfterDirect")
        .expect("query cache after direct traversal")
        .is_empty());
}
