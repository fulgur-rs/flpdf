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
fn job_json_file_split_pages_reports_each_chunk_filename_when_verbose() {
    // qpdf reports one "wrote file" line per real chunk from inside the
    // per-chunk split loop (`libqpdf/QPDFJob.cc:3019-3021`), never the
    // requested output template. Confirmed live against qpdf 11.9.0.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-verbose.json"),
        br#"{"inputFile":"input.pdf","outputFile":"split.pdf","splitPages":"1","verbose":"","staticId":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-verbose.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    for page in 1..=3 {
        assert!(
            stdout.contains(&format!("wrote file split-{page}.pdf")),
            "stdout missing chunk report for split-{page}.pdf: {stdout}"
        );
    }
    assert!(
        !stdout.contains("wrote file split.pdf\n"),
        "verbose report must not name the unsplit output template: {stdout}"
    );
}

#[test]
fn job_json_file_split_pages_reports_earlier_chunks_after_a_later_chunk_fails() {
    // qpdf reports each chunk from inside the per-chunk split loop
    // (`libqpdf/QPDFJob.cc:3019-3021`), immediately after that chunk's write
    // succeeds, so a later chunk's failure still leaves the reports for
    // every chunk written before it. Confirmed live: `qpdf --verbose
    // --split-pages=1` with out-2.pdf pre-occupied by a directory still
    // prints "wrote file out-1.pdf" before failing.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::create_dir(directory.path().join("out-2.pdf")).unwrap();
    fs::write(
        directory.path().join("split-partial.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","splitPages":"1","verbose":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-partial.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("wrote file out-1.pdf"),
        "stdout missing the earlier, successfully written chunk's report: {stdout}"
    );
    assert!(directory.path().join("out-1.pdf").is_file());
}

#[test]
fn job_json_file_split_pages_allows_the_same_input_and_output_path() {
    // qpdf only runs the same-file rejection when `!m->split_pages`
    // (`libqpdf/QPDFJob.cc:627`): a splitting write never truncates the
    // original input in place, so aliasing input and output is not
    // destructive when splitting. Confirmed live against qpdf 11.9.0.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    let input = directory.path().join("input.pdf");
    fs::copy(fixture, &input).unwrap();
    let before = fs::read(&input).unwrap();
    fs::write(
        directory.path().join("split-same.json"),
        serde_json::json!({
            "inputFile": &input,
            "outputFile": &input,
            "splitPages": "1",
        })
        .to_string(),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-same.json")
        .assert()
        .code(0);

    assert_eq!(fs::read(&input).unwrap(), before, "input must be untouched");
    for page in 1..=3 {
        assert!(directory.path().join(format!("input-{page}.pdf")).is_file());
    }
}

#[test]
fn job_json_file_split_pages_empty_value_defaults_to_one() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-empty.json"),
        br#"{"inputFile":"input.pdf","outputFile":"empty-split.pdf","splitPages":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-empty.json")
        .assert()
        .code(0);

    for page in 1..=3 {
        assert!(directory
            .path()
            .join(format!("empty-split-{page}.pdf"))
            .is_file());
    }
    assert!(!directory.path().join("empty-split.pdf").exists());
}

#[test]
fn job_json_file_split_pages_rejects_standard_output_like_qpdf() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-stdout.json"),
        br#"{"inputFile":"input.pdf","outputFile":"-","splitPages":"1"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-stdout.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--split-pages may not be used when writing to standard output",
        ));
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
        vec![Some(0), Some(90), Some(0)],
        "qpdf rotate=90:2 targets only output page 2"
    );
}

#[test]
fn job_json_file_split_pages_rejects_a_negative_value() {
    // Unlike a genuinely non-numeric value (which qpdf's strtoll silently
    // treats as 0, falling through to an unsplit write -- see
    // job_json_file_split_pages_non_numeric_value_falls_through_to_one_unsplit_output),
    // a negative value parses successfully in qpdf and is truthy in its
    // `if (m->split_pages)` checks, only failing later during the actual
    // split loop's unsigned narrowing conversion (libqpdf/QPDFJob.cc:2970).
    // flpdf currently rejects it at parse time instead, a known tracked
    // divergence in diagnostic text/path (not in whether the operation
    // succeeds; both refuse) -- see flpdf-sp4g.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-negative.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","splitPages":"-5"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-negative.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            ".splitPages: invalid page count -5",
        ));
}

#[test]
fn job_json_file_split_pages_rejects_replace_input_like_qpdf() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-replace.json"),
        br#"{"inputFile":"input.pdf","replaceInput":"","splitPages":"1"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-replace.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--split-pages may not be used with --replace-input",
        ));
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
fn job_json_file_split_pages_non_numeric_value_falls_through_to_one_unsplit_output() {
    // qpdf converts the parameter with `QUtil::string_to_int`, whose
    // `strtoll` stage performs no conversion and returns 0 for a string with
    // no leading digit run; 0 is falsy in qpdf's `if (m->split_pages)`
    // checks, so a malformed value behaves exactly like an explicit "0" and
    // silently falls through to an ordinary, unsplit write rather than being
    // rejected. Confirmed live against qpdf 11.9.0.
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("split-invalid.json"),
        br#"{"inputFile":"input.pdf","outputFile":"out.pdf","splitPages":"not-a-number","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=split-invalid.json")
        .assert()
        .code(0);

    assert!(directory.path().join("out.pdf").is_file());
    assert!(!directory.path().join("out-1.pdf").exists());
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
