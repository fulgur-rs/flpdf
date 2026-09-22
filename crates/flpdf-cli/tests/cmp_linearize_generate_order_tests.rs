//! qpdf 11.9.0 parity for linearized Generate setup order around direct /Kids pages.

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::process::Command as ProcessCommand;

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let available = ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false);
    if !available && std::env::var_os("CI").is_some() {
        panic!("{EXPECTED_QPDF_VERSION} is required for linearization order parity");
    }
    available
}

fn direct_kid_boundary_fixture(eligible: usize) -> Vec<u8> {
    let mut objects = BTreeMap::new();
    let qtest_refs = (3..(3 + eligible))
        .map(|object| format!("{object} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    objects.insert(
        1,
        format!("<< /Type /Catalog /Pages 2 0 R /QTest [{qtest_refs}] >>").into_bytes(),
    );
    objects.insert(
        2,
        b"<< /Type /Pages /Kids [<< /Type /Page /MediaBox [0 0 1 1] >>] /Count 1 >>".to_vec(),
    );
    for object in 3..(3 + eligible) {
        objects.insert(object, format!("<< /K {object} >>").into_bytes());
    }

    let mut bytes = b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (object, body) in &objects {
        offsets.insert(*object, bytes.len());
        bytes.extend_from_slice(format!("{object} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = bytes.len();
    let size = 3 + eligible;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for object in 1..size {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&object]).as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn objstm_count(bytes: &[u8]) -> usize {
    bytes
        .windows(b"/Type /ObjStm".len())
        .filter(|window| *window == b"/Type /ObjStm")
        .count()
}
#[test]
fn linearized_generate_plans_objstm_before_repairing_direct_kids() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("direct-kid-boundary.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, direct_kid_boundary_fixture(98)).expect("write boundary fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args([
            "--linearize",
            "--object-streams=generate",
            "--deterministic-id",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must linearize boundary fixture");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "rewrite",
            "--linearize",
            "--object-streams=generate",
            "--deterministic-id",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must linearize boundary fixture");

    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf direct-kid warning status"
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "exit codes differ");
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "direct-kid diagnostics differ"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert_eq!(objstm_count(&qpdf_bytes), 1, "qpdf oracle container count");
    assert_eq!(
        objstm_count(&flpdf_bytes),
        objstm_count(&qpdf_bytes),
        "ObjStm container count differs"
    );
    assert_eq!(flpdf_bytes, qpdf_bytes, "output bytes differ from qpdf");
}
