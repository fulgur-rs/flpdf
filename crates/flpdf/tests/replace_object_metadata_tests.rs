use flpdf::{ObjectHandle, ObjectRef, Pdf};

// QPDF::updateCache changes the replacement's object generation and the
// cache extents, but preserves QPDFValue's description and parsed offset.
// See qpdf 11.9.0 QPDF.cc:1842-1858 and QPDFObject_private.hh:139-143.
#[test]
fn replace_object_retains_parsed_metadata_with_and_without_a_cached_target() {
    for cached in [false, true] {
        let mut pdf = Pdf::empty().expect("empty document");
        let object_ref = ObjectRef::new(20, 0);
        let existing = cached.then(|| pdf.get_object_handle(object_ref));
        let replacement = ObjectHandle::parse_with_description(b"  42", "replacement input")
            .expect("parse replacement");
        replacement
            .set_object_description(&pdf, "replacement input")
            .expect("set diagnostic description");
        let alias = replacement.clone();
        let offset = replacement.try_get_parsed_offset().expect("parsed offset");
        assert_eq!(offset, 0);

        let replaced = pdf
            .replace_object(object_ref, replacement)
            .expect("replace object");

        for handle in [Some(replaced), Some(alias), existing]
            .into_iter()
            .flatten()
        {
            assert_eq!(handle.try_get_parsed_offset().expect("offset"), offset);
            assert_eq!(handle.description(), b"replacement input");
            assert_eq!(handle.try_get_int_value().expect("integer"), 42);
        }
    }
}

#[test]
fn replace_object_uses_replacement_metadata_instead_of_the_old_value_metadata() {
    let mut pdf = Pdf::empty().expect("empty document");
    let object_ref = ObjectRef::new(20, 0);
    let original =
        ObjectHandle::parse_with_description(b"  17", "old input").expect("parse original");
    original
        .set_object_description(&pdf, "old input")
        .expect("set old description");
    let retained = pdf
        .replace_object(object_ref, original)
        .expect("insert original");

    let replacement = ObjectHandle::integer(42);
    pdf.replace_object(object_ref, replacement)
        .expect("replace original");

    assert_eq!(retained.try_get_parsed_offset().expect("offset"), -1);
    assert_eq!(retained.description(), b"object 20 0");
    assert_eq!(retained.try_get_int_value().expect("integer"), 42);
}
