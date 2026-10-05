use std::collections::BTreeSet;

use flpdf::{AcroFormDocumentHelper, ObjectHandle, Pdf};

fn field(name: &[u8]) -> ObjectHandle {
    ObjectHandle::dictionary(vec![
        (b"/FT".to_vec(), ObjectHandle::name(b"Tx".to_vec())),
        (b"/T".to_vec(), ObjectHandle::string(name.to_vec())),
    ])
}

#[test]
fn add_form_field_is_public_and_keeps_same_name_fields_without_renaming() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let first = pdf
        .make_indirect_object_handle(field(b"Repeat"))
        .expect("first field allocation");
    let second = pdf
        .make_indirect_object_handle(field(b"Repeat"))
        .expect("second field allocation");

    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze empty form");
    helper
        .add_form_field(first.clone())
        .expect("add first field without renaming");
    helper
        .add_form_field(second.clone())
        .expect("add second same-name field without renaming");

    let same_name_fields = helper
        .get_fields_with_qualified_name("Repeat")
        .expect("read canonical name cache");
    assert_eq!(
        same_name_fields,
        BTreeSet::from([first.object_ref().unwrap(), second.object_ref().unwrap()])
    );
}

#[test]
fn add_form_field_preserves_direct_field_entry_and_qpdf_cache_behavior() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let direct = field(b"Direct");
    let mut helper = AcroFormDocumentHelper::new(&mut pdf).expect("analyze empty form");

    helper
        .add_form_field(direct)
        .expect("append direct field like qpdf addFormField");

    assert!(
        helper
            .fields()
            .expect("project indirect form fields")
            .is_empty(),
        "qpdf ignores direct fields while traversing the AcroForm cache"
    );
    let root = pdf.get_object_handle(pdf.root_ref().expect("document root"));
    let fields = root
        .try_get_key(b"/AcroForm")
        .expect("read /AcroForm")
        .try_get_key(b"/Fields")
        .expect("read /Fields")
        .try_get_array_as_vector()
        .expect("resolve /Fields");
    assert_eq!(fields.len(), 1);
    assert!(fields[0].is_direct());
    assert!(fields[0]
        .try_get_key(b"/T")
        .unwrap()
        .try_is_string()
        .unwrap());
}
