use flpdf::{
    check_reader, check_reader_strict, check_reader_with_options, EncryptedError, Error,
    PdfOpenOptions, Severity,
};
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
fn check_rejects_a_missing_root_at_the_input_boundary() {
    let input =
        b"%PDF-1.4\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\nstartxref\n9\n%%EOF\n";

    assert!(matches!(
        check_reader(Cursor::new(input.to_vec())),
        Err(Error::System(message)) if message == "unable to find /Root dictionary"
    ));
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
fn check_preserves_repair_warnings_before_terminal_open_error() {
    let input = include_bytes!("../../../tests/fixtures/test_driver/open_repair_failure.pdf");
    let report = check_reader(Cursor::new(input)).unwrap();

    assert!(!report.valid);
    assert!(report.summary.is_none());
    let diagnostics = report.diagnostics.entries();
    assert_eq!(diagnostics.len(), 4);
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(diagnostics[0].message, "file is damaged");
    assert_eq!(diagnostics[1].severity, Severity::Warning);
    assert_eq!(diagnostics[1].message, "can't find startxref");
    assert_eq!(diagnostics[2].severity, Severity::Warning);
    assert_eq!(
        diagnostics[2].message,
        "Attempting to reconstruct cross-reference table"
    );
    assert_eq!(diagnostics[3].severity, Severity::Error);
    assert_eq!(
        diagnostics[3].message,
        "parse error at byte 0: unable to find trailer dictionary while recovering damaged file"
    );
}

#[test]
fn check_reports_linearized_pdf_without_warning() {
    let input = include_bytes!("../../../tests/fixtures/compat/linearized-one-page.pdf");
    let report = check_reader(Cursor::new(input)).unwrap();

    assert!(report.valid);
    assert!(
        report.diagnostics.entries().is_empty(),
        "qpdf-clean fixture produced diagnostics: {:?}",
        report.diagnostics.entries()
    );
    // The repository fixture is qpdf-clean and its linearization parameter
    // dictionary is not object 1, covering the faithful detector route.
    assert!(report.summary.expect("summary present").linearized);
}

#[test]
fn check_weak_crypto_warning_is_library_scoped() {
    let report = check_reader_with_options(
        Cursor::new(encrypted_v1_owner_password_fixture()),
        PdfOpenOptions {
            password: b"owner".to_vec(),
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    let warning = report
        .diagnostics
        .entries()
        .iter()
        .find(|entry| entry.severity == Severity::Warning && entry.message.contains("weak crypto"))
        .expect("weak crypto warning should be reported");

    assert!(!warning.message.contains("--allow-weak-crypto"));
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
            && entry.message == "Attempting to reconstruct cross-reference table"));
}

#[test]
fn check_preserves_repair_warnings_when_the_root_gate_fails() {
    // The xref is corrupted the same way as `corrupt_xref_pdf`, forcing
    // reconstruction (which accumulates repair diagnostics on the
    // successfully-opened `Pdf`), but /Root points at a nonexistent object.
    // `pdf.root_handle()` therefore fails *after* a successful open, so this
    // exercises the root-gate error path distinctly from
    // `check_preserves_repair_warnings_before_terminal_open_error`, whose
    // fixture fails to open at all.
    let input = corrupt_xref_dangling_root_pdf();
    let error = check_reader(Cursor::new(input)).expect_err("dangling /Root must be terminal");

    let (source, diagnostics) = error
        .open_failure()
        .expect("repair diagnostics must survive the root-gate error");
    assert!(
        matches!(source, Error::System(message) if message == "unable to find /Root dictionary")
    );
    assert!(diagnostics
        .entries()
        .iter()
        .any(|entry| entry.severity == Severity::Warning
            && entry.message == "Attempting to reconstruct cross-reference table"));
}

#[test]
fn check_preserves_encrypted_classification_after_repair_warnings() {
    let mut input = encrypted_v1_owner_password_fixture();
    let xref = input
        .windows(4)
        .position(|window| window == b"xref")
        .expect("encrypted fixture should contain an xref keyword");
    input[xref + 2] = b'X';

    let error = check_reader_with_options(
        Cursor::new(input),
        PdfOpenOptions {
            repair: true,
            password: b"wrong".to_vec(),
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect_err("authentication failure must remain a hard check error");
    let (source, diagnostics) = error
        .open_failure()
        .expect("repair warnings must remain attached to the terminal error");
    assert!(matches!(
        source,
        Error::Encrypted(EncryptedError::BadPassword)
    ));
    assert!(!diagnostics.entries().is_empty());
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

/// `corrupt_xref_pdf`'s reconstruction path, but /Root points at a
/// nonexistent object instead of the real catalog, so the open succeeds
/// (with repair diagnostics) while `Pdf::root_handle` still fails.
fn corrupt_xref_dangling_root_pdf() -> Vec<u8> {
    let mut bytes = corrupt_xref_pdf();
    let root_ref = b"/Root 1 0 R";
    let pos = bytes
        .windows(root_ref.len())
        .position(|window| window == root_ref)
        .expect("fixture should contain /Root 1 0 R");
    bytes.splice(pos..pos + root_ref.len(), b"/Root 99 0 R".iter().copied());
    bytes
}

fn encrypted_v1_owner_password_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let xref_offset = bytes.len();
    let trailer = b"trailer\n<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 /P -3904 /O <94e8094419662a774442fb072e3d9f19e9d130ec09a4d0061e78fe920f7ab62f> /U <13f520c882d052bf57b416b747c13979bded7ea31240fe41928852aca3894c49> >> /ID [<000102030405060708090a0b0c0d0e0f><000102030405060708090a0b0c0d0e0f>] >>\nstartxref\n";
    bytes.extend_from_slice(format!("xref\n0 3\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(trailer);
    bytes.extend_from_slice(xref_offset.to_string().as_bytes());
    bytes.extend_from_slice(b"\n%%EOF\n");
    bytes
}
