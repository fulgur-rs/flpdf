//! qpdf 11.9.0 parity for inherited primary page attributes in multi-source `--pages`.

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command as ProcessCommand;

const SECONDARY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/one-page.pdf"
);

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
}

fn primary_with_inherited_box() -> Vec<u8> {
    let objects = [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (
            2,
            "<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] /Resources << /Font << /Helv 5 0 R >> >> /MediaBox [0 0 200 200] >>",
        ),
        (3, "<< /Type /Page /Parent 2 0 R >>"),
        (4, "<< /Type /Page /Parent 2 0 R >>"),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (number, body) in objects {
        offsets.insert(number, bytes.len());
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for number in 1..=5 {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn multi_source_pages_preserves_primary_inherited_mediabox_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping inherited MediaBox differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary inherited MediaBox directory");
    let qpdf_directory = directory.path().join("qpdf");
    let flpdf_directory = directory.path().join("flpdf");
    fs::create_dir(&qpdf_directory).expect("create qpdf directory");
    fs::create_dir(&flpdf_directory).expect("create flpdf directory");
    for target in [&qpdf_directory, &flpdf_directory] {
        fs::write(target.join("primary.pdf"), primary_with_inherited_box()).expect("write primary");
        fs::copy(SECONDARY, target.join("secondary.pdf")).expect("copy secondary");
    }

    let args = [
        "--qdf",
        "--static-id",
        "primary.pdf",
        "--pages",
        ".",
        "1",
        "secondary.pdf",
        "1",
        "--",
        "out.pdf",
    ];
    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(&qpdf_directory)
        .args(args)
        .output()
        .expect("run qpdf inherited MediaBox oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(&flpdf_directory)
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("run flpdf inherited MediaBox");

    assert_eq!(qpdf.status.code(), Some(0));
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        fs::read(qpdf_directory.join("out.pdf")).expect("read qpdf output"),
        fs::read(flpdf_directory.join("out.pdf")).expect("read flpdf output")
    );
}
