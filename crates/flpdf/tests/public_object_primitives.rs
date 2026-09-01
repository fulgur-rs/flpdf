use std::collections::BTreeSet;

use flpdf::{ObjectHandle, Pdf};

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
