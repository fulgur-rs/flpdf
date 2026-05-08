use flpdf::{check_reader, write_pdf, Object, ObjectRef, Pdf};
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
