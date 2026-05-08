use flpdf::{load_xref_and_trailer, Error, ObjectRef, XrefOffset};
use std::fs::File;
use std::io::BufReader;

#[test]
fn loads_xref_table_and_trailer() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut reader = BufReader::new(file);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, 110);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(9))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Offset(58))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn loads_xref_stream_and_trailer() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [0u8, 0, 0, 0, 0, 1, 0, 0, 0x0A, 0, 1, 0, 0, 0x14, 0];

    let xref_stream_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /W [1 3 1] /Index [0 3] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let xref_object_offset = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = xref_object_offset;
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, u64::try_from(startxref).unwrap());
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(10))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Offset(20))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(startxref, loaded.startxref as usize);
    assert_eq!(startxref, xref_object_offset);
}

#[test]
fn loads_xref_stream_without_index_uses_size_range() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [
        0, 0, 0, 0, 0, // object 0 free
        1, 0, 0, 0x0A, 0, // object 1 at offset 10
        1, 0, 0, 0x14, 0, // object 2 at offset 20
    ];

    let xref_stream_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /W [1 3 1] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(10))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Offset(20))
    );
}

#[test]
fn rejects_xref_stream_when_range_exceeds_size() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [
        0, 0, 0, 0, 0, // object 0 free
        1, 0, 0, 0x0A, 0, // object 1 at offset 10
        1, 0, 0, 0x14, 0, // object 2 at offset 20
    ];

    let xref_stream_obj =
        format!("3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 3] /Length {} >>\nstream\n", xref_entries.len()).into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let err = load_xref_and_trailer(&mut reader).expect_err("stream range exceeds /Size");
    let message = format!("{err}");
    assert!(message.contains("xref range exceeds /Size"));
    assert!(matches!(err, Error::Parse { .. }));
}

#[test]
fn parses_xref_stream_with_compressed_entries() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    let xref_entries = [
        0, 0, 0, 0, 0, // object 0 free
        2, 0, 0, 0x02, 0, // object 1 compressed (type 2)
    ];

    let xref_stream_obj =
        format!("3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 2] /Length {} >>\nstream\n", xref_entries.len()).into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_obj);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut reader = std::io::Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Compressed {
            stream: 2,
            index: 0
        })
    );
}
