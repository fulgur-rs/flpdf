use std::io::Cursor;

#[test]
fn pdf_module_exposes_the_canonical_document_type() {
    let bytes = include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec();
    let mut from_module = flpdf::pdf::Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(from_module.version(), "1.7");
    assert_eq!(from_module.root_ref(), Some(flpdf::ObjectRef::new(1, 0)));
    assert_eq!(
        from_module
            .trailer()
            .try_get_key(b"/Root")
            .unwrap()
            .object_ref(),
        from_module.root_ref()
    );

    let _: &mut flpdf::Pdf<_> = &mut from_module;
}

#[test]
fn root_handle_observes_a_root_replacement_made_through_the_live_trailer() {
    // root_handle() must not keep returning a stale catalog after /Root is
    // replaced through the live trailer handle -- matching root_ref(), which
    // always re-reads /Root fresh rather than trusting a cached handle.
    let bytes = include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec();
    let mut pdf = flpdf::pdf::Pdf::open(Cursor::new(bytes)).unwrap();

    let original_root = pdf.root_handle().expect("original root");
    let original_ref = original_root.object_ref();

    let replacement = pdf
        .make_indirect_object_handle(flpdf::ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            flpdf::ObjectHandle::name(b"Catalog".to_vec()),
        )]))
        .expect("install replacement catalog");
    let replacement_ref = replacement.object_ref();
    assert_ne!(replacement_ref, original_ref);

    pdf.trailer()
        .replace_key(b"/Root", replacement.clone())
        .expect("replace /Root through the live trailer");

    let observed_root = pdf
        .root_handle()
        .expect("root_handle must observe the replacement");
    assert_eq!(
        observed_root.object_ref(),
        replacement_ref,
        "root_handle() returned the stale pre-replacement catalog"
    );
}
