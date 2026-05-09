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
        .stdout(predicate::str::contains("media-box: [0 0 595.28 842]"))
        .stdout(predicate::str::contains("media-box: [0 0 200 100]"));
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
