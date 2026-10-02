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
