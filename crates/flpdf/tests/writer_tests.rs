use flpdf::{check_reader, write_pdf, write_qdf, Object, ObjectRef, Pdf};
use std::collections::BTreeMap;
use std::fs::File;
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

    let xref = parse_last_xref_entries(&output);
    assert_eq!(xref.get(&0), Some(&b'f'));
    assert_eq!(xref.get(&1), Some(&b'n'));
    assert_eq!(xref.get(&3), Some(&b'n'));
    assert!(!xref.contains_key(&2));
    assert!(!xref.contains_key(&4));
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

fn parse_last_xref_entries(bytes: &[u8]) -> BTreeMap<u32, u8> {
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

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let bytes_u24 = value.to_be_bytes();
    bytes.extend_from_slice(&bytes_u24[1..]);
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
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
