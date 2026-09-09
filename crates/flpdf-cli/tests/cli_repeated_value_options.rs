use assert_cmd::Command;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/inherited-resources-one-page.pdf")
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

fn run_qpdf(args: &[OsString]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[OsString]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn repeated_args(first: &str, second: &str, output: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--static-id"),
        OsString::from("--stream-data=uncompress"),
        OsString::from("--newline-before-endstream=y"),
        OsString::from(first),
        OsString::from(second),
        fixture().into_os_string(),
        OsString::from("--pages"),
        OsString::from("."),
        OsString::from("1"),
        OsString::from("--"),
        output.as_os_str().to_owned(),
    ]
}

#[test]
fn repeated_value_options_follow_qpdf_last_setting_order() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    for (first, second) in [
        (
            "--remove-unreferenced-resources=no",
            "--remove-unreferenced-resources=yes",
        ),
        (
            "--remove-unreferenced-resources=yes",
            "--remove-unreferenced-resources=no",
        ),
    ] {
        let qpdf_output = temp.path().join(format!("qpdf-{first}-{second}.pdf"));
        let flpdf_output = temp.path().join(format!("flpdf-{first}-{second}.pdf"));
        let qpdf_args = repeated_args(first, second, &qpdf_output);
        let flpdf_args = repeated_args(first, second, &flpdf_output);

        let qpdf = run_qpdf(&qpdf_args);
        assert!(
            qpdf.status.success(),
            "qpdf failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let flpdf = run_flpdf(&flpdf_args);
        assert!(
            flpdf.status.success(),
            "flpdf failed for {first} then {second}: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
        assert_eq!(
            fs::read(&flpdf_output).expect("flpdf output"),
            fs::read(&qpdf_output).expect("qpdf output"),
            "last value must win for {first} then {second}"
        );
    }
}

/// qpdf's input and output selectors are the exception to last-setting order:
/// each occurrence reaches its own setter, and the setter rejects the second
/// one because the first already chose the input or output
/// (`QPDFJob_config.cc:27-39,54-62`).
#[test]
fn repeated_input_and_output_selectors_stay_usage_errors() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let output = temp.path().join("out.pdf");
    let cases: [Vec<OsString>; 2] = [
        vec![
            OsString::from("--static-id"),
            OsString::from("--empty"),
            OsString::from("--empty"),
            output.as_os_str().to_owned(),
        ],
        vec![
            OsString::from("--static-id"),
            fixture().into_os_string(),
            OsString::from("--replace-input"),
            OsString::from("--replace-input"),
        ],
    ];

    for args in cases {
        let qpdf = run_qpdf(&args);
        let flpdf = run_flpdf(&args);
        assert_eq!(
            qpdf.status.code(),
            Some(2),
            "qpdf should reject {args:?}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?}: status");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{args:?}: stderr");
        assert!(!output.exists(), "{args:?}: a rejected job must not write");
    }
}

/// qpdf fails at the first offending token in argv order, so an unknown option
/// ahead of a repeated selector wins and one behind it does not.
#[test]
fn the_first_usage_failure_in_argv_order_is_reported() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let output = temp.path().join("out.pdf");
    let cases: [Vec<OsString>; 4] = [
        vec![
            OsString::from("--definitely-unknown"),
            OsString::from("--empty"),
            OsString::from("--empty"),
            output.as_os_str().to_owned(),
        ],
        vec![
            OsString::from("--empty"),
            OsString::from("--empty"),
            OsString::from("--definitely-unknown"),
            output.as_os_str().to_owned(),
        ],
        // A later prescan failure must not leapfrog the earlier selector
        // either: qpdf stops at the second `--empty` callback.
        vec![
            OsString::from("--empty"),
            OsString::from("--empty"),
            OsString::from("--compression-level"),
            output.as_os_str().to_owned(),
        ],
        vec![
            OsString::from("--empty"),
            OsString::from("--empty"),
            OsString::from("--stream-data=bogus"),
            output.as_os_str().to_owned(),
        ],
    ];

    for args in cases {
        let qpdf = run_qpdf(&args);
        let flpdf = run_flpdf(&args);
        assert_eq!(qpdf.status.code(), Some(2), "qpdf should reject {args:?}");
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{args:?}: status");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{args:?}: stderr");
    }
}

/// A value that fails its own conversion is qpdf's error even when a later
/// occurrence would have been valid: `Config::compressionLevel` runs
/// `QUtil::string_to_int` for every argv occurrence
/// (`QPDFJob_config.cc:135-139`). Self-override is therefore limited to the
/// choice-valued options clap validates as it parses.
#[test]
fn an_overridden_value_is_still_validated() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let output = temp.path().join("out.pdf");
    let args = vec![
        OsString::from("--static-id"),
        OsString::from("--compression-level=999999999999999999999999999999999999"),
        OsString::from("--compression-level=9"),
        fixture().into_os_string(),
        output.as_os_str().to_owned(),
    ];

    let qpdf = run_qpdf(&args);
    let flpdf = run_flpdf(&args);
    assert_eq!(qpdf.status.code(), Some(2), "qpdf rejects the first value");
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert!(!output.exists(), "a rejected job must not write");
}

/// Every option whose value the argv layer validates at each occurrence can
/// take the last setting, including the ones clap types as plain strings.
#[test]
fn choice_options_validated_per_occurrence_take_the_last_value() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    for (first, second) in [
        ("--compress-streams=y", "--compress-streams=n"),
        ("--flatten-annotations=screen", "--flatten-annotations=all"),
    ] {
        let qpdf_output = temp.path().join(format!("qpdf-{first}-{second}.pdf"));
        let flpdf_output = temp.path().join(format!("flpdf-{first}-{second}.pdf"));
        let build = |output: &Path| {
            let mut args = vec![OsString::from("--static-id")];
            if !first.starts_with("--compress-streams") {
                // Keep DEFLATE out of the comparison: the default backend is
                // miniz_oxide, whose bytes may differ from qpdf's zlib. The
                // `--compress-streams` case already ends uncompressed.
                args.push(OsString::from("--stream-data=uncompress"));
            }
            args.extend([
                OsString::from(first),
                OsString::from(second),
                fixture().into_os_string(),
                output.as_os_str().to_owned(),
            ]);
            args
        };

        let qpdf = run_qpdf(&build(&qpdf_output));
        let flpdf = run_flpdf(&build(&flpdf_output));
        assert!(
            qpdf.status.success(),
            "qpdf failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{first} {second}");
        assert_eq!(
            fs::read(&flpdf_output).expect("flpdf output"),
            fs::read(&qpdf_output).expect("qpdf output"),
            "last value must win for {first} then {second}"
        );
    }
}
