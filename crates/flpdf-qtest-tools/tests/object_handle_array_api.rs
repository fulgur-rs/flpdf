//! External-crate coverage for the public canonical array mutation boundary.

use flpdf::{Object, ObjectHandle, ObjectRef, Pdf, PdfWriter};

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

#[test]
fn external_consumer_can_mark_a_direct_child_array_dirty_for_write_back() {
    let mut pdf = Pdf::empty().expect("create an empty PDF");
    let root_ref = pdf.root_ref().expect("empty PDF has a catalog root");
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root)
        .expect("resolve the catalog before mutating it");

    root.replace_key(
        b"/DirectValues",
        ObjectHandle::array(vec![ObjectHandle::integer(1)]),
    )
    .expect("install the direct child array");
    let direct_values = root.get_key(b"/DirectValues");
    direct_values
        .append_array_item(ObjectHandle::integer(2))
        .expect("mutate the direct child array");
    pdf.mark_object_handle_dirty(&direct_values)
        .expect("mark the containing indirect catalog dirty");

    let output = {
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("write the updated catalog");
        writer.get_buffer().expect("take memory output")
    };

    let mut reopened = Pdf::open_mem_owned(output).expect("reopen written PDF");
    let catalog = reopened
        .resolve_object(root_ref)
        .expect("resolve the rewritten catalog");
    assert_eq!(
        catalog.as_dict().and_then(|dict| dict.get("DirectValues")),
        Some(&Object::Array(vec![Object::Integer(1), Object::Integer(2)]))
    );
}
