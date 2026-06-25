use assert_cmd::Command;
use predicates::prelude::*;
use std::collections::BTreeMap;

/// Parse the output PDF and assert how many signatures the library detects.
///
/// Stronger than a raw `/ByteRange` byte scan (gemini review on PR #424): it
/// confirms the `/FT /Sig` field + its signature dictionary are structurally
/// intact and detectable via [`flpdf::signatures`], or — for the drop case —
/// that they are genuinely gone.
fn assert_signature_count(output: &std::path::Path, expected: usize) {
    let bytes = std::fs::read(output).expect("read output PDF");
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).expect("parse output PDF");
    let sigs = flpdf::signatures(&mut pdf).expect("inspect signatures in output");
    assert_eq!(
        sigs.len(),
        expected,
        "expected {expected} signature(s) in the output (qpdf-compatible)"
    );
}

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

/// Two-page signed document; the signature field widget lives on page 1
/// (obj 5, on page 3's `/Annots`) and page 2 (obj 9) is plain. Selecting page 2
/// alone drops the only object that keeps the signature reachable.
fn build_two_page_signed_acroform_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 9 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
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
        (9, b"<< /Type /Page /Parent 2 0 R >>"),
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
fn full_rewrite_of_signed_pdf_proceeds_qpdf_compatible() {
    // qpdf does NOT refuse a full rewrite of a signed PDF — it proceeds, leaving
    // signatures present-but-invalid (verified, qpdf 11.9.0: exit 0, no warning).
    // flpdf matches pre-v1.0: the signed-full-rewrite refusal was removed
    // (flpdf-hn1g.13; signed preserve-by-default protection deferred post-v1.0,
    // flpdf-hn1g.14). So a plain `rewrite --full-rewrite` of a signed PDF exits 0
    // and preserves the signature objects (not stripped — that needs the explicit
    // --remove-restrictions opt-in, covered separately below).
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
    .success();

    assert!(output.exists());
    // The signature objects survive the full rewrite (present-but-invalid),
    // matching qpdf — they are not stripped without --remove-restrictions.
    assert_signature_count(&output, 1);
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

#[test]
fn signed_pages_extraction_proceeds_qpdf_compatible() {
    // Direct regression for flpdf-hn1g.13: a signed `--pages` extraction (always
    // a full rewrite) used to be REFUSED (exit 2) when the signature field was a
    // retained-page widget. qpdf does not refuse — it proceeds, leaving the
    // signature present-but-invalid. flpdf now matches: exit 0, signature objects
    // preserved. (build_signed_acroform_pdf's sig field is a widget on page 1.)
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, build_signed_acroform_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        input.to_str().unwrap(),
        "--pages",
        ".",
        "1",
        "--",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("refusing full rewrite").not());

    assert!(output.exists());
    // The signature field is on the retained page, so it survives the rewrite
    // (present-but-invalid), matching qpdf.
    assert_signature_count(&output, 1);
}

#[test]
fn signed_pages_dropping_signature_page_matches_qpdf() {
    // When `--pages` drops the page that owns the signature widget, the
    // signature field becomes unreferenced (its page and AcroForm /Fields entry
    // are gone) and is garbage-collected — the signature disappears from the
    // output. This is NOT a silent-removal policy violation: qpdf does exactly
    // the same (verified, qpdf 11.9.0: `qpdf in.pdf --pages in.pdf 2 -- out`
    // produces output with no /FT /Sig and no /ByteRange). flpdf matches it.
    // (Removing a signature still *referenced* in the output, or via flpdf-
    // specific logic qpdf does not apply, would be the violation — not this GC.)
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed2.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, build_two_page_signed_acroform_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        input.to_str().unwrap(),
        "--pages",
        ".",
        "2",
        "--",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    // The signature's page was dropped, so the signature is gone — like qpdf.
    assert_signature_count(&output, 0);
}

#[test]
fn incremental_rewrite_of_signed_pdf_succeeds_without_warning() {
    // The incremental-update path appends to the source bytes verbatim, so a
    // signed input's byte ranges stay intact — the signature is preserved and
    // still valid, with no warning. `--remove-unreferenced-resources=no` stays on
    // the incremental path (a plain `rewrite` defaults to `auto`, which forces a
    // full rewrite — that now proceeds qpdf-compatibly, but it shifts byte
    // positions and so invalidates the signature, hence this test pins the
    // incremental path). No --remove-restrictions, so no signatures are stripped.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("signed.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, build_signed_acroform_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--remove-unreferenced-resources=no",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains("refusing full rewrite").not())
    .stderr(predicate::str::contains("removed signatures").not())
    .stderr(predicate::str::contains("invalidated").not());

    // The incremental path appends to the source bytes verbatim, so the original
    // signature dictionary (and its signed /ByteRange) survives untouched.
    let bytes = std::fs::read(&output).unwrap();
    let haystack = String::from_utf8_lossy(&bytes);
    assert!(
        haystack.contains("/ByteRange"),
        "incremental rewrite must preserve the signed /ByteRange"
    );
    assert!(
        haystack.contains("/SubFilter /adbe.pkcs7.detached"),
        "incremental rewrite must preserve the signature dictionary"
    );
}
