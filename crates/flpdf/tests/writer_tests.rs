use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::{
    check_reader, filters, load_xref_and_trailer, parse_object, write_pdf, write_qdf, Object,
    ObjectRef, Pdf, XrefForm, XrefOffset,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::io::{BufReader, Cursor};

#[test]
fn rewrites_minimal_pdf_to_valid_pdf() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn write_pdf_preserves_source_bytes() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Marker (SENTINEL-UNTOUCHED) >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    assert!(output.len() > source.len());
    assert_eq!(&output[..source.len()], &source[..]);
    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains(&format!("/Prev {}", startxref)));
}

#[test]
fn write_pdf_twice_builds_valid_prev_chain() {
    let source_bytes = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let source_startxref = parse_startxref(&source_bytes);
    let mut generations = Vec::new();

    let mut source = source_bytes;

    for _ in 0..3 {
        let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
        let mut output = Vec::new();
        write_pdf(&mut pdf, &mut output).unwrap();

        let report = check_reader(Cursor::new(&output)).unwrap();
        assert!(
            report.valid,
            "generation {} diagnostics: {:?}",
            generations.len(),
            report.diagnostics.entries()
        );

        generations.push(output.clone());

        source = output.clone();
    }

    let mut prior_size = None;
    let mut prior_startxref = source_startxref;
    let mut prior_generations = None;

    for (generation, bytes) in generations.iter().enumerate() {
        let current_startxref = parse_startxref(bytes);
        let parsed = Pdf::open(Cursor::new(bytes)).unwrap();
        let current_generations = parse_last_xref_generations(bytes);
        let trailer = parsed.trailer();

        let prev = trailer.get("Prev");
        let Some(Object::Integer(previous_xref)) = prev else {
            panic!("generation {generation} is missing trailer /Prev")
        };

        assert_eq!(
            *previous_xref as u64, prior_startxref,
            "generation {generation} /Prev mismatch"
        );

        let size = trailer
            .get("Size")
            .and_then(as_integer)
            .expect("generation trailer missing integer /Size");
        if let Some(previous_size) = prior_size {
            assert!(
                size >= previous_size,
                "generation {generation} reduced /Size from {previous_size} to {size}",
            );
        }

        prior_size = Some(size);
        prior_startxref = current_startxref;

        if let Some(previous_generations) = prior_generations.as_ref() {
            for (object_number, previous_generation) in previous_generations {
                if let Some(current_generation) = current_generations.get(object_number) {
                    assert!(
                        current_generation >= previous_generation,
                        "generation {object_number} decreased from {previous_generation} to {current_generation} in generation {generation}",
                    );
                }
            }
        }

        prior_generations = Some(current_generations);
    }
}

#[test]
fn write_pdf_rewriting_chain_is_self_consistent_on_open() {
    let source = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let mut snapshots = Vec::new();
    let mut current = source;

    for _ in 0..2 {
        let mut pdf = Pdf::open(Cursor::new(current.clone())).unwrap();
        let mut output = Vec::new();
        write_pdf(&mut pdf, &mut output).unwrap();
        snapshots.push(output.clone());
        current = output;
    }

    let mut chain = Vec::<u64>::new();

    for generation in (1..snapshots.len()).rev() {
        let current = &snapshots[generation];
        let previous = &snapshots[generation - 1];

        let pdf = Pdf::open(Cursor::new(current)).unwrap();
        let prev = pdf
            .trailer()
            .get("Prev")
            .and_then(as_integer)
            .expect("trailer missing /Prev") as u64;

        chain.push(prev);
        assert_eq!(prev, parse_startxref(previous));

        let report = check_reader(Cursor::new(current)).unwrap();
        assert!(
            report.valid,
            "rewritten generation {generation} diagnostics: {:?}",
            report.diagnostics.entries()
        );
    }

    assert!(
        !chain.is_empty(),
        "expected /Prev values while validating rewritten chain",
    );
}

#[test]
fn write_pdf_emits_only_touched_objects() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let _ = pdf.resolve(ObjectRef::new(1, 0)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    assert!(output.len() > source.len());
    assert_eq!(&output[..source.len()], &source[..]);
    assert_eq!(count_substrings(&output, b"1 0 obj"), 2);
    assert_eq!(count_substrings(&output, b"2 0 obj"), 1);
}

#[test]
fn write_pdf_omits_unmapped_compressed_object_refs_from_xref() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let mut objects = Vec::new();
    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut objects,
    );

    let object_stream_body = b"2 0 7";
    add_object(
        format!(
            "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
            object_stream_body.len()
        )
        .as_bytes(),
        &mut bytes,
        &mut objects,
    );
    bytes.extend_from_slice(object_stream_body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[1] as u32, 0);

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();
    let xref = parse_last_xref_entries(&output);
    assert_eq!(xref.get(&0), Some(&b'f'));
    assert_eq!(xref.get(&1), Some(&b'n'));
    assert_eq!(xref.get(&3), Some(&b'n'));
    if matches!(loaded.last_xref_form, XrefForm::Stream) {
        assert_eq!(xref.get(&2), Some(&b'n'));
    } else {
        assert!(!xref.contains_key(&2));
    }
    assert_eq!(xref.get(&4), Some(&b'n'));
}

#[test]
fn write_pdf_incremental_trailer_strips_xref_stream_only_keys() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let mut objects = Vec::new();
    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut objects,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut objects,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[1] as u32, 0);

    let xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, xref_offset as u32, 0);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&xref_entries).unwrap();
    let compressed_xref = encoder.finish().unwrap();

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} /Filter /FlateDecode /F 10 /FFilter /FlateDecode /FDecodeParms <<>> /XRefStm 123 >>\nstream\n",
            compressed_xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed_xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let trailer_section = parse_last_trailer_section(&output);
    assert!(!trailer_section.contains(" /Type /XRef"));
    assert!(!trailer_section.contains(" /W ["));
    assert!(!trailer_section.contains(" /Index ["));
    assert!(!trailer_section.contains(" /Length "));
    assert!(!trailer_section.contains(" /XRefStm "));
}

#[test]
fn write_pdf_rewrites_xref_stream_input_as_xref_stream_output() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let root_ref = pdf.root_ref().expect("expected /Root");
    let Object::Dictionary(mut root_dict) = pdf.resolve(root_ref).unwrap() else {
        panic!("root should be a dictionary");
    };
    root_dict.insert("FlpdfRegression", Object::Boolean(true));
    pdf.set_object(root_ref, Object::Dictionary(root_dict));

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    let expected_prev = i64::try_from(source_xref_offset).unwrap();
    let actual_prev = as_integer(
        loaded
            .trailer
            .get("Prev")
            .expect("expected /Prev in xref stream"),
    );
    assert_eq!(actual_prev, Some(expected_prev));
    assert_eq!(&output[..source.len()], &source[..]);
}

#[test]
fn write_pdf_uses_non_colliding_xref_stream_object_number_for_new_objects() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 255);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    pdf.set_object(
        ObjectRef::new(4, 0),
        parse_object(b"<< /Generated true >>").unwrap(),
    );

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert!(loaded.entries.contains_key(&ObjectRef::new(4, 0)));
    assert!(loaded.entries.contains_key(&ObjectRef::new(5, 0)));
}

#[test]
fn write_pdf_preserves_xref_stream_trailer_metadata_and_declared_size() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"6 0 obj\n<< /Producer (flpdf-test) >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 255);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);
    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[2] as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 10 /Root 1 0 R /Info 6 0 R /ID [<0011> <2233>] /W [1 3 1] /Index [0 4 6 1] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(as_integer(loaded.trailer.get("Size").unwrap()), Some(10));
    assert_eq!(loaded.trailer.get_ref("Info"), Some(ObjectRef::new(6, 0)));
    assert!(loaded.trailer.get("ID").is_some());
}

#[test]
fn write_pdf_preserves_large_compressed_xref_stream_index() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"4 0 obj\n<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let large_index = 70_000;
    let mut xref_entries = Vec::new();
    append_xref_stream_entry_w4(&mut xref_entries, 0, 0, 65535);
    append_xref_stream_entry_w4(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry_w4(&mut xref_entries, 2, 4, large_index);
    append_xref_stream_entry_w4(&mut xref_entries, 1, object_offsets[1] as u32, 0);
    append_xref_stream_entry_w4(&mut xref_entries, 1, object_offsets[2] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry_w4(&mut xref_entries, 1, source_xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 4 4] /Index [0 6] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Compressed {
            stream: 4,
            index: large_index
        })
    );
}

#[test]
fn write_pdf_preserves_xref_stream_free_tombstones() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Deleted false >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut first_xref_entries = Vec::new();
    append_xref_stream_entry(&mut first_xref_entries, 0, 0, 255);
    append_xref_stream_entry(&mut first_xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut first_xref_entries, 1, object_offsets[1] as u32, 0);
    append_xref_stream_entry(&mut first_xref_entries, 1, object_offsets[2] as u32, 0);

    let first_xref_offset = bytes.len();
    append_xref_stream_entry(&mut first_xref_entries, 1, first_xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
            first_xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&first_xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut tombstone_xref_entries = Vec::new();
    append_xref_stream_entry(&mut tombstone_xref_entries, 0, 0, 1);
    let tombstone_xref_offset = bytes.len();
    append_xref_stream_entry(
        &mut tombstone_xref_entries,
        1,
        tombstone_xref_offset as u32,
        0,
    );
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 3 1] /Index [2 1 5 1] /Length {} /Prev {} >>\nstream\n",
            tombstone_xref_entries.len(),
            first_xref_offset,
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&tombstone_xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{tombstone_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let latest_entries = parse_last_xref_entries(&output);
    assert_eq!(latest_entries.get(&2), Some(&b'f'));
}

#[test]
fn write_pdf_rewrites_flate_object_stream_member_and_recomputes_first() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let member_2_source =
        parse_object(format!("<< /Type /Packed /Payload ({}) >>", "A".repeat(400)).as_bytes())
            .unwrap();
    let member_2_rewritten =
        parse_object(format!("<< /Type /Packed /Payload ({}) >>", "A".repeat(401)).as_bytes())
            .unwrap();
    let member_3_source =
        parse_object(format!("<< /Type /Packed /Payload ({}) >>", "B".repeat(420)).as_bytes())
            .unwrap();

    let mut member2 = Vec::new();
    member_2_source.write_pdf(&mut member2);
    let mut member3 = Vec::new();
    member_3_source.write_pdf(&mut member3);
    let (stream_data, first) = build_flate_objstm_payload(&[(2, &member2[..]), (3, &member3[..])]);
    let obj_stream_offset = bytes.len();

    let obj_stream = format!(
        "4 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
        first,
        stream_data.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&obj_stream);
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 1, obj_stream_offset as u32, 0);

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.resolve(ObjectRef::new(2, 0)).unwrap(), member_2_source);
    pdf.set_object(ObjectRef::new(2, 0), member_2_rewritten.clone());

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    let Object::Stream(rewritten_obj_stream) = rewritten.resolve(ObjectRef::new(4, 0)).unwrap()
    else {
        panic!("expected object stream object");
    };

    let decoded =
        filters::decode_stream_data(&rewritten_obj_stream.dict, &rewritten_obj_stream.data)
            .unwrap();
    let parsed_first = as_integer(
        rewritten_obj_stream
            .dict
            .get("First")
            .expect("object stream missing /First"),
    )
    .expect("object stream /First should be integer");
    let parsed_first = usize::try_from(parsed_first).unwrap();
    assert!(
        parsed_first <= decoded.len(),
        "/First must fit in decoded stream"
    );
    assert_eq!(
        parsed_first,
        parse_objstm_first_header_len(&decoded, parsed_first).len()
    );

    let members = parse_objstm_members(&decoded, parsed_first);
    assert_eq!(members[0].0, 2);
    assert_eq!(members[1].0, 3);

    let first_member = parse_objstm_member(&decoded, parsed_first, &members, 0);
    let second_member = parse_objstm_member(&decoded, parsed_first, &members, 1);
    assert_eq!(first_member, member_2_rewritten);
    assert_eq!(second_member, member_3_source);
}

#[test]
fn write_pdf_rewrites_null_object_revision() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    let mut object_offsets = Vec::new();
    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(b"2 0 obj\nnull\nendobj\n", &mut bytes, &mut object_offsets);

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n\n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let null_object = pdf.resolve(ObjectRef::new(2, 0)).unwrap();
    assert_eq!(null_object, Object::Null);

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    assert_eq!(count_substrings(&output, b"2 0 obj"), 2);
}

#[test]
fn rewrites_pdf_with_real_numbers() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");

    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Contents 4 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");

    for offset in object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n\n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<<\n  /Size {}\n  /Root 1 0 R\n>>\nstartxref\n{xref_offset}\n%%EOF\n",
            4 + 1,
        )
        .as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn rewrites_pdf_with_real_number_fixture() {
    let file = File::open("../../tests/fixtures/real-numbers-regression.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let page = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Dictionary(page_dict) = page else {
        panic!("expected page dictionary")
    };
    assert_eq!(
        page_dict.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(1000.0),
            Object::Real(0.75),
        ]))
    );
    assert_eq!(
        page_dict.get("TrimBox"),
        Some(&Object::Array(vec![
            Object::Real(1.0),
            Object::Real(-0.25),
            Object::Real(0.25),
            Object::Real(-1.5),
        ]))
    );

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn rewrites_linearized_pdf_preserving_hint_object() {
    let input = linearized_fixture_pdf();
    let mut pdf = Pdf::open(Cursor::new(input)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let output_text = String::from_utf8_lossy(&output);
    assert!(output_text.contains(" /Linearized "));
    assert!(output_text.contains("1 0 obj"));

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn rewrites_repaired_pdf_in_best_effort_mode() {
    let input = corrupt_xref_pdf();
    assert!(Pdf::open(Cursor::new(input.clone())).is_err());

    let mut pdf = Pdf::open_best_effort(Cursor::new(input)).unwrap();
    assert!(!pdf.repair_diagnostics().entries().is_empty());
    assert_eq!(pdf.version(), "1.7");

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn writes_qdf_with_object_generations() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 3 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Orphan >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", 4).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    bytes.extend_from_slice(format!("{:010} 00003 n\n", offsets[0]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n\n", offsets[1]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n\n", offsets[2]).as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 3 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("1 3 obj"));
    assert!(rendered.contains("3 0 obj"));
    assert!(rendered.contains(" 00003 n"));
}

fn count_substrings(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }

    let mut count = 0;
    let mut start = 0;
    while let Some(position) = haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        count += 1;
        start += position + needle.len();
    }

    count
}

fn parse_startxref(bytes: &[u8]) -> u64 {
    let marker = b"startxref";
    let eof = bytes
        .windows(b"%%EOF".len())
        .rposition(|window| window == b"%%EOF")
        .unwrap_or(bytes.len());
    let search = &bytes[..eof];

    let Some(pos) = search
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        panic!("missing startxref marker")
    };

    let mut cursor = pos + marker.len();
    while cursor < search.len() && search[cursor].is_ascii_whitespace() {
        cursor += 1;
    }

    let start = cursor;
    while cursor < search.len() && search[cursor].is_ascii_digit() {
        cursor += 1;
    }

    if start == cursor {
        panic!("missing startxref offset")
    }

    let value = std::str::from_utf8(&search[start..cursor]).unwrap();
    value.parse::<u64>().unwrap()
}

fn parse_last_xref_generations(bytes: &[u8]) -> BTreeMap<u32, u16> {
    let rendered = String::from_utf8_lossy(bytes);
    let xref_pos = if let Some(pos) = rendered.rfind("\nxref\n") {
        pos
    } else if let Some(pos) = rendered.rfind("xref\n") {
        pos.saturating_sub(1)
    } else {
        panic!("missing xref section");
    };

    let section_pos = if &rendered[xref_pos + 1..xref_pos + 6] == "xref\n" {
        xref_pos + 1
    } else {
        panic!("missing xref section");
    };

    let mut lines = rendered[section_pos + 5..].lines();
    let mut generations = BTreeMap::new();

    while let Some(section) = lines.next() {
        if section == "trailer" {
            break;
        }

        let mut fields = section.split_whitespace();
        let start: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("invalid xref subsection header: {section}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref start token {section}"));
        let count: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("missing xref count for start {start}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref count token {section}"));

        for index in 0..count {
            let entry_line = lines
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let mut entry_fields = entry_line.split_whitespace();

            let _ = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let generation: u16 = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing generation in xref subsection {start} {count}"))
                .parse()
                .unwrap_or_else(|_| {
                    panic!("invalid generation in xref subsection {start} {count}")
                });

            generations.insert(start + index, generation);
        }
    }

    generations
}

fn parse_last_trailer_section(bytes: &[u8]) -> String {
    let rendered = String::from_utf8_lossy(bytes);
    let trailer_pos = rendered
        .rfind("trailer")
        .unwrap_or_else(|| panic!("missing trailer"));
    let trailer_start = rendered[trailer_pos..]
        .find("<<")
        .map(|offset| trailer_pos + offset)
        .unwrap_or_else(|| panic!("missing trailer dictionary"));
    let startxref_pos = rendered[trailer_pos..]
        .find("startxref")
        .map(|offset| trailer_pos + offset)
        .unwrap_or_else(|| panic!("missing startxref"));

    rendered[trailer_start..startxref_pos].to_string()
}

fn as_integer(object: &Object) -> Option<i64> {
    match object {
        Object::Integer(value) => Some(*value),
        _ => None,
    }
}

fn parse_last_xref_entries(bytes: &[u8]) -> BTreeMap<u32, u8> {
    let startxref = usize::try_from(parse_startxref(bytes)).unwrap();
    if bytes[startxref..].starts_with(b"xref") {
        return parse_last_xref_table_entries(bytes, startxref);
    }

    let mut cursor = Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut cursor)
        .expect("output should include a valid xref stream dictionary");
    let stream_data = latest_xref_stream_data(bytes, startxref);
    let (w0, w1, w2) = xref_stream_widths(&loaded.trailer);
    let ranges = xref_stream_ranges(&loaded.trailer);
    let entry_width = w0 + w1 + w2;
    let mut entries = BTreeMap::new();

    let mut pos = 0;
    for (start, count) in ranges {
        for index in 0..count {
            let entry = &stream_data[pos..pos + entry_width];
            let object_type = if w0 == 0 {
                1
            } else {
                read_be_usize(&entry[..w0])
            };

            entries.insert(
                start + index,
                match object_type {
                    0 => b'f',
                    1 | 2 => b'n',
                    other => panic!("unsupported xref stream entry type {other}"),
                },
            );
            pos += entry_width;
        }
    }

    entries
}

fn parse_last_xref_table_entries(bytes: &[u8], startxref: usize) -> BTreeMap<u32, u8> {
    let rendered = String::from_utf8_lossy(&bytes[startxref..]);
    let mut lines = rendered[5..].lines();
    let mut entries = BTreeMap::new();

    while let Some(section) = lines.next() {
        if section == "trailer" {
            break;
        }

        let mut fields = section.split_whitespace();
        let start: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("invalid xref subsection header: {section}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref start token {section}"));
        let count: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("missing xref count for start {start}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref count token {section}"));

        for index in 0..count {
            let entry_line = lines
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let mut entry_fields = entry_line.split_whitespace();
            let _ = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let _ = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing generation in xref subsection {start} {count}"));
            let status = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing status in xref subsection {start} {count}"));

            entries.insert(start + index, status.as_bytes()[0]);
        }
    }

    entries
}

fn latest_xref_stream_data(bytes: &[u8], startxref: usize) -> &[u8] {
    let tail = &bytes[startxref..];
    let stream_marker = b"stream";
    let stream_pos = tail
        .windows(stream_marker.len())
        .position(|window| window == stream_marker)
        .unwrap_or_else(|| panic!("missing xref stream data"));
    let mut data_start = startxref + stream_pos + stream_marker.len();
    if bytes.get(data_start) == Some(&b'\r') {
        data_start += 1;
    }
    if bytes.get(data_start) == Some(&b'\n') {
        data_start += 1;
    }
    let end_marker = b"endstream";
    let data_end = bytes[data_start..]
        .windows(end_marker.len())
        .position(|window| window == end_marker)
        .map(|offset| data_start + offset)
        .unwrap_or_else(|| panic!("missing xref endstream"));
    let mut data_end = data_end;
    while data_end > data_start && bytes[data_end - 1].is_ascii_whitespace() {
        data_end -= 1;
    }

    &bytes[data_start..data_end]
}

fn xref_stream_widths(trailer: &flpdf::Dictionary) -> (usize, usize, usize) {
    let Some(Object::Array(widths)) = trailer.get("W") else {
        panic!("xref stream missing /W")
    };
    assert_eq!(widths.len(), 3);
    (
        as_integer(&widths[0]).unwrap() as usize,
        as_integer(&widths[1]).unwrap() as usize,
        as_integer(&widths[2]).unwrap() as usize,
    )
}

fn xref_stream_ranges(trailer: &flpdf::Dictionary) -> Vec<(u32, u32)> {
    if let Some(Object::Array(index)) = trailer.get("Index") {
        return index
            .chunks_exact(2)
            .map(|chunk| {
                (
                    as_integer(&chunk[0]).unwrap() as u32,
                    as_integer(&chunk[1]).unwrap() as u32,
                )
            })
            .collect();
    }

    let size = as_integer(trailer.get("Size").expect("xref stream missing /Size")).unwrap() as u32;
    vec![(0, size)]
}

fn read_be_usize(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0usize, |value, byte| (value << 8) | usize::from(*byte))
}

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let bytes_u24 = value.to_be_bytes();
    bytes.extend_from_slice(&bytes_u24[1..]);
}

fn build_flate_objstm_payload(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    let mut header = String::new();
    let mut body = Vec::new();

    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{} {} ", number, offset));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }

    let mut decoded = Vec::new();
    decoded.extend_from_slice(header.as_bytes());
    decoded.extend_from_slice(&body);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&decoded).unwrap();
    let encoded = encoder.finish().unwrap();

    (encoded, header.len())
}

fn parse_objstm_first_header_len(decoded: &[u8], declared_first: usize) -> &[u8] {
    &decoded[..declared_first]
}

fn parse_objstm_members(decoded: &[u8], first: usize) -> Vec<(u32, usize)> {
    let header = std::str::from_utf8(&decoded[..first]).unwrap();
    let tokens: Vec<&str> = header.split_whitespace().map(str::trim).collect();
    let mut members = Vec::new();

    for pair in tokens.chunks(2) {
        assert_eq!(pair.len(), 2);
        let number = pair[0].parse().unwrap();
        let offset = pair[1].parse().unwrap();
        members.push((number, offset));
    }

    members
}

fn parse_objstm_member(
    decoded: &[u8],
    first: usize,
    members: &[(u32, usize)],
    index: usize,
) -> Object {
    let (number, start) = members[index];
    let start = first + start;
    let end = if index + 1 < members.len() {
        first + members[index + 1].1
    } else {
        decoded.len()
    };
    let _ = number;
    parse_object(trim_right_ws(&decoded[start..end])).unwrap()
}

fn trim_right_ws(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |idx| idx + 1);
    &bytes[..end]
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}

fn append_xref_stream_entry_w4(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u32) {
    entries.push(entry_type);
    entries.extend_from_slice(&field1.to_be_bytes());
    entries.extend_from_slice(&field2.to_be_bytes());
}

fn linearized_fixture_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Linearized 1 /L 100 /E 0 /N 1 /T 1 >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 1 /Kids [4 0 R] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 595.28 841.89] /Contents 5 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"5 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    let object_count = offsets.len() + 1;
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 2 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            object_count
        )
        .as_bytes(),
    );

    bytes
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

    let mut corrupted = bytes;
    let Some(pos) = corrupted.windows(4).position(|window| window == b"xref") else {
        unreachable!("fixture should contain xref token")
    };
    if let Some(byte) = corrupted.get_mut(pos + 2) {
        *byte = b'z';
    }
    corrupted
}
