use flpdf::{check_reader, check_reader_strict, Severity};
use std::fs::File;
use std::io::{BufReader, Cursor};

#[test]
fn check_reports_valid_minimal_pdf() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let report = check_reader(BufReader::new(file)).unwrap();
    assert!(report.valid);
    assert_eq!(report.diagnostics.entries().len(), 0);
}

#[test]
fn check_reports_missing_header() {
    let input = std::io::Cursor::new(b"not a pdf".to_vec());
    let report = check_reader(input).unwrap();
    assert!(!report.valid);
    assert!(report
        .diagnostics
        .entries()
        .iter()
        .any(|entry| entry.severity == Severity::Error));
}

#[test]
fn check_reports_linearized_pdf_warning() {
    let input = linearized_fixture_pdf();
    let report = check_reader(Cursor::new(input)).unwrap();

    assert!(report.valid);
    assert!(report
        .diagnostics
        .entries()
        .iter()
        .any(|entry| entry.severity == Severity::Warning && entry.message.contains("linearized")));
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

#[test]
fn strict_check_propagates_corrupt_xref_as_error() {
    let input = corrupt_xref_pdf();
    let result = check_reader_strict(Cursor::new(input));
    assert!(result.is_err(), "strict variant should not repair the xref");
}

#[test]
fn strict_check_succeeds_on_clean_pdf() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let report = check_reader_strict(BufReader::new(file)).unwrap();
    assert!(report.valid);
    assert_eq!(report.diagnostics.entries().len(), 0);
}

#[test]
fn check_reports_repaired_xref_warning() {
    let input = corrupt_xref_pdf();
    let report = check_reader(Cursor::new(input)).unwrap();

    assert!(report.valid);
    assert!(report
        .diagnostics
        .entries()
        .iter()
        .any(|entry| entry.severity == Severity::Warning
            && entry.message.contains("repaired by linear object scan")));
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
