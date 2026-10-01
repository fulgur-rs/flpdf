//! qpdf 11.9.0 parity for a direct primary Catalog in multi-source `--pages`.

use assert_cmd::Command;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

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

fn assert_cli_output_matches(label: &str, actual: &Output, expected: &Output) {
    assert_eq!(
        actual.status.code(),
        expected.status.code(),
        "{label}: exit status differs"
    );
    assert_eq!(actual.stdout, expected.stdout, "{label}: stdout differs");
    assert_eq!(actual.stderr, expected.stderr, "{label}: stderr differs");
}

#[test]
fn pages_subcommands_match_qpdf_outputs_exactly() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping native pages report differential");
        return;
    }

    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let qpdf_npages = ProcessCommand::new("qpdf")
        .args(["--show-npages"])
        .arg(&input)
        .output()
        .expect("run qpdf --show-npages");
    let flpdf_npages = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["pages", "--show-npages"])
        .arg(&input)
        .output()
        .expect("run flpdf pages --show-npages");
    assert_cli_output_matches("pages --show-npages", &flpdf_npages, &qpdf_npages);

    let qpdf_pages = ProcessCommand::new("qpdf")
        .args(["--show-pages"])
        .arg(&input)
        .output()
        .expect("run qpdf --show-pages");
    let flpdf_pages = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("pages")
        .arg(&input)
        .output()
        .expect("run flpdf pages");
    assert_cli_output_matches("pages", &flpdf_pages, &qpdf_pages);
}

#[test]
fn pages_subcommands_preserve_qpdf_repair_warning_order() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping native pages warning differential");
        return;
    }

    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    for (label, qpdf_flag, flpdf_flag) in [
        (
            "pages --show-npages",
            "--show-npages",
            Some("--show-npages"),
        ),
        ("pages", "--show-pages", None),
    ] {
        let qpdf = ProcessCommand::new("qpdf")
            .arg(qpdf_flag)
            .arg(&input)
            .output()
            .expect("run qpdf page inspection on repairable input");
        let mut flpdf = Command::cargo_bin("flpdf").expect("flpdf binary");
        flpdf.env("FLPDF_PROGNAME", "qpdf").arg("pages");
        if let Some(flag) = flpdf_flag {
            flpdf.arg(flag);
        }
        let flpdf = flpdf
            .arg(&input)
            .output()
            .expect("run flpdf page inspection on repairable input");

        assert_eq!(qpdf.status.code(), Some(3), "qpdf warning exit for {label}");
        assert_cli_output_matches(label, &flpdf, &qpdf);
    }
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
