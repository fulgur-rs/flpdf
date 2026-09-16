//! qpdf 11.9.0 parity for the `--collate` page-specification usage error.
//!
//! `QPDFJob::handlePageSpecs` rejects a `--collate` list whose length is
//! neither one nor the number of page specifications
//! (`libqpdf/QPDFJob.cc:2474-2479`). It raises that through `usage`, so the CLI
//! renders qpdf's usage exit: a leading blank line, the program name and the
//! message, then the four-line `For help:` block (`qpdf/qpdf.cc:12-22,37-38`).
//!
//! The error surfaces from inside the page-selection lifecycle rather than from
//! `checkConfiguration`, so it reaches a different reporting site than the
//! other usage errors; this pins that it still takes the usage exit on each of
//! the three ways a primary document is created — an ordinary file, `--empty`,
//! and `--json-input`.

use assert_cmd::Command as CargoCommand;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

/// Return `true` when the caller should skip because qpdf 11.9.0 is missing.
///
/// CI installs qpdf, so a missing oracle there is a build failure rather than a
/// silent gap.
#[must_use]
fn skip_without_qpdf_11_9() -> bool {
    let version = ShellCommand::new("qpdf").arg("--version").output().ok();
    if version.as_ref().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).lines().next() == Some("qpdf version 11.9.0")
    }) {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for collate usage parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

#[test]
fn collate_count_mismatch_takes_qpdf_usage_exit() {
    if skip_without_qpdf_11_9() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let input_arg = input.to_string_lossy().into_owned();

    let json_input = temp.path().join("complete.json");
    let dumped = ShellCommand::new("qpdf")
        .arg("--json-output=2")
        .arg(&input)
        .arg(&json_input)
        .status()
        .expect("qpdf runs");
    assert_eq!(dumped.code(), Some(0), "qpdf must dump the JSON input");
    let json_arg = json_input.to_string_lossy().into_owned();

    // One case per branch of `QPDFJob::create_qpdf`'s document creation.
    let cases: [(&str, Vec<String>); 3] = [
        (
            "ordinary file",
            vec![input_arg.clone(), "--pages".into(), ".".into(), "1".into()],
        ),
        (
            "--empty",
            vec![
                "--empty".into(),
                "--pages".into(),
                input_arg.clone(),
                "1".into(),
            ],
        ),
        (
            "--json-input",
            vec![
                "--json-input".into(),
                json_arg,
                "--pages".into(),
                ".".into(),
                "1".into(),
            ],
        ),
    ];

    for (name, middle) in cases {
        let args = |output: &Path| -> Vec<String> {
            let mut all = vec!["--collate=1,2,3".to_string()];
            all.extend(middle.iter().cloned());
            all.push("--".to_string());
            all.push(output.to_string_lossy().into_owned());
            all
        };

        let expected = ShellCommand::new("qpdf")
            .args(args(&temp.path().join("qpdf.pdf")))
            .output()
            .expect("qpdf runs");
        assert_eq!(
            expected.status.code(),
            Some(2),
            "{name}: qpdf must reject the collate count mismatch"
        );

        let actual = CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .env("FLPDF_PROGNAME", "qpdf")
            .env("FLPDF_STATIC_ID_QUIET", "1")
            .args(args(&temp.path().join("flpdf.pdf")))
            .output()
            .unwrap();

        assert_eq!(
            actual.status.code(),
            expected.status.code(),
            "{name}: exit code must match qpdf"
        );
        assert!(actual.stdout.is_empty(), "{name}");
        assert_eq!(
            String::from_utf8_lossy(&actual.stderr),
            String::from_utf8_lossy(&expected.stderr),
            "{name}: stderr must match qpdf byte for byte"
        );
    }
}
