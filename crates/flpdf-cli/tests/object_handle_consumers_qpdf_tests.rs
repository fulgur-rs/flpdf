//! Live qpdf probes for the ObjectHandle consumer cutover.

use assert_cmd::Command;
use std::process::Command as ShellCommand;

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
        .unwrap_or(false)
}

fn build_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

fn run_json_parity(name: &str, pdf: Vec<u8>) {
    if !qpdf_available() {
        eprintln!("skipping {name}: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join(format!("{name}.pdf"));
    std::fs::write(&input, pdf).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2", "--json-key=attachments"])
        .arg(&input)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=attachments"])
        .arg(&input)
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(0), "qpdf {name}: {:?}", qpdf);
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "flpdf {name}");
    assert_eq!(flpdf.stdout, qpdf.stdout, "JSON mismatch for {name}");
    assert_eq!(flpdf.stderr, qpdf.stderr, "diagnostic mismatch for {name}");
}

fn run_json_outcome_probe(name: &str, pdf: Vec<u8>) {
    if !qpdf_available() {
        eprintln!("skipping {name}: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join(format!("{name}.pdf"));
    std::fs::write(&input, pdf).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2", "--json-key=attachments"])
        .arg(&input)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=attachments"])
        .arg(&input)
        .output()
        .unwrap();

    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "status mismatch for {name}"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout, "stdout mismatch for {name}");
    if qpdf.status.code() == Some(3) {
        let warning_lines = |stderr: &[u8]| {
            String::from_utf8_lossy(stderr)
                .lines()
                .filter(|line| line.starts_with("WARNING:"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            warning_lines(&flpdf.stderr),
            warning_lines(&qpdf.stderr),
            "warning mismatch for {name}"
        );
    }
}

#[test]
fn json_attachments_match_qpdf_for_all_ef_keys() {
    let f_stream = b"<< /Type /EmbeddedFile /Subtype /text#2fplain /Params << /CreationDate (D:20260101000000Z) /ModDate (D:20260202120000+09'00') /CheckSum <000102030405060708090a0b0c0d0e0f> >> /Length 3 >>\nstream\nabc\nendstream";
    let uf_stream = b"<< /Type /EmbeddedFile /Length 0 >>\nstream\n\nendstream";
    let extra_stream = b"<< /Type /EmbeddedFile /Length 0 >>\nstream\n\nendstream";
    run_json_parity(
        "all-ef-keys",
        build_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
            b"<< /EmbeddedFiles 5 0 R >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            b"<< /Names [(all-keys) 6 0 R] >>",
            b"<< /Type /Filespec /F (f.txt) /UF (u.txt) /Desc (description) /EF << /F 7 0 R /UF 8 0 R /Extra 9 0 R >> >>",
            f_stream,
            uf_stream,
            extra_stream,
        ]),
    );
}

#[test]
fn json_attachments_match_qpdf_for_direct_filespec_leaves() {
    run_json_parity(
        "direct-filespec",
        build_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
            b"<< /EmbeddedFiles 5 0 R >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            b"<< /Names [(direct) << /Type /Filespec /F (direct.txt) /UF (direct.txt) /EF << /F 6 0 R >> >>] >>",
            b"<< /Type /EmbeddedFile /Length 0 >>\nstream\n\nendstream",
        ]),
    );
}

#[test]
fn json_attachments_preserve_distinct_invalid_explicit_utf8_keys() {
    run_json_parity(
        "invalid-explicit-utf8-name-collision",
        build_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
            b"<< /EmbeddedFiles 5 0 R >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            b"<< /Names [(\xef\xbb\xbf\xfe) 6 0 R (\xef\xbb\xbf\xff) 8 0 R] >>",
            b"<< /Type /Filespec /F (a.txt) /EF << /F 7 0 R >> >>",
            b"<< /Type /EmbeddedFile /Length 0 >>\nstream\n\nendstream",
            b"<< /Type /Filespec /F (b.txt) /EF << /F 9 0 R >> >>",
            b"<< /Type /EmbeddedFile /Length 0 >>\nstream\n\nendstream",
        ]),
    );
}

#[test]
fn json_attachments_malformed_outcomes_match_qpdf() {
    run_json_outcome_probe(
        "scalar-filespec",
        build_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
            b"<< /EmbeddedFiles 5 0 R >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            b"<< /Names [(scalar) 7] >>",
            b"<< /Limits [(scalar) (scalar)] >>",
            b"7",
        ]),
    );
    run_json_outcome_probe(
        "nonstream-ef",
        build_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
            b"<< /EmbeddedFiles 5 0 R >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            b"<< /Names [(nonstream) 6 0 R] >>",
            b"<< /Type /Filespec /F (f.txt) /EF << /F 7 0 R >> >>",
            b"1",
        ]),
    );
    run_json_outcome_probe(
        "non-dictionary-filespec",
        build_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
            b"<< /EmbeddedFiles 5 0 R >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            b"<< /Names [(broken) 6 0 R] >>",
            b"7",
            b"<< >>",
        ]),
    );
}
