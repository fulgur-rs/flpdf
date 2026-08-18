//! qpdf 11.9.0 JSON input/update CLI boundary tests (flpdf-3yn9.16).
//!
//! The core JSON document boundary is tested in `flpdf`; this suite pins the
//! qpdf-shaped job ordering that consumes it: complete input creation,
//! update-before-transform, JSON output, and page-tree selection.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::process::Command as ShellCommand;

const COMPLETE_JSON: &str = "../../tests/fixtures/compat/json-input/complete.json";
const UPDATE_JSON: &str = "../../tests/fixtures/compat/json-input/update.json";
const MINIMAL_PDF: &str = "../../tests/fixtures/minimal.pdf";
const ONE_PAGE_PDF: &str = "../../tests/fixtures/compat/one-page.pdf";

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        });
    if version
        .as_deref()
        .is_some_and(|stdout| stdout.lines().next() == Some("qpdf version 11.9.0"))
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for cli_json_input oracle tests on CI: {version:?}");
    }
    eprintln!("skipping qpdf JSON-input oracle: qpdf 11.9.0 is not available");
    true
}

fn normalize_program_prefix(stderr: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(stderr.len());
    let lines: Vec<&[u8]> = stderr.split(|byte| *byte == b'\n').collect();
    for (index, line) in lines.iter().enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = [b"qpdf: ".as_slice(), b"flpdf: ".as_slice()]
            .iter()
            .find_map(|prefix| (*line).strip_prefix(*prefix))
            .unwrap_or(line);
        normalized.extend_from_slice(line);
        if index + 1 < lines.len() {
            normalized.push(b'\n');
        }
    }
    normalized
}

#[test]
fn normalize_program_prefix_treats_crlf_as_lf() {
    assert_eq!(
        normalize_program_prefix(b"qpdf: first\r\nqpdf: second\r\n"),
        b"first\nsecond\n"
    );
}

#[test]
fn json_input_creates_pdf_from_complete_document() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("created.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-input", COMPLETE_JSON])
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    let bytes = fs::read(&output).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-"),
        "JSON input must produce PDF bytes"
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf", "--json-stream-data=inline"])
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains("/Custom"))
        .stdout(predicate::str::contains("SGVsbG8gSlNPTgo="));
}

#[test]
fn json_input_can_feed_json_output_without_reopening_as_pdf() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-input", COMPLETE_JSON, "--json=2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"jsonversion\": 2"))
        .stdout(predicate::str::contains("/Custom"));
}

#[test]
fn json_input_inspection_modes_match_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    for option in ["--check", "--show-npages", "--show-pages"] {
        let qpdf = ShellCommand::new("qpdf")
            .args(["--json-input", option, COMPLETE_JSON])
            .output()
            .unwrap();
        let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
            .env("FLPDF_PROGNAME", "qpdf")
            .args(["--json-input", option, COMPLETE_JSON])
            .output()
            .unwrap();

        assert!(qpdf.status.success(), "qpdf {option} failed: {qpdf:?}");
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{option}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "{option} stderr");
        match option {
            "--check" | "--show-npages" => assert_eq!(
                normalize_program_prefix(&flpdf.stdout),
                normalize_program_prefix(&qpdf.stdout),
                "{option}"
            ),
            "--show-pages" => assert!(
                String::from_utf8_lossy(&flpdf.stdout).starts_with("page 1: 3 0 R\n"),
                "show-pages must inspect the JSON-created page tree: {:?}",
                flpdf.stdout
            ),
            _ => unreachable!("test option is fixed above"),
        }
    }
}

#[test]
fn update_from_json_check_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .arg(format!("--update-from-json={UPDATE_JSON}"))
        .args(["--check", MINIMAL_PDF])
        .output()
        .unwrap();
    let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .arg(format!("--update-from-json={UPDATE_JSON}"))
        .args(["--check", MINIMAL_PDF])
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_program_prefix(&flpdf.stdout),
        normalize_program_prefix(&qpdf.stdout)
    );
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn json_input_json_output_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--json-input", "--json=2", COMPLETE_JSON, "-"])
        .output()
        .unwrap();
    let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .args(["--json-input", "--json=2", COMPLETE_JSON])
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf failed: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn update_from_json_json_output_matches_qpdf_11_9() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .arg(format!("--update-from-json={UPDATE_JSON}"))
        .args(["--json=2", MINIMAL_PDF, "-"])
        .output()
        .unwrap();
    let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .arg(format!("--update-from-json={UPDATE_JSON}"))
        .args(["--json=2", MINIMAL_PDF])
        .output()
        .unwrap();

    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");
    assert!(flpdf.status.success(), "flpdf failed: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn malformed_json_matches_qpdf_status_and_does_not_create_output() {
    if skip_if_qpdf_missing() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let complete = temp.path().join("malformed-complete.json");
    let update = temp.path().join("malformed-update.json");
    let complete_output = temp.path().join("complete.pdf");
    let update_output = temp.path().join("update.pdf");
    fs::write(
        &complete,
        r#"{"qpdf":[{"jsonversion":2},{"obj:1 0 R":{}}]}"#,
    )
    .unwrap();
    fs::write(&update, br#"{"qpdf":["#).unwrap();

    let qpdf_complete = ShellCommand::new("qpdf")
        .args(["--json-input"])
        .arg(&complete)
        .arg(&complete_output)
        .output()
        .unwrap();
    let flpdf_complete = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .args(["--json-input"])
        .arg(&complete)
        .arg(&complete_output)
        .output()
        .unwrap();
    assert_eq!(qpdf_complete.status.code(), Some(2));
    assert_eq!(flpdf_complete.status.code(), Some(2));
    assert_eq!(
        normalize_program_prefix(&flpdf_complete.stderr),
        normalize_program_prefix(&qpdf_complete.stderr)
    );
    assert!(!complete_output.exists());

    let qpdf_update = ShellCommand::new("qpdf")
        .arg(format!("--update-from-json={}", update.display()))
        .arg(MINIMAL_PDF)
        .arg(&update_output)
        .output()
        .unwrap();
    let flpdf_update = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .arg(format!("--update-from-json={}", update.display()))
        .arg(MINIMAL_PDF)
        .arg(&update_output)
        .output()
        .unwrap();
    assert_eq!(qpdf_update.status.code(), Some(2));
    assert_eq!(flpdf_update.status.code(), Some(2));
    assert_eq!(
        normalize_program_prefix(&flpdf_update.stderr),
        normalize_program_prefix(&qpdf_update.stderr)
    );
    assert!(!update_output.exists());
}

#[test]
fn missing_json_input_matches_qpdf_and_does_not_create_output() {
    if skip_if_qpdf_missing() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("missing.json");
    let output = temp.path().join("missing.pdf");
    let qpdf = ShellCommand::new("qpdf")
        .args(["--json-input"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .args(["--json-input"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert_eq!(
        normalize_program_prefix(&flpdf.stderr),
        normalize_program_prefix(&qpdf.stderr)
    );
    assert!(!output.exists());
}

#[test]
fn update_from_json_requires_qpdf_equals_parameter_shape() {
    if skip_if_qpdf_missing() {
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--update-from-json", UPDATE_JSON, MINIMAL_PDF, "-"])
        .output()
        .unwrap();
    let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .args(["--update-from-json", UPDATE_JSON, MINIMAL_PDF, "-"])
        .output()
        .unwrap();
    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&qpdf.stderr)
        .contains("--update-from-json must be given as --update-from-json=qpdf-json file"));
    assert!(String::from_utf8_lossy(&flpdf.stderr).contains("equal sign is needed"));
}

#[test]
fn json_create_and_update_support_qdf_output() {
    let temp = tempfile::tempdir().unwrap();
    let created = temp.path().join("created.qdf.pdf");
    let updated = temp.path().join("updated.qdf.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-input", COMPLETE_JSON, "--qdf"])
        .arg(&created)
        .assert()
        .success();
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(format!("--update-from-json={UPDATE_JSON}"))
        .args([MINIMAL_PDF, "--qdf"])
        .arg(&updated)
        .assert()
        .success();

    for path in [created, updated] {
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.windows(b"%QDF-1.0".len()).any(|w| w == b"%QDF-1.0"));
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--check"])
            .arg(&path)
            .assert()
            .success();
    }
}

#[test]
fn update_from_json_reaches_json_output_and_keeps_new_objects_until_rewrite() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(format!("--update-from-json={UPDATE_JSON}"))
        .arg(MINIMAL_PDF)
        .args(["--json=2", "--json-key=qpdf", "--json-stream-data=inline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("obj:1 0 R"))
        .stdout(predicate::str::contains("/Custom"))
        .stdout(predicate::str::contains("VXBkYXRlZCBKU09OCg=="));
}

#[test]
fn update_from_json_is_applied_before_rotate() {
    let temp = tempfile::tempdir().unwrap();
    let update = temp.path().join("page-update.json");
    let output = temp.path().join("rotated.pdf");
    fs::write(
        &update,
        r#"{
  "qpdf": [
    {"jsonversion": 2},
    {"obj:3 0 R": {"value": {
      "/Contents": "7 0 R",
      "/MediaBox": [0, 0, 612, 792],
      "/Parent": "6 0 R",
      "/Resources": {"/Font": "1 0 R", "/ProcSet": ["/PDF", "/Text", "/ImageB", "/ImageC", "/ImageI"]},
      "/Rotate": 90,
      "/Trans": {},
      "/Type": "/Page"
    }}}
  ]
}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(format!("--update-from-json={}", update.display()))
        .arg(ONE_PAGE_PDF)
        .args(["--rotate=+90"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf"])
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains("/Rotate"))
        .stdout(predicate::str::contains("180"));
}

#[test]
fn update_from_json_is_applied_before_page_selection() {
    let temp = tempfile::tempdir().unwrap();
    let update = temp.path().join("page-update.json");
    let output = temp.path().join("selected.pdf");
    fs::write(
        &update,
        r#"{
  "qpdf": [
    {"jsonversion": 2},
    {"obj:3 0 R": {"value": {
      "/Contents": "7 0 R",
      "/MediaBox": [0, 0, 612, 792],
      "/Parent": "6 0 R",
      "/Resources": {"/Font": "1 0 R", "/ProcSet": ["/PDF", "/Text", "/ImageB", "/ImageC", "/ImageI"]},
      "/Rotate": 90,
      "/Trans": {},
      "/Type": "/Page"
    }}}
  ]
}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(format!("--update-from-json={}", update.display()))
        .arg(ONE_PAGE_PDF)
        .args(["--pages", ".", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=qpdf"])
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains("/Rotate"))
        .stdout(predicate::str::contains("90"));
}

#[test]
fn json_input_reaches_page_tree_selection() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("selected.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-input", COMPLETE_JSON, "--pages", ".", "--"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-npages"])
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::diff("1\n"));
}

#[test]
fn update_from_json_missing_file_fails_without_creating_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("should-not-exist.pdf");
    let missing = temp.path().join("missing-update.json");

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(format!("--update-from-json={}", missing.display()))
        .arg(MINIMAL_PDF)
        .arg(&output)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("missing-update.json"));

    assert!(!output.exists());
}
