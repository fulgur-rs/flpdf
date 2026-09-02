use flpdf::{ObjectHandle, ObjectRef, Pdf, PdfWriter};

fn lazy_swap_fixture() -> Vec<u8> {
    let objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
        (2, b"<< /Type /Pages /Kids [] /Count 0 >>".as_slice()),
        (3, b"<< /Marker 1 >>".as_slice()),
        (4, b"<< /Marker 2 >>".as_slice()),
    ];
    let mut bytes = b"%PDF-1.3\n".to_vec();
    let mut offsets = vec![0usize; objects.len() + 1];
    for (number, body) in objects {
        offsets[number] = bytes.len();
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn indirect_marker(pdf: &Pdf<std::io::Cursor<Vec<u8>>>, marker: i64) -> ObjectHandle {
    pdf.make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
        b"/Marker".to_vec(),
        ObjectHandle::integer(marker),
    )]))
    .expect("marker dictionary becomes indirect")
}

#[test]
fn swap_objects_preserves_alias_identity_and_swaps_values() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let first = indirect_marker(&pdf, 1);
    let second = indirect_marker(&pdf, 2);
    let first_ref = first.object_ref().expect("first object reference");
    let second_ref = second.object_ref().expect("second object reference");
    let first_alias = pdf.get_object_handle(first_ref);
    let second_alias = pdf.get_object_handle(second_ref);

    pdf.swap_objects(first_ref, second_ref)
        .expect("swap object bodies");

    assert!(first.is_same_object_as(&first_alias));
    assert!(second.is_same_object_as(&second_alias));
    assert_eq!(first.get_key(b"/Marker").as_integer(), Some(2));
    assert_eq!(second.get_key(b"/Marker").as_integer(), Some(1));
    assert!(pdf.get_object_handle(first_ref).is_same_object_as(&first));
    assert!(pdf.get_object_handle(second_ref).is_same_object_as(&second));
}

#[test]
fn swap_objects_resolves_lazy_source_values_before_swapping() {
    let mut pdf = Pdf::open_mem_owned(lazy_swap_fixture()).expect("lazy PDF");
    let first_ref = ObjectRef::new(3, 0);
    let second_ref = ObjectRef::new(4, 0);
    let first = pdf.get_object_handle(first_ref);
    let second = pdf.get_object_handle(second_ref);
    assert!(!first.is_resolved());
    assert!(!second.is_resolved());

    pdf.swap_objects(first_ref, second_ref)
        .expect("resolve and swap source values");

    assert!(first.is_resolved());
    assert!(second.is_resolved());
    assert_eq!(first.get_key(b"/Marker").as_integer(), Some(2));
    assert_eq!(second.get_key(b"/Marker").as_integer(), Some(1));
}

#[test]
fn swapped_values_are_visible_to_the_writer() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let first = indirect_marker(&pdf, 1);
    let second = indirect_marker(&pdf, 2);
    let first_ref = first.object_ref().expect("first object reference");
    let second_ref = second.object_ref().expect("second object reference");
    let trailer = pdf.trailer();
    trailer
        .replace_key(b"/First", first.clone())
        .expect("reference first object");
    trailer
        .replace_key(b"/Second", second.clone())
        .expect("reference second object");
    pdf.mark_object_handle_dirty(&trailer)
        .expect("mark trailer dirty");

    pdf.swap_objects(first_ref, second_ref)
        .expect("swap object bodies");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("memory output");
    writer.write().expect("write swapped document");
    let output = writer.get_buffer().expect("written PDF buffer");
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("/Marker 2"));
    assert!(output.contains("/Marker 1"));
}

#[test]
fn swap_objects_resolves_an_unknown_generation_to_null() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let first = indirect_marker(&pdf, 1);
    let first_ref = first.object_ref().expect("first object reference");
    let unknown = ObjectRef::new(first_ref.number + 100, first_ref.generation);

    pdf.swap_objects(first_ref, unknown)
        .expect("unknown objects resolve to null before swapping");
    assert!(first.is_null());
    assert_eq!(
        pdf.get_object_handle(unknown)
            .get_key(b"/Marker")
            .as_integer(),
        Some(1)
    );
}
