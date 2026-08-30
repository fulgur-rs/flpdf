use assert_cmd::Command;
use flpdf::{PageDocumentHelper, Pdf};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn expected_usage(message: &str) -> String {
    format!(
        "\nflpdf: {message}\n\nFor help:\n  flpdf --help=usage       usage information\n  \
flpdf --help=topic       help on a topic\n  flpdf --help=--option    help on an option\n  \
flpdf --help             general help and a topic list\n\n"
    )
}

#[test]
fn job_json_file_runs_through_the_production_qpdf_job() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("minimal.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"minimal.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .code(0)
        .stdout("");

    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_usage_errors_use_the_qpdf_job_file_boundary() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("bad.json"),
        br#"{"objectStreams":"potato"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=bad.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "error with job-json file bad.json",
        ));
}

#[test]
fn job_json_file_missing_output_reports_one_diagnostic() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("missing-output.json"),
        br#"{"inputFile":"input.pdf"}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=missing-output.json")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr,
        expected_usage("an output file name is required; use - for standard output")
    );
}

#[test]
fn job_json_file_progress_reports_qpdf_write_progress_to_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("progress.json"),
        br#"{"inputFile":"input.pdf","outputFile":"output.pdf","progress":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=progress.json")
        .assert()
        .code(0)
        .stdout(predicates::str::contains("write progress: 0%"))
        .stdout(predicates::str::contains("write progress: 100%"));
}

#[test]
fn job_json_file_preserves_qpdf_warning_status() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    fs::copy(fixture, directory.path().join("repairable.pdf")).unwrap();
    fs::write(
        directory.path().join("warning.json"),
        br#"{"inputFile":"repairable.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=warning.json")
        .assert()
        .code(3)
        .stderr(predicates::str::contains(
            "operation succeeded with warnings",
        ));

    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_rejects_same_input_and_output_without_truncating_input() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    fs::copy(fixture, &input).unwrap();
    fs::hard_link(&input, &output).unwrap();
    let before = fs::read(&input).unwrap();
    fs::write(
        directory.path().join("same.json"),
        serde_json::json!({
            "inputFile": input,
            "outputFile": output,
        })
        .to_string(),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=same.json")
        .assert()
        .code(2)
        .stderr(predicates::str::diff(expected_usage(
            "input file and output file are the same; use --replace-input to intentionally overwrite the input",
        )));

    assert_eq!(fs::read(&input).unwrap(), before);
}

#[test]
fn job_json_file_dash_output_is_written_to_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("stdout.json"),
        br#"{"inputFile":"input.pdf","outputFile":"-"}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=stdout.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!directory.path().join("-").exists());
}

#[test]
fn job_json_file_split_pages_writes_qpdf_named_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split.json"),
        br#"{"inputFile":"input.pdf","outputFile":"split.pdf","splitPages":"1","staticId":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    for page in 1..=3 {
        assert!(
            directory.path().join(format!("split-{page}.pdf")).is_file(),
            "qpdf split-pages output split-{page}.pdf is missing"
        );
    }
    assert!(
        !directory.path().join("split.pdf").exists(),
        "splitPages must not fall through to one unsplit output"
    );
}

#[test]
fn job_json_file_rotate_applies_to_the_selected_page() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("rotate.json"),
        br#"{"inputFile":"input.pdf","outputFile":"rotated.pdf","rotate":"90:2","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=rotate.json")
        .assert()
        .code(0);

    assert_eq!(
        page_rotations(&fs::read(directory.path().join("rotated.pdf")).unwrap()),
        vec![None, Some(90), None],
        "qpdf rotate=90:2 targets only output page 2"
    );
}

#[test]
fn job_json_file_rotate_trailing_colon_means_all_pages_like_qpdf() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("rotate-all.json"),
        br#"{"inputFile":"input.pdf","outputFile":"rotated-all.pdf","rotate":"90:","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=rotate-all.json")
        .assert()
        .code(0);

    assert_eq!(
        page_rotations(&fs::read(directory.path().join("rotated-all.pdf")).unwrap()),
        vec![Some(90), Some(90), Some(90)],
        "qpdf treats an empty range after rotate's colon as all pages"
    );
}

#[test]
fn job_json_file_remove_restrictions_disables_signature_fields() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/acroform-sig-widget.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("remove.json"),
        br#"{"inputFile":"input.pdf","outputFile":"removed.pdf","removeRestrictions":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=remove.json")
        .assert()
        .code(0);

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("removed.pdf")).unwrap(),
    ))
    .unwrap();
    assert!(
        pdf.signatures().unwrap().is_empty(),
        "qpdf removeRestrictions removes signature fields"
    );
    let root = pdf.root_handle().unwrap();
    let acroform = root.try_get_key(b"/AcroForm").unwrap();
    pdf.resolve(&acroform).unwrap();
    let fields = acroform.try_get_key(b"/Fields").unwrap();
    pdf.resolve(&fields).unwrap();
    assert!(fields.as_array().is_some_and(|items| items.is_empty()));
    let sig_flags = acroform.try_get_key(b"/SigFlags").unwrap();
    pdf.resolve(&sig_flags).unwrap();
    assert_eq!(sig_flags.as_integer(), Some(0));
}

fn page_rotations(bytes: &[u8]) -> Vec<Option<i64>> {
    let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).unwrap();
    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    pages
        .into_iter()
        .map(|page_ref| {
            let page = pdf.get_object_handle(page_ref);
            pdf.resolve(&page).unwrap();
            let rotate = page.try_get_key(b"/Rotate").unwrap();
            pdf.resolve(&rotate).unwrap();
            rotate.as_integer()
        })
        .collect()
}
