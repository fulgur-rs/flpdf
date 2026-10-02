use flpdf::{Error, ObjectHandle};

const UNINITIALIZED_UNPARSE_ERROR: &str =
    "attempted to dereference an uninitialized QPDFObjectHandle";

#[test]
fn unparse_returns_qpdf_error_for_an_uninitialized_direct_handle() {
    let handle = ObjectHandle::uninitialized();

    let error = handle
        .unparse()
        .expect_err("qpdf unparse delegates direct handles to unparseResolved");

    assert!(matches!(error, Error::Internal(message) if message == UNINITIALIZED_UNPARSE_ERROR));
}

#[test]
fn unparse_resolved_returns_qpdf_error_for_an_uninitialized_handle() {
    let handle = ObjectHandle::uninitialized();

    let error = handle
        .unparse_resolved()
        .expect_err("qpdf unparseResolved fails at its initial dereference");

    assert!(matches!(error, Error::Internal(message) if message == UNINITIALIZED_UNPARSE_ERROR));
}
