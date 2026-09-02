//! Ordinary read-only inspection routes through the qpdf-shaped QPDFJob boundary.

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use std::process::Command as ShellCommand;

#[path = "support/text.rs"]
mod text;
use text::EOL;

const ONE_PAGE_PDF: &str = "../../tests/fixtures/compat/one-page.pdf";
const REPAIRABLE_PDF: &str = "../../tests/fixtures/test_driver/repairable_input.pdf";
const WEAK_RC4_PDF: &str = "../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf";

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf").arg("--version").output().ok();
    let is_expected = version.as_ref().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).lines().next() == Some("qpdf version 11.9.0")
    });
    if is_expected {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for ordinary inspection oracle tests");
    }
    eprintln!("skipping ordinary inspection oracle: qpdf 11.9.0 is not available");
    true
}

fn normalize_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }
    normalized
}

fn one_page_with_image_pdf() -> Vec<u8> {
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 0 >>\nstream\n\nendstream\nendobj\n".as_slice(),
        b"5 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".as_slice(),
    ];
    let mut bytes = b"%PDF-1.3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn ordinary_show_npages_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-npages", ONE_PAGE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-npages", ONE_PAGE_PDF])
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_pages_preserves_qpdf_page_identity_line() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-pages", ONE_PAGE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-pages", ONE_PAGE_PDF])
        .output()
        .unwrap();

    assert!(qpdf.status.success());
    assert!(flpdf.status.success());
    let qpdf_stdout = String::from_utf8_lossy(&qpdf.stdout);
    let qpdf_first_line = qpdf_stdout
        .lines()
        .next()
        .expect("qpdf must emit one page identity line");
    assert!(
        String::from_utf8_lossy(&flpdf.stdout).starts_with(qpdf_first_line),
        "flpdf page identity must retain qpdf's first line: {:?}",
        flpdf.stdout
    );
}

#[test]
fn ordinary_show_pages_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-pages", ONE_PAGE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-pages", ONE_PAGE_PDF])
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_pages_with_images_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = PathBuf::from(directory.path()).join("input.pdf");
    fs::write(&input, one_page_with_image_pdf()).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-pages", "--with-images"])
        .arg(&input)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-pages", "--with-images"])
        .arg(&input)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_pages_with_images_omits_empty_image_section_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-pages", "--with-images", ONE_PAGE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-pages", "--with-images", ONE_PAGE_PDF])
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_npages_completes_repair_warnings_with_status_three() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-npages", REPAIRABLE_PDF])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--repair", "--show-npages", REPAIRABLE_PDF])
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(3));
    assert_eq!(flpdf.status.code(), Some(3));
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn ordinary_show_npages_matches_qpdf_without_weak_crypto_advisory() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--show-npages",
            "--allow-weak-crypto",
            "--password=user-v2",
            WEAK_RC4_PDF,
        ])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--show-npages",
            "--allow-weak-crypto",
            "--password=user-v2",
            WEAK_RC4_PDF,
        ])
        .output()
        .unwrap();

    assert!(qpdf.status.success());
    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert!(!String::from_utf8_lossy(&flpdf.stderr).contains("encrypted PDF uses weak crypto"));
}

#[test]
fn ordinary_show_pages_omits_effective_inherited_attributes_like_qpdf() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
          /MediaBox [0 0 612 792] /Rotate 90 \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );
    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), &bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--show-pages"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(format!("page 1: 3 0 R{EOL}  content:{EOL}    4 0 R{EOL}"));
}

#[test]
fn ordinary_show_pages_reports_malformed_contents_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--show-pages",
            "../../tests/fixtures/compat/chained-indirect-contents.pdf",
        ])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--show-pages",
            "../../tests/fixtures/compat/chained-indirect-contents.pdf",
        ])
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

fn pdf_with_catalog_and_optional_pages(pages_object: Option<&[u8]>) -> tempfile::NamedTempFile {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog = if pages_object.is_some() {
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice()
    } else {
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice()
    };
    let mut offsets = vec![bytes.len()];
    bytes.extend_from_slice(catalog);
    if let Some(pages_object) = pages_object {
        offsets.push(bytes.len());
        bytes.extend_from_slice(pages_object);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), bytes).unwrap();
    file
}

fn assert_show_npages_matches_qpdf(path: &std::path::Path) {
    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-npages", path.to_str().unwrap()])
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-npages", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "qpdf stdout={:?} stderr={:?}; flpdf stdout={:?} stderr={:?}",
        qpdf.stdout,
        qpdf.stderr,
        flpdf.stdout,
        flpdf.stderr
    );
    assert_eq!(
        normalize_newlines(&flpdf.stdout),
        normalize_newlines(&qpdf.stdout)
    );
    assert_eq!(
        normalize_newlines(&flpdf.stderr),
        normalize_newlines(&qpdf.stderr)
    );
}

#[test]
fn show_npages_reads_the_present_pages_count_without_walking_kids() {
    if skip_if_qpdf_missing() {
        return;
    }

    let file = pdf_with_catalog_and_optional_pages(Some(
        b"2 0 obj\n<< /Type /Pages /Kids [] /Count 99 >>\nendobj\n",
    ));

    assert_show_npages_matches_qpdf(file.path());
}

#[test]
fn show_npages_preserves_qpdf_missing_pages_warning_and_status() {
    if skip_if_qpdf_missing() {
        return;
    }

    let file = pdf_with_catalog_and_optional_pages(None);

    assert_show_npages_matches_qpdf(file.path());
}

#[test]
fn show_npages_reports_negative_count_verbatim_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }

    let file = pdf_with_catalog_and_optional_pages(Some(
        b"2 0 obj\n<< /Type /Pages /Kids [] /Count -5 >>\nendobj\n",
    ));

    assert_show_npages_matches_qpdf(file.path());
}

#[test]
fn show_npages_matches_qpdf_on_non_integer_count() {
    if skip_if_qpdf_missing() {
        return;
    }

    let file = pdf_with_catalog_and_optional_pages(Some(
        b"2 0 obj\n<< /Type /Pages /Kids [] /Count (bad) >>\nendobj\n",
    ));

    assert_show_npages_matches_qpdf(file.path());
}
