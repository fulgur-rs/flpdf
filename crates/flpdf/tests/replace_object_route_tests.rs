use flpdf::{ObjectHandle, ObjectRef, Pdf};

#[test]
fn public_replace_object_keeps_the_target_handle_identity() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let target_ref = pdf.root_ref().expect("empty PDF root");
    let target = pdf.get_object_handle(target_ref);
    pdf.resolve(&target)
        .expect("resolve root before replacement");

    let replacement =
        ObjectHandle::dictionary(vec![(b"/Marker".to_vec(), ObjectHandle::integer(42))]);
    let returned = pdf
        .replace_object(target_ref, replacement)
        .expect("replace_object accepts a direct owned value");

    assert!(returned.is_same_object_as(&target));
    assert_eq!(target.object_ref(), Some(target_ref));
    assert_eq!(target.get_key(b"/Marker").as_integer(), Some(42));
}

#[test]
fn public_replace_object_rejects_an_indirect_replacement() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let target_ref = ObjectRef::new(1, 0);
    let indirect_replacement = pdf.get_object_handle(ObjectRef::new(2, 0));

    let error = pdf
        .replace_object(target_ref, indirect_replacement)
        .expect_err("qpdf rejects an indirect replacement handle");
    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: QPDF::replaceObject called with indirect object handle"
    );
}
