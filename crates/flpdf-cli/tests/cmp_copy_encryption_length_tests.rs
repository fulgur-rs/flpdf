//! qpdf 11.9.0 parity for malformed Standard-handler `/Length` copy output.
//!
//! Full output-byte comparison is enabled by the qpdf-zlib-compat feature;
//! the default backend intentionally has different, valid Flate bytes.

use std::path::Path;
use std::process::Command;

const PLAIN_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/one-page.pdf"
);
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
        panic!("qpdf 11.9.0 is required for malformed copy-encryption parity");
    }
    available
}

fn write_malformed_length_fixture(path: &Path) {
    let qpdf = Command::new("qpdf")
        .args([
            "--static-id",
            "--allow-weak-crypto",
            "--encrypt",
            "",
            "",
            "128",
            "--",
            PLAIN_FIXTURE,
        ])
        .arg(path)
        .output()
        .expect("qpdf 11.9.0 must create a V=2 donor");
    assert!(
        qpdf.status.success(),
        "qpdf donor creation failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let mut bytes = std::fs::read(path).expect("read encrypted fixture");
    let marker = b"/Length 128";
    let offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("encrypted fixture has a V=2 /Length entry");
    bytes[offset + 1..offset + 7].copy_from_slice(b"Wength");
    std::fs::write(path, bytes).expect("write malformed fixture");
}

#[test]
fn malformed_copy_encryption_length_warns_and_writes_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("malformed-length.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    write_malformed_length_fixture(&input);

    let qpdf = Command::new("qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf 11.9.0 must run");
    assert_eq!(qpdf.status.code(), Some(3), "qpdf warning status");
    assert!(qpdf.stdout.is_empty(), "qpdf warning path has no stdout");
    assert!(
        qpdf_output.exists(),
        "qpdf writes output despite the warning"
    );

    let flpdf = Command::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must run");
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf and qpdf warning status; flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert!(flpdf.stdout.is_empty(), "flpdf warning path has no stdout");
    assert!(
        flpdf_output.exists(),
        "flpdf writes output despite the warning"
    );
    assert_eq!(
        flpdf.stderr, qpdf.stderr,
        "copy-encryption type warning must match qpdf"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert!(!flpdf_bytes.is_empty(), "flpdf output must contain a PDF");
    assert_eq!(&flpdf_bytes[..8], &qpdf_bytes[..8], "output headers match");
    assert!(
        flpdf_bytes
            .windows(b"/Length 0".len())
            .any(|window| window == b"/Length 0"),
        "flpdf must preserve qpdf's zero-length fallback in the output dictionary"
    );

    #[cfg(feature = "qpdf-zlib-compat")]
    assert_eq!(
        flpdf_bytes, qpdf_bytes,
        "malformed copy-encryption output must be byte-identical to qpdf"
    );
}
