//! qpdf 11.9.0 parity for linearized output's source PDF version.
//!
//! Full output-byte comparison is enabled by the qpdf-zlib-compat feature;
//! the default backend intentionally has different, valid Flate bytes.

mod common;

use common::{build_pdf, write_linearized_with_settings, WriterTestSettings};
use flpdf::Pdf;
use std::io::Cursor;
use std::process::Command;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let available = Command::new("qpdf")
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
        panic!("qpdf 11.9.0 is required for linearization version parity");
    }
    available
}

fn one_page_pdf_with_version(version: &str) -> Vec<u8> {
    let mut bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_owned(),
            ),
        ],
        1,
    );
    let header = format!("%PDF-{version}");
    bytes[..header.len()].copy_from_slice(header.as_bytes());
    bytes
}

#[test]
fn linearize_preserves_pdf_header_version_below_1_2_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    for version in ["1.0", "1.1"] {
        let input = directory.path().join(format!("input-{version}.pdf"));
        let qpdf_output = directory.path().join(format!("qpdf-{version}.pdf"));
        let input_bytes = one_page_pdf_with_version(version);
        std::fs::write(&input, &input_bytes).expect("write low-version fixture");

        let qpdf = Command::new("qpdf")
            .args(["--warning-exit-0", "--static-id", "--linearize"])
            .arg(&input)
            .arg(&qpdf_output)
            .output()
            .expect("qpdf 11.9.0 must run");
        assert!(
            qpdf.status.success(),
            "qpdf linearization failed for PDF {version}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let mut pdf = Pdf::open(Cursor::new(input_bytes)).expect("open low-version fixture");
        let settings = WriterTestSettings {
            static_id: true,
            ..WriterTestSettings::default()
        };
        let flpdf_output = write_linearized_with_settings(&mut pdf, &settings)
            .expect("flpdf linearization must succeed");

        let qpdf_output = std::fs::read(&qpdf_output).expect("read qpdf output");
        assert_eq!(
            &flpdf_output[..8],
            &qpdf_output[..8],
            "linearized PDF {version} header must match qpdf"
        );
        #[cfg(feature = "qpdf-zlib-compat")]
        assert_eq!(
            flpdf_output, qpdf_output,
            "linearized PDF {version} must remain byte-identical to qpdf"
        );
    }
}
