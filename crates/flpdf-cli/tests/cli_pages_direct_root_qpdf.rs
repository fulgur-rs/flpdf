//! qpdf 11.9.0 parity for a direct primary Catalog in multi-source `--pages`.

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as ProcessCommand;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
}

fn show_npages(path: &Path) -> Vec<u8> {
    let output = ProcessCommand::new("qpdf")
        .args(["--show-npages"])
        .arg(path)
        .output()
        .expect("run qpdf --show-npages");
    assert!(
        output.status.success(),
        "qpdf --show-npages failed: {output:?}"
    );
    output.stdout
}

fn check(path: &Path) {
    let output = ProcessCommand::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .expect("run qpdf --check");
    assert!(output.status.success(), "qpdf --check failed: {output:?}");
}

#[test]
fn multi_source_pages_accepts_a_direct_root_primary_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping direct-root pages differential");
        return;
    }

    let temporary = tempfile::tempdir().expect("create direct-root pages directory");
    let primary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/direct-root-one-page.pdf");
    let secondary =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let qpdf_output = temporary.path().join("qpdf.pdf");
    let flpdf_output = temporary.path().join("flpdf.pdf");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id", "--qdf", "--pages", ".", "1"])
        .arg(&secondary)
        .args(["1", "--"])
        .arg(&primary)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf direct-root pages oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id", "--qdf", "--pages", ".", "1"])
        .arg(&secondary)
        .args(["1", "--"])
        .arg(&primary)
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf direct-root pages");

    assert_eq!(qpdf.status.code(), Some(0), "qpdf failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);

    check(&qpdf_output);
    check(&flpdf_output);
    assert_eq!(show_npages(&flpdf_output), show_npages(&qpdf_output));

    let trailer = ProcessCommand::new("qpdf")
        .args(["--show-object=trailer"])
        .arg(&qpdf_output)
        .output()
        .expect("inspect qpdf direct-root pages trailer");
    assert!(trailer.status.success());
    assert!(String::from_utf8_lossy(&trailer.stdout).contains("/Root <<"));
}
