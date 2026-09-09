use assert_cmd::Command;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn in_use_generation_65536_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        b"3 0 obj\n43\nendobj\n".as_slice(),
        b"4 0 obj\n44\nendobj\n".as_slice(),
        b"5 0 obj\n45\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for (index, offset) in offsets.iter().enumerate() {
        let generation = if index == 4 { 65_536 } else { 0 };
        bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn previous_generation_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        b"3 0 obj\n43\nendobj\n".as_slice(),
        b"4 0 obj\n44\nendobj\n".as_slice(),
        b"5 0 obj\n45\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let previous_xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");

    let latest_xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for (index, offset) in offsets.iter().enumerate() {
        let generation = if index == 4 { 65_535 } else { 0 };
        bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /Prev {previous_xref_offset} >>\nstartxref\n{latest_xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

fn run_qpdf(path: &std::path::Path) -> Output {
    ProcessCommand::new("qpdf")
        .args(["--show-xref", path.to_str().unwrap()])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(path: &std::path::Path) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .args(["--show-xref", path.to_str().unwrap()])
        .output()
        .expect("flpdf should spawn")
}

#[test]
fn show_xref_preserves_in_use_generation_outside_object_ref_range() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("in-use-generation-65536.pdf");
    std::fs::write(&input, in_use_generation_65536_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&input);
    let flpdf = run_flpdf(&input);

    assert!(qpdf.status.success(), "qpdf failed: {:?}", qpdf);
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf
        .stdout
        .windows(b"5/65536: uncompressed".len())
        .any(|window| { window == b"5/65536: uncompressed" }));
}

#[test]
fn show_xref_discards_lower_raw_generation_after_prev_chain() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("prev-generation.pdf");
    std::fs::write(&input, previous_generation_pdf()).expect("write fixture");

    let qpdf = run_qpdf(&input);
    let flpdf = run_flpdf(&input);

    assert!(qpdf.status.success(), "qpdf failed: {:?}", qpdf);
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    let stdout = String::from_utf8_lossy(&qpdf.stdout);
    assert!(
        stdout.contains("5/65535: uncompressed"),
        "qpdf stdout: {stdout}"
    );
    assert!(
        !stdout.contains("5/0: uncompressed"),
        "qpdf stdout: {stdout}"
    );
}
