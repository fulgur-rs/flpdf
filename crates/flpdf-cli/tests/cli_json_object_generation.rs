//! qpdf JSON v2 object-map identity tests for raw xref generations.

use assert_cmd::Command;
use std::io::Write;
use std::process::Command as ProcessCommand;

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

/// Build a two-revision PDF where object 4 is deleted at generation 0 and
/// reused at generation 1. The final trailer also contains a dangling 4 0 R,
/// which must not become an object-map entry merely because it was parsed.
fn incremental_generation_pdf_bytes() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n");
    let off5 = pdf.len();
    pdf.extend_from_slice(b"5 0 obj\n[ /PDF /Text ]\nendobj\n");
    let off6 = pdf.len();
    pdf.extend_from_slice(b"6 0 obj\n<< /Name /F1 >>\nendobj\n");

    let first_xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 7\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n{off6:010} 00000 n \ntrailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{first_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let second_xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \n4 1\n0000000000 00001 f \ntrailer\n<< /Size 7 /Root 1 0 R /Prev {first_xref} >>\nstartxref\n{second_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let off4_1 = pdf.len();
    pdf.extend_from_slice(b"4 1 obj\n[ 7 0 R ]\nendobj\n");
    let off7 = pdf.len();
    pdf.extend_from_slice(b"7 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n");
    let off3_final = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 1 R >>\nendobj\n",
    );

    let final_xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \n3 2\n{off3_final:010} 00000 n \n{off4_1:010} 00001 n \n7 1\n{off7:010} 00000 n \ntrailer\n<< /Size 8 /Root 1 0 R /Prev {second_xref} /Gone 4 0 R >>\nstartxref\n{final_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

/// Make a standard fixture expose an in-use object-zero xref row. qpdf keeps
/// that raw identity in `getAllObjects`, even though object zero is not a
/// valid indirect reference in ordinary PDF object syntax.
fn object_zero_in_use_pdf_bytes() -> Vec<u8> {
    let mut pdf = include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec();
    let old = b"0000000000 65535 f ";
    let new = b"0000000009 00000 n ";
    assert_eq!(old.len(), new.len());
    let offset = pdf
        .windows(old.len())
        .position(|window| window == old)
        .expect("fixture must contain the object-zero xref row");
    pdf[offset..offset + old.len()].copy_from_slice(new);
    pdf
}

fn assert_json_object_map_matches_qpdf(bytes: Vec<u8>, expected_keys: &[&str]) {
    if !qpdf_available() {
        return;
    }

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&bytes).unwrap();
    let path = input.path();

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--json=2", "--json-output=2"])
        .arg(path)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", "--json-output=2"])
        .arg(path)
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(0),
        "qpdf stderr: {:?}",
        qpdf.stderr
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);

    let json: serde_json::Value = serde_json::from_slice(&qpdf.stdout).unwrap();
    let object_map = json["qpdf"][1].as_object().unwrap();
    let actual_keys: Vec<_> = object_map.keys().map(String::as_str).collect();
    assert_eq!(actual_keys, expected_keys);
}

#[test]
fn json_object_map_preserves_only_the_active_incremental_generation() {
    assert_json_object_map_matches_qpdf(
        incremental_generation_pdf_bytes(),
        &[
            "obj:1 0 R",
            "obj:2 0 R",
            "obj:3 0 R",
            "obj:4 1 R",
            "obj:5 0 R",
            "obj:6 0 R",
            "obj:7 0 R",
            "trailer",
        ],
    );
}

#[test]
fn json_object_map_keeps_an_in_use_object_zero_xref_identity() {
    assert_json_object_map_matches_qpdf(
        object_zero_in_use_pdf_bytes(),
        &[
            "obj:0 0 R",
            "obj:1 0 R",
            "obj:2 0 R",
            "obj:3 0 R",
            "obj:4 0 R",
            "obj:5 0 R",
            "obj:6 0 R",
            "obj:7 0 R",
            "trailer",
        ],
    );
}
