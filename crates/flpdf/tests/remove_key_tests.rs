use flpdf::{ObjectHandle, ObjectRef, Pdf};

#[test]
fn remove_key_resolves_an_indirect_dictionary_before_mutating_it() {
    let bytes = include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec();
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open document");
    let dictionary = pdf.get_object_handle(ObjectRef::new(2, 0));
    assert!(!dictionary.is_resolved());

    dictionary
        .remove_key(b"/Count")
        .expect("remove dictionary key");

    assert!(!dictionary
        .try_has_key(b"/Count")
        .expect("check removed key"));
}

#[test]
fn remove_key_warns_for_a_non_dictionary_with_document_context() {
    let mut pdf = Pdf::empty().expect("empty document");
    pdf.set_suppress_warnings(true);
    let scalar = ObjectHandle::integer(42);
    scalar
        .set_object_description(&pdf, "test scalar")
        .expect("description");
    assert!(!pdf.any_warnings());

    scalar
        .remove_key(b"/Missing")
        .expect("remove dictionary key");

    assert_eq!(pdf.num_warnings(), 1);
    assert_eq!(scalar.try_get_int_value().expect("integer"), 42);
}

#[test]
fn duplicate_page_annotation_copy_replaces_the_shared_annotation_array() {
    let bytes =
        include_bytes!("../../../tests/fixtures/compat/link-annot-no-acroform.pdf").to_vec();
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open annotated page");
    let first_page = flpdf::PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("pages")[0]
        .object_ref()
        .expect("indirect page");
    let source_annots = pdf
        .get_object_handle(first_page)
        .try_get_key(b"/Annots")
        .expect("annotations");
    let source_count = source_annots
        .try_get_array_n_items()
        .expect("annotation count");
    assert!(source_count > 0);
    let original = source_annots
        .try_get_array_item(0)
        .expect("first annotation");
    let result =
        flpdf::rebuild_page_tree(&mut pdf, &[first_page, first_page]).expect("duplicate page");

    flpdf::copy_duplicate_page_annotations(&mut pdf, &result).expect("copy annotations");

    let copied = pdf
        .get_object_handle(result.new_kids[1])
        .try_get_key(b"/Annots")
        .expect("copied annotations");
    assert_eq!(
        copied.try_get_array_n_items().expect("copied count"),
        source_count
    );
    assert!(!copied
        .try_get_array_item(0)
        .expect("copied annotation")
        .is_same_object_as(&original));
    assert_eq!(
        source_annots.try_get_array_n_items().expect("source count"),
        source_count
    );
}
