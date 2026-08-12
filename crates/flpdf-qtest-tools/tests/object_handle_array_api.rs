//! External-crate coverage for the public canonical array mutation boundary.

use flpdf::{Object, ObjectHandle, ObjectRef, Pdf};

#[test]
fn external_consumer_can_mutate_an_indirect_array_in_place() {
    let mut pdf = Pdf::empty().expect("create an empty PDF");
    let array_ref = ObjectRef::new(9, 0);
    pdf.set_object(
        array_ref,
        Object::Array(vec![
            Object::String(b"keep".to_vec()),
            Object::String(b"replace".to_vec()),
        ]),
    );

    let array = pdf.get_object_handle(array_ref);
    array
        .set_array_item(1, ObjectHandle::string(b"updated".to_vec()))
        .expect("external consumer can call the canonical mutator");

    let items = array.as_array().expect("object remains an array");
    assert_eq!(items[0].as_string(), Some(b"keep".to_vec()));
    assert_eq!(items[1].as_string(), Some(b"updated".to_vec()));
}
