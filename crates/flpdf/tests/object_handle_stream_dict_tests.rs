use flpdf::{Error, ObjectHandle, ObjectRef, Pdf};
use std::fs;
use std::rc::Rc;

#[test]
fn resolving_stream_dictionary_accessor_is_public() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/object_handle.rs"))
        .expect("read object_handle source");

    assert!(
        source.contains("pub fn try_get_stream_dict"),
        "ObjectHandle must expose the qpdf getDict resolving boundary"
    );
}

#[test]
fn direct_stream_returns_its_live_dictionary() {
    let dictionary =
        ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(3))]);
    let stream = ObjectHandle::stream(dictionary.clone(), Rc::new(b"abc".to_vec()));

    let returned = stream
        .try_get_stream_dict()
        .expect("direct stream getDict equivalent");
    assert!(returned.is_same_object_as(&dictionary));
}

#[test]
fn indirect_stream_is_resolved_before_returning_its_dictionary() {
    let mut pdf = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/qpdf-test98-minimal.pdf").to_vec(),
    )
    .expect("open qpdf test 98 fixture");
    let stream = pdf.get_object_handle(ObjectRef::new(4, 0));

    assert!(!stream.is_resolved());
    let returned = stream
        .try_get_stream_dict()
        .expect("indirect stream getDict equivalent");
    assert!(stream.is_resolved());
    assert_eq!(
        returned.try_get_key(b"/Length").unwrap().as_integer(),
        Some(44)
    );
}

#[test]
fn non_stream_and_uninitialized_handles_use_qpdf_stream_error_boundary() {
    for handle in [ObjectHandle::integer(1), ObjectHandle::uninitialized()] {
        let type_name = handle.type_name().expect("type name");
        let error = handle
            .try_get_stream_dict()
            .expect_err("non-stream values must not return a dictionary");
        assert!(
            matches!(error, Error::System(message) if message == format!(
                "operation for stream attempted on object of type {type_name}"
            ))
        );
    }
}

#[test]
fn dropped_document_resolution_failure_is_propagated() {
    let stream = {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/qpdf-test98-minimal.pdf").to_vec(),
        )
        .expect("open qpdf test 98 fixture");
        pdf.get_object_handle(ObjectRef::new(4, 0))
    };

    let error = stream
        .try_get_stream_dict()
        .expect_err("dropped document must reject lazy resolution");
    assert!(matches!(error, Error::System(message) if message
        == "operation for stream attempted on object of type destroyed"));
}
