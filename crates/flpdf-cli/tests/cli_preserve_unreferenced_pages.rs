//! Multi-source `--pages` preserve-unreferenced parity against qpdf 11.9.0.
//!
//! qpdf keeps the primary document as the output/base QPDF while it copies
//! foreign pages (`libqpdf/QPDFJob.cc:2360-2632`). Therefore its writer-level
//! `--preserve-unreferenced` option still sees genuinely unreferenced objects
//! from the primary input. The CLI's fresh merged-document route must retain
//! those objects as well.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

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

fn run_qpdf(args: &[&str]) -> std::process::Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn normalize_qdf(input: &Path, output: &Path) -> Vec<u8> {
    let result = run_qpdf(&[
        "--qdf",
        "--object-streams=disable",
        "--no-original-object-ids",
        "--preserve-unreferenced",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "qpdf QDF normalization failed for {}: {}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(output).expect("normalized QDF should be readable")
}

fn has_primary_orphan_marker(qdf: &[u8]) -> bool {
    qdf.windows(b"unreachable root".len())
        .any(|window| window == b"unreachable root")
}

#[test]
fn multi_source_pages_preserve_primary_unreferenced_objects_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let primary = fixture("null-visible-preserve-unreachable.pdf");
    let foreign = fixture("one-page.pdf");
    let qpdf_default_output = temp.path().join("qpdf-default.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_default_output = temp.path().join("flpdf-default.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let qpdf_default_qdf = temp.path().join("qpdf-default-qdf.pdf");
    let qpdf_qdf = temp.path().join("qpdf-qdf.pdf");
    let flpdf_default_qdf = temp.path().join("flpdf-default-qdf.pdf");
    let flpdf_qdf = temp.path().join("flpdf-qdf.pdf");

    let qpdf_default_result = run_qpdf(&[
        primary.to_str().unwrap(),
        "--pages",
        foreign.to_str().unwrap(),
        "1",
        "--",
        qpdf_default_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_default_result.status.success(),
        "qpdf default multi-source --pages failed: {}",
        String::from_utf8_lossy(&qpdf_default_result.stderr)
    );

    let qpdf_result = run_qpdf(&[
        "--preserve-unreferenced",
        primary.to_str().unwrap(),
        "--pages",
        foreign.to_str().unwrap(),
        "1",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert!(
        qpdf_result.status.success(),
        "qpdf multi-source --pages failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--pages"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_default_output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--preserve-unreferenced", "--pages"])
        .arg(&foreign)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_default_qdf = normalize_qdf(&qpdf_default_output, &qpdf_default_qdf);
    let qpdf_qdf = normalize_qdf(&qpdf_output, &qpdf_qdf);
    let flpdf_default_qdf = normalize_qdf(&flpdf_default_output, &flpdf_default_qdf);
    let flpdf_qdf = normalize_qdf(&flpdf_output, &flpdf_qdf);
    assert!(
        !has_primary_orphan_marker(&qpdf_default_qdf),
        "qpdf default output must drop the primary orphan marker"
    );
    assert!(
        !has_primary_orphan_marker(&flpdf_default_qdf),
        "flpdf default output must keep dropping the primary orphan marker"
    );
    assert!(
        has_primary_orphan_marker(&qpdf_qdf),
        "qpdf preserve output must retain the primary orphan marker"
    );
    assert!(
        has_primary_orphan_marker(&flpdf_qdf),
        "flpdf preserve output must retain the primary orphan marker"
    );
}
