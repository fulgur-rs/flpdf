use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

#[test]
fn check_valid_fixture_exits_successfully() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn check_encrypted_fixture_accepts_correct_empty_password_flag() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "--password=",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn check_encrypted_fixture_rejects_wrong_password() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "--password=wrong",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("incorrect password"))
    .stderr(predicate::str::contains("--password"));
}

#[test]
fn check_rejects_rc4_encrypted_input_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rc4.pdf");
    std::fs::write(&input, encrypted_v1_owner_password_fixture()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--password=owner"])
        .arg(&input)
        .assert()
        .failure()
        .stderr(predicate::str::contains("weak crypto"))
        .stderr(predicate::str::contains("--allow-weak-crypto"));
}

#[test]
fn check_allows_rc4_encrypted_input_with_warning_when_opted_in() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("rc4.pdf");
    std::fs::write(&input, encrypted_v1_owner_password_fixture()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--allow-weak-crypto", "--password=owner"])
        .arg(&input)
        .assert()
        .success()
        .stdout(predicate::str::contains("PDF check succeeded"))
        .stderr(predicate::str::contains("warning"))
        .stderr(predicate::str::contains("weak crypto"));
}

#[test]
fn check_repair_encrypted_fixture_rejects_wrong_password_actionably() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "--repair",
        "--password=wrong",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("incorrect password"))
    .stderr(predicate::str::contains("--password"));
}

#[test]
fn rewrite_encrypted_fixture_is_rejected_until_decrypt_output_is_supported() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("--password=")
        .arg("../../tests/fixtures/compat/encrypted-r4-three-page.pdf")
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "encrypted PDF output is not supported yet",
        ));

    assert!(!output.exists());
}

#[test]
fn check_encrypted_fixture_uses_empty_default_password() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--check",
        "../../tests/fixtures/compat/encrypted-r4-three-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn check_encrypted_fixture_reads_password_file_and_strips_newline() {
    let temp = tempfile::tempdir().unwrap();
    let password_file = temp.path().join("password.txt");
    std::fs::write(&password_file, b"\r\n").unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check"])
        .arg(format!("--password-file={}", password_file.display()))
        .arg("../../tests/fixtures/compat/encrypted-r4-three-page.pdf")
        .assert()
        .success()
        .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn password_and_password_file_are_mutually_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let password_file = temp.path().join("password.txt");
    std::fs::write(&password_file, b"").unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--password="])
        .arg(format!("--password-file={}", password_file.display()))
        .arg("../../tests/fixtures/compat/encrypted-r4-three-page.pdf")
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn rewrite_fixture_creates_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("../../tests/fixtures/minimal.pdf")
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn rewrite_repaired_fixture_with_repair_flag() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_pdf()).unwrap();

    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--repair",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn check_subcommand_succeeds() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn pages_subcommand_prints_each_page() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["pages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("page 1: 3 0 R"))
        .stdout(predicate::str::contains("page 2: 6 0 R"));
}

#[test]
fn pages_subcommand_prints_count() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["pages", "--count", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2"));
}

#[test]
fn dump_object_subcommand_accepts_ref() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["dump-object", "1 0", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn qdf_subcommand_rewrites_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "qdf",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn qdf_subcommand_dumps_all_objects() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture_with_orphan_object();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "qdf",
        fixture.path().to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    // The qdf output contains a binary header marker (non-UTF-8 bytes), so we
    // read raw bytes and search for the target substring as bytes.
    let rendered = std::fs::read(&output).unwrap();
    assert!(
        rendered.windows(b"5 0 obj".len()).any(|w| w == b"5 0 obj"),
        "expected '5 0 obj' in qdf output"
    );
}

#[test]
fn rewrite_subcommand_rewrites_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

// ---------------------------------------------------------------------------
// qpdf-style top-level flat flags
//
// These exist so the qpdf qtest acceptance harness (which PATH-shims
// `qpdf` → `flpdf` with no arg translation) can drive flpdf with the
// commands its `.test` files already use. The behaviour mirrors the
// equivalent `flpdf rewrite ...` subcommand invocation.
// ---------------------------------------------------------------------------

/// Build a single-page PDF in memory.  Same shape as the helper in
/// cli_linearize.rs; duplicated here to keep this test self-contained
/// without re-exporting test helpers between integration test crates.
fn one_page_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn top_level_linearize_rewrites_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--linearize", "--static-id"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn top_level_linearize_accepts_compress_streams_and_pass1() {
    // Mirrors the COMMAND from upstream qpdf's linearize-pass1.test:
    //   qpdf --linearize --static-id --compress-streams=n \
    //        --linearize-pass1=b.pdf in.pdf a.pdf
    // We do not assert byte-equality with qpdf's golden output here —
    // that is a separate, much larger gate. We assert only that the CLI
    // parses, runs to completion, writes both files, and emits no
    // stdout/stderr (qpdf qtest's subtest 1 condition).
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("a.pdf");
    let pass1 = temp.path().join("b.pdf");
    std::fs::write(&input, one_page_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--linearize", "--static-id", "--compress-streams=n"])
        .arg(format!("--linearize-pass1={}", pass1.display()))
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    assert!(output.exists());
    assert!(pass1.exists());
}

// ---------------------------------------------------------------------------
// Version validation tests
// ---------------------------------------------------------------------------

#[test]
fn rewrite_force_version_invalid_abc_exits_nonzero() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--force-version=abc",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("invalid --force-version"));
}

#[test]
fn rewrite_force_version_with_newline_exits_nonzero() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .arg("--force-version=1.4\n")
    .assert()
    .failure()
    .stderr(predicate::str::contains("invalid --force-version"));
}

#[test]
fn rewrite_min_version_invalid_abc_exits_nonzero() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--min-version=abc",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("invalid --min-version"));
}

#[test]
fn rewrite_valid_force_version_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--force-version=1.4",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn rewrite_valid_min_version_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--min-version=1.3",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}

#[test]
fn show_info_with_repair_flag_handles_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--repair", "--show-info", input.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Title = (Corrupt fixture)"));
}

#[test]
fn show_info_without_repair_rejects_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-info", input.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn check_without_repair_rejects_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", input.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn check_with_repair_accepts_corrupt_xref() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("corrupt.pdf");
    std::fs::write(&input, corrupt_xref_with_info_pdf()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--repair", "--check", input.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn dump_object_accepts_ref_without_suffix() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--dump-object", "1 0", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn dump_object_accepts_ref_with_r_suffix() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--dump-object", "1 0 R", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn dump_object_rejects_invalid_ref() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--dump-object", "bad", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .failure();
}

#[test]
fn show_info_prints_document_info() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-info", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Title = (Fixture PDF)"))
        .stdout(predicate::str::contains("Creator = (flpdf)"));
}

#[test]
fn show_catalog_prints_root_dictionary() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-catalog", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Catalog: <<"))
        .stdout(predicate::str::contains("/Type /Catalog"));
}

#[test]
fn show_metadata_prints_stream_summary() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-metadata", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Metadata: stream"))
        .stdout(predicate::str::contains("/XML"));
}

#[test]
fn show_outline_prints_titles() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-outline", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("1: Chapter One"));
}

#[test]
fn show_fonts_prints_summary() {
    let fixture = fixture_with_metadata_outline_and_fonts();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-fonts", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("F1"))
        .stdout(predicate::str::contains("F2"));
}

#[test]
fn show_fonts_prints_inline_dictionary_fonts() {
    let fixture = fixture_with_inline_font_dictionary();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-fonts", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("FDirect"))
        .stdout(predicate::str::contains("type: /Font"));
}

#[test]
fn show_npages_prints_total_pages() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-npages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("2"));
}

#[test]
fn show_pages_lists_each_page() {
    let fixture = fixture_with_nested_pages();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-pages", fixture.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("page 1: 3 0 R"))
        .stdout(predicate::str::contains("page 2: 6 0 R"))
        .stdout(predicate::str::contains("media-box: [ 0 0 595.28 842 ]"))
        .stdout(predicate::str::contains("media-box: [ 0 0 200 100 ]"));
}

fn fixture_with_metadata_outline_and_fonts() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R /Metadata 4 0 R /Info 5 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>\nendobj\n";
    let metadata_data = b"<xmpmeta>Fixture metadata</xmpmeta>";
    let object4 = format!(
        "4 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        metadata_data.len(),
        String::from_utf8_lossy(metadata_data)
    )
    .into_bytes();
    let object5 = b"5 0 obj\n<< /Title (Fixture PDF) /Creator (flpdf) >>\nendobj\n";
    let object6 = b"6 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 7 0 R /F2 8 0 R >> >> /MediaBox [0 0 612 792] /Contents 9 0 R >>\nendobj\n";
    let object7 = b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>\nendobj\n";
    let object8 = b"8 0 obj\n<< /Type /Font /Subtype /Type0 /BaseFont /Courier >>\nendobj\n";
    let content_data = b"BT /F1 12 Tf (Hello) Tj ET";
    let object9 = format!(
        "9 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();
    let object10 =
        b"10 0 obj\n<< /Title (Chapter One) /Parent 3 0 R /Dest [6 0 R /Fit] >>\nendobj\n";

    let objects = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4,
        object5.to_vec(),
        object6.to_vec(),
        object7.to_vec(),
        object8.to_vec(),
        object9,
        object10.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for object in &objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    let file = fixture.as_file_mut();
    file.write_all(&bytes).unwrap();

    fixture
}

fn fixture_with_inline_font_dictionary() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /FDirect << /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >> >> >> /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n";
    let content_data = b"HelloPDF\n";
    let object4 = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();

    let mut offsets = Vec::new();
    let objects: Vec<Vec<u8>> = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    for object in &objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    fixture.as_file_mut().write_all(&bytes).unwrap();

    fixture
}

fn fixture_with_orphan_object() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n";
    let content_data = b"Hello PDF";
    let object4 = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();
    let object5 = b"5 0 obj\n<< /Type /Orphan >>\nendobj\n";

    let objects = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4,
        object5.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for object in &objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    fixture.as_file_mut().write_all(&bytes).unwrap();

    fixture
}

fn fixture_with_nested_pages() -> tempfile::NamedTempFile {
    let mut fixture = tempfile::NamedTempFile::new().unwrap();

    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] /MediaBox [0 0 595.28 841.89] >>\nendobj\n";
    let object3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 842] /Contents 5 0 R >>\nendobj\n";
    let object4 = b"4 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] /Rotate 90 >>\nendobj\n";
    let object5 = b"5 0 obj\n<< /Length 14 >>\nstream\nBT (one) Tj ET\nendstream\nendobj\n";
    let object6 = b"6 0 obj\n<< /Type /Page /Parent 4 0 R /Rotate 90 /MediaBox [0 0 200 100] /Contents 7 0 R >>\nendobj\n";
    let object7 = b"7 0 obj\n<< /Length 15 >>\nstream\nBT (two) Tj ET\nendstream\nendobj\n";
    let objects = vec![
        object1.to_vec(),
        object2.to_vec(),
        object3.to_vec(),
        object4.to_vec(),
        object5.to_vec(),
        object6.to_vec(),
        object7.to_vec(),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for object in &objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(format!("{:010} 65535 f\n", 0).as_bytes());
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );

    fixture.write_all(&bytes).unwrap();

    fixture
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

fn corrupt_xref_with_info_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Info 5 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n".to_vec();
    let obj4 = b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".to_vec();
    let obj5 = b"5 0 obj\n<< /Title (Corrupt fixture) /Creator (flpdf) >>\nendobj\n".to_vec();

    let mut offsets = Vec::new();
    for object in [&obj1, &obj2, &obj3, &obj4, &obj5] {
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
            "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
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
