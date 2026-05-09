use flpdf::{check_reader, Severity};
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
