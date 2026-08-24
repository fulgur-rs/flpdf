//! JSON page-section traversal must reuse qpdf's per-document page list cache.

use assert_cmd::Command;
use std::path::Path;
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const PAGE_REPAIR_WARNING: &str =
    "operation for array attempted on object of type integer: treating as empty";

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
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

fn malformed_page_tree_pdf() -> Vec<u8> {
    let objects: [(u32, &[u8]); 4] = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Type /Pages /Parent 2 0 R /Kids 42 /Count 1 >>"),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = [0u64; 4];
    for (index, (number, body)) in objects.iter().enumerate() {
        offsets[index] = pdf.len() as u64;
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = pdf.len() as u64;
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn run_qpdf(input: &Path) -> Output {
    ShellCommand::new("qpdf")
        .args(["--json=2"])
        .arg(input)
        .output()
        .expect("qpdf should spawn")
}

#[test]
fn json_sections_reuse_the_qpdf_page_list_cache() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("malformed-page-tree.pdf");
    std::fs::write(&input, malformed_page_tree_pdf()).unwrap();

    let qpdf = run_qpdf(&input);
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2"])
        .arg(&input)
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(3));
    assert_eq!(flpdf.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&qpdf.stderr)
            .matches(PAGE_REPAIR_WARNING)
            .count(),
        1,
        "qpdf repairs the page tree once per document"
    );
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr)
            .matches(PAGE_REPAIR_WARNING)
            .count(),
        1,
        "flpdf must reuse the page list across JSON sections"
    );
    serde_json::from_slice::<serde_json::Value>(&qpdf.stdout).unwrap();
    serde_json::from_slice::<serde_json::Value>(&flpdf.stdout).unwrap();
}
