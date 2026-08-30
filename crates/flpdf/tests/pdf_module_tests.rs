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
