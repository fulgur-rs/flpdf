// qpdf 11.9.0 derives its CLI prefix from argv[0] (`qpdf/qpdf.cc:27-43`,
// `QUtil.cc:788-803`) and installs the same name on QPDFJob's logger via
// `initializeFromArgv` (`QPDFJob_argv.cc:418-429`). flpdf's explicit env is a
// test override so the same rendered CLI diagnostics can be exercised here.
use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use std::process::Output;

// qpdf-shaped argument errors must retain the argv-derived CLI prefix. These
// cases exercise flpdf's canonical qpdf preflight with both the default name
// and the existing qtest override. The native static-ID notice below is an
// FLPDF-only diagnostic and deliberately keeps its product label.
fn run_flpdf(args: &[String], prefix: Option<&str>) -> Output {
    let mut command = Command::cargo_bin("flpdf").expect("flpdf binary");
    match prefix {
        Some(prefix) => {
            command.env("FLPDF_PROGNAME", prefix);
        }
        None => {
            command.env_remove("FLPDF_PROGNAME");
        }
    }
    command.args(args).output().expect("run flpdf")
}

#[test]
fn qpdf_cli_usage_errors_follow_progname() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/minimal.pdf")
        .canonicalize()
        .expect("minimal PDF fixture")
        .to_string_lossy()
        .into_owned();
    let temp = tempfile::tempdir().expect("temporary directory");
    let output = temp.path().join("out.pdf").to_string_lossy().into_owned();
    let cases: Vec<(&str, Vec<String>, i32, &str)> = vec![
        (
            "invalid compress-streams value on the ordinary rewrite route",
            vec![
                "--compress-streams=x".into(),
                "--empty".into(),
                output.clone(),
            ],
            2,
            "--compress-streams must be given as",
        ),
        (
            "out-of-range minimum PDF version on the flat CLI route",
            vec![
                "--min-version=2147483648".into(),
                "--empty".into(),
                output.clone(),
            ],
            2,
            "2147483648",
        ),
        (
            "unknown qpdf help topic",
            vec!["--help=bogus".into()],
            2,
            "unknown help option bogus",
        ),
        (
            "invalid compress-streams value on the page-output route",
            vec![
                "--compress-streams=x".into(),
                "--pages".into(),
                fixture.clone(),
                "1".into(),
                "--".into(),
                output.clone(),
            ],
            2,
            "--compress-streams must be given as",
        ),
        (
            "invalid JSON key",
            vec![
                "--json-output".into(),
                "--json=2".into(),
                "--json-key=bogus".into(),
                fixture.clone(),
            ],
            2,
            "--json-key must be given as",
        ),
        (
            "JSON output with version one",
            vec!["--json-output".into(), "--json=1".into(), fixture.clone()],
            2,
            "json key \"qpdf\" is only valid for json version > 1",
        ),
        (
            "invalid JSON stream-data value",
            vec![
                "--json-output".into(),
                "--json-stream-data=bogus".into(),
                fixture,
            ],
            2,
            "--json-stream-data must be given as",
        ),
    ];

    for (name, args, expected_code, message) in cases {
        for (prefix, env) in [("flpdf", None), ("qpdf", Some("qpdf"))] {
            let output = run_flpdf(&args, env);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(expected_code),
                "{name}, prefix={prefix}: {stderr}"
            );
            let first_nonblank_line = stderr
                .lines()
                .find(|line| !line.is_empty())
                .unwrap_or_default();
            assert!(
                first_nonblank_line.starts_with(&format!("{prefix}: ")),
                "{name}, expected {prefix}: prefix, got {stderr:?}"
            );
            if prefix == "qpdf" {
                assert!(
                    !stderr.contains("flpdf:"),
                    "{name}, stale flpdf prefix remained: {stderr:?}"
                );
            }
            assert!(
                stderr.contains(message),
                "{name}, expected message containing {message:?}, got {stderr:?}"
            );
        }
    }
}

#[test]
fn flpdf_only_static_id_notice_keeps_product_prefix_under_qpdf_shim() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let output = temp.path().join("out.pdf");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/minimal.pdf")
        .canonicalize()
        .expect("minimal PDF fixture");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("rewrite")
        .arg("--static-id")
        .arg(fixture)
        .arg(output)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "flpdf: warning: --static-id is for testing only",
        ))
        .stderr(predicate::str::contains("qpdf: warning:").not());
}
