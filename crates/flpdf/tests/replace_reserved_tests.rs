use flpdf::{ObjectHandle, Pdf};

#[test]
fn replace_reserved_rebinds_the_reserved_handle_to_the_replacement() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let reserved = pdf.new_reserved().expect("create reserved object");
    let replacement = ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(2)]);

    pdf.replace_reserved(reserved.clone(), replacement)
        .expect("replace reserved object");

    assert!(!reserved.is_reserved());
    let items = reserved
        .as_array()
        .expect("reserved handle now holds an array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].as_integer(), Some(1));
    assert_eq!(items[1].as_integer(), Some(2));
}

#[test]
fn replace_reserved_rejects_a_non_reserved_handle_before_mutation() {
    let mut pdf = Pdf::empty().expect("create empty PDF");
    let target = ObjectHandle::integer(1);

    let error = pdf
        .replace_reserved(target.clone(), ObjectHandle::integer(2))
        .expect_err("qpdf rejects a non-reserved replacement target");

    assert_eq!(
        error.to_string(),
        "replaceReserved called with non-reserved object"
    );
    assert_eq!(target.as_integer(), Some(1));
}

#[test]
fn try_unparse_resolved_preserves_qpdf_reserved_error() {
    let pdf = Pdf::empty().expect("create empty PDF");
    let reserved = pdf.new_reserved().expect("create reserved object");

    let error = reserved
        .try_unparse_resolved()
        .expect_err("qpdf unparseResolved rejects a reserved object");

    assert_eq!(
        error.to_string(),
        "QPDFObjectHandle: attempting to unparse a reserved object"
    );
}

#[test]
fn try_unparse_resolved_returns_the_normal_value_for_a_direct_handle() {
    let value = ObjectHandle::integer(42);

    assert_eq!(
        value.try_unparse_resolved().expect("unparse integer"),
        b"42"
    );
}

#[test]
fn try_unparse_resolved_preserves_qpdf_destroyed_error() {
    let destroyed = {
        let pdf = Pdf::empty().expect("create empty PDF");
        pdf.new_reserved().expect("create reserved object")
    };

    let error = destroyed
        .try_unparse_resolved()
        .expect_err("qpdf unparseResolved rejects a destroyed object");

    assert_eq!(
        error.to_string(),
        "attempted to unparse a QPDFObjectHandle from a destroyed QPDF"
    );
}

#[test]
fn replace_reserved_accepts_qpdfs_direct_null_target() {
    let mut pdf = Pdf::empty().expect("create empty PDF");

    pdf.replace_reserved(ObjectHandle::null(), ObjectHandle::integer(7))
        .expect("qpdf passes a direct null target through as 0 0");
}

#[test]
fn shallow_copy_preserves_a_qpdf_raw_dictionary_key() {
    let source = ObjectHandle::dictionary(vec![(b"Canonical".to_vec(), ObjectHandle::integer(1))]);
    source
        .replace_key(b"Array1", ObjectHandle::integer(2))
        .expect("install qpdf's raw key");

    let copy = source.shallow_copy().expect("shallow copy dictionary");
    let entries = copy.as_dictionary().expect("copied dictionary");
    assert!(entries.contains_key(b"Array1".as_slice()));
    assert!(!entries.contains_key(b"/Array1".as_slice()));
}
