/// Integration tests for --json and related flags (flpdf-9hc.11.13).
///
/// Covers the flag matrix described in the acceptance criteria:
///   --json stdout / --json-output file / --json-key / --json-object /
///   --json-key invalid / --json-object invalid /
///   --json-stream-data inline / --json-stream-data file side files.
use assert_cmd::Command;
use flpdf::{filters, ObjectHandle};
use predicates::prelude::*;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::Command as ShellCommand;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn is_qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

const EXPECTED_QPDF_ORACLE_VERSION: &str = "11.9.0";
const QPDF_ORACLE_SKIP_CHILD: &str = "FLPDF_QPDF_ORACLE_SKIP_CHILD";

#[derive(Debug, PartialEq, Eq)]
enum QpdfOracleAction {
    Run,
    Skip(String),
    ConfigurationFailure(String),
}

fn qpdf_oracle_action(version_stdout: Result<&[u8], String>, on_ci: bool) -> QpdfOracleAction {
    let expected = format!("qpdf version {EXPECTED_QPDF_ORACLE_VERSION}");
    let reason = match version_stdout {
        Ok(stdout) => {
            let stdout = String::from_utf8_lossy(stdout);
            match stdout.lines().next().map(str::trim) {
                Some(found) if found == expected => return QpdfOracleAction::Run,
                Some(found) => format!("expected {expected}, found {found}"),
                None => format!("expected {expected}, found <no version output>"),
            }
        }
        Err(reason) => reason,
    };
    if on_ci {
        QpdfOracleAction::ConfigurationFailure(reason)
    } else {
        QpdfOracleAction::Skip(reason)
    }
}

#[must_use]
fn skip_unless_qpdf_11_9() -> bool {
    let on_ci = std::env::var_os("CI").is_some();
    let action = match ShellCommand::new("qpdf").arg("--version").output() {
        Ok(output) if output.status.success() => qpdf_oracle_action(Ok(&output.stdout), on_ci),
        Ok(output) => qpdf_oracle_action(
            Err(format!("qpdf --version exited with {}", output.status)),
            on_ci,
        ),
        Err(error) => {
            qpdf_oracle_action(Err(format!("unable to run qpdf --version: {error}")), on_ci)
        }
    };
    match action {
        QpdfOracleAction::Run => false,
        QpdfOracleAction::Skip(reason) => {
            let mut stderr = std::io::stderr().lock();
            writeln!(stderr, "skipping qpdf JSON /dev/full oracle: {reason}")
                .expect("write qpdf JSON oracle skip reason");
            true
        }
        QpdfOracleAction::ConfigurationFailure(reason) => {
            panic!("qpdf JSON oracle configuration failure on CI: {reason}")
        }
    }
}

#[test]
fn qpdf_oracle_guard_accepts_exact_11_9_first_line() {
    assert_eq!(
        qpdf_oracle_action(Ok(b"qpdf version 11.9.0\nRun qpdf --copyright\n"), false),
        QpdfOracleAction::Run
    );
}

#[test]
fn qpdf_oracle_guard_reports_mismatch_locally_and_fails_ci() {
    let found = b"qpdf version 12.0.0\n";

    assert_eq!(
        qpdf_oracle_action(Ok(found), false),
        QpdfOracleAction::Skip(
            "expected qpdf version 11.9.0, found qpdf version 12.0.0".to_string()
        )
    );
    assert_eq!(
        qpdf_oracle_action(Ok(found), true),
        QpdfOracleAction::ConfigurationFailure(
            "expected qpdf version 11.9.0, found qpdf version 12.0.0".to_string()
        )
    );
}

#[test]
fn qpdf_oracle_guard_reports_missing_binary_locally_and_fails_ci() {
    let missing = Err("unable to run qpdf --version: qpdf not found".to_string());

    assert_eq!(
        qpdf_oracle_action(missing.clone(), false),
        QpdfOracleAction::Skip("unable to run qpdf --version: qpdf not found".to_string())
    );
    assert_eq!(
        qpdf_oracle_action(missing, true),
        QpdfOracleAction::ConfigurationFailure(
            "unable to run qpdf --version: qpdf not found".to_string()
        )
    );
}

#[test]
fn json_output_version_is_a_mode_and_positional_output_is_used() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.json");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-output=2"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stdout("");

    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(json["qpdf"][0]["jsonversion"], 2);
    assert!(json.get("version").is_none());
}

#[test]
fn test_json_schema_validates_v1_output_without_changing_it() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=1", "--test-json-schema"])
        .arg(fixture)
        .assert()
        .success()
        .stderr("")
        .stdout(predicates::str::contains("\"version\": 1"));
}

#[test]
fn qpdf_oracle_guard_local_skip_child() {
    if std::env::var_os(QPDF_ORACLE_SKIP_CHILD).is_none() {
        return;
    }
    assert!(skip_unless_qpdf_11_9());
}

#[test]
fn qpdf_oracle_guard_exposes_local_skip_reason_without_nocapture() {
    let empty_path = tempfile::tempdir().unwrap();
    let output = ShellCommand::new(std::env::current_exe().unwrap())
        .args(["--exact", "qpdf_oracle_guard_local_skip_child"])
        .env(QPDF_ORACLE_SKIP_CHILD, "1")
        .env("PATH", empty_path.path())
        .env_remove("CI")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("skipping qpdf JSON /dev/full oracle: unable to run qpdf --version:"),
        "{output:?}"
    );
}

/// One-page PDF with a single content stream so we have at least one stream
/// object in the qpdf section.
fn one_page_pdf_with_stream() -> Vec<u8> {
    let content_data = b"BT /F1 12 Tf (Hello) Tj ET";
    let stream_obj = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data),
    );

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(stream_obj.as_bytes());
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 5\n\
         0000000000 65535 f \n\
         {off1:010} 00000 n \n\
         {off2:010} 00000 n \n\
         {off3:010} 00000 n \n\
         {off4:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn write_temp_pdf(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.as_file_mut().write_all(bytes).unwrap();
    f
}

fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    let max = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
    for (number, body) in objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..=max {
        if let Some(offset) = offsets.get(&number) {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            out.extend_from_slice(b"0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            max + 1
        )
        .as_bytes(),
    );
    out
}

fn short_name_tree_pair_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests << /Kids [<< /Names [(m)] >>] >> >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (One) /Parent 4 0 R /Dest (m) >>"),
        ],
        1,
    )
}

fn lazy_malformed_orphan_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Broken [1 2"),
        ],
        1,
    )
}

fn escaped_raw_dictionary_names_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /#22 1 /A 2 /Nested << /#22 3 /A 4 >> >>\nendobj\n",
    );
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        b"4 0 obj\n<< /Length 0 /#22 5 /A 6 /Nested << /#22 7 /A 8 >> >>\nstream\n\nendstream\nendobj\n",
    );
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R /#22 9 /A 10 >>\n\
             startxref\n{xref_start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

// ---------------------------------------------------------------------------
// Test 1: --json outputs JSON to stdout
// ---------------------------------------------------------------------------

#[test]
fn json_flag_outputs_json_to_stdout() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json", input.path().to_str().unwrap()])
        .assert()
        .success()
        // qpdf JSON v2 top-level key "version"
        .stdout(predicate::str::contains("\"version\""))
        // "pages" section is present
        .stdout(predicate::str::contains("\"pages\""))
        // stderr is empty — no spurious warnings for a clean PDF
        .stderr(predicate::str::is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn json_stdout_to_dev_full_matches_qpdf_success() {
    use std::fs::File;
    use std::process::Stdio;

    if skip_unless_qpdf_11_9() {
        return;
    }
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2"])
        .arg(input.path())
        .stdout(Stdio::from(File::create("/dev/full").unwrap()))
        .output()
        .unwrap();
    let flpdf = ShellCommand::new(assert_cmd::cargo_bin!("flpdf"))
        .args(["--json=2"])
        .arg(input.path())
        .stdout(Stdio::from(File::create("/dev/full").unwrap()))
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "{qpdf:?}");
    assert!(flpdf.status.success(), "{flpdf:?}");
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[cfg(target_os = "linux")]
#[test]
fn json_output_dev_full_matches_qpdf_success() {
    if skip_unless_qpdf_11_9() {
        return;
    }
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2"])
        .arg(input.path())
        .arg("/dev/full")
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-output=2"])
        .arg(input.path())
        .arg("/dev/full")
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "{qpdf:?}");
    assert!(flpdf.status.success(), "{flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn json_fatal_preserves_partial_stdout() {
    let input = write_temp_pdf(&short_name_tree_pair_pdf());
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json=2",
            "--json-key=outlines",
            input.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.starts_with(b"{\n  \"version\": 2,"));
    assert!(!output.stdout.ends_with(b"}\n"));
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err());
}

#[test]
fn json_fatal_preserves_partial_output_file() {
    let input = write_temp_pdf(&short_name_tree_pair_pdf());
    let temp = tempfile::tempdir().unwrap();
    let output_path = temp.path().join("out.json");
    std::fs::write(&output_path, b"pre-existing content").unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json=2",
            "--json-key=outlines",
            input.path().to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let bytes = std::fs::read(&output_path).unwrap();
    assert!(bytes.starts_with(b"{\n  \"version\": 2,"));
    assert!(!bytes.ends_with(b"}\n"));
    assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
}

#[test]
fn lazy_object_failure_matches_qpdf_null_fallback() {
    if !is_qpdf_available() {
        return;
    }

    let input = write_temp_pdf(&lazy_malformed_orphan_pdf());
    let args = ["--json=2", "--json-key=qpdf"];
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(!qpdf.status.success());

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();

    assert!(!flpdf.status.success());
    assert_eq!(flpdf.stdout, qpdf.stdout);
}

fn assert_same_json_output_is_rejected_without_modifying_input(
    input_arg: &str,
    output_arg: &str,
    input_path: &std::path::Path,
    current_dir: Option<&std::path::Path>,
) {
    let original = std::fs::read(input_path).unwrap();
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    let output = cmd
        .args(["--json-output=2", input_arg, output_arg])
        .output()
        .unwrap();
    assert_eq!(std::fs::read(input_path).unwrap(), original);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "input file and output file are the same; choose a different --json-output path"
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn json_output_rejects_input_path_without_modifying_input() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let path = input.path().to_str().unwrap();
    assert_same_json_output_is_rejected_without_modifying_input(path, path, input.path(), None);
}

#[test]
fn json_output_rejects_relative_alias_without_modifying_input() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    assert_same_json_output_is_rejected_without_modifying_input(
        "input.pdf",
        "./input.pdf",
        &input_path,
        Some(temp.path()),
    );
}

#[cfg(unix)]
#[test]
fn json_output_rejects_symlink_to_input_without_modifying_input() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    let output_path = temp.path().join("output.json");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();
    symlink(&input_path, &output_path).unwrap();

    assert_same_json_output_is_rejected_without_modifying_input(
        input_path.to_str().unwrap(),
        output_path.to_str().unwrap(),
        &input_path,
        None,
    );
}

#[cfg(unix)]
#[test]
fn json_output_rejects_hardlink_to_input_without_modifying_input() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    let output_path = temp.path().join("output.json");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();
    std::fs::hard_link(&input_path, &output_path).unwrap();

    assert_same_json_output_is_rejected_without_modifying_input(
        input_path.to_str().unwrap(),
        output_path.to_str().unwrap(),
        &input_path,
        None,
    );
}

#[test]
fn json_output_overwrites_distinct_existing_file() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let output_path = temp.path().join("output.json");
    std::fs::write(&output_path, b"stale output").unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json-output=2",
            input.path().to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let output = std::fs::read(&output_path).unwrap();
    assert_ne!(output, b"stale output");
    serde_json::from_slice::<serde_json::Value>(&output).unwrap();
}

#[cfg(unix)]
#[test]
fn json_output_overwrites_distinct_write_only_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let output_path = temp.path().join("output.json");
    std::fs::write(&output_path, b"stale output").unwrap();
    std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o200)).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json-output=2",
            input.path().to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = std::fs::read(&output_path).unwrap();
    assert_ne!(output, b"stale output");
    serde_json::from_slice::<serde_json::Value>(&output).unwrap();
}

#[cfg(unix)]
#[test]
fn json_output_reports_identity_check_io_error_without_modifying_input() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let original = std::fs::read(input.path()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let non_directory = temp.path().join("not-a-directory");
    std::fs::write(&non_directory, b"blocker").unwrap();
    let output_path = non_directory.join("output.json");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json-output=2",
            input.path().to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "unable to inspect --json-output file",
        ));

    assert_eq!(std::fs::read(input.path()).unwrap(), original);
}

// ---------------------------------------------------------------------------
// Test 2: --json --json-output writes to file, stdout is empty
// ---------------------------------------------------------------------------

#[test]
fn json_output_flag_writes_to_file_and_stdout_empty() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output=2",
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::is_empty());

    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("\"jsonversion\""),
        "expected JSON in output file"
    );
}

// ---------------------------------------------------------------------------
// Test 3: --json --json-key pages — only pages section present
// ---------------------------------------------------------------------------

#[test]
fn json_key_pages_limits_output() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-key",
        "pages",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("\"pages\""))
    // With --json-key pages, the "qpdf" top-level key must not appear
    // (it would contain the object map).
    .stdout(predicate::str::contains("\"qpdf\"").not())
    // The "encrypt" top-level key must not appear.
    .stdout(predicate::str::contains("\"encrypt\"").not());
}

// ---------------------------------------------------------------------------
// Test 4: --json --json-object 3 — only obj:3 0 R in qpdf section
// ---------------------------------------------------------------------------

#[test]
fn json_object_selector_limits_qpdf_section() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-object",
        "3",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    // Object 3 is the page dict; it should appear.
    .stdout(predicate::str::contains("\"obj:3 0 R\""))
    // Object 1 (catalog) should NOT appear.
    .stdout(predicate::str::contains("\"obj:1 0 R\"").not());
}

// ---------------------------------------------------------------------------
// Test 5: --json-key invalid — exit code != 0, error on stderr
// ---------------------------------------------------------------------------

#[test]
fn json_key_invalid_exits_nonzero_with_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-key",
        "invalidkey",
        input.path().to_str().unwrap(),
    ])
    .assert()
    // The acceptance criteria require exit code 2 specifically (not just
    // any nonzero) so a regression to code 1 is caught.
    .code(2)
    .stderr(predicate::str::contains("--json-key"));
}

#[test]
fn json_v2_rejects_v1_only_object_keys_before_input_io() {
    for key in ["objects", "objectinfo"] {
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        let assert = cmd
            .args([
                "--json=2",
                "--json-key",
                key,
                "/definitely/missing/json-key-validation.pdf",
            ])
            .assert()
            .code(2);
        let output = assert.get_output();
        assert!(output.stdout.is_empty(), "{key}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "flpdf: json keys \"objects\" and \"objectinfo\" are only valid for json version 1\n",
            "{key}"
        );
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 versioned JSON key oracle"]
fn live_qpdf_json_v2_rejects_v1_only_object_keys_with_same_diagnostic() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let expected = "json keys \"objects\" and \"objectinfo\" are only valid for json version 1";

    for key in ["objects", "objectinfo"] {
        let key_arg = format!("--json-key={key}");
        let qpdf = std::process::Command::new("qpdf")
            .args(["--json=2", &key_arg])
            .arg(input.path())
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--json=2", &key_arg])
            .arg(input.path())
            .output()
            .unwrap();

        assert_eq!(qpdf.status.code(), Some(2), "{key}");
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{key}");
        let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
        let flpdf_stderr = String::from_utf8_lossy(&flpdf.stderr);
        let qpdf_line = qpdf_stderr
            .lines()
            .find(|line| !line.is_empty())
            .unwrap()
            .strip_prefix("qpdf: ")
            .unwrap();
        let flpdf_line = flpdf_stderr
            .lines()
            .find(|line| !line.is_empty())
            .unwrap()
            .strip_prefix("flpdf: ")
            .unwrap();
        assert_eq!(qpdf_line, expected, "{key}");
        assert_eq!(flpdf_line, qpdf_line, "{key}");
    }
}

// ---------------------------------------------------------------------------
// Test 6: --json-object xyz — exit code != 0, error on stderr
// ---------------------------------------------------------------------------

#[test]
fn json_object_invalid_exits_nonzero_with_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-object",
        "xyz",
        input.path().to_str().unwrap(),
    ])
    .assert()
    // Exit code 2 specifically (see sibling test rationale).
    .code(2)
    .stderr(predicate::str::contains("--json-object"))
    .stderr(predicate::str::contains("xyz"));
}

// ---------------------------------------------------------------------------
// Test 7: --json-stream-data inline — stream entries contain "data" field
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_inline_includes_data_field() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output=2",
        "--json-stream-data",
        "inline",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    // Inline mode encodes stream bytes as base64 under "data" key.
    .stdout(predicate::str::contains("\"data\""));
}

// ---------------------------------------------------------------------------
// Test 8: --json-output + --json-stream-data file + --json-stream-prefix
//         — side files are created
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_file_creates_side_files() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");
    let prefix = temp.path().join("sf").to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-stream-data",
        "file",
        "--json-stream-prefix",
        &prefix,
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .success();

    // The JSON output should reference "datafile" entries.
    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("\"datafile\""),
        "expected datafile entries in JSON output"
    );

    // At least one side file should exist (object 4 is the content stream).
    let side_file = format!("{prefix}-4");
    assert!(
        std::path::Path::new(&side_file).exists(),
        "expected side file {side_file} to exist"
    );
}

#[test]
fn json_stream_data_file_to_stdout_requires_explicit_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&input_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "\nqpdf: please specify --json-stream-prefix since the input file name is unknown\n\n\
For help:\n  qpdf --help=usage       usage information\n  qpdf --help=topic       help on a topic\n  \
qpdf --help=--option    help on an option\n  qpdf --help             general help and a topic list\n\n"
    );
    assert!(!temp.path().join("stream-4").exists());
}

#[test]
fn json_stream_data_file_to_stdout_empty_prefix_requires_explicit_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--json=2",
            "--json-stream-data=file",
            "--json-stream-prefix=",
        ])
        .arg(&input_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "\nqpdf: please specify --json-stream-prefix since the input file name is unknown\n\n\
For help:\n  qpdf --help=usage       usage information\n  qpdf --help=topic       help on a topic\n  \
qpdf --help=--option    help on an option\n  qpdf --help             general help and a topic list\n\n"
    );
    assert!(!temp.path().join("-4").exists());
}

#[test]
fn missing_stream_prefix_does_not_mask_missing_input() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("missing.pdf");

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&input_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("open {}:", input_path.display())),
        "expected input-open error, got: {stderr}"
    );
    assert!(
        !stderr.contains("please specify --json-stream-prefix"),
        "the unresolved prefix must not mask the input-open error: {stderr}"
    );
}

#[test]
fn missing_stream_prefix_does_not_mask_malformed_input() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("malformed.pdf");
    std::fs::write(&input_path, b"not a PDF").unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&input_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&input_path.display().to_string()),
        "expected malformed-input diagnostic, got: {stderr}"
    );
    assert!(
        !stderr.contains("please specify --json-stream-prefix"),
        "the unresolved prefix must not mask the malformed-input diagnostic: {stderr}"
    );
}

#[test]
fn json_stream_data_file_to_stdout_uses_explicit_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    let prefix = temp.path().join("streams");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json=2",
            "--json-stream-data=file",
            "--json-stream-prefix",
        ])
        .arg(&prefix)
        .arg(&input_path)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("\"datafile\""),
        "expected JSON to refer to stream side file: {output:?}"
    );
    assert!(
        temp.path().join("streams-4").exists(),
        "expected explicit-prefix side file"
    );
}

#[test]
fn json_output_file_mode_defaults_stream_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    let output_path = temp.path().join("output.json");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json-output=2", "--json-stream-data=file"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        std::fs::read_to_string(&output_path)
            .unwrap()
            .contains("\"datafile\""),
        "expected JSON to refer to default-prefix side file"
    );
    assert!(
        temp.path().join("output.json-4").exists(),
        "expected output filename to be the default stream prefix"
    );
}

#[test]
fn json_output_file_mode_empty_prefix_defaults_stream_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    let output_path = temp.path().join("output.json");
    let expected_side_file = temp.path().join("output.json-4");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--json=2",
            "--json-stream-data=file",
            "--json-stream-prefix=",
            "--json-output=2",
        ])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output_path).unwrap()).unwrap();
    assert_eq!(
        json["qpdf"][1]["obj:4 0 R"]["stream"]["datafile"],
        expected_side_file.to_string_lossy().as_ref()
    );
    assert!(expected_side_file.exists());
}

#[test]
#[ignore = "live qpdf 11.9.0 missing JSON stream prefix oracle"]
fn live_qpdf_json_file_stdout_requires_prefix() {
    if skip_unless_qpdf_11_9() {
        return;
    }

    let qpdf_dir = tempfile::tempdir().unwrap();
    let flpdf_dir = tempfile::tempdir().unwrap();
    let qpdf_input = qpdf_dir.path().join("input.pdf");
    let flpdf_input = flpdf_dir.path().join("input.pdf");
    let input = one_page_pdf_with_stream();
    std::fs::write(&qpdf_input, &input).unwrap();
    std::fs::write(&flpdf_input, input).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .current_dir(qpdf_dir.path())
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&qpdf_input)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(flpdf_dir.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&flpdf_input)
        .output()
        .unwrap();

    assert_eq!(flpdf.status, qpdf.status);
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(!qpdf_dir.path().join("stream-4").exists());
    assert!(!flpdf_dir.path().join("stream-4").exists());
}

#[test]
#[ignore = "live qpdf 11.9.0 empty JSON stream prefix oracle"]
fn live_qpdf_json_file_stdout_empty_prefix_requires_prefix() {
    if skip_unless_qpdf_11_9() {
        return;
    }

    let qpdf_dir = tempfile::tempdir().unwrap();
    let flpdf_dir = tempfile::tempdir().unwrap();
    let qpdf_input = qpdf_dir.path().join("input.pdf");
    let flpdf_input = flpdf_dir.path().join("input.pdf");
    let input = one_page_pdf_with_stream();
    std::fs::write(&qpdf_input, &input).unwrap();
    std::fs::write(&flpdf_input, input).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .current_dir(qpdf_dir.path())
        .args([
            "--json=2",
            "--json-stream-data=file",
            "--json-stream-prefix=",
        ])
        .arg(&qpdf_input)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(flpdf_dir.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args([
            "--json=2",
            "--json-stream-data=file",
            "--json-stream-prefix=",
        ])
        .arg(&flpdf_input)
        .output()
        .unwrap();

    assert_eq!(flpdf.status, qpdf.status);
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(!qpdf_dir.path().join("-4").exists());
    assert!(!flpdf_dir.path().join("-4").exists());
}

// ---------------------------------------------------------------------------
// Regression: --json-output follows qpdf's inline stream-data default.
//
// qpdf's --json-output mode selects inline stream data unless an explicit
// --json-stream-data value overrides it. The ordinary --json mode retains its
// separate default.
// ---------------------------------------------------------------------------

#[test]
fn json_output_without_stream_data_flag_uses_qpdf_inline_payload() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output=2",
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .success();

    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("\"data\""),
        "qpdf --json-output defaults stream-data to inline"
    );
    assert!(!content.contains("\"datafile\""));
}

// ---------------------------------------------------------------------------
// Regression: --json-key=pages + --json-stream-data=file must NOT write
// side files for streams whose qpdf entry was filtered out.
//
// CodeRabbit flagged that side files were being written for every stream
// regardless of --json-key / --json-object scoping, which both spams the
// filesystem and exposes stream content the JSON output doesn't reference.
// ---------------------------------------------------------------------------

#[test]
fn json_key_pages_does_not_write_side_files_for_filtered_streams() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let prefix = temp.path().join("sf").to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-key",
        "pages",
        "--json-stream-data",
        "file",
        "--json-stream-prefix",
        &prefix,
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success();

    // --json-key=pages filters out the qpdf section entirely, so there
    // should be no datafile references in the final JSON and therefore no
    // side files should be written.
    let side_file = format!("{prefix}-4");
    assert!(
        !std::path::Path::new(&side_file).exists(),
        "no side file should be written when qpdf section is filtered out (got {side_file})"
    );
}

// ---------------------------------------------------------------------------
// Regression: JSON output mode is independently usable.
//
// qpdf treats --json-output as a mode that selects JSON output even without a
// separate --json flag. The other selectors still need an explicit output
// mode; they must not be silently ignored.
// ---------------------------------------------------------------------------

#[test]
fn json_key_without_json_flag_is_usage_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json-key", "pages", input.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("missing output file"));
}

#[test]
fn json_output_without_json_flag_is_a_json_mode() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json-output=2",
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::is_empty());
    assert!(std::fs::read_to_string(out_path)
        .unwrap()
        .contains("\"jsonversion\": 2"));
}

#[test]
fn json_stream_data_without_json_flag_is_usage_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json-stream-data",
        "inline",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2)
    .stderr(predicate::str::contains("missing output file"));
}

// ---------------------------------------------------------------------------
// Regression: --json must not silently coexist with a subcommand.
//
// CodeRabbit flagged that `flpdf --json rewrite in out` parsed as the
// rewrite subcommand while keeping --json, so the JSON branch never ran.
// args_conflicts_with_subcommands now makes this a clean usage error.
// ---------------------------------------------------------------------------

#[test]
fn json_flag_conflicts_with_subcommand() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "rewrite",
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

// ---------------------------------------------------------------------------
// Regression: --json is exclusive with other top-level modes / OUTPUT.
//
// CodeRabbit flagged that `flpdf --json --check in` or `flpdf --json in out`
// silently ignored the second mode because run_json wins main's dispatch
// chain. clap conflicts_with_all now turns these into usage errors.
// ---------------------------------------------------------------------------

#[test]
fn json_flag_conflicts_with_check_mode() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json", "--check", input.path().to_str().unwrap()])
        .assert()
        .code(2);
}

#[test]
fn json_flag_accepts_output_positional() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        input.path().to_str().unwrap(),
        out.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::is_empty());
    assert!(std::fs::read_to_string(out)
        .unwrap()
        .contains("\"version\": 2"));
}

#[test]
fn json_flag_conflicts_with_show_linearization() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--show-linearization",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

#[test]
fn json_flag_conflicts_with_linearize_pass1() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let p1 = temp.path().join("pass1.bin");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--linearize-pass1",
        p1.to_str().unwrap(),
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

#[test]
fn json_flag_conflicts_with_compress_streams() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--compress-streams=n",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

/// `--json-output` dispatches through the same `run_json` boundary as
/// `--json`. Every rewrite or inspection flag that the latter rejects must be
/// rejected here too; otherwise the second flag is silently dropped before
/// its consumer can run. Keep this table aligned with `Cli::json`'s
/// `conflicts_with_all` list.
#[test]
fn json_output_conflicts_with_the_json_exclusive_flag_set() {
    let cases: &[&[&str]] = &[
        &["--check"],
        &["--linearize"],
        &["--static-id"],
        &["--deterministic-id"],
        &["--static-aes-iv"],
        &["--show-object=trailer"],
        &["--show-npages"],
        &["--show-pages"],
        &["--show-xref"],
        &["--show-linearization"],
        &["--show-encryption"],
        &["--compress-streams=n"],
        &["--linearize-pass1=pass1"],
        &["--remove-restrictions"],
        &["--decrypt"],
        &["--encrypt", "u", "o", "128", "--"],
        &["--copy-encryption=donor.pdf"],
        &["--add-attachment", "file.bin", "--"],
        &["--remove-attachment=key"],
        &["--list-attachments"],
        &["--show-attachment=key"],
        &["--copy-attachments-from", "donor.pdf", "--"],
        &["--no-original-object-ids"],
        &["--qdf"],
        &["--coalesce-contents"],
        &["--flatten-annotations=all"],
        &["--preserve-unreferenced"],
    ];

    for extra in cases {
        let output = Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--json-output=2"])
            .args(*extra)
            .arg("../../tests/fixtures/minimal.pdf")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "--json-output must reject {extra:?}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cannot be used with"),
            "--json-output conflict for {extra:?} must be a usage error; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// ===========================================================================
// flpdf-5st: --json-stream-data must apply DecodeLevel to the stream payload.
//
// build_qpdf_json_v2_with_options is invoked with DecodeLevel::Generalized, so
// inline `data` and file-mode side files must carry the *filter-decoded*
// content (qpdf --decode-level=generalized), not the raw compressed bytes.
// The fixtures above use unfiltered streams and cannot catch this — these
// tests use a FlateDecode-wrapped content stream where decoded != raw.
// ===========================================================================

/// One-page PDF whose content stream (object `4 0 R`) is FlateDecode-wrapped.
fn one_page_pdf_with_flate_stream(content: &[u8]) -> Vec<u8> {
    let d = ObjectHandle::dictionary(vec![(
        b"/Filter".to_vec(),
        ObjectHandle::name(b"FlateDecode".to_vec()),
    )]);
    let encoded = filters::encode_stream_data(&d, content).expect("encode FlateDecode stream");

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter /FlateDecode /DecodeParms << /Predictor 1 >> >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&encoded);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 5\n\
         0000000000 65535 f \n\
         {off1:010} 00000 n \n\
         {off2:010} 00000 n \n\
         {off3:010} 00000 n \n\
         {off4:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// One-page PDF whose content stream (object `4 0 R`) is RunLengthDecode-
/// wrapped. qpdf classifies RunLengthDecode as specialized compression, so
/// the default generalized JSON decode level must keep the raw bytes and
/// original filter dictionary.
fn one_page_pdf_with_run_length_stream(content: &[u8]) -> Vec<u8> {
    let d = ObjectHandle::dictionary(vec![(
        b"/Filter".to_vec(),
        ObjectHandle::name(b"RunLengthDecode".to_vec()),
    )]);
    let encoded = filters::encode_stream_data(&d, content).expect("encode RunLength stream");

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter /RunLengthDecode >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&encoded);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 5\n\
         0000000000 65535 f \n\
         {off1:010} 00000 n \n\
         {off2:010} 00000 n \n\
         {off3:010} 00000 n \n\
         {off4:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn one_page_pdf_with_unsupported_stream(content: &[u8]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter /DCTDecode /DecodeParms << /ColorTransform 0 >> >>\nstream\n",
            content.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 5\n\
         0000000000 65535 f \n\
         {off1:010} 00000 n \n\
         {off2:010} 00000 n \n\
         {off3:010} 00000 n \n\
         {off4:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn assert_stream_json_is_qpdf_exact(pdf: &[u8], stream_mode: &str) {
    if !is_qpdf_available() {
        return;
    }

    let input = write_temp_pdf(pdf);
    let temp = tempfile::tempdir().unwrap();
    let prefix = temp.path().join("stream");
    let stream_mode_arg = format!("--json-stream-data={stream_mode}");
    let prefix_arg = format!("--json-stream-prefix={}", prefix.display());
    let args = [
        "--json=2",
        "--json-key=qpdf",
        "--json-object=4",
        stream_mode_arg.as_str(),
        prefix_arg.as_str(),
    ];

    let qpdf = std::process::Command::new("qpdf")
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf 11.9.0 failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let qpdf_side_file =
        (stream_mode == "file").then(|| std::fs::read(format!("{}-4", prefix.display())).unwrap());

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        flpdf.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    if let Some(qpdf_bytes) = qpdf_side_file {
        assert_eq!(
            std::fs::read(format!("{}-4", prefix.display())).unwrap(),
            qpdf_bytes
        );
    }
}

#[test]
fn unfiltered_inline_stream_json_is_qpdf_exact() {
    assert_stream_json_is_qpdf_exact(&one_page_pdf_with_stream(), "inline");
}

#[test]
fn filtered_inline_stream_json_is_qpdf_exact() {
    assert_stream_json_is_qpdf_exact(
        &one_page_pdf_with_flate_stream(b"qpdf filtered inline bytes"),
        "inline",
    );
}

#[test]
fn filtered_stream_json_without_payload_is_qpdf_exact() {
    assert_stream_json_is_qpdf_exact(
        &one_page_pdf_with_flate_stream(b"qpdf filtered dictionary bytes"),
        "none",
    );
}

#[test]
fn unfiltered_file_stream_json_and_payload_are_qpdf_exact() {
    assert_stream_json_is_qpdf_exact(&one_page_pdf_with_stream(), "file");
}

#[cfg(target_os = "linux")]
#[test]
fn file_stream_to_dev_full_matches_qpdf_success_and_complete_json() {
    use std::os::unix::fs::symlink;

    if !is_qpdf_available() {
        return;
    }

    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let prefix = temp.path().join("stream");
    symlink("/dev/full", temp.path().join("stream-4")).unwrap();
    let prefix_arg = format!("--json-stream-prefix={}", prefix.display());
    let args = [
        "--json=2",
        "--json-key=qpdf",
        "--json-object=4",
        "--json-stream-data=file",
        prefix_arg.as_str(),
    ];

    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "{qpdf:?}");
    serde_json::from_slice::<serde_json::Value>(&qpdf.stdout).unwrap();

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();

    assert!(flpdf.status.success(), "{flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    serde_json::from_slice::<serde_json::Value>(&flpdf.stdout).unwrap();
}

#[test]
fn filtered_file_stream_json_and_payload_are_qpdf_exact() {
    assert_stream_json_is_qpdf_exact(
        &one_page_pdf_with_flate_stream(b"qpdf filtered file bytes"),
        "file",
    );
}

#[test]
fn specialized_filter_falls_back_at_generalized_decode_level_json_exact() {
    assert_stream_json_is_qpdf_exact(
        &one_page_pdf_with_run_length_stream(b"qpdf specialized fallback bytes"),
        "inline",
    );
}

#[test]
fn specialized_filter_file_falls_back_at_generalized_decode_level_json_exact() {
    assert_stream_json_is_qpdf_exact(
        &one_page_pdf_with_run_length_stream(b"qpdf specialized file fallback bytes"),
        "file",
    );
}

#[test]
fn unsupported_filter_inline_fallback_json_is_qpdf_exact() {
    assert_stream_json_is_qpdf_exact(
        &one_page_pdf_with_unsupported_stream(b"raw unsupported filter bytes"),
        "inline",
    );
}

#[test]
fn raw_dictionary_names_are_ordered_before_json_escaping_like_qpdf() {
    if !is_qpdf_available() {
        return;
    }

    let input = write_temp_pdf(&escaped_raw_dictionary_names_pdf());
    let args = ["--json=2", "--json-key=qpdf"];
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf 11.9.0 failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        flpdf.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
}

/// Minimal RFC 4648 base64 encoder, for asserting on inline `data` values.
fn base64_encode(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(A[((n >> 18) & 0x3F) as usize] as char);
        out.push(A[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// --json-stream-data file: side files must hold the filter-decoded content.
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_file_side_file_holds_decoded_content() {
    let content = b"BT /F1 24 Tf 1 0 0 1 100 700 Tm (Decoded side-file payload) Tj ET";
    let input = write_temp_pdf(&one_page_pdf_with_flate_stream(content));
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");
    let prefix = temp.path().join("sf").to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output=2",
        "--json-stream-data",
        "file",
        "--json-stream-prefix",
        &prefix,
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .success();

    let side_file = format!("{prefix}-4");
    let written = std::fs::read(&side_file).expect("side file must exist");
    assert_ne!(
        written, content,
        "qpdf --json-output defaults DecodeLevel to none, so file mode keeps raw FlateDecode bytes"
    );
}

// ---------------------------------------------------------------------------
// --json-stream-data inline: the base64 `data` must be the decoded content.
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_inline_holds_decoded_content() {
    let content = b"BT /F1 24 Tf 1 0 0 1 100 700 Tm (Decoded inline payload) Tj ET";
    let input = write_temp_pdf(&one_page_pdf_with_flate_stream(content));

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-stream-data",
        "inline",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    // Inline mode at DecodeLevel::Generalized must base64 the decoded content.
    .stdout(predicate::str::contains(base64_encode(content)));
}
