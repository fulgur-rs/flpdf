//! qpdf 11.9.0 `--keep-files-open` and threshold parity.
//!
//! The qpdf oracle is `vendor/qpdf-qtest/keep-files-open.test`: its four
//! cases exercise the automatic distinct-file threshold and explicit y/n
//! overrides on the `--empty --pages` route.  The observable contract is the
//! verbose selection line; the output PDF is also required to be created.

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;

fn minimal_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf")
}

fn run_keep_files_open_case(
    file_count: usize,
    extra_options: &[&str],
    expected_selection: Option<&str>,
) {
    let temp = tempfile::tempdir().expect("temporary qtest input directory");
    let fixture = minimal_fixture();
    let mut inputs = Vec::with_capacity(file_count);
    for number in 1..=file_count {
        let path = temp.path().join(format!("{number:03}-kfo.pdf"));
        fs::copy(&fixture, &path).expect("copy qtest input fixture");
        inputs.push(path);
    }
    let output = temp.path().join("a.pdf");

    let mut args = vec!["--verbose", "--static-id", "--empty"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
    args.extend(extra_options.iter().map(|option| (*option).to_owned()));
    args.push("--pages".to_owned());
    args.extend(inputs.iter().map(|path| path.display().to_string()));
    args.push("--".to_owned());
    args.push(output.display().to_string());

    let result = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(&args)
        .output()
        .expect("flpdf invocation");
    assert!(
        result.status.success(),
        "keep-files-open case failed: status={:?}\nstdout={}\nstderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    match expected_selection {
        Some(expected_selection) => {
            assert!(
                stdout.contains(expected_selection),
                "missing selection line {expected_selection:?} in:\n{stdout}"
            );
            assert!(
                !stdout.contains(if expected_selection.ends_with("=y\n") {
                    "selecting --keep-open-files=n"
                } else {
                    "selecting --keep-open-files=y"
                }),
                "opposite keep-files-open selection was emitted:\n{stdout}"
            );
        }
        None => assert!(
            !stdout.contains("selecting --keep-open-files="),
            "explicit keep-files-open must not emit an automatic selection line:\n{stdout}"
        ),
    }
    assert!(
        output.is_file(),
        "qpdf-shaped page job did not write output"
    );
    let page_count = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--show-npages", output.to_str().expect("output path")])
        .output()
        .expect("show output page count");
    assert!(
        page_count.status.success(),
        "output page-count inspection failed: {}",
        String::from_utf8_lossy(&page_count.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&page_count.stdout).trim(),
        file_count.to_string(),
        "empty page job must copy one page from each selected source"
    );
}

#[test]
fn automatic_threshold_disables_keep_files_open_above_boundary() {
    run_keep_files_open_case(
        51,
        &["--keep-files-open-threshold=50"],
        Some("flpdf: selecting --keep-open-files=n\n"),
    );
}

#[test]
fn automatic_threshold_keeps_files_open_within_boundary() {
    run_keep_files_open_case(10, &[], Some("flpdf: selecting --keep-open-files=y\n"));
}

#[test]
fn threshold_accepts_qpdf_numeric_prefix_conversion() {
    run_keep_files_open_case(
        1,
        &["--keep-files-open-threshold=50junk"],
        Some("flpdf: selecting --keep-open-files=y\n"),
    );
}

#[test]
fn negative_threshold_matches_qpdf_unsigned_conversion_error() {
    let temp = tempfile::tempdir().expect("temporary qtest input directory");
    let fixture = minimal_fixture();
    let input = temp.path().join("001-kfo.pdf");
    fs::copy(fixture, &input).expect("copy qtest input fixture");
    let output = temp.path().join("a.pdf");

    let result = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--verbose",
            "--static-id",
            "--empty",
            "--keep-files-open-threshold=-1",
            "--pages",
        ])
        .arg(&input)
        .args(["--"])
        .arg(&output)
        .output()
        .expect("flpdf invocation");
    assert!(!result.status.success(), "negative threshold must fail");
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("underflow converting -1 to 64-bit unsigned integer"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !output.exists(),
        "failed configuration must not write output"
    );
}

#[test]
fn explicit_keep_files_open_y_overrides_automatic_selection() {
    run_keep_files_open_case(9, &["--keep-files-open=y"], None);
}

#[test]
fn explicit_keep_files_open_n_overrides_automatic_selection() {
    run_keep_files_open_case(9, &["--keep-files-open=n"], None);
}
