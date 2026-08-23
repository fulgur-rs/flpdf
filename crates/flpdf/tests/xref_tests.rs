use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, load_xref_and_trailer_with_repair,
    Diagnostics, Dictionary, Error, LoadedXref, Object, ObjectRef, Pdf, PdfOpenOptions, PdfWriter,
    XrefEntry, XrefForm,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Write};

#[test]
fn loaded_xref_remains_constructible_with_original_public_fields() {
    let loaded = LoadedXref {
        version: "1.7".to_string(),
        startxref: 110,
        entries: BTreeMap::new(),
        trailer: Dictionary::new(),
        last_xref_form: XrefForm::Table,
        repair_diagnostics: Diagnostics::default(),
    };

    assert_eq!(loaded.version, "1.7");
}

#[test]
fn loads_xref_table_and_trailer() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut reader = BufReader::new(file);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, 110);
    assert_eq!(loaded.last_xref_form, XrefForm::Table);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed { offset: 58 })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn accepts_plus_prefixed_startxref_integer_token() {
    let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
          trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n",
    );
    bytes.extend_from_slice(format!("+{xref_offset}\n%%EOF\n").as_bytes());

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.startxref, xref_offset as u64);
}

#[test]
fn rejects_non_integer_startxref_token_at_token_start() {
    let bytes = b"%PDF-1.7\nstartxref\n/not-an-offset\n%%EOF\n";

    let error = load_xref_and_trailer(&mut Cursor::new(bytes)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "parse error at byte 19: expected unsigned integer"
    );
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
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 10 })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed { offset: 20 })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(startxref, loaded.startxref as usize);
    assert_eq!(startxref, xref_object_offset);
}

#[test]
fn indirect_xref_stream_filter_metadata() {
    let logical_entry = [(1, 1, 0)];
    let bytes = hybrid_xref_stream_with_filter_metadata(
        21,
        5,
        "/Filter 10 0 R",
        vec![
            (1, b"<< /Type /Catalog >>".to_vec()),
            (10, b"[20 0 R]".to_vec()),
            (20, b"/FlateDecode".to_vec()),
        ],
        &build_encoded_xref_stream_entries(&logical_entry),
        false,
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("xref stream filter metadata should resolve through the active table");
    let xref_offset = loaded
        .entries
        .get(&ObjectRef::new(5, 0))
        .and_then(|entry| match entry {
            XrefEntry::Uncompressed { offset } => Some(*offset),
            _ => None,
        })
        .expect("xref stream object should remain live after decoding");
    assert!(xref_offset > 0);
}

#[test]
fn indirect_xref_stream_decode_parms() {
    let logical_entry = [(1, 1, 0)];
    let bytes = hybrid_xref_stream_with_filter_metadata(
        26,
        5,
        "/Filter /FlateDecode /DecodeParms 11 0 R",
        vec![
            (1, b"<< /Type /Catalog >>".to_vec()),
            (
                11,
                b"<< /Predictor 22 0 R /Columns 23 0 R /Colors 24 0 R /BitsPerComponent 25 0 R >>"
                    .to_vec(),
            ),
            (22, b"12".to_vec()),
            (23, b"7".to_vec()),
            (24, b"1".to_vec()),
            (25, b"8".to_vec()),
        ],
        &build_encoded_xref_stream_entries(&logical_entry),
        true,
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("xref stream decode parameters should resolve through the active table");
    let xref_offset = loaded
        .entries
        .get(&ObjectRef::new(5, 0))
        .and_then(|entry| match entry {
            XrefEntry::Uncompressed { offset } => Some(*offset),
            _ => None,
        })
        .expect("PNG predictor must be removed before xref rows are parsed");
    assert!(xref_offset > 0);
}

#[test]
fn hybrid_xref_stream_resolves_indirect_type_from_active_classic_table() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_stream_offset = bytes.len() as u64;
    let xref_stream_header = format!(
        "2 0 obj\n<< /Type 5 0 R /Size 6 /Root 1 0 R /W [1 4 2] /Index [0 6] /Length {} >>\nstream\n",
        6 * 7
    );
    let xref_stream_suffix = b"\nendstream\nendobj\n";
    let type_target_offset = xref_stream_offset
        + xref_stream_header.len() as u64
        + (6 * 7) as u64
        + xref_stream_suffix.len() as u64;
    let xref_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (1, xref_stream_offset, 0),
        (0, 0, 0),
        (0, 0, 0),
        (1, type_target_offset, 0),
    ]);
    bytes.extend_from_slice(xref_stream_header.as_bytes());
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(xref_stream_suffix);

    assert_eq!(bytes.len() as u64, type_target_offset);
    bytes.extend_from_slice(b"5 0 obj\n/XRef\nendobj\n");

    let classic_xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 6\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{xref_stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{type_target_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{classic_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("hybrid xref stream should resolve indirect /Type");
    assert_eq!(loaded.last_xref_form, XrefForm::Table);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: xref_stream_offset
        })
    );
}

#[test]
fn hybrid_xref_stream_resolves_indirect_length_from_active_classic_table() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_stream_offset = bytes.len() as u64;
    let xref_stream_header =
        "2 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 4 2] /Index [0 6] /Length 5 0 R >>\nstream\n"
            .to_string();
    let xref_stream_suffix = b"\nendstream\nendobj\n";
    let length_target_offset = xref_stream_offset
        + xref_stream_header.len() as u64
        + (6 * 7) as u64
        + xref_stream_suffix.len() as u64;
    let xref_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (1, xref_stream_offset, 0),
        (0, 0, 0),
        (0, 0, 0),
        (1, length_target_offset, 0),
    ]);
    bytes.extend_from_slice(xref_stream_header.as_bytes());
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(xref_stream_suffix);
    assert_eq!(bytes.len() as u64, length_target_offset);
    bytes.extend_from_slice(b"5 0 obj\n42\nendobj\n");

    let classic_xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 6\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{xref_stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{length_target_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{classic_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("hybrid xref stream should resolve indirect /Length");
    assert!(loaded.repair_diagnostics.entries().is_empty());
}

#[test]
fn xref_stream_direct_length_accepts_payload_adjacent_endstream_without_diagnostics() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 1 1] /Index [0 1] /Length 3 >>\nstream\n",
    );
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(b"endstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert!(loaded.repair_diagnostics.entries().is_empty());
}

#[test]
fn strict_xref_stream_rejects_endobj_as_a_stream_terminator() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 1 1] /Index [0 1] /Length 3 >>\nstream\n",
    );
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(b"endobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("strict xref bootstrap must require endstream");
    assert!(
        matches!(err, Error::Parse { ref message, .. } if message.contains("endstream")),
        "unexpected error: {err}"
    );

    let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true)
        .expect("repair mode may recover at endobj");
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
}

#[test]
fn xref_stream_unavailable_indirect_length_uses_bounded_recovery_diagnostics() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 1 1] /Index [0 1] /Length 9 0 R >>\nstream\n",
    );
    let payload_offset = bytes.len();
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.offset))
            .collect::<Vec<_>>(),
        vec![
            (
                format!(
                    "(xref stream: object 1 0, offset {xref_offset}): stream dictionary lacks /Length key"
                ),
                Some(xref_offset as u64),
            ),
            (
                format!(
                    "(xref stream: object 1 0, offset {payload_offset}): attempting to recover stream length"
                ),
                Some(payload_offset as u64),
            ),
            (
                format!(
                    "(xref stream: object 1 0, offset {payload_offset}): recovered stream length: 4"
                ),
                Some(payload_offset as u64),
            ),
        ]
    );
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
        Some(&XrefEntry::Uncompressed { offset: 10 })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed { offset: 20 })
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
        Some(&XrefEntry::Compressed {
            stream: 2,
            index: 0
        })
    );
}

#[test]
fn loads_latest_xref_stream_free_entries_over_previous_live_entries() {
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

    assert_eq!(loaded.entries.get(&ObjectRef::new(2, 0)), None);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: obj1_offset
        })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(3, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: latest_xref_offset
        })
    );
}

#[test]
fn previous_xref_sections_retain_distinct_generations() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let object_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let previous_xref_offset = bytes.len() as u64;
    let previous_entries = build_encoded_xref_stream_entries(&[(0, 0, 0), (1, object_offset, 0)]);
    bytes.extend_from_slice(&make_xref_stream_object(2, 2, None, 1, &previous_entries));

    let latest_xref_offset = bytes.len() as u64;
    let latest_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, object_offset, 2),
        (1, latest_xref_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(
        3,
        3,
        Some(previous_xref_offset),
        1,
        &latest_entries,
    ));
    bytes.extend_from_slice(format!("startxref\n{latest_xref_offset}\n%%EOF\n").as_bytes());

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).unwrap();

    assert!(matches!(
        loaded.entries.get(&ObjectRef::new(1, 2)),
        Some(XrefEntry::Uncompressed { .. })
    ));
    assert!(matches!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(XrefEntry::Uncompressed { .. })
    ));
}

#[test]
fn previous_xref_section_offset_resolves_through_active_table() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let previous_object_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<< /Marker /Previous >>\nendobj\n");

    let previous_xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 3\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{previous_object_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{previous_xref_offset}\n%%EOF\n")
            .as_bytes(),
    );

    let latest_xref_offset = bytes.len() as u64;
    let object9_offset = latest_xref_offset;
    bytes.extend_from_slice(format!("9 0 obj\n{previous_xref_offset}\nendobj\n").as_bytes());
    let latest_xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"9 1\n");
    bytes.extend_from_slice(format!("{object9_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 10 /Root 1 0 R /Prev 9 0 R >>\nstartxref\n{latest_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("indirect /Prev should resolve through the latest xref table");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: previous_object_offset
        })
    );
}

#[test]
fn xref_stream_type_zero_ignores_a_wide_generation() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_offset = bytes.len() as u64;
    let mut entries = Vec::new();
    for _ in 0..2 {
        entries.push(0);
        entries.extend_from_slice(&0u32.to_be_bytes());
        entries.extend_from_slice(&u32::MAX.to_be_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "1 0 obj\n<< /Type /XRef /Size 2 /W [1 4 4] /Index [0 2] /Length {} >>\nstream\n",
            entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let loaded =
        load_xref_and_trailer(&mut Cursor::new(bytes)).expect("type-0 generation is ignored");
    assert_eq!(loaded.entries.get(&ObjectRef::new(1, 0)), None);
}

#[test]
fn warns_when_xref_size_is_not_one_plus_the_highest_deleted_object() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for _ in 1..6 {
        bytes.extend_from_slice(b"0000000000 00000 f \n");
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).unwrap();
    assert!(loaded
        .entries
        .keys()
        .all(|object_ref| object_ref.number != 5));
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| {
            diagnostic.message
                == "reported number of objects (5) is not one plus the highest object number (5)"
                && diagnostic.offset.is_none()
        }));
}

#[test]
fn preserves_recovery_diagnostics_from_previous_xref_streams() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let previous_xref_offset = bytes.len() as u64;
    let previous_entries =
        build_encoded_xref_stream_entries(&[(0, 0, 0), (1, obj1_offset, 0), (0, 0, 0)]);
    bytes.extend_from_slice(&make_xref_stream_object_with_declared_length(
        2,
        3,
        None,
        1,
        XrefStreamIndex::full(3),
        &previous_entries,
        previous_entries.len() + 10,
    ));

    let latest_xref_offset = bytes.len() as u64;
    let latest_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, obj1_offset, 0),
        (1, previous_xref_offset, 0),
        (1, latest_xref_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(
        3,
        4,
        Some(previous_xref_offset),
        1,
        &latest_entries,
    ));
    bytes.extend_from_slice(format!("startxref\n{latest_xref_offset}\n%%EOF\n").as_bytes());

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert!(loaded.repair_diagnostics.entries().iter().any(|entry| {
        entry.message.contains("recovered stream length")
            && entry.message.contains("(xref stream: object 2 0,")
    }));
}

#[test]
fn preserves_previous_xref_diagnostics_through_linear_scan_fallback() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let bad_prev = bytes.len() as u64;
    bytes.extend_from_slice(b"not-an-xref-section\n");

    let previous_xref_offset = bytes.len() as u64;
    let previous_entries =
        build_encoded_xref_stream_entries(&[(0, 0, 0), (1, obj1_offset, 0), (0, 0, 0)]);
    bytes.extend_from_slice(&make_xref_stream_object_with_declared_length(
        2,
        3,
        Some(bad_prev),
        1,
        XrefStreamIndex::full(3),
        &previous_entries,
        previous_entries.len() + 10,
    ));

    let latest_xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 3\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{previous_xref_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 3 /Root 1 0 R /Prev {previous_xref_offset} >>\nstartxref\n{latest_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    let messages: Vec<_> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(messages.len(), 6, "diagnostics must be preserved once");
    assert!(messages[0].ends_with("expected endstream"));
    assert!(messages[1].ends_with("attempting to recover stream length"));
    assert!(messages[2].ends_with(&format!(
        "recovered stream length: {}",
        previous_entries.len() + 1
    )));
    assert_eq!(
        &messages[3..],
        [
            "file is damaged",
            "expected integer",
            "Attempting to reconstruct cross-reference table",
        ]
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

fn flate_encode(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn hybrid_xref_stream_with_filter_metadata(
    size: u32,
    indexed_object: u32,
    metadata: &str,
    objects: Vec<(u32, Vec<u8>)>,
    logical_entries: &[u8],
    png_predictor: bool,
) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();

    for (object_number, body) in objects {
        offsets.insert(object_number, bytes.len() as u64);
        bytes.extend_from_slice(format!("{object_number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_stream_offset = bytes.len() as u64;
    let mut logical_entries = logical_entries.to_vec();
    assert_eq!(logical_entries.len(), 7);
    logical_entries[1..5].copy_from_slice(&xref_stream_offset.to_be_bytes()[4..]);
    offsets.insert(2, xref_stream_offset);
    let mut filter_input = Vec::with_capacity(logical_entries.len() + usize::from(png_predictor));
    if png_predictor {
        filter_input.push(0);
    }
    filter_input.extend_from_slice(&logical_entries);
    let compressed = flate_encode(&filter_input);
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /XRef /Size {size} /Root 1 0 R /W [1 4 2] /Index [{indexed_object} 1] /Length {} {metadata} >>\nstream\n",
            compressed.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let classic_xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for object_number in 0..size {
        if object_number == 0 {
            bytes.extend_from_slice(b"0000000000 65535 f \n");
        } else if let Some(offset) = offsets.get(&object_number) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{classic_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

#[derive(Clone, Copy)]
struct XrefStreamIndex {
    start: u32,
    count: u32,
}

impl XrefStreamIndex {
    const fn full(size: u32) -> Self {
        Self {
            start: 0,
            count: size,
        }
    }
}

fn make_xref_stream_object(
    object_number: u32,
    size: u32,
    prev_offset: Option<u64>,
    root_ref_number: u32,
    entries: &[u8],
) -> Vec<u8> {
    make_xref_stream_object_with_index(
        object_number,
        size,
        prev_offset,
        root_ref_number,
        XrefStreamIndex::full(size),
        entries,
    )
}

fn make_xref_stream_object_with_index(
    object_number: u32,
    size: u32,
    prev_offset: Option<u64>,
    root_ref_number: u32,
    index: XrefStreamIndex,
    entries: &[u8],
) -> Vec<u8> {
    make_xref_stream_object_with_declared_length(
        object_number,
        size,
        prev_offset,
        root_ref_number,
        index,
        entries,
        entries.len(),
    )
}

fn make_xref_stream_object_with_declared_length(
    object_number: u32,
    size: u32,
    prev_offset: Option<u64>,
    root_ref_number: u32,
    index: XrefStreamIndex,
    entries: &[u8],
    declared_length: usize,
) -> Vec<u8> {
    let prev = prev_offset
        .map(|offset| format!(" /Prev {offset}"))
        .unwrap_or_default();

    let mut object = format!(
        "{object_number} 0 obj\n<< /Type /XRef /Size {size} /Root {root_ref_number} 0 R /W [1 4 2] /Index [{} {}] /Length {declared_length}{prev} >>\nstream\n",
        index.start,
        index.count,
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
    assert_eq!(loaded.repair_diagnostics.entries().len(), 3);
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|entry| entry.message == "Attempting to reconstruct cross-reference table"));
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
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

#[test]
fn rejects_startxref_offset_beyond_eof_without_panic() {
    // Regression test for GitHub issue #304: a `startxref` offset pointing past
    // the end of the file must yield a descriptive parse error, not panic when
    // the xref stream branch slices `bytes[xref_pos..]`.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Point startxref well beyond the end of the buffer.
    let beyond_eof = bytes.len() + 4096;
    bytes.extend_from_slice(format!("startxref\n{beyond_eof}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let err =
        load_xref_and_trailer(&mut reader).expect_err("startxref past EOF should error, not panic");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream offset is beyond end of file"),
        "expected descriptive offset error, got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

#[test]
fn xref_stream_parse_error_offset_is_absolute() {
    // When `startxref` points to an in-bounds but malformed location, the error
    // from parsing the indirect object must be reported in absolute file
    // coordinates (`xref_pos + relative_offset`), not relative to the sliced
    // tail. Here the tail starts with a non-integer token, so the parse fails at
    // relative offset 0, which must surface as the absolute `garbage_pos`.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let garbage_pos = bytes.len();
    bytes.extend_from_slice(b"not-an-indirect-object\n");
    bytes.extend_from_slice(format!("startxref\n{garbage_pos}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let err =
        load_xref_and_trailer(&mut reader).expect_err("malformed xref stream object should error");
    let Error::Parse { offset, .. } = err else {
        panic!("expected Error::Parse, got {err:?}");
    };
    assert_eq!(
        offset, garbage_pos,
        "parse error offset must be absolute (xref_pos + relative)"
    );
}

#[test]
fn xref_stream_body_parse_error_offset_includes_indirect_header() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_pos = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /XRef /Size ");
    let body_error_pos = bytes.len();
    bytes.extend_from_slice(b"] >>\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes());

    let mut reader = Cursor::new(bytes);
    let err = load_xref_and_trailer(&mut reader)
        .expect_err("invalid xref stream body syntax should error");
    let Error::Parse { offset, .. } = err else {
        panic!("expected Error::Parse, got {err:?}");
    };
    assert_eq!(
        offset, body_error_pos,
        "body parse error offset must include both xref_pos and the indirect header"
    );
}

#[test]
fn rejects_startxref_offset_exactly_at_eof_without_panic() {
    // Boundary companion to the test above: when `startxref` equals the file
    // length exactly, `bytes.get(xref_pos..)` yields an empty slice rather than
    // `None`. That empty tail must still produce the descriptive
    // "beyond end of file" error instead of slipping into a generic parse
    // failure at offset 0.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Choose a target offset past the current trailer, then pad the file so its
    // total length equals that offset exactly (the empty-slice boundary).
    let target = bytes.len() + 256;
    bytes.extend_from_slice(format!("startxref\n{target}\n%%EOF\n").as_bytes());
    while bytes.len() < target {
        bytes.push(b' ');
    }
    assert_eq!(
        bytes.len(),
        target,
        "file length must equal startxref offset"
    );

    let mut reader = Cursor::new(bytes);
    let err =
        load_xref_and_trailer(&mut reader).expect_err("startxref at EOF should error, not panic");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream offset is beyond end of file"),
        "expected descriptive offset error, got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// Best-effort recovery must not inspect a `/Type /ObjStm` object stream's
/// contents during the linear scan. qpdf recovers the ObjStm container as an
/// uncompressed entry but does not synthesize entries for packed members.
///
/// The ObjStm carries no `/Filter`, so `decode_stream_data` is a passthrough and
/// its raw bytes are the cross-reference pairs header `objnum offset ...` that
/// the recovery routine walks. We pack a single compressed object (number 7).
#[test]
fn best_effort_recovers_objstm_compressed_entries() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    // A plain catalog object so the linear scan also yields a normal entry.
    let obj1 = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    bytes.extend_from_slice(&obj1);

    // Object stream object number 5. Its payload begins with the pairs header
    // `7 0` (compressed object 7 at intra-stream offset 0) followed by the
    // object body that lives at `/First`. qpdf's reconstruction scan does not
    // inspect these stream contents.
    let objstm_obj_number: u32 = 5;
    let compressed_obj_number: u32 = 7;
    let objstm_data = b"7 0 <</Foo 1>>".to_vec();
    let objstm_obj = format!(
        "{objstm_obj_number} 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        objstm_data.len()
    )
    .into_bytes();
    let objstm_offset = bytes.len() as u64;
    bytes.extend_from_slice(&objstm_obj);
    bytes.extend_from_slice(&objstm_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // A valid xref + trailer, then corrupt the `xref` keyword so strict parsing
    // fails and best-effort falls into the linear-scan recovery path.
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    // Corrupt the standalone `xref` table keyword (xref -> xrzf) at its known
    // offset, leaving `startxref` intact so strict parsing reaches and rejects
    // the malformed table rather than failing on a missing `startxref`.
    assert_eq!(
        &bytes[start_xref..start_xref + 4],
        b"xref",
        "fixture layout changed: start_xref must point at the table keyword"
    );
    bytes[start_xref + 2] = b'z';

    // Strict mode must reject the corrupt xref.
    load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref should fail in strict mode");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    // The ObjStm object itself recovers as a normal offset entry.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(objstm_obj_number, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: objstm_offset
        })
    );
    // The packed object is not independently rediscovered from the ObjStm.
    assert_eq!(
        loaded
            .entries
            .get(&ObjectRef::new(compressed_obj_number, 0)),
        None,
        "reconstruction must not synthesize a type-2 entry from ObjStm contents"
    );
}

#[test]
fn best_effort_recovers_objstm_with_indirect_length() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let objstm_data = b"7 0 <</Foo 1>>";
    let objstm_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"5 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 6 0 R >>\nstream\n");
    bytes.extend_from_slice(objstm_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("6 0 obj\n{}\nendobj\n", objstm_data.len()).as_bytes());

    let start_xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    bytes[start_xref + 2] = b'z';

    load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("corrupt xref must fail strict loading");
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(5, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: objstm_offset
        })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(7, 0)),
        None,
        "indirect stream length must not enable ObjStm content recovery"
    );
}

/// An ObjStm whose stream payload contains a header-like line (`9 0 obj`) makes
/// the linear scan record a spurious object *inside* the stream. qpdf does not
/// parse the ObjStm during reconstruction, so neither the spurious header nor
/// the stream's `/Length` can cause packed object (7) to become a type-2 entry.
#[test]
fn best_effort_recovers_objstm_truncated_by_in_stream_header() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Payload: the `7 0` pair (compressed object 7 at intra-stream offset 0),
    // then a line that looks like an indirect-object header. The linear scan
    // records `9 0 obj` at its in-stream offset, truncating object 5's window.
    let objstm_data = b"7 0\n9 0 obj\n".to_vec();
    let objstm_obj = format!(
        "5 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        objstm_data.len()
    )
    .into_bytes();
    let objstm_offset = bytes.len() as u64;
    bytes.extend_from_slice(&objstm_obj);
    bytes.extend_from_slice(&objstm_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    // The ObjStm itself recovers as a normal offset entry.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(5, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: objstm_offset
        })
    );
    // The packed object remains absent despite the in-stream header.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(7, 0)),
        None,
        "reconstruction must not inspect ObjStm contents after a header-like line"
    );
}

/// qpdf accepts a recovered trailer even when the linear scan finds no
/// indirect objects.
#[test]
fn best_effort_accepts_recovered_trailer_without_objects() {
    // Header + corrupt xref + trailer, but zero indirect objects to scan.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 1 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("recovered trailer is sufficient");
    assert!(loaded.entries.is_empty());
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        vec![
            "file is damaged",
            "expected integer",
            "Attempting to reconstruct cross-reference table",
        ]
    );
}

#[test]
fn best_effort_candidate_discovery_resolves_indirect_xref_type() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let candidate_offset = bytes.len() as u64;
    let entries = build_encoded_xref_stream_entries(&[(0, 0, 0), (1, candidate_offset, 0)]);
    let candidate_header = format!(
        "1 0 obj\n<< /Type 5 0 R /Size 2 /Root 1 0 R /W [1 4 2] /Index [0 2] /Length {} >>\nstream\n",
        entries.len()
    );
    let candidate_suffix = b"\nendstream\nendobj\n";
    let target_offset = candidate_offset
        + candidate_header.len() as u64
        + entries.len() as u64
        + candidate_suffix.len() as u64;
    bytes.extend_from_slice(candidate_header.as_bytes());
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(candidate_suffix);
    assert_eq!(bytes.len() as u64, target_offset);
    bytes.extend_from_slice(b"5 0 obj\n/XRef\nendobj\n");
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("reconstruction should discover an xref stream through indirect /Type");
    assert_eq!(
        loaded.trailer.get("Type"),
        Some(&Object::Reference(ObjectRef::new(5, 0)))
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: candidate_offset
        })
    );
}

#[test]
fn best_effort_candidate_discovery_resolves_indirect_xref_length_before_recovery() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let candidate_offset = bytes.len() as u64;
    let candidate_header =
        b"1 0 obj\n<< /Type /XRef /Size 4 /Root 3 0 R /W [1 8 2] /Index [0 4] /Length 2 0 R >>\nstream\n";
    let candidate_suffix = b"\nendstream\nendobj\n";
    let holder_object = b"2 0 obj\n44\nendobj\n";
    let root_object = b"3 0 obj\n<< /Type /Catalog >>\nendobj\n";
    let holder_offset =
        candidate_offset + candidate_header.len() as u64 + 44 + candidate_suffix.len() as u64;
    let root_offset = holder_offset + holder_object.len() as u64;
    let stream_payload = {
        // The free xref row ignores the wide fields, so they can contain a
        // line-anchored `endstream` token inside the real payload. qpdf's
        // resolved indirect `/Length` keeps that token inside the stream;
        // boundary recovery would truncate the payload before all fields.
        let mut payload = vec![0];
        payload.extend_from_slice(b"\nendstre");
        payload.extend_from_slice(b"am");
        for field1 in [candidate_offset, holder_offset, root_offset] {
            payload.push(1);
            payload.extend_from_slice(&field1.to_be_bytes());
            payload.extend_from_slice(&0u16.to_be_bytes());
        }
        payload
    };
    assert_eq!(stream_payload.len(), 44);
    bytes.extend_from_slice(candidate_header);
    bytes.extend_from_slice(&stream_payload);
    bytes.extend_from_slice(candidate_suffix);
    bytes.extend_from_slice(holder_object);
    bytes.extend_from_slice(root_object);
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");

    // The pinned qpdf oracle must also accept the repaired candidate. Keep the
    // probe optional for developer environments without qpdf, matching the
    // existing differential tests in this suite, but never treat an arbitrary
    // qpdf version or an error exit as an oracle result.
    let expected_qpdf_version = "qpdf version 11.9.0";
    let qpdf_version = std::process::Command::new("qpdf").arg("--version").output();
    match qpdf_version {
        Ok(version)
            if version.status.success()
                && String::from_utf8_lossy(&version.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == expected_qpdf_version) =>
        {
            let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
            let path = directory.path().join("indirect-length-candidate.pdf");
            std::fs::write(&path, &bytes).expect("write indirect-length candidate fixture");
            // The damaged startxref is intentional, so qpdf emits warnings.
            // --warning-exit-0 makes the expected warning exit successful while
            // still returning nonzero for an actual oracle failure.
            let qpdf = std::process::Command::new("qpdf")
                .args(["--warning-exit-0", "--show-xref"])
                .arg(&path)
                .output()
                .expect("run pinned qpdf --show-xref");
            assert!(
                qpdf.status.success(),
                "qpdf --show-xref failed (exit {:?}):\nstdout:\n{}\nstderr:\n{}",
                qpdf.status.code(),
                String::from_utf8_lossy(&qpdf.stdout),
                String::from_utf8_lossy(&qpdf.stderr)
            );
            let stdout = String::from_utf8_lossy(&qpdf.stdout);
            assert!(
                stdout.contains(&format!("1/0: uncompressed; offset = {candidate_offset}")),
                "qpdf --show-xref did not recover the candidate:\nstdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&qpdf.stderr)
            );
            assert!(
                stdout.contains(&format!("3/0: uncompressed; offset = {root_offset}")),
                "qpdf --show-xref did not retain the resolved root:\nstdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&qpdf.stderr)
            );
        }
        Ok(version) => eprintln!(
            "skipping qpdf 11.9.0 oracle probe: got {}",
            String::from_utf8_lossy(&version.stdout)
                .lines()
                .next()
                .unwrap_or("unrecognized version")
        ),
        Err(error) => eprintln!("skipping qpdf 11.9.0 oracle probe: {error}"),
    }

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("reconstruction should trust the resolved indirect /Length");
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert_eq!(
        loaded.trailer.get("Type"),
        Some(&Object::Name(b"XRef".to_vec()))
    );
    let relevant_diagnostics: Vec<_> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .filter(|diagnostic| {
            diagnostic.message.contains("stream length")
                || diagnostic.message.ends_with("expected endstream")
        })
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert!(
        relevant_diagnostics.is_empty(),
        "resolved indirect /Length must not trigger stream-boundary recovery: {relevant_diagnostics:?}"
    );
}

#[test]
fn best_effort_candidate_discovery_and_reentry_recover_mismatched_indirect_xref_length() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let candidate_offset = bytes.len() as u64;
    let candidate_header =
        b"1 0 obj\n<< /Type /XRef /Size 4 /Root 3 0 R /W [1 4 2] /Index [0 4] /Length 2 0 R >>\nstream\n";
    let candidate_suffix = b"\nendstream\nendobj\n";
    let holder_object = b"2 0 obj\n3\nendobj\n";
    let root_object = b"3 0 obj\n<< /Type /Catalog >>\nendobj\n";
    let stream_payload_len = 28u64;
    let holder_offset = candidate_offset
        + candidate_header.len() as u64
        + stream_payload_len
        + candidate_suffix.len() as u64;
    let root_offset = holder_offset + holder_object.len() as u64;
    let stream_payload = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, candidate_offset, 0),
        (1, holder_offset, 0),
        (1, root_offset, 0),
    ]);
    assert_eq!(stream_payload.len(), stream_payload_len as usize);
    bytes.extend_from_slice(candidate_header);
    bytes.extend_from_slice(&stream_payload);
    bytes.extend_from_slice(candidate_suffix);
    bytes.extend_from_slice(holder_object);
    bytes.extend_from_slice(root_object);
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("reconstruction should recover a mismatched indirect /Length");
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert_eq!(
        loaded.trailer.get("Type"),
        Some(&Object::Name(b"XRef".to_vec()))
    );

    let recovered_diagnostics: Vec<_> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .filter(|diagnostic| {
            diagnostic.message.contains("recovered stream length")
                || diagnostic.message.ends_with("expected endstream")
                || diagnostic
                    .message
                    .ends_with("attempting to recover stream length")
        })
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    let expected = vec![
        format!(
            "(object 1 0, offset {}): expected endstream",
            candidate_offset + candidate_header.len() as u64 + 3
        ),
        format!(
            "(object 1 0, offset {}): attempting to recover stream length",
            candidate_offset + candidate_header.len() as u64
        ),
        format!(
            "(object 1 0, offset {}): recovered stream length: 29",
            candidate_offset + candidate_header.len() as u64
        ),
        format!(
            "(xref stream: object 1 0, offset {}): expected endstream",
            candidate_offset + candidate_header.len() as u64 + 3
        ),
        format!(
            "(xref stream: object 1 0, offset {}): attempting to recover stream length",
            candidate_offset + candidate_header.len() as u64
        ),
        format!(
            "(xref stream: object 1 0, offset {}): recovered stream length: 29",
            candidate_offset + candidate_header.len() as u64
        ),
    ];
    assert_eq!(recovered_diagnostics, expected);
}

/// Build a damaged document that strict parsing rejects (corrupt `xref` keyword)
/// so best-effort falls into the linear scan: a recoverable catalog (object 1),
/// then `count` malformed candidate objects each beginning with `body_suffix`
/// after their `N 0 obj` header and never closed, then a valid trailer. The
/// candidates use distinct object numbers (2..=count+1) so they cannot collapse
/// to a single recovered entry — exercising the per-candidate cost.
fn linear_scan_dos_fixture(count: u32, body_suffix: &str) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    for number in 2..=count + 1 {
        bytes.extend_from_slice(format!("\n{number} 0 obj {body_suffix}").as_bytes());
    }
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"\nzref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// A flood of candidate objects whose bodies are unterminated literal strings
/// (`N 0 obj (`) must not drive recovery to quadratic cost. The pre-fix scan
/// re-parsed each candidate to end-of-file at every advancing byte; the
/// qpdf-style line scan reads only the `N 0 obj` header per line and never parses
/// the body, so every distinct candidate is recovered and recovery completes.
#[test]
fn best_effort_unterminated_literal_flood_is_linear() {
    // Large enough that the pre-fix O(n^2) parse would take many seconds (the
    // validated PoC timed out past ~25k candidates); the linear scan is instant.
    let count: u32 = 30_000;
    let loaded =
        load_xref_and_trailer_best_effort(&mut Cursor::new(linear_scan_dos_fixture(count, "(")))
            .unwrap();

    // The catalog (object 1) and every distinct candidate are recovered: the
    // header scan never bails out early at a malformed body.
    assert!(loaded.entries.contains_key(&ObjectRef::new(1, 0)));
    assert!(loaded.entries.contains_key(&ObjectRef::new(count + 1, 0)));
    assert_eq!(loaded.entries.len(), count as usize + 1);
}

/// The companion attack whose candidate bodies open a dictionary with an
/// unterminated literal (`N 0 obj << /K (`). This is the case that proves the
/// ObjStm-recovery second pass stays linear: each candidate starts with `<<`, so
/// it is parsed, but only within the window bounded by the next candidate's
/// offset, so the unterminated literal cannot scan to end-of-file.
#[test]
fn best_effort_unterminated_dict_flood_is_linear() {
    let count: u32 = 30_000;
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(linear_scan_dos_fixture(
        count, "<< /K (",
    )))
    .unwrap();

    assert!(loaded.entries.contains_key(&ObjectRef::new(1, 0)));
    assert!(loaded.entries.contains_key(&ObjectRef::new(count + 1, 0)));
    assert_eq!(loaded.entries.len(), count as usize + 1);
}

/// Build a damaged document whose recoverable region (catalog object 1, then a
/// second object) is separated by `count` copies of `filler_line`. With a
/// `filler_line` that records nothing — a whitespace-only or comment-only line —
/// the first-token read for each filler line must be bounded to that line.
/// Otherwise it skips forward to object 2 and re-scans the same suffix on every
/// iteration, an O(n^2) blowup.
fn linear_scan_filler_fixture(count: u32, filler_line: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    for _ in 0..count {
        bytes.extend_from_slice(filler_line);
    }
    bytes.extend_from_slice(b"5 0 obj\n<< >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// A flood of whitespace-only lines (`<spaces>\n`) between two objects must not
/// drive recovery to quadratic cost. Spaces break up the end-of-line run so
/// `next_line_start` cannot collapse them, so the linearity relies on bounding
/// the first-token read to the current line.
#[test]
fn best_effort_whitespace_only_line_flood_is_linear() {
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(linear_scan_filler_fixture(
        60_000, b"   \n",
    )))
    .unwrap();
    assert!(loaded.entries.contains_key(&ObjectRef::new(1, 0)));
    assert!(loaded.entries.contains_key(&ObjectRef::new(5, 0)));
    assert_eq!(loaded.entries.len(), 2);
}

/// A flood of comment-only lines (`%...\n`) must likewise stay linear: comments
/// are skipped like whitespace when reading a token, so without the per-line
/// bound each comment line would skip forward to the next object and re-scan.
#[test]
fn best_effort_comment_only_line_flood_is_linear() {
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(linear_scan_filler_fixture(
        60_000,
        b"%filler comment\n",
    )))
    .unwrap();
    assert!(loaded.entries.contains_key(&ObjectRef::new(1, 0)));
    assert!(loaded.entries.contains_key(&ObjectRef::new(5, 0)));
    assert_eq!(loaded.entries.len(), 2);
}

/// A comment between an object header's tokens (`7 %c<EOL>0 obj`) must still be
/// recovered: qpdf's tokenizer skips `%...EOL` comments in token-leading
/// position, so the header is recovered at the number-token offset. qpdf 11.9.0
/// recovers `7/0` from this fixture.
#[test]
fn best_effort_recovers_object_with_comment_between_header_tokens() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let obj7_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"7 %a comment\n0 obj\n<< >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(7, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: obj7_offset
        })
    );
}

#[test]
fn best_effort_recovers_plus_prefixed_object_number() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let object_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"+7 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 7 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(7, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: object_offset
        })
    );
}

#[test]
fn best_effort_reconstruction_applies_qpdf_100_byte_token_limit() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let token_99 = format!("{}7", "0".repeat(98));
    let token_100 = format!("{}8", "0".repeat(99));
    assert_eq!(token_99.len(), 99);
    assert_eq!(token_100.len(), 100);

    let object_7_offset = bytes.len() as u64;
    bytes.extend_from_slice(format!("{token_99} 0 obj\n<< /Type /Catalog >>\nendobj\n").as_bytes());
    bytes.extend_from_slice(format!("{token_100} 0 obj\n<< >>\nendobj\n").as_bytes());
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 9 /Root 7 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(7, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: object_7_offset
        })
    );
    assert!(!loaded.entries.contains_key(&ObjectRef::new(8, 0)));
}

/// The line scan must honour qpdf's `reconstruct_xref` guards: a token sequence
/// that does not begin on its own line is attributed to the line where it starts
/// (not a preceding whitespace-only line), and `insertReconstructedXrefEntry`
/// rejects `obj <= 0` and `gen` outside `0..65535`. qpdf 11.9.0 recovers only
/// object 1 from this fixture.
#[test]
fn best_effort_line_scan_honours_qpdf_reconstruct_guards() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // Recovered normally.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    // A whitespace-only line before the next object records nothing: its first
    // token does not begin on this line (the first-token read is bounded to it).
    bytes.extend_from_slice(b"   \n");
    // obj 0 is rejected (`obj > 0`).
    bytes.extend_from_slice(b"0 0 obj\n<< >>\nendobj\n");
    // generation 65535 is rejected (`gen < 65535`).
    bytes.extend_from_slice(b"2 65535 obj\n<< >>\nendobj\n");

    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
    );
    assert!(!loaded.entries.contains_key(&ObjectRef::new(0, 0)));
    assert!(!loaded.entries.contains_key(&ObjectRef::new(2, 65535)));
    assert_eq!(loaded.entries.len(), 1);
}

/// When recovery finds objects but no `trailer` keyword exists and none of the
/// reconstructed objects is a `/Type /XRef` stream, `reconstruct_xref` must
/// fail with "unable to find trailer dictionary while recovering damaged
/// file" (qpdf 11.9.0 `QPDF.cc:615`).
#[test]
fn best_effort_errors_when_trailer_missing() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable indirect object so `recover_xref_entries` succeeds, but not
    // a stream, so it is not a `/Type /XRef` candidate either.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    // Corrupt xref keyword and rename the `trailer` keyword to `traile_` so the
    // literal marker is absent.
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("traile_\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let err = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect_err("missing trailer keyword should fail");
    let message = format!("{err}");
    assert!(
        message.contains("unable to find trailer dictionary while recovering damaged file"),
        "got {message}"
    );
    let (source, diagnostics) = err
        .open_failure()
        .expect("terminal repair failure carries warnings");
    assert!(matches!(source, Error::Parse { .. }), "got {source:?}");
    assert_eq!(diagnostics.entries().len(), 3);
}

/// When the `trailer` keyword is present but followed by a non-dictionary
/// token, qpdf's own `reconstruct_xref` (`QPDF.cc:564-570`) does not stop the
/// scan or throw there ("Oh well. It was worth a try.") -- it just leaves the
/// trailer unset and keeps scanning. With no `/Type /XRef` candidate either,
/// the terminal error is the same "unable to find trailer dictionary while
/// recovering damaged file" as a file with no `trailer` keyword at all.
#[test]
fn best_effort_errors_when_trailer_not_dictionary() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable indirect object so `recover_xref_entries` succeeds, but not
    // a stream, so it is not a `/Type /XRef` candidate either.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    // `trailer` followed by a bare integer rather than a `<<...>>` dictionary.
    bytes.extend_from_slice(format!("trailer\n42\nstartxref\n{start_xref}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect_err("non-dictionary trailer should fail");
    let message = format!("{err}");
    assert!(
        message.contains("unable to find trailer dictionary while recovering damaged file"),
        "got {message}"
    );
    let (source, diagnostics) = err
        .open_failure()
        .expect("terminal repair failure carries warnings");
    assert!(matches!(source, Error::Parse { .. }), "got {source:?}");
    assert_eq!(diagnostics.entries().len(), 3);
}

/// qpdf's reconstruction scan keeps the first successfully parsed trailer
/// dictionary and ignores a later malformed/non-dictionary candidate. A raw
/// last-occurrence search would select `trailer 42` and fail recovery instead.
#[test]
fn best_effort_recovery_keeps_first_valid_trailer_before_invalid_candidate() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let object_offset = 9;
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 2 /Root 1 0 R >>\ntrailer\n42\nstartxref\n{start_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.trailer.get("Size"), Some(&Object::Integer(2)));
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: object_offset
        })
    );
}

/// A malformed candidate must not terminate qpdf's forward scan; a later
/// valid dictionary is accepted when no earlier valid trailer exists.
#[test]
fn best_effort_recovery_skips_invalid_trailer_before_valid_candidate() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!(
            "trailer\n42\ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.trailer.get("Size"), Some(&Object::Integer(2)));
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// Once a valid dictionary has been accepted, a later valid dictionary is
/// ignored as well (`QPDF::setTrailer` is first-valid-wins, not last-wins).
#[test]
fn best_effort_recovery_keeps_first_valid_trailer_before_later_valid_candidate() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 2 /Root 1 0 R >>\ntrailer\n<< /Size 99 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(loaded.trailer.get("Size"), Some(&Object::Integer(2)));
}

/// When `startxref` is absent, repair pushes a "can't find startxref" error
/// and retries `parse_xref_from_start` at offset 0, which fails at the header
/// and pushes a second error. Only the first (triggering) error appears in the
/// warning sequence: qpdf has no offset-0 retry, so the follow-up failure has
/// no counterpart in qpdf's stderr for the same input.
#[test]
fn repair_diagnostics_report_only_the_triggering_error() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable object and a valid trailer so recovery itself succeeds.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    // Note: NO `startxref` keyword at all.
    bytes.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n%%EOF\n");
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    let messages: Vec<&str> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "file is damaged",
            "can't find startxref",
            "Attempting to reconstruct cross-reference table",
        ],
        "expected the qpdf warning sequence with only the first error"
    );

    assert_eq!(
        loaded.repair_diagnostics.entries()[1].offset,
        None,
        "qpdf suppresses the synthetic EOF offset for missing startxref"
    );
    // qpdf prints no offset for any of this missing-startxref sequence.
    assert_eq!(loaded.repair_diagnostics.entries()[0].offset, None);
    assert_eq!(loaded.repair_diagnostics.entries()[2].offset, None);

    // Recovery still produced usable entries and a trailer.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn repair_missing_header_uses_version_1_2_and_preserves_strict_rejection() {
    let mut bytes = b"notpdf!!\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
          trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n",
    );
    bytes.extend_from_slice(format!("{xref_offset}\n%%EOF\n").as_bytes());

    let loaded =
        load_xref_and_trailer_best_effort(&mut Cursor::new(bytes.clone())).expect("repair header");

    assert_eq!(loaded.version, "1.2");
    assert_eq!(
        loaded.repair_diagnostics.entries(),
        [flpdf::Diagnostic::warning("can't find PDF header", None)]
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
    );

    let strict = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("strict loading must still reject a missing header");
    assert_eq!(
        strict.to_string(),
        "parse error at byte 0: missing PDF header"
    );
}

#[test]
fn repair_finds_a_valid_header_in_the_first_1024_bytes_and_uses_it_as_origin() {
    let mut logical_pdf = b"%PDF-1.7\n".to_vec();
    logical_pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = logical_pdf.len();
    logical_pdf.extend_from_slice(
        b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
          trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n",
    );
    logical_pdf.extend_from_slice(format!("{xref_offset}\n%%EOF\n").as_bytes());

    // qpdf accepts a pattern whose first byte is still inside [0, 1024).
    let mut bytes = vec![b'x'; 1023];
    bytes.extend_from_slice(&logical_pdf);

    let loaded =
        load_xref_and_trailer_best_effort(&mut Cursor::new(bytes.clone())).expect("repair header");
    assert_eq!(loaded.version, "1.7");
    assert!(loaded.repair_diagnostics.entries().is_empty());
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
    );

    let mut pdf = Pdf::open_mem_owned_with_options(
        bytes.clone(),
        PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("open with qpdf header origin");
    let root = pdf.root_ref().expect("root reference");
    assert!(pdf
        .resolve_object(root)
        .expect("resolve root")
        .as_dict()
        .is_some());

    pdf.set_object(root, Object::Boolean(false));
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("memory output");
    writer.write().expect("qpdf full rewrite");
    let output_root = writer
        .get_renumbered_obj_gen(root)
        .expect("root mapping")
        .expect("root is emitted");
    let rewritten = writer.get_buffer().expect("writer buffer");
    assert!(
        !rewritten.starts_with(&bytes),
        "qpdf full rewrite must emit a fresh document rather than copy the repaired prefix"
    );
    let mut reopened = Pdf::open_mem_owned_with_options(
        rewritten,
        PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("reopen incremental output");
    assert!(
        reopened.repair_diagnostics().entries().is_empty(),
        "header-relative incremental xref must not require repair"
    );
    assert_eq!(
        reopened
            .resolve_object(output_root)
            .expect("resolve rewritten root")
            .as_bool(),
        Some(false)
    );

    let strict = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("strict loading keeps the ordinary start-at-zero boundary");
    assert_eq!(
        strict.to_string(),
        "parse error at byte 0: missing PDF header"
    );

    // A pattern beginning exactly at byte 1024 is outside qpdf's search range.
    let mut outside_search_range = vec![b'x'; 1024];
    outside_search_range.extend_from_slice(&logical_pdf);
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(outside_search_range))
        .expect("repair without an in-range header");
    assert_eq!(loaded.version, "1.2");
    assert_eq!(
        loaded.repair_diagnostics.entries().first(),
        Some(&flpdf::Diagnostic::warning("can't find PDF header", None))
    );
}

#[test]
fn repair_invalid_header_version_uses_version_1_2_and_preserves_strict_version() {
    let mut bytes = b"%PDF-x.y\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
          trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n",
    );
    bytes.extend_from_slice(format!("{xref_offset}\n%%EOF\n").as_bytes());

    let loaded =
        load_xref_and_trailer_best_effort(&mut Cursor::new(bytes.clone())).expect("repair header");

    assert_eq!(loaded.version, "1.2");
    assert_eq!(
        loaded.repair_diagnostics.entries(),
        [flpdf::Diagnostic::warning("can't find PDF header", None)]
    );

    let strict = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect("strict keeps its raw version");
    assert_eq!(strict.version, "x.y");
    assert!(strict.repair_diagnostics.entries().is_empty());
}

#[test]
fn repair_non_utf8_header_version_uses_version_1_2_and_preserves_strict_error() {
    let mut bytes = b"%PDF-\xff\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        b"xref\n0 2\n0000000000 65535 f \n0000000007 00000 n \n\
          trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n",
    );
    bytes.extend_from_slice(format!("{xref_offset}\n%%EOF\n").as_bytes());

    let loaded =
        load_xref_and_trailer_best_effort(&mut Cursor::new(bytes.clone())).expect("repair header");

    assert_eq!(loaded.version, "1.2");
    assert_eq!(
        loaded.repair_diagnostics.entries(),
        [flpdf::Diagnostic::warning("can't find PDF header", None)]
    );

    let strict = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("strict loading must reject a non-UTF-8 header version");
    assert_eq!(
        strict.to_string(),
        "parse error at byte 5: PDF version is not utf-8"
    );
}

#[test]
fn header_version_uses_qpdfs_valid_numeric_prefix() {
    let mut bytes = b"%PDF-1.7suffix\n".to_vec();
    let object_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \n\
             trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let repaired = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes.clone()))
        .expect("repair uses qpdf's numeric version prefix");
    assert_eq!(repaired.version, "1.7");
    assert!(repaired.repair_diagnostics.entries().is_empty());

    let strict = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect("strict keeps its raw version");
    assert_eq!(strict.version, "1.7suffix");
    assert!(strict.repair_diagnostics.entries().is_empty());
}

#[test]
fn missing_header_and_startxref_terminal_failure_preserves_warning_order() {
    let error = load_xref_and_trailer_best_effort(&mut Cursor::new(b"not a PDF".to_vec()))
        .expect_err("unrecoverable malformed input");
    let (_, diagnostics) = error
        .open_failure()
        .expect("repair diagnostics survive terminal failure");

    assert_eq!(
        diagnostics.entries(),
        [
            flpdf::Diagnostic::warning("can't find PDF header", None),
            flpdf::Diagnostic::warning("file is damaged", None),
            flpdf::Diagnostic::warning("can't find startxref", None),
            flpdf::Diagnostic::warning("Attempting to reconstruct cross-reference table", None,),
        ]
    );
}

/// When the triggering error is not `Error::Parse` (here: `Error::Missing`
/// because the xref stream pointed to by `startxref` lacks the required `/W`
/// entry), the trigger warning falls back to the error's `Display` form and
/// the `startxref` offset, since a non-parse error carries no byte offset of
/// its own.
#[test]
fn repair_reports_non_parse_trigger_error_via_display() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    // A recoverable object and a valid trailer so the linear scan succeeds.
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = bytes.len() as u64;
    // An xref stream whose dictionary is missing the required /W entry.
    bytes.extend_from_slice(
        b"2 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /Index [0 3] /Length 0 >>\nstream\n\nendstream\nendobj\n",
    );
    bytes.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    let messages: Vec<&str> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "file is damaged",
            "missing required PDF entry: XRef stream /W",
            "Attempting to reconstruct cross-reference table",
        ],
        "expected the qpdf warning sequence with the Display-formatted trigger"
    );
    assert_eq!(
        loaded.repair_diagnostics.entries()[1].offset,
        Some(xref_offset),
        "expected the non-parse trigger warning to fall back to the startxref offset"
    );
    // qpdf prints no offset for the surrounding warnings (#1 and #3); only
    // the trigger warning carries one.
    assert_eq!(loaded.repair_diagnostics.entries()[0].offset, None);
    assert_eq!(loaded.repair_diagnostics.entries()[2].offset, None);

    // Recovery still produced usable entries and a trailer.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 9 })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// When `startxref` is absent but the FIRST indirect object in the file is
/// itself a valid xref stream with no `/Prev`, repair pushes a single "can't
/// find startxref" error and resets the retry offset to 0. `parse_xref_from_start`
/// then skips the `%PDF-` header comment and parses that xref stream
/// successfully, so the accumulated-error warning arm runs. The emitted
/// diagnostics are the same three-warning sequence qpdf produces for this
/// input (qpdf does not distinguish recovery methods), and in particular must
/// NOT claim a linear object scan ran: the stream parse keeps
/// `XrefForm::Stream`, whereas a linear scan would force `XrefForm::Table`.
#[test]
fn with_repair_appends_diagnostic_when_stream_parse_succeeds() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    // The xref stream is the FIRST indirect object (object number 1) so that,
    // after `skip_ws` skips the `%PDF-` comment line, `parse_xref_from_start`
    // parses it directly. It carries no `/Prev`.
    let xref_offset = bytes.len() as u64;
    let xref_entries = build_encoded_xref_stream_entries(&[(0, 0, 0), (1, xref_offset, 0)]);
    let xref_object = make_xref_stream_object(1, 2, None, 1, &xref_entries);
    bytes.extend_from_slice(&xref_object);

    // Deliberately NO `startxref` keyword: only an `%%EOF` marker follows.
    bytes.extend_from_slice(b"%%EOF\n");
    assert!(
        !bytes.windows(b"startxref".len()).any(|w| w == b"startxref"),
        "fixture must not contain a startxref keyword"
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    // The xref STREAM parse succeeded (not a linear scan, which sets Table).
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);

    // The qpdf-compatible warning sequence, one diagnostic per line.
    let messages: Vec<&str> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "file is damaged",
            "can't find startxref",
            "Attempting to reconstruct cross-reference table",
        ],
        "expected the qpdf warning sequence"
    );
    assert!(
        !messages.iter().any(|m| m.contains("linear object scan")),
        "must not claim a linear scan ran: {messages:?}"
    );
    // qpdf prints no offset for the missing-startxref trigger either.
    assert_eq!(loaded.repair_diagnostics.entries()[0].offset, None);
    assert_eq!(loaded.repair_diagnostics.entries()[1].offset, None);
    assert_eq!(loaded.repair_diagnostics.entries()[2].offset, None);

    // The stream's own entries are present (e.g. object 1 at its offset).
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: xref_offset
        })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// A `/Prev` chain that points back at itself is a circular reference: strict
/// mode must reject it with qpdf's loop diagnostic, while best-effort must
/// stop following the chain and return `Ok` with the entries seen so far.
#[test]
fn circular_prev_recovers_with_repair_and_rejected_strict() {
    // Build a single valid xref table whose own offset we then feed into its
    // trailer `/Prev`, so the chain revisits the same offset (a 1-node cycle).
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
    // `/Prev` points back at this same xref section.
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 2 /Root 1 0 R /Prev {xref_offset} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    // Strict mode rejects the cycle.
    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("circular /Prev should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("loop detected following xref tables"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");

    // Best-effort stops following the cycle and returns the entries it has.
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: obj1_offset as u64
        })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn circular_prev_xref_stream_recovers_without_classic_trailer() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len() as u64;
    let entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, catalog_offset, 0),
        (1, xref_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(
        2,
        3,
        Some(xref_offset),
        1,
        &entries,
    ));
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    assert!(!bytes.windows(7).any(|window| window == b"trailer"));

    let strict = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("strict mode must reject the circular /Prev");
    assert!(strict
        .to_string()
        .contains("loop detected following xref tables"));

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: catalog_offset
        })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: xref_offset
        })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    let messages: Vec<_> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "file is damaged",
            "loop detected following xref tables",
            "Attempting to reconstruct cross-reference table"
        ]
    );
}

#[test]
fn circular_prev_repair_accepts_empty_linear_scan() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 1 /Root 1 0 R /Prev {xref_offset} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("recovered trailer is sufficient despite an empty object scan");
    assert!(loaded.entries.is_empty());
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        vec![
            "file is damaged",
            "loop detected following xref tables",
            "Attempting to reconstruct cross-reference table",
        ]
    );
}

/// A `/Prev` offset pointing at a malformed (non-circular) location makes
/// `merge_previous_xref_sections` error. Strict mode propagates that error;
/// best-effort records it as a diagnostic and falls back to the linear scan.
#[test]
fn merge_failure_falls_back_to_linear_scan() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // A bogus location that is neither `xref` nor a valid xref stream object.
    let bad_prev = bytes.len();
    bytes.extend_from_slice(b"not-an-xref-section\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 2 /Root 1 0 R /Prev {bad_prev} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    // Strict mode propagates the merge-error from `merge_previous_xref_sections`,
    // surfaced as the failure to parse the malformed `/Prev` target as an xref
    // stream object.
    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("malformed /Prev target should fail strict parse");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
    assert!(format!("{err}").contains("expected integer"), "got {err}");

    // Best-effort records the error and recovers via the linear object scan,
    // emitting the qpdf-compatible warning sequence.
    let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true).unwrap();
    assert!(
        !loaded.repair_diagnostics.entries().is_empty(),
        "expected a repair diagnostic from the merge fallback"
    );
    assert!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|entry| entry.message == "Attempting to reconstruct cross-reference table"),
        "expected the qpdf reconstruction warning"
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: obj1_offset as u64
        })
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

/// qpdf reads the classic free-row offset as an offset-sized field and does not
/// retain it in the effective reader xref table. The value may therefore exceed
/// `u32::MAX` without affecting strict loading.
#[test]
fn classic_free_next_field_is_not_retained_or_u32_limited() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    // Object 0: free-list head, generation 65535, next = 0 (fits u32, accepted).
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    // Object 1: free, offset 9999999999 > u32::MAX is still accepted.
    bytes.extend_from_slice(b"9999999999 00000 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("free xref next is not part of the effective table");
    assert_eq!(loaded.entries.get(&ObjectRef::new(1, 0)), None);
}

/// An xref-table entry whose status byte is neither `f` nor `n` must be rejected
/// in strict mode. In `parse_xref_table`, the `in_use` byte is matched against
/// `b'f'` / `b'n'`; any other byte (here `x`) takes the `_ =>` arm and returns
/// the "xref table entry status is not f or n" error.
#[test]
fn rejects_xref_table_bad_entry_status() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    // Object 1: status byte `x` is neither `f` nor `n` -> `_ =>` arm.
    bytes.extend_from_slice(b"0000000009 00000 x \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("invalid xref entry status should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref table entry status is not f or n"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// Well-formed xref-table entries followed by a `trailer` keyword whose value is
/// not a dictionary must be rejected in strict mode. In `parse_xref_table`, once
/// the entry loop completes and the outer loop breaks on `trailer`, the trailer
/// is parsed as an object; when that object is not `Object::Dictionary` the `_ =>`
/// arm returns the "trailer is not a dictionary" error. Here a bare integer `42`
/// follows the keyword.
#[test]
fn rejects_xref_table_trailer_not_dictionary() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    // `trailer` keyword followed by a bare integer instead of a `<<...>>` dict.
    bytes.extend_from_slice(format!("trailer\n42\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("non-dictionary trailer should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("trailer is not a dictionary"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// Build a minimal `%PDF` buffer whose `startxref` points at `xref_offset` and
/// whose `xref_obj` bytes are appended at that offset. Used by the xref-stream
/// error tests below that build a malformed stream object inline (because the
/// shared `make_xref_stream_object` helper hardcodes `/W [1 4 2]` and
/// `/Index [0 size]`, which several of these tests need to vary).
fn pdf_with_xref_object(xref_obj: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(xref_obj);
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

/// `parse_xref_stream`: when `startxref` points at an indirect object that
/// parses as a plain dictionary rather than a `stream`, the non-`Object::Stream`
/// arm returns qpdf's `xref not found` parse error.
#[test]
fn rejects_xref_stream_non_stream_object() {
    // A dictionary indirect object (no `stream`/`endstream`) at the xref offset.
    let xref_obj = b"3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R >>\nendobj\n";
    let bytes = pdf_with_xref_object(xref_obj);

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("non-stream xref object should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("xref not found"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_widths`: a `/W` whose value is not an array (here an integer)
/// takes the non-`Object::Array` arm and returns `Error::Parse("/W must be
/// array")`.
#[test]
fn rejects_xref_stream_w_not_array() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W 5 /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W non-array should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/W must be array"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_widths`: a `/W` array whose length is not exactly three takes the
/// `values.len() != 3` arm and returns `Error::Parse("/W must contain three
/// integers")`.
#[test]
fn rejects_xref_stream_w_wrong_length() {
    let data = [1u8, 0x0A];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W wrong length should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("/W must contain three integers"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_index`: an `/Index` array with an odd number of integers takes
/// the `values.len() % 2 != 0` arm and returns `Error::Parse("/Index must
/// contain an even number of integers")`.
#[test]
fn rejects_xref_stream_index_odd_length() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 3 1] /Index [0] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Index odd length should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("/Index must contain an even number of integers"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_index`: an `/Index` whose value is neither absent nor an array
/// (here an integer) takes the `_ =>` arm and returns `Error::Parse("/Index must
/// be array")`.
#[test]
fn rejects_xref_stream_index_not_array() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 3 1] /Index 5 /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Index non-array should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/Index must be array"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: a `/W [0 0 0]` makes `entry_width == 0`, taking the
/// zero-width guard and returning `Error::Parse("invalid cross-reference stream
/// widths")`.
#[test]
fn rejects_xref_stream_zero_widths() {
    // With all widths zero the decoded stream data is irrelevant; provide none.
    let xref_obj =
        b"3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [0 0 0] /Index [0 1] /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec();

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W [0 0 0] should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("invalid cross-reference stream widths"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_widths`: qpdf 11.9.0 rejects each `/W` value wider than its
/// `qpdf_offset_t` before calculating the entry size. The flpdf boundary must
/// make the same decision before decoding stream bytes.
#[test]
fn rejects_xref_stream_w_above_qpdf_offset_width() {
    let data = [0u8; 9];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [9 0 0] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/W values wider than qpdf offset fields should fail");
    let message = format!("{err}");
    assert!(
        message.contains("Cross-reference stream's /W contains impossibly large values"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: when the entry width implied by `/W` requires more
/// bytes than the decoded stream provides, the `cursor.pos + entry_width >
/// len` guard returns `Error::Parse("xref stream data truncated")`. Here `/W
/// [1 3 1]` needs 5 bytes per entry across two declared entries but only one
/// entry's worth of data is present.
#[test]
fn rejects_xref_stream_truncated_data() {
    // /Index declares 2 entries (10 bytes) but only 5 bytes of data are present.
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 2] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("truncated xref stream data should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream data truncated"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: a type field (`/W[0]`) wide enough to hold a value
/// greater than 255 takes the `u8::try_from` failure arm and returns
/// `Error::Parse("xref stream object type does not fit u8")`. Here `/W [2 1 1]`
/// gives the type field two bytes and the data encodes type value `0x0100`.
#[test]
fn rejects_xref_stream_object_type_overflow() {
    // One entry: type = 0x0100 (256, > u8::MAX), field1 = 0x0A, field2 = 0.
    let data = [0x01u8, 0x00, 0x0A, 0x00];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [2 1 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("xref type > 255 should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream object type does not fit u8"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: a type-2 (compressed) entry whose field1 (the
/// containing stream's object number) exceeds `u32::MAX` takes the
/// `u32::try_from(field1)` failure arm and returns `Error::Parse("xref stream
/// object number does not fit u32")`. This needs `w1 >= 5` bytes so field1 can
/// hold a value above `u32::MAX`; `/W [1 5 1]` gives field1 five bytes encoding
/// `0x01_0000_0000` (2^32).
#[test]
fn rejects_xref_stream_type2_stream_number_overflow() {
    // type = 2, field1 = 0x01_00_00_00_00 (2^32, > u32::MAX), field2 = 0.
    let data = [2u8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 5 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("type-2 stream number > u32 should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("xref stream object number does not fit u32"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_xref_entries`: an entry whose `object_type` is 3 (neither free, in-use,
/// nor compressed) takes the `_ =>` arm and returns `Error::Unsupported(
/// "unsupported xref entry type 3")`.
#[test]
fn rejects_xref_stream_unsupported_entry_type() {
    // One entry with type byte 3.
    let data = [3u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 1 /Root 1 0 R /W [1 3 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("unsupported xref entry type should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("unsupported xref entry type 3"),
        "got {message}"
    );
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

/// `parse_non_negative_u64` (via the `/Size` lookup in `parse_xref_stream`):
/// a `/Size` that is not an integer (here a name) takes the non-`Object::Integer`
/// arm and returns `Error::Parse("/Size is not integer")`.
#[test]
fn rejects_xref_stream_size_not_integer() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size /Big /Root 1 0 R /W [1 3 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Size non-integer should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/Size is not integer"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `parse_non_negative_u64` (via the `/Size` lookup in `parse_xref_stream`):
/// a negative `/Size` takes the `*integer < 0` arm and returns
/// `Error::Parse("/Size is negative")`.
#[test]
fn rejects_xref_stream_negative_size() {
    let data = [1u8, 0, 0, 0x0A, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size -1 /Root 1 0 R /W [1 3 1] /Index [0 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let err = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect_err("/Size negative should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("/Size is negative"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `build_xref_ranges`: an `/Index` chunk with a zero count (`[0 0 1 1]`) takes
/// the `chunk[1] == 0` skip arm, so that chunk contributes no range. Loading
/// succeeds and only object 1 (from the `[1 1]` chunk) is present.
#[test]
fn xref_stream_index_zero_count_range_skipped() {
    // Only the second chunk `1 1` yields a range: object 1 at offset 0x14.
    let data = [1u8, 0, 0, 0x14, 0];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 3 1] /Index [0 0 1 1] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let loaded = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect("zero-count index chunk should be skipped, load should succeed");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 0x14 })
    );
    // Object 0 came only from the skipped zero-count chunk, so it is absent.
    assert_eq!(loaded.entries.get(&ObjectRef::new(0, 0)), None);
}

/// `parse_xref_entries`: a `/W` with `w0 == 0` (`[0 3 1]`) takes the
/// `object_type` default-to-1 arm, so every entry is treated as a type-1
/// in-use entry yielding `XrefEntry::Uncompressed`. Loading succeeds.
#[test]
fn loads_xref_stream_with_w0_zero_defaults_type_one() {
    // w0 == 0: no type byte; field1 = offset (3 bytes), field2 = generation (1).
    let data = [
        0, 0, 0x0A, 0, // object 0 -> offset 0x0A
        0, 0, 0x14, 0, // object 1 -> offset 0x14
    ];
    let xref_obj = format!(
        "3 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [0 3 1] /Index [0 2] /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    let mut xref_obj = xref_obj;
    xref_obj.extend_from_slice(&data);
    xref_obj.extend_from_slice(b"\nendstream\nendobj\n");

    let loaded = load_xref_and_trailer(&mut Cursor::new(pdf_with_xref_object(&xref_obj)))
        .expect("w0 == 0 should default to type 1, load should succeed");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(0, 0)),
        Some(&XrefEntry::Uncompressed { offset: 0x0A })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefEntry::Uncompressed { offset: 0x14 })
    );
}

/// `ByteCursor::read_fixed`: an xref table that declares more entries than the
/// file actually contains ends mid-entry, so reading the missing entry's
/// fixed-width offset field hits the `pos + width > len` guard and returns
/// `Error::Parse` with an "unexpected end of" message. The `startxref` keyword
/// is placed BEFORE the xref section (it is located by `rposition`, so its
/// position in the file is irrelevant) so the file can end mid-table with no
/// trailing tokens for the fixed-width reader to mistake for entry fields.
#[test]
fn rejects_xref_table_truncated_entry() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    // Emit `startxref` ahead of the xref section. Pad the offset to a fixed
    // 10-digit width (leading zeros parse fine) so the marker length does not
    // depend on the offset's decimal magnitude, making the offset a simple sum.
    let xref_offset = bytes.len() + "startxref\n0000000000\n%%EOF\n".len();
    bytes.extend_from_slice(format!("startxref\n{xref_offset:010}\n%%EOF\n").as_bytes());
    assert_eq!(
        bytes.len(),
        xref_offset,
        "xref must follow the startxref marker exactly"
    );

    // Declare 2 entries but provide only the first, then end the file: the
    // second entry's 10-digit offset field runs off the end of the buffer.
    bytes.extend_from_slice(b"xref\n0 2\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("truncated xref table entry should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("unexpected end of fixed-width field"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `ByteCursor::read_byte`: an xref table whose final entry supplies the
/// 10-digit offset and 5-digit generation but ends before the in-use status
/// byte drives `read_byte` to the `bytes.get(pos)` `None` arm, returning
/// `Error::Parse("unexpected end of input")`. As in the truncated-entry test,
/// `startxref` is placed before the xref section so the file can end mid-entry.
#[test]
fn rejects_xref_table_truncated_status_byte() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len() + "startxref\n0000000000\n%%EOF\n".len();
    bytes.extend_from_slice(format!("startxref\n{xref_offset:010}\n%%EOF\n").as_bytes());
    assert_eq!(
        bytes.len(),
        xref_offset,
        "xref must follow the startxref marker exactly"
    );

    // One declared entry: offset + generation present, but the file ends before
    // the status byte, so `read_byte` exhausts the buffer.
    bytes.extend_from_slice(b"xref\n0 1\n");
    bytes.extend_from_slice(b"0000000000 65535");

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("xref entry missing status byte should fail strict parse");
    let message = format!("{err}");
    assert!(message.contains("unexpected end of input"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

/// `ByteCursor::read_unsigned` (via `read_u32` for the subsection count in
/// `parse_xref_table`): a subsection header that supplies the start object
/// number but no count integer makes `read_unsigned` find no digits at the
/// `trailer` keyword, taking the `start == pos` arm and returning
/// `Error::Parse("expected unsigned integer")`.
#[test]
fn rejects_xref_table_missing_object_count() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_offset = bytes.len();
    // Subsection header `0` with no count integer; `trailer` follows directly,
    // so reading the count finds no digits.
    bytes.extend_from_slice(b"xref\n0\ntrailer\n<< /Size 1 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("missing xref subsection count should fail strict parse");
    let message = format!("{err}");
    assert!(
        message.contains("expected unsigned integer"),
        "got {message}"
    );
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

// Build a document whose `startxref` points at a cross-reference stream.
// `trailer_section` chooses whether an earlier revision leaves a classic
// `xref`/`trailer` section in the file: qpdf's reconstruction pass recovers the
// trailer from the `trailer` keyword, so only a document that has one can be
// reconstructed at all.
fn xref_stream_document(trailer_section: bool) -> (Vec<u8>, u64) {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let mut previous_offset = None;
    if trailer_section {
        let table_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{table_offset}\n%%EOF\n")
                .as_bytes(),
        );
        previous_offset = Some(table_offset);
    }

    let xref_stream_offset = bytes.len() as u64;
    let entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, obj1_offset, 0),
        (1, xref_stream_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(2, 3, previous_offset, 1, &entries));
    bytes.extend_from_slice(format!("startxref\n{xref_stream_offset}\n%%EOF\n").as_bytes());

    (bytes, xref_stream_offset)
}

fn xref_stream_document_with_indirect_size(size_value: i64) -> Vec<u8> {
    xref_stream_document_with_indirect_size_object(format!("{size_value}\nendobj\n").as_bytes())
}

fn xref_stream_document_with_indirect_size_object(object_body: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_stream_offset = bytes.len() as u64;
    let entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, obj1_offset, 0),
        (1, xref_stream_offset, 0),
    ]);
    let mut xref_stream = format!(
        "2 0 obj\n<< /Type /XRef /Size 3 0 R /Root 1 0 R /W [1 4 2] /Index [0 3] /Length {} >>\nstream\n",
        entries.len()
    )
    .into_bytes();
    xref_stream.extend_from_slice(&entries);
    xref_stream.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(&xref_stream);

    // The candidate's indirect `/Size` is available only through the
    // reconstruction line-scan table, not through the candidate stream's
    // own xref entries.
    bytes.extend_from_slice(b"3 0 obj\n");
    bytes.extend_from_slice(object_body);
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
    bytes
}

fn classic_xref_document_with_indirect_size(size_value: i64) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let size_offset = bytes.len() as u64;
    bytes.extend_from_slice(format!("3 0 obj\n{size_value}\nendobj\n").as_bytes());

    let xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{catalog_offset:010} 00000 n \n0000000000 65535 f \n{size_offset:010} 00000 n \ntrailer\n<< /Size 3 0 R /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    bytes
}

fn classic_xref_with_indirect_size_header_mismatch(size_value: i64) -> (Vec<u8>, u64) {
    classic_xref_with_indirect_size_header_mismatch_body(size_value, b"endobj\n")
}

fn classic_xref_with_indirect_size_header_mismatch_missing_endobj(
    size_value: i64,
) -> (Vec<u8>, u64) {
    classic_xref_with_indirect_size_header_mismatch_body(size_value, b"")
}

fn classic_xref_with_indirect_size_header_mismatch_body(
    size_value: i64,
    size_object_terminator: &[u8],
) -> (Vec<u8>, u64) {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let size_offset = bytes.len() as u64;
    bytes.extend_from_slice(format!("3 0 obj\n{size_value}\n").as_bytes());
    bytes.extend_from_slice(size_object_terminator);
    let wrong_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"4 0 obj\n<< /Foo true >>\nendobj\n");

    let xref_offset = bytes.len() as u64;
    bytes.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{catalog_offset:010} 00000 n \n0000000000 65535 f \n{wrong_offset:010} 00000 n \n{wrong_offset:010} 00000 n \ntrailer\n<< /Size 3 0 R /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    (bytes, size_offset)
}

fn ignore_xref_streams_options(repair: bool) -> PdfOpenOptions {
    PdfOpenOptions {
        repair,
        ignore_xref_streams: true,
        ..PdfOpenOptions::default()
    }
}

fn classic_xref_with_hybrid_only_entry() -> (Vec<u8>, u64) {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let hybrid_only_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<< /HybridOnly true >>\nendobj\n");

    let xref_stream_offset = bytes.len() as u64;
    let xref_stream_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65535),
        (1, catalog_offset, 0),
        (1, hybrid_only_offset, 0),
        (1, xref_stream_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(
        3,
        4,
        None,
        1,
        &xref_stream_entries,
    ));

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 4 /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    (bytes, hybrid_only_offset)
}

fn classic_xref_with_shared_indirect_size_object() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let size_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"3 0 obj\n4\n");

    let xref_stream_offset = bytes.len() as u64;
    let entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (1, xref_stream_offset, 0),
        (1, size_offset, 0),
    ]);
    let mut xref_stream = format!(
        "2 0 obj\n<< /Type /XRef /Size 3 0 R /Root 1 0 R /W [1 4 2] /Index [0 4] /Length {} >>\nstream\n",
        entries.len()
    )
    .into_bytes();
    xref_stream.extend_from_slice(&entries);
    xref_stream.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(&xref_stream);

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{xref_stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{size_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 3 0 R /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    bytes
}

fn classic_xref_with_shared_indirect_xrefstm_and_size_object() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let size_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n0000000000\n");

    let xref_stream_offset = bytes.len() as u64;
    bytes[size_offset + 8..size_offset + 18]
        .copy_from_slice(format!("{xref_stream_offset:010}").as_bytes());
    let entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (1, xref_stream_offset, 0),
        (1, size_offset as u64, 0),
    ]);
    let mut xref_stream = format!(
        "2 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 4 2] /Index [0 4] /Length {} >>\nstream\n",
        entries.len()
    )
    .into_bytes();
    xref_stream.extend_from_slice(&entries);
    xref_stream.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(&xref_stream);

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{xref_stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{size_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 3 0 R /Root 1 0 R /XRefStm 3 0 R >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    bytes
}

fn xref_stream_prev_loop_with_shared_indirect_previous_offset() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let size_body_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n0000000000\n");
    let previous_two_offset = bytes.len() as u64;
    let previous_two_offset_bytes = format!("{previous_two_offset:010}");
    bytes[size_body_offset + 8..size_body_offset + 18]
        .copy_from_slice(previous_two_offset_bytes.as_bytes());

    let previous_two_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (0, 0, 0),
        (1, size_body_offset as u64, 0),
        (1, previous_two_offset, 0),
        (0, 0, 0),
        (0, 0, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_with_prev_ref(
        4,
        7,
        "/Prev 3 0 R",
        &previous_two_entries,
    ));

    let previous_one_offset = bytes.len() as u64;
    let previous_one_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (0, 0, 0),
        (1, size_body_offset as u64, 0),
        (1, previous_two_offset, 0),
        (1, previous_one_offset, 0),
        (0, 0, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_with_prev_ref(
        5,
        7,
        "/Prev 3 0 R",
        &previous_one_entries,
    ));

    let latest_offset = bytes.len() as u64;
    let latest_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65_535),
        (1, catalog_offset, 0),
        (0, 0, 0),
        (1, size_body_offset as u64, 0),
        (1, previous_two_offset, 0),
        (1, previous_one_offset, 0),
        (1, latest_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_with_prev_ref(
        6,
        7,
        &format!("/Prev {previous_one_offset}"),
        &latest_entries,
    ));
    bytes.extend_from_slice(format!("startxref\n{latest_offset}\n%%EOF\n").as_bytes());

    bytes
}

fn make_xref_stream_with_prev_ref(
    object_number: u32,
    size: u32,
    prev: &str,
    entries: &[u8],
) -> Vec<u8> {
    let mut object = format!(
        "{object_number} 0 obj\n<< /Type /XRef /Size {size} /Root 1 0 R /W [1 4 2] /Index [0 {size}] /Length {} {prev} >>\nstream\n",
        entries.len()
    )
    .into_bytes();
    object.extend_from_slice(entries);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

#[test]
fn classic_xref_table_reads_entries_from_its_xrefstm() {
    let (bytes, hybrid_only_offset) = classic_xref_with_hybrid_only_entry();
    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes.clone())).expect("hybrid xref loads");
    assert_eq!(loaded.last_xref_form, XrefForm::Table);
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: hybrid_only_offset
        }),
        "the classic trailer's /XRefStm contributes its hybrid-only entry"
    );

    let mut pdf = Pdf::open_mem_owned(bytes).expect("the reader sees the hybrid-only object");
    assert_eq!(
        pdf.resolve_object(ObjectRef::new(2, 0))
            .expect("resolve hybrid-only object")
            .as_dict()
            .and_then(|dict| dict.get("HybridOnly"))
            .and_then(Object::as_bool),
        Some(true)
    );
}

#[test]
fn hybrid_xref_reuses_an_indirect_size_resolution_diagnostic() {
    let bytes = classic_xref_with_shared_indirect_size_object();
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode should load the hybrid xref");

    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
            .count(),
        1,
        "the shared indirect /Size object warning must be emitted once"
    );
}

#[test]
fn hybrid_xref_commits_shared_indirect_xrefstm_size_cache() {
    let bytes = classic_xref_with_shared_indirect_xrefstm_and_size_object();
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode should load the hybrid xref");

    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
            .count(),
        1,
        "the /XRefStm resolution must share its cache with post-chain /Size"
    );
}

#[test]
fn xref_stream_prev_loop_reuses_an_indirect_previous_offset_cache() {
    let bytes = xref_stream_prev_loop_with_shared_indirect_previous_offset();
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode should report the repeated /Prev loop");

    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
            .count(),
        1,
        "the shared indirect /Prev object warning must be emitted once"
    );
}

/// qpdf's `read_xrefStream` accepts a stream only when it has `/Type /XRef`.
/// An otherwise valid hybrid stream without that type must therefore be
/// rejected rather than contributing entries from a classic trailer's
/// `/XRefStm`.
#[test]
fn rejects_hybrid_xref_stream_without_xref_type() {
    let (mut bytes, _) = classic_xref_with_hybrid_only_entry();
    let type_marker = b"/Type /XRef";
    let type_pos = bytes
        .windows(type_marker.len())
        .position(|window| window == type_marker)
        .expect("hybrid fixture contains an xref type");
    // Keep the fixture's stored xref offsets valid while removing its `/Type`
    // key: `/Bogus /Yep` has the same length as `/Type /XRef`.
    bytes[type_pos..type_pos + type_marker.len()].copy_from_slice(b"/Bogus /Yep");

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("untyped hybrid stream must not be accepted as xref");
    let message = format!("{err}");
    assert!(message.contains("xref not found"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}

#[test]
fn classic_xref_table_hybrid_entries_match_pinned_qpdf_show_xref() {
    // cov:ignore-start: CI provides qpdf; this fallback is for developer hosts only.
    if std::process::Command::new("qpdf")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("qpdf not available; skipping hybrid xref differential");
        return;
    }
    // cov:ignore-end

    let (bytes, hybrid_only_offset) = classic_xref_with_hybrid_only_entry();
    let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
    let path = directory.path().join("classic-xref-xrefstm.pdf");
    std::fs::write(&path, bytes).expect("write hybrid xref fixture");

    let qpdf = std::process::Command::new("qpdf")
        .arg("--show-xref")
        .arg(&path)
        .output()
        .expect("run pinned qpdf --show-xref");
    assert!(
        qpdf.status.success(),
        "qpdf --show-xref failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&qpdf.stdout),
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(
        String::from_utf8_lossy(&qpdf.stdout)
            .contains(&format!("2/0: uncompressed; offset = {hybrid_only_offset}")),
        "qpdf --show-xref output:\n{}",
        String::from_utf8_lossy(&qpdf.stdout)
    );
}

fn classic_xref_with_xrefstm_value(value: &[u8]) -> (Vec<u8>, u64) {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R /XRefStm ");
    bytes.extend_from_slice(value);
    bytes.extend_from_slice(format!(" >>\nstartxref\n{table_offset}\n%%EOF\n").as_bytes());

    (bytes, table_offset)
}

#[test]
fn classic_xref_table_rejects_non_integer_xrefstm_at_the_table_offset() {
    for value in [b"/NotAnOffset".as_slice(), b"<< /Offset 42 >>", b"42.5"] {
        let (bytes, table_offset) = classic_xref_with_xrefstm_value(value);
        let error = load_xref_and_trailer(&mut Cursor::new(bytes))
            .expect_err("a classic /XRefStm must be an integer");
        assert_eq!(
            error.to_string(),
            format!("parse error at byte {table_offset}: invalid /XRefStm"),
            "value: {}",
            String::from_utf8_lossy(value),
        );
    }
}

#[test]
fn classic_xref_table_reports_negative_xrefstm_as_an_invalid_seek() {
    let (bytes, _) = classic_xref_with_xrefstm_value(b"-1");
    let error = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("negative /XRefStm must fail while seeking the stream");
    assert!(
        matches!(error, Error::Io(ref source) if source.kind() == std::io::ErrorKind::InvalidInput),
        "unexpected error: {error}"
    );
}

#[test]
fn ignored_classic_xrefstm_skips_non_integer_validation() {
    let (bytes, _) = classic_xref_with_xrefstm_value(b"/NotAnOffset");
    let pdf = Pdf::open_mem_owned_with_options(bytes, ignore_xref_streams_options(false))
        .expect("ignore_xref_streams skips the classic /XRefStm branch");
    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
}

#[test]
fn classic_xref_table_reports_a_non_stream_xrefstm_at_its_target_offset() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let not_a_stream_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<< /NotAnXRefStream true >>\nendobj\n");

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 3 /Root 1 0 R /XRefStm {not_a_stream_offset} >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let error = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("the /XRefStm target must be an xref stream");
    assert_eq!(
        error.to_string(),
        format!("parse error at byte {not_a_stream_offset}: xref not found")
    );
}

#[test]
fn classic_xref_table_preserves_recovery_diagnostics_from_its_xrefstm() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_stream_offset = bytes.len() as u64;
    let xref_stream_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65535),
        (1, catalog_offset, 0),
        (1, xref_stream_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object_with_declared_length(
        2,
        3,
        None,
        1,
        XrefStreamIndex::full(3),
        &xref_stream_entries,
        xref_stream_entries.len() + 10,
    ));

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 3 /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode recovers the hybrid xref stream length");
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| {
            diagnostic.message.contains("recovered stream length")
                && diagnostic.message.contains("(xref stream: object 2 0,")
        }));
}

#[test]
fn classic_xref_entries_take_precedence_over_its_xrefstm_entries() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let classic_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<< /Source /Classic >>\nendobj\n");

    let xref_stream_offset = bytes.len() as u64;
    let xref_stream_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 65535),
        (1, catalog_offset, 0),
        (1, xref_stream_offset, 0),
        (1, xref_stream_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(
        3,
        4,
        None,
        1,
        &xref_stream_entries,
    ));

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{classic_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 4 /Root 1 0 R /XRefStm {xref_stream_offset} >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).expect("hybrid xref loads");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: classic_offset
        })
    );
}

#[test]
fn classic_xref_prev_continues_past_the_xrefstms_discarded_prev() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let previous_only_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<< /Source /ClassicPrev >>\nendobj\n");
    let discarded_prev_only_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"4 0 obj\n<< /Source /DiscardedStreamPrev >>\nendobj\n");

    let classic_prev_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{previous_only_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 7 /Root 1 0 R >>\n");

    let discarded_prev_offset = bytes.len() as u64;
    bytes.extend_from_slice(&make_xref_stream_object_with_index(
        5,
        7,
        None,
        1,
        XrefStreamIndex { start: 4, count: 1 },
        &build_encoded_xref_stream_entries(&[(1, discarded_prev_only_offset, 0)]),
    ));

    let xref_stream_offset = bytes.len() as u64;
    bytes.extend_from_slice(&make_xref_stream_object_with_index(
        6,
        7,
        Some(discarded_prev_offset),
        1,
        XrefStreamIndex { start: 6, count: 1 },
        &build_encoded_xref_stream_entries(&[(1, xref_stream_offset, 0)]),
    ));

    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 7 /Root 1 0 R /XRefStm {xref_stream_offset} /Prev {classic_prev_offset} >>\nstartxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes)).expect("hybrid xref loads");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: previous_only_offset
        }),
        "the classic trailer's /Prev remains the continuation"
    );
    assert!(
        !loaded.entries.contains_key(&ObjectRef::new(4, 0)),
        "the hybrid stream's /Prev must not be followed"
    );
}

// `Pdf` deliberately has no `Debug`, so `Result::expect_err` is unavailable.
fn open_error(bytes: Vec<u8>, options: PdfOpenOptions, context: &str) -> Error {
    Pdf::open_mem_owned_with_options(bytes, options)
        .err()
        .unwrap_or_else(|| panic!("{context}"))
}

#[test]
fn ignore_xref_streams_reports_xref_not_found_at_the_stream_offset() {
    let (bytes, xref_stream_offset) = xref_stream_document(false);

    // Without the option the same document parses as a cross-reference stream.
    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes.clone())).expect("xref stream loads");
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);

    let err = open_error(
        bytes,
        ignore_xref_streams_options(false),
        "ignoring xref streams leaves no cross-reference at the offset",
    );
    assert_eq!(
        err.to_string(),
        format!("parse error at byte {xref_stream_offset}: xref not found")
    );
}

#[test]
fn ignore_xref_streams_falls_back_to_reconstruction() {
    let (bytes, xref_stream_offset) = xref_stream_document(true);

    let mut pdf = Pdf::open_mem_owned_with_options(bytes, ignore_xref_streams_options(true))
        .expect("reconstruction recovers the document");

    // qpdf 11.9.0 observed with `--ignore-xref-streams` on a document whose
    // startxref points at a cross-reference stream:
    //   WARNING: ...: file is damaged
    //   WARNING: ... (offset N): xref not found
    //   WARNING: ...: Attempting to reconstruct cross-reference table
    let diagnostics = pdf.repair_diagnostics();
    let entries = diagnostics.entries();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>(),
        [
            "file is damaged",
            "xref not found",
            "Attempting to reconstruct cross-reference table",
        ]
    );
    assert_eq!(
        entries.iter().map(|entry| entry.offset).collect::<Vec<_>>(),
        [None, Some(xref_stream_offset), None]
    );

    // The reconstruction pass, not the ignored cross-reference stream, supplied
    // the offsets: it finds `1 0 obj` by scanning the body.
    let root = pdf
        .root_ref()
        .expect("root reference recovered from trailer");
    assert_eq!(root, ObjectRef::new(1, 0));
    assert_eq!(
        pdf.resolve_object(root)
            .expect("resolve root")
            .as_dict()
            .and_then(|dict| dict.get("Type"))
            .and_then(|value| value.as_name()),
        Some(b"Catalog".as_slice())
    );
}

#[test]
fn ignore_xref_streams_finds_a_candidate_but_cannot_decode_it() {
    let (bytes, xref_stream_offset) = xref_stream_document(false);

    let err = open_error(
        bytes,
        ignore_xref_streams_options(true),
        "a document with no trailer keyword cannot be reconstructed",
    );
    let (source, diagnostics) = err.open_failure().expect("open failure diagnostics");

    // The warnings preceding the failure match qpdf 11.9.0 exactly, observed on
    // this document shape with `--ignore-xref-streams`.
    assert_eq!(
        diagnostics
            .entries()
            .iter()
            .map(|entry| (entry.message.as_str(), entry.offset))
            .collect::<Vec<_>>(),
        [
            ("file is damaged", None),
            ("xref not found", Some(xref_stream_offset)),
            ("Attempting to reconstruct cross-reference table", None),
        ]
    );

    // qpdf 11.9.0's `reconstruct_xref` (`QPDF.cc:577-608`) finds the
    // reconstructed `/Type /XRef` stream candidate (the line scan does not
    // consult `ignore_xref_streams`) and re-enters `read_xref` at its offset;
    // `read_xrefStream` then honors the option and refuses to read it, so the
    // terminal error is the candidate-decode failure rather than "no trailer".
    assert_eq!(
        source.to_string(),
        "parse error at byte 0: error decoding candidate xref stream while recovering damaged file"
    );
}

#[test]
fn candidate_recovery_warns_when_xref_size_is_not_one_plus_highest_object() {
    let (mut bytes, xref_stream_offset) = xref_stream_document(false);
    let size_token = b"/Size 3";
    let size_offset = bytes
        .windows(size_token.len())
        .position(|window| window == size_token)
        .expect("candidate xref stream has a /Size entry");
    bytes[size_offset + b"/Size ".len()] = b'4';

    let original_suffix = format!("startxref\n{xref_stream_offset}\n%%EOF\n");
    assert!(bytes.ends_with(original_suffix.as_bytes()));
    bytes.truncate(bytes.len() - original_suffix.len());
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("candidate xref-stream recovery should succeed");

    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    assert_eq!(loaded.startxref, xref_stream_offset);
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| {
            diagnostic.message
                == "reported number of objects (4) is not one plus the highest object number (2)"
                && diagnostic.offset.is_none()
        }));
}

#[test]
fn candidate_recovery_resolves_matching_indirect_xref_size() {
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(
        xref_stream_document_with_indirect_size(4),
    ))
    .expect("candidate xref-stream recovery should succeed");

    assert!(!loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| { diagnostic.message.contains("reported number of objects") }));
}

#[test]
fn candidate_recovery_warns_for_mismatching_indirect_xref_size() {
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(
        xref_stream_document_with_indirect_size(3),
    ))
    .expect("candidate xref-stream recovery should succeed");

    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| {
            diagnostic.message
                == "reported number of objects (3) is not one plus the highest object number (3)"
                && diagnostic.offset.is_none()
        }));
}

#[test]
fn candidate_recovery_does_not_duplicate_indirect_size_resolution_diagnostics() {
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(
        xref_stream_document_with_indirect_size_object(b"4\n"),
    ))
    .expect("candidate xref-stream recovery should succeed");
    assert_eq!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
            .count(),
        1,
        "the cached indirect /Size object warning must be emitted once"
    );
}

#[test]
fn normal_xref_warns_for_mismatching_indirect_xref_size() {
    let loaded = load_xref_and_trailer(&mut Cursor::new(classic_xref_document_with_indirect_size(
        3,
    )))
    .expect("normal xref loading should resolve an indirect trailer /Size");

    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| {
            diagnostic.message
                == "reported number of objects (3) is not one plus the highest object number (3)"
                && diagnostic.offset.is_none()
        }));
}

#[test]
fn normal_xref_accepts_matching_indirect_xref_size() {
    let loaded = load_xref_and_trailer(&mut Cursor::new(classic_xref_document_with_indirect_size(
        4,
    )))
    .expect("normal xref loading should resolve an indirect trailer /Size");

    assert!(!loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("reported number of objects")));
}

#[test]
fn repair_reconstructs_xref_after_an_indirect_size_header_mismatch() {
    let (bytes, size_offset) = classic_xref_with_indirect_size_header_mismatch(5);
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode should reconstruct the mismatched xref entry");

    assert_eq!(
        loaded.entries.get(&ObjectRef::new(3, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: size_offset,
        }),
        "qpdf reconstruction must replace the stale offset before /Size validation"
    );
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("expected 3 0 obj")));
}

#[test]
fn repair_revalidates_indirect_xref_size_after_header_reconstruction() {
    let (bytes, _) = classic_xref_with_indirect_size_header_mismatch(3);
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode should reconstruct and validate the xref");

    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| {
            diagnostic.message
                == "reported number of objects (3) is not one plus the highest object number (4)"
        }));
}

#[test]
fn repair_forwards_size_resolution_diagnostics_after_header_reconstruction() {
    let (bytes, _) = classic_xref_with_indirect_size_header_mismatch_missing_endobj(3);
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("repair mode should retain the recovered /Size diagnostic");

    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("expected endobj")));
}

#[test]
fn candidate_recovery_can_be_full_rewritten_without_prev() {
    // qpdf's writer always emits a fresh document. After a corrupt `startxref`
    // is recovered via the xref-stream-candidate fallback, the canonical
    // writer must therefore produce a valid output without carrying the
    // repaired source's incremental history into the new trailer.
    let (mut bytes, xref_stream_offset) = xref_stream_document(false);
    let original_suffix = format!("startxref\n{xref_stream_offset}\n%%EOF\n");
    assert!(bytes.ends_with(original_suffix.as_bytes()));
    bytes.truncate(bytes.len() - original_suffix.len());
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");

    let mut pdf =
        Pdf::open_with_repair(Cursor::new(bytes)).expect("candidate recovery recovers the trailer");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("memory output");
    writer.write().expect("qpdf full rewrite succeeds");
    let out = writer.get_buffer().expect("writer buffer");
    let out_str = String::from_utf8_lossy(&out);

    assert!(
        !out_str.contains("/Prev"),
        "qpdf full rewrite must not carry incremental history, got:\n{out_str}"
    );

    Pdf::open_mem_owned(out).expect("the full-rewrite output must be strictly reopenable");
}

#[test]
fn ignore_xref_streams_applies_to_previous_xref_sections() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let previous_xref_offset = bytes.len() as u64;
    let previous_entries = build_encoded_xref_stream_entries(&[
        (0, 0, 0),
        (1, obj1_offset, 0),
        (1, previous_xref_offset, 0),
    ]);
    bytes.extend_from_slice(&make_xref_stream_object(2, 3, None, 1, &previous_entries));

    // The newest section is a classic table, so only the `/Prev` hop reaches the
    // cross-reference stream reader.
    let table_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 3 /Root 1 0 R /Prev {previous_xref_offset} >>\n\
             startxref\n{table_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let loaded =
        load_xref_and_trailer(&mut Cursor::new(bytes.clone())).expect("both sections load");
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Uncompressed {
            offset: previous_xref_offset
        }),
        "the `/Prev` cross-reference stream contributes entry 2 0"
    );

    let err = open_error(
        bytes,
        ignore_xref_streams_options(false),
        "the `/Prev` hop must honour the option too",
    );
    assert_eq!(
        err.to_string(),
        format!("parse error at byte {previous_xref_offset}: xref not found")
    );
}

#[test]
fn ignore_xref_streams_precedes_the_end_of_file_offset_check() {
    // qpdf's read_xrefStream reads nothing at the offset when the option is set,
    // so a startxref past the end of the file still reports "xref not found"
    // rather than the offset diagnostic the stream reader would produce.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let past_eof = bytes.len() as u64 + 4096;
    bytes.extend_from_slice(format!("startxref\n{past_eof}\n%%EOF\n").as_bytes());

    let err = load_xref_and_trailer(&mut Cursor::new(bytes.clone()))
        .expect_err("startxref past EOF fails without the option");
    assert_eq!(
        err.to_string(),
        format!("parse error at byte {past_eof}: xref stream offset is beyond end of file")
    );

    let err = open_error(
        bytes,
        ignore_xref_streams_options(false),
        "startxref past EOF fails with the option",
    );
    assert_eq!(
        err.to_string(),
        format!("parse error at byte {past_eof}: xref not found")
    );
}

#[test]
fn ignore_xref_streams_leaves_classic_xref_tables_untouched() {
    let bytes = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();

    let mut pdf = Pdf::open_mem_owned_with_options(bytes, ignore_xref_streams_options(true))
        .expect("a classic cross-reference table is unaffected");
    assert!(
        pdf.repair_diagnostics().entries().is_empty(),
        "no reconstruction is triggered for a classic table"
    );
    let root = pdf.root_ref().expect("root reference");
    assert!(pdf
        .resolve_object(root)
        .expect("resolve root")
        .as_dict()
        .is_some());
}

// The "succeeded but with accumulated parse errors" warning path in
// `load_xref_and_trailer_with_repair` is exercised by
// `with_repair_appends_diagnostic_when_stream_parse_succeeds`.
//
// Unreachable arms via the public API (documented, not tested):
//
// * `ByteCursor::read_be_u64`'s own `pos + width > len` end-of-stream guard is
//   shadowed in the xref-stream path: `parse_xref_entries` checks
//   `cursor.pos + entry_width > len` (full entry width) BEFORE any
//   `read_be_u64` call, and the per-field reads sum to exactly `entry_width`.
//   Truncated stream data therefore surfaces as "xref stream data truncated"
//   (see `rejects_xref_stream_truncated_data`), and `read_be_u64`'s guard is
//   never the one that fires through `load_xref_and_trailer`.
//
// * The empty-`parse_errors` (`first()` returning `None`) arm of
//   `push_repair_diagnostics`: every call site passes a non-empty
//   `parse_errors`. Each call is either preceded by a push onto `parse_errors`,
//   or guarded by `!parse_errors.is_empty()`, so the slice is never empty at a
//   call site.
//
// * The `startxref` `usize::try_from` overflow arm of
//   `load_xref_and_trailer_with_repair` (both the repair and strict variants) is
//   unreachable on 64-bit targets, where `usize::try_from(u64)` cannot overflow.
//
// * The `/Prev` `usize::try_from` overflow arm of
//   `merge_previous_xref_sections` is unreachable on 64-bit targets, where
//   `usize::try_from(u64)` cannot overflow.
//
