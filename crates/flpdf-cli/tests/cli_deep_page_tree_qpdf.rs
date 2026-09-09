//! qpdf 11.9.0 parity for deep, non-cyclic `/Pages` trees.

use assert_cmd::Command;
use std::io::Write;
use std::process::{Command as ShellCommand, Output};

const QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn run_qpdf(args: &[String]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[String]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn deep_nested_pages_pdf(depth: usize) -> Vec<u8> {
    assert!(depth > 0);
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0_u64; depth + 3];
    let mut write_object = |number: usize, body: &[u8]| {
        offsets[number] = bytes.len() as u64;
        writeln!(&mut bytes, "{number} 0 obj").unwrap();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    };

    write_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    for level in 0..depth {
        let number = level + 2;
        let child = if level + 1 == depth {
            depth + 2
        } else {
            number + 1
        };
        let body = format!("<< /Type /Pages /Count 1 /Kids [{child} 0 R] >>");
        write_object(number, body.as_bytes());
    }
    write_object(depth + 2, b"<< /Type /Page /MediaBox [0 0 612 792] >>");

    let xref_offset = bytes.len();
    writeln!(&mut bytes, "xref\n0 {}", offsets.len()).unwrap();
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        writeln!(&mut bytes, "{offset:010} 00000 n ").unwrap();
    }
    writeln!(
        &mut bytes,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF",
        depth + 3
    )
    .unwrap();
    bytes
}

#[test]
fn deep_page_tree_survives_check_json_and_page_selection_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{QPDF_VERSION} is required on CI");
        }
        eprintln!("skipping: {QPDF_VERSION} is unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("deep-pages.pdf");
    std::fs::write(&input, deep_nested_pages_pdf(2_000)).expect("write deep page tree");
    let input = input.to_str().expect("UTF-8 temporary path").to_owned();

    let check_args = vec!["--check".to_owned(), input.clone()];
    let qpdf_check = run_qpdf(&check_args);
    let flpdf_check = run_flpdf(&check_args);
    assert_success(&qpdf_check, "qpdf --check");
    assert_success(&flpdf_check, "flpdf --check");
    assert_eq!(
        flpdf_check.stdout, qpdf_check.stdout,
        "deep --check report must remain byte-identical to qpdf"
    );

    let json_args = vec!["--json=2".to_owned(), input.clone()];
    let qpdf_json = run_qpdf(&json_args);
    let flpdf_json = run_flpdf(&json_args);
    assert_success(&qpdf_json, "qpdf --json=2");
    assert_success(&flpdf_json, "flpdf --json=2");
    assert_eq!(
        flpdf_json.stdout, qpdf_json.stdout,
        "deep --json=2 output must remain byte-identical to qpdf"
    );

    let qpdf_output = temp.path().join("qpdf-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-pages.pdf");
    let qpdf_pages_args = vec![
        "--static-id".to_owned(),
        "--stream-data=uncompress".to_owned(),
        input.clone(),
        "--pages".to_owned(),
        ".".to_owned(),
        "1-z".to_owned(),
        "--".to_owned(),
        qpdf_output.to_str().expect("UTF-8 qpdf path").to_owned(),
    ];
    let flpdf_pages_args = [
        qpdf_pages_args[..7].to_vec(),
        vec![flpdf_output.to_str().expect("UTF-8 flpdf path").to_owned()],
    ]
    .concat();
    assert_success(&run_qpdf(&qpdf_pages_args), "qpdf --pages . 1-z --");
    assert_success(&run_flpdf(&flpdf_pages_args), "flpdf --pages . 1-z --");
    assert_eq!(
        std::fs::read(&flpdf_output).expect("read flpdf page-selection output"),
        std::fs::read(&qpdf_output).expect("read qpdf page-selection output"),
        "deep page selection must remain byte-identical to qpdf"
    );
}
