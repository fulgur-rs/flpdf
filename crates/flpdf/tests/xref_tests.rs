use flpdf::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, Error, ObjectRef, XrefOffset,
};
use std::fs::File;
use std::io::BufReader;
use std::io::Cursor;

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

#[test]
fn loads_previous_xref_stream_entries_for_omitted_objects() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(obj1);

    let previous_xref_offset = bytes.len() as u64;
    let previous_xref_entries =
        build_encoded_xref_stream_entries(&[(0, 0, 0), (1, obj1_offset, 0), (2, 12, 0), (0, 0, 0)]);

    let previous_xref_object = make_xref_stream_object(2, 4, None, 1, &previous_xref_entries);
    bytes.extend_from_slice(&previous_xref_object);

    let latest_xref_offset = bytes.len() as u64;
    let latest_xref_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, obj1_offset, 0),
        (0, 0, 0),
        (1, latest_xref_offset, 0),
    ]);

    let latest_xref_object =
        make_xref_stream_object(3, 4, Some(previous_xref_offset), 1, &latest_xref_entries);
    bytes.extend_from_slice(&latest_xref_object);

    bytes.extend_from_slice(format!("startxref\n{latest_xref_offset}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Compressed {
            stream: 12,
            index: 0
        })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(obj1_offset))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(3, 0)),
        Some(&XrefOffset::Offset(latest_xref_offset))
    );
}

fn build_encoded_xref_stream_entries(entries: &[(u8, u64, u64)]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(entries.len() * 7);
    for &(entry_type, field1, field2) in entries {
        encoded.push(entry_type);
        encoded.extend_from_slice(&field1.to_be_bytes()[4..]);
        encoded.extend_from_slice(&field2.to_be_bytes()[6..]);
    }
    encoded
}

fn make_xref_stream_object(
    object_number: u32,
    size: u32,
    prev_offset: Option<u64>,
    root_ref_number: u32,
    entries: &[u8],
) -> Vec<u8> {
    let prev = prev_offset
        .map(|offset| format!(" /Prev {offset}"))
        .unwrap_or_default();

    let mut object = format!(
        "{} 0 obj\n<< /Type /XRef /Size {size} /Root {root_ref_number} 0 R /W [1 4 2] /Index [0 {size}] /Length {}{} >>\nstream\n",
        object_number,
        entries.len(),
        prev
    )
    .into_bytes();
    object.extend_from_slice(entries);

    // Keep stream data trivially decodable with no postprocessing.
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

#[test]
fn best_effort_recovers_from_corrupt_xref_data() {
    let bytes = corrupt_xref_pdf();

    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref should fail in strict mode");
    let message = format!("{err}");
    assert!(!message.is_empty());

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.repair_diagnostics.entries().len(), 1);
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|entry| entry.message.contains("repaired by linear object scan")));
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(9))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

fn corrupt_xref_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n".to_vec();
    let obj4 = b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".to_vec();

    let mut offsets = Vec::new();
    for object in &[obj1, obj2, obj3, obj4] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let mut corrupted = bytes.clone();
    let Some(pos) = corrupted.windows(4).position(|window| window == b"xref") else {
        unreachable!("fixture should contain xref token")
    };
    if let Some(byte) = corrupted.get_mut(pos + 2) {
        *byte = b'z';
    }
    corrupted
}
