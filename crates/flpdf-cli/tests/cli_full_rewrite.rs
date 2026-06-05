use assert_cmd::Command;
use predicates::prelude::*;
use std::collections::BTreeMap;

fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    for &(num, bytes) in objects {
        offsets.insert(num, out.len() as u64);
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = out.len() as u64;
    let max_num = objects.iter().map(|&(n, _)| n).max().unwrap_or(0);
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        match offsets.get(&i) {
            Some(&off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );
    out
}

fn build_signed_acroform_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signed) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

#[test]
fn full_rewrite_flag_produces_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);
}

#[test]
fn full_rewrite_output_is_valid_pdf() {
    use flpdf::{check_reader, Pdf};
    use std::io::Cursor;

    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    let bytes = std::fs::read(&output).unwrap();
    let report = check_reader(Cursor::new(&bytes)).unwrap();
    assert!(
        report.valid,
        "full-rewrite CLI output should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // Trailer must not have /Prev.
    let pdf = Pdf::open(Cursor::new(&bytes)).unwrap();
    assert!(
        pdf.trailer().get("Prev").is_none(),
        "full-rewrite output must not have /Prev"
    );
}

#[test]
fn full_rewrite_and_linearize_are_mutually_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "--linearize",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("cannot be used together"));
}

#[test]
fn full_rewrite_of_signed_pdf_prints_actionable_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, build_signed_acroform_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "refusing full rewrite of signed PDF",
    ))
    .stderr(predicate::str::contains("Signed"))
    .stderr(predicate::str::contains("--remove-restrictions"))
    .stderr(predicate::str::contains("incremental rewrite"));
}

#[test]
fn remove_restrictions_allows_signed_full_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, build_signed_acroform_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "--remove-restrictions",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);
}

#[test]
fn remove_restrictions_allows_signed_linearized_rewrite() {
    // Regression for the --linearize path: the destructive opt-in must apply
    // to the linearize branch too. The branch strips the signatures
    // (clear_sig_flags + strip_signature_values) before writing, so the
    // rewrite succeeds and warns instead of being refused.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, build_signed_acroform_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--linearize",
        "--remove-restrictions",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("removed signatures"));

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);
}
