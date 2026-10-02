use flpdf::{ObjectHandle, ObjectRef, Pdf};

#[test]
fn public_replace_object_keeps_the_target_handle_identity() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let target_ref = pdf.root_ref().expect("empty PDF root");
    let target = pdf.get_object_handle(target_ref);
    target
        .try_is_scalar()
        .expect("resolve root before replacement");

    let replacement =
        ObjectHandle::dictionary(vec![(b"/Marker".to_vec(), ObjectHandle::integer(42))]);
    let returned = pdf
        .replace_object(target_ref, replacement)
        .expect("replace_object accepts a direct owned value");

    assert!(returned.is_same_object_as(&target));
    assert_eq!(target.object_ref(), Some(target_ref));
    assert_eq!(
        target.try_get_key(b"/Marker").unwrap().as_integer(),
        Some(42)
    );
}

#[test]
fn public_replace_object_rejects_an_indirect_replacement() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let target_ref = ObjectRef::new(1, 0);
    let indirect_replacement = pdf.get_object_handle(ObjectRef::new(2, 0));

    let error = pdf
        .replace_object(target_ref, indirect_replacement)
        .expect_err("qpdf rejects an indirect replacement handle");
    assert!(matches!(error, flpdf::Error::Internal(message)
        if message == "QPDF::replaceObject called with indirect object handle"));
}

#[test]
fn public_replace_object_accepts_foreign_descendants_without_copying_them() {
    for cached in [false, true] {
        let mut source = Pdf::empty().unwrap();
        let foreign = source
            .make_indirect_object_handle(ObjectHandle::integer(17))
            .unwrap();
        let mut target = Pdf::empty().unwrap();
        let object_ref = ObjectRef::new(100, 0);
        let retained = cached.then(|| target.get_object_handle(object_ref));
        let replacement = ObjectHandle::dictionary(vec![(
            b"/Nested".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::dictionary(vec![(
                b"/Foreign".to_vec(),
                foreign.clone(),
            )])]),
        )]);

        let result = target.replace_object(object_ref, replacement).unwrap();

        let child = result
            .try_get_key(b"/Nested")
            .unwrap()
            .try_get_array_item(0)
            .unwrap()
            .try_get_key(b"/Foreign")
            .unwrap();
        assert!(child.is_same_object_as(&foreign));
        assert_eq!(child.try_get_int_value().unwrap(), 17);
        if let Some(retained) = retained {
            assert!(retained.is_same_object_as(&result));
        }
    }
}

#[test]
fn public_replace_object_accepts_a_direct_value_with_foreign_context() {
    let source = Pdf::empty().unwrap();
    let mut target = Pdf::empty().unwrap();
    target.set_suppress_warnings(true);
    let value = ObjectHandle::integer(23);
    value
        .set_object_description(&source, "foreign parsed value")
        .unwrap();
    let alias = value.clone();

    let result = target
        .replace_object(ObjectRef::new(100, 0), value)
        .unwrap();
    drop(source);

    assert!(result.is_same_object_as(&alias));
    assert_eq!(result.try_get_int_value().unwrap(), 23);
    assert_eq!(result.description(), b"foreign parsed value");
    assert_eq!(result.try_get_name().unwrap(), b"/QPDFFakeName");
    assert_eq!(target.num_warnings(), 1);
}

#[test]
fn public_replace_object_rejects_uninitialized_like_an_indirect_value() {
    let mut pdf = Pdf::empty().unwrap();
    let error = pdf
        .replace_object(ObjectRef::new(100, 0), ObjectHandle::uninitialized())
        .unwrap_err();
    assert!(matches!(error, flpdf::Error::Internal(message)
        if message == "QPDF::replaceObject called with indirect object handle"));
}

#[test]
fn writing_a_replacement_with_foreign_children_keeps_the_writer_owner_error() {
    let mut source = Pdf::empty().unwrap();
    let foreign = source
        .make_indirect_object_handle(ObjectHandle::integer(17))
        .unwrap();
    let mut target = Pdf::empty().unwrap();
    let root_ref = target.root_ref().unwrap();
    let pages = target
        .root_handle()
        .unwrap()
        .try_get_key(b"/Pages")
        .unwrap();
    let replacement = ObjectHandle::dictionary(vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Catalog".to_vec())),
        (b"/Pages".to_vec(), pages),
        (b"/Foreign".to_vec(), foreign),
    ]);
    target
        .replace_object(root_ref, replacement)
        .expect("replaceObject accepts the graph");

    let mut writer = flpdf::PdfWriter::new(&mut target);
    writer.set_output_memory().unwrap();
    let error = writer
        .write()
        .expect_err("enqueueObject checks indirect ownership");
    assert!(matches!(error, flpdf::Error::Internal(message)
        if message == "QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file."));
}
