use assert_cmd::Command;
use flpdf::{PageDocumentHelper, Pdf};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

fn expected_usage(message: &str) -> String {
    format!(
        "\nflpdf: {message}\n\nFor help:\n  flpdf --help=usage       usage information\n  \
flpdf --help=topic       help on a topic\n  flpdf --help=--option    help on an option\n  \
flpdf --help             general help and a topic list\n\n"
    )
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn page_count(path: &std::path::Path) -> usize {
    let mut pdf = Pdf::open(Cursor::new(fs::read(path).unwrap())).unwrap();
    PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap()
        .len()
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
fn job_json_file_collate_values_match_qpdf() {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();

    let q_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "q.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "2,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("q.json"),
        serde_json::to_vec(&q_job).unwrap(),
    )
    .unwrap();
    let f_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "f.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "2,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("f.json"),
        serde_json::to_vec(&f_job).unwrap(),
    )
    .unwrap();

    let q_output = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=q.json")
        .output()
        .unwrap();
    assert!(
        q_output.status.success(),
        "qpdf job JSON failed: {}",
        String::from_utf8_lossy(&q_output.stderr)
    );
    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=f.json")
        .assert()
        .code(0)
        .stdout("");

    assert_eq!(page_count(&directory.path().join("q.pdf")), 6);
    assert_eq!(
        page_count(&directory.path().join("f.pdf")),
        page_count(&directory.path().join("q.pdf"))
    );

    let q_zero_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "q-zero.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "0,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("q-zero.json"),
        serde_json::to_vec(&q_zero_job).unwrap(),
    )
    .unwrap();
    let f_zero_job = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "f-zero.pdf",
        "pages": [
            {"file": "input.pdf", "range": "1-3"},
            {"file": "input.pdf", "range": "1-3"}
        ],
        "collate": "0,1",
        "staticId": ""
    });
    fs::write(
        directory.path().join("f-zero.json"),
        serde_json::to_vec(&f_zero_job).unwrap(),
    )
    .unwrap();
    let q_zero_output = ProcessCommand::new("/usr/bin/qpdf")
        .current_dir(directory.path())
        .arg("--job-json-file=q-zero.json")
        .output()
        .unwrap();
    assert!(q_zero_output.status.success());
    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=f-zero.json")
        .assert()
        .code(0)
        .stdout("");

    assert_eq!(page_count(&directory.path().join("q-zero.pdf")), 3);
    assert_eq!(
        page_count(&directory.path().join("f-zero.pdf")),
        page_count(&directory.path().join("q-zero.pdf"))
    );
}

#[test]
fn job_json_file_coalesce_contents_replaces_a_page_contents_array() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/multi-contents-one-page.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("coalesce.json"),
        br#"{"inputFile":"input.pdf","outputFile":"coalesced.pdf","coalesceContents":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=coalesce.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("coalesced.pdf")).unwrap(),
    ))
    .unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let contents = page.try_get_key(b"/Contents").unwrap();
    pdf.resolve(&contents).unwrap();
    assert!(
        contents.as_stream_dict().is_some(),
        "coalesceContents must replace an array with one stream"
    );
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, page_ref).unwrap(),
        b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET\nBT /F1 12 Tf 100 680 Td (World) Tj ET\n"
    );
}

#[test]
fn job_json_file_flatten_rotation_bakes_rotate_into_page_content() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page-r90.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("flatten.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flattened.pdf","flattenRotation":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flatten.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("flattened.pdf")).unwrap(),
    ))
    .unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    assert!(
        !page.has_key(b"/Rotate"),
        "flattenRotation must remove /Rotate"
    );

    let media_box = page.try_get_key(b"/MediaBox").unwrap();
    pdf.resolve(&media_box).unwrap();
    let media_box = media_box.as_array().unwrap();
    assert_eq!(
        media_box
            .iter()
            .map(|value| value.as_integer())
            .collect::<Vec<_>>(),
        vec![Some(0), Some(0), Some(792), Some(612)]
    );

    assert!(
        flpdf::pages::page_content_bytes(&mut pdf, page_ref)
            .unwrap()
            .starts_with(b"q\n0 -1 1 0 0 612 cm\n"),
        "flattenRotation must prepend qpdf's 90-degree matrix"
    );
}

#[test]
fn job_json_file_flatten_rotation_preserves_orphan_widget_warning_status() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("flatten-warning.json"),
        br#"{"inputFile":"input.pdf","outputFile":"flattened.pdf","flattenRotation":"","staticId":""}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=flatten-warning.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("this widget annotation is not reachable from /AcroForm"),
        "flattenRotation must preserve qpdf's orphan-widget warning"
    );
    assert!(directory.path().join("flattened.pdf").is_file());
}

#[test]
fn job_json_file_generate_appearances_clears_need_marker_and_adds_ap() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("input.pdf"),
        job_json_appearance_fixture(),
    )
    .unwrap();
    fs::write(
        directory.path().join("appearances.json"),
        br#"{"inputFile":"input.pdf","outputFile":"generated.pdf","generateAppearances":"","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=appearances.json")
        .assert()
        .code(0)
        .stdout("");

    let mut pdf = Pdf::open(Cursor::new(
        fs::read(directory.path().join("generated.pdf")).unwrap(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let acroform = root.try_get_key(b"/AcroForm").unwrap();
    pdf.resolve(&acroform).unwrap();
    let need_appearances = acroform.try_get_key(b"/NeedAppearances").unwrap();
    pdf.resolve(&need_appearances).unwrap();
    assert_ne!(
        need_appearances.as_boolean(),
        Some(true),
        "generateAppearances must clear qpdf's NeedAppearances marker"
    );

    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let annots = page.try_get_key(b"/Annots").unwrap();
    pdf.resolve(&annots).unwrap();
    let widget = annots.as_array().unwrap()[0].clone();
    pdf.resolve(&widget).unwrap();
    let appearance = widget.try_get_key(b"/AP").unwrap();
    pdf.resolve(&appearance).unwrap();
    let normal = appearance.try_get_key(b"/N").unwrap();
    pdf.resolve(&normal).unwrap();
    assert!(
        normal.as_stream_dict().is_some(),
        "generateAppearances must install a normal widget appearance"
    );
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

fn job_json_appearance_fixture() -> Vec<u8> {
    assemble_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >> >>\n",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>\n",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [4 0 R] >>\n",
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1) /V (Hello) /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720] /P 3 0 R >>\n",
        b"<< /Length 14 >>\nstream\nBT (pg) Tj ET\nendstream\n",
    ])
}

fn assemble_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0u64; objects.len() + 1];
    for (index, body) in objects.iter().enumerate() {
        let object_number = index + 1;
        offsets[object_number] = bytes.len() as u64;
        bytes.extend_from_slice(format!("{object_number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"endobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}
