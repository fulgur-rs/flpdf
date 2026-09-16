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

/// The first-page trailer: the `trailer << ... >>` block that precedes the
/// linearized file's `startxref\n0`. Only this one walks the input trailer's
/// keys; the main (second-half) trailer writes `/Size` unconditionally
/// (`QPDFWriter.cc:1170-1172`), so a whole-file search would always match.
fn first_page_trailer(output: &[u8]) -> &[u8] {
    let start = output
        .windows(b"trailer <<".len())
        .position(|window| window == b"trailer <<")
        .expect("linearized output has a first-page trailer");
    let end = output[start..]
        .windows(b">>".len())
        .position(|window| window == b">>")
        .expect("first-page trailer is closed")
        + start;
    &output[start..end]
}

/// A trailer whose size key is misspelled must not gain a real `/Size`.
///
/// qpdf's `writeTrailer` walks the input trailer's keys and substitutes the
/// object count only for a key literally named `/Size`
/// (`QPDFWriter.cc:1174-1191`); the `t_lin_first` `/Prev` is appended inside
/// that same branch. A trailer spelling it `/Siqe` therefore keeps the
/// misspelling and gains neither key.
#[test]
fn linearize_omits_size_and_prev_when_the_trailer_key_is_misspelled() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

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
    // Misspell the trailer's size key exactly as qtest's bad9.pdf does.
    let trailer_start = bytes
        .windows(b"trailer".len())
        .rposition(|window| window == b"trailer")
        .expect("fixture has a trailer");
    let size_offset = bytes[trailer_start..]
        .windows(b"/Size".len())
        .position(|window| window == b"/Size")
        .expect("fixture trailer has /Size")
        + trailer_start;
    bytes[size_offset..size_offset + b"/Size".len()].copy_from_slice(b"/Siqe");

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("input.pdf");
    let qpdf_output_path = directory.path().join("qpdf.pdf");
    std::fs::write(&input, &bytes).expect("write misspelled-size fixture");

    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--static-id", "--linearize"])
        .arg(&input)
        .arg(&qpdf_output_path)
        .output()
        .expect("qpdf 11.9.0 must run");
    assert!(
        qpdf.status.success(),
        "qpdf linearization failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_output = std::fs::read(&qpdf_output_path).expect("read qpdf output");
    assert!(
        !first_page_trailer(&qpdf_output)
            .windows(b"/Size".len())
            .any(|w| w == b"/Size"),
        "oracle guard: qpdf must not write /Size into the first-page trailer \
         for a misspelled key"
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open misspelled-size fixture");
    let settings = WriterTestSettings {
        static_id: true,
        ..WriterTestSettings::default()
    };
    let flpdf_output = write_linearized_with_settings(&mut pdf, &settings)
        .expect("flpdf linearization must succeed");

    let flpdf_first_trailer = first_page_trailer(&flpdf_output);
    assert!(
        !flpdf_first_trailer
            .windows(b"/Size".len())
            .any(|w| w == b"/Size"),
        "flpdf must not add a /Size the input trailer never had"
    );
    assert!(
        !flpdf_first_trailer
            .windows(b"/Prev".len())
            .any(|w| w == b"/Prev"),
        "flpdf must not add the /Size branch's /Prev either"
    );
    assert!(
        flpdf_first_trailer
            .windows(b"/Siqe".len())
            .any(|w| w == b"/Siqe"),
        "the misspelled key itself must survive verbatim"
    );
    #[cfg(feature = "qpdf-zlib-compat")]
    assert_eq!(
        flpdf_output, qpdf_output,
        "misspelled-size linearized output must remain byte-identical to qpdf"
    );
}
