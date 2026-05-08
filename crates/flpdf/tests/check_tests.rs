use flpdf::{check_reader, Severity};
use std::fs::File;
use std::io::BufReader;

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
