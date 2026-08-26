//! External-crate coverage for the public canonical array mutation boundary.

use flpdf::{ObjectHandle, ObjectRef, Pdf, PdfWriter};

#[test]
fn external_consumer_can_mutate_an_indirect_array_in_place() {
    let mut pdf = Pdf::empty().expect("create an empty PDF");
    let array_ref = ObjectRef::new(9, 0);
    pdf.set_object_handle(
        array_ref,
        ObjectHandle::array(vec![
            ObjectHandle::string(b"keep".to_vec()),
            ObjectHandle::string(b"replace".to_vec()),
        ]),
    )
    .expect("install array");

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
    let catalog = reopened.get_object_handle(root_ref);
    reopened
        .resolve(&catalog)
        .expect("resolve the rewritten catalog");
    assert_eq!(
        catalog.get_key(b"/DirectValues").as_array().map(|items| {
            items
                .iter()
                .filter_map(ObjectHandle::as_integer)
                .collect::<Vec<_>>()
        }),
        Some(vec![1, 2])
    );
}
