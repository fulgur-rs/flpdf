use std::collections::BTreeSet;
use std::rc::Rc;

use flpdf::{AcroFormDocumentHelper, Error, Matrix, ObjectHandle, Pdf};

#[test]
fn qpdf_object_handle_primitives_are_available_to_external_crates() {
    let name = ObjectHandle::name(b"FlateDecode".to_vec());
    assert!(name.try_is_name_and_equals(b"FlateDecode").unwrap());

    let names = ObjectHandle::array(vec![
        ObjectHandle::name(b"Identity".to_vec()),
        ObjectHandle::name(b"FlateDecode".to_vec()),
    ]);
    assert!(names.try_is_or_has_name(b"FlateDecode").unwrap());

    let filter = ObjectHandle::dictionary(vec![
        (
            b"/Type".to_vec(),
            ObjectHandle::name(b"CryptFilterDecodeParms".to_vec()),
        ),
        (
            b"/Subtype".to_vec(),
            ObjectHandle::name(b"Identity".to_vec()),
        ),
    ]);
    assert!(filter
        .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"Identity")
        .unwrap());

    let resources = ObjectHandle::dictionary(vec![
        (
            b"/Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"/F1".to_vec(), ObjectHandle::integer(1))]),
        ),
        (
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Im0".to_vec(), ObjectHandle::integer(2))]),
        ),
    ]);
    let expected: BTreeSet<Vec<u8>> = [b"/F1".to_vec(), b"/Im0".to_vec()].into_iter().collect();
    assert_eq!(resources.get_resource_names().unwrap(), expected);
}

#[test]
fn qpdf_stream_type_predicate_is_available_to_external_crates() {
    let stream = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Member".to_vec())),
        ]),
        Rc::new(Vec::new()),
    );

    assert!(stream.try_is_stream_of_type(b"ObjStm", b"Member").unwrap());
    assert!(!stream.try_is_stream_of_type(b"XRef", b"Member").unwrap());
    assert!(!ObjectHandle::dictionary(Vec::new())
        .try_is_stream_of_type(b"ObjStm", b"")
        .unwrap());
}

#[test]
fn ownerless_integer_type_error_uses_qpdf_object_error_boundary() {
    let error = ObjectHandle::null()
        .try_get_int_value()
        .expect_err("a null handle without a document must raise the object error");

    assert!(matches!(
        error,
        Error::System(message)
            if message == "operation for integer attempted on object of type null: returning 0"
    ));
}

#[test]
fn acroform_transform_and_rename_boundaries_are_public() {
    let mut pdf = Pdf::empty().unwrap();
    let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
    let transformed = helper
        .transform_annotations(ObjectHandle::array(Vec::new()), Matrix::default())
        .unwrap();

    assert!(transformed.new_annotations.is_empty());
    assert!(transformed.new_fields.is_empty());
    assert!(transformed.old_fields.is_empty());
    helper
        .add_and_rename_form_fields(transformed.new_fields)
        .unwrap();
}

#[test]
fn get_resource_names_excludes_null_valued_entries_like_qpdf() {
    // qpdf's getKeys() (libqpdf/QPDF_Dictionary.cc:118-125) excludes any key
    // whose value is null, and getResourceNames() (QPDFObjectHandle.cc:1156-1170)
    // collects second-level names through getKeys(). A `/F1 null` entry must
    // not appear in the result.
    let resources = ObjectHandle::dictionary(vec![(
        b"/Font".to_vec(),
        ObjectHandle::dictionary(vec![
            (b"/F1".to_vec(), ObjectHandle::null()),
            (b"/F2".to_vec(), ObjectHandle::integer(1)),
        ]),
    )]);
    let expected: BTreeSet<Vec<u8>> = [b"/F2".to_vec()].into_iter().collect();
    assert_eq!(resources.get_resource_names().unwrap(), expected);
}

#[test]
fn object_handle_reports_and_clears_its_owning_pdf_identity() {
    let mut pdf = Pdf::empty().unwrap();
    let root = pdf.root_handle().unwrap();
    assert!(root.owning_pdf_unique_id().is_some());

    drop(pdf);

    assert!(root.owning_pdf_unique_id().is_none());
}
