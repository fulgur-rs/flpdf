//! qpdf differential coverage for provider diagnostics during plain rewrites.

use assert_cmd::cargo::cargo_bin;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const NON_STREAM_WARNING: &str =
    "WARNING: page object 3 0: item index 1 (from 0): ignoring non-stream in an array of streams";

fn build_malformed_contents_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 8 0 R 6 0 R] >>".to_vec(),
        ),
        (4, stream_object(b"q Q")),
        (6, stream_object(b"BT ET")),
        (8, b"<< /NotAStream true >>".to_vec()),
    ];
    let mut offsets = BTreeMap::new();
    for (number, body) in &objects {
        offsets.insert(*number, bytes.len() as u64);
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref = bytes.len() as u64;
    let size = 9;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        if let Some(offset) = offsets.get(&number) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 65535 f \n");
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn stream_object(body: &[u8]) -> Vec<u8> {
    let mut object = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    object.extend_from_slice(body);
    object.extend_from_slice(b"\nendstream");
    object
}

fn qpdf_is_available() -> bool {
    match Command::new("qpdf").arg("--version").output() {
        Ok(output)
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(str::trim)
                    == Some(EXPECTED_QPDF_VERSION) =>
        {
            true
        }
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let found = stdout.lines().next().unwrap_or("<empty stdout>");
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required; found {found:?}");
            }
            eprintln!("skipping qpdf differential: found {found:?}");
            false
        }
        Err(error) => {
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required: {error}");
            }
            eprintln!("skipping qpdf differential: {error}");
            false
        }
    }
}

fn run_qpdf(input: &Path, output: &Path) -> Output {
    Command::new("qpdf")
        .args([
            "--coalesce-contents",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

fn run_flpdf(input: &Path, output: &Path) -> Output {
    Command::new(cargo_bin!("flpdf"))
        .args([
            "rewrite",
            "--coalesce-contents",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

fn warning_count(output: &Output) -> usize {
    String::from_utf8_lossy(&output.stderr)
        .matches(NON_STREAM_WARNING)
        .count()
}

fn warning_lines(output: &Output) -> Vec<&[u8]> {
    output
        .stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"WARNING:"))
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .collect()
}

fn qdf_without_id(path: &Path) -> Vec<u8> {
    let output = Command::new("qpdf")
        .args([
            "--qdf",
            "--static-id",
            "--object-streams=disable",
            path.to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "qpdf qdf conversion failed: {:?}",
        output.stderr
    );
    let mut normalized = Vec::new();
    for line in output.stdout.split_inclusive(|byte| *byte == b'\n') {
        if line
            .windows(b"/ID [<".len())
            .any(|window| window == b"/ID [<")
        {
            continue;
        }
        normalized.extend_from_slice(line);
    }
    normalized
}

#[test]
fn plain_coalesce_rewrite_reports_each_non_stream_warning_once_like_qpdf() {
    if !qpdf_is_available() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("malformed-contents.pdf");
    let qpdf_output = temp.path().join("qpdf-output.pdf");
    let flpdf_output = temp.path().join("flpdf-output.pdf");
    std::fs::write(&input, build_malformed_contents_pdf()).unwrap();

    let qpdf = run_qpdf(&input, &qpdf_output);
    let flpdf = run_flpdf(&input, &flpdf_output);

    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf stderr: {:?}",
        qpdf.stderr
    );
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf stderr: {:?}",
        flpdf.stderr
    );
    assert_eq!(warning_count(&qpdf), 1, "qpdf stderr: {:?}", qpdf.stderr);
    assert_eq!(warning_count(&flpdf), 1, "flpdf stderr: {:?}", flpdf.stderr);
    assert_eq!(warning_lines(&flpdf), warning_lines(&qpdf));
    assert_eq!(qdf_without_id(&flpdf_output), qdf_without_id(&qpdf_output));
    assert!(qpdf_output.is_file());
    assert!(flpdf_output.is_file());
}
