use flpdf::job::{
    JobDocument, JobExitCode, JsonJobOptions, JsonJobOutput, JsonStreamData, PageSpecInput, QPDFJob,
};
use flpdf::json_inspect::DecodeLevel;
use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, PageRange, Pdf, PdfOpenOptions, PdfWriter, QPDFLogger};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(target_os = "linux")]
use std::path::PathBuf;

const COMPLETE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.3"},
    {
      "obj:1 0 R": {"value": {"/Pages": "2 0 R", "/Type": "/Catalog"}},
      "obj:2 0 R": {"value": {"/Count": 0, "/Kids": [], "/Type": "/Pages"}},
      "trailer": {"value": {"/Root": "1 0 R", "/Size": 3}}
    }
  ]
}"#;

const ROOTLESS_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.3"},
    {"trailer": {"value": {}}}
  ]
}"#;

const UPDATE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2},
    {"obj:1 0 R": {"value": {"/Marker": true, "/Pages": "2 0 R", "/Type": "/Catalog"}}}
  ]
}"#;

#[derive(Default)]
struct SinkState {
    bytes: Vec<u8>,
}

struct RecordingSink {
    state: Arc<Mutex<SinkState>>,
}

struct FailingSink;

impl Pipeline for RecordingSink {
    fn identifier(&self) -> &str {
        "job lifecycle test sink"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.state.lock().unwrap().bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

impl Pipeline for FailingSink {
    fn identifier(&self) -> &str {
        "job lifecycle failing sink"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Err(PipelineError::runtime("warning sink failed"))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn logger_with_warning_sink() -> (QPDFLogger, Arc<Mutex<SinkState>>) {
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger.set_warn(Some(PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    })));
    (logger, state)
}

fn logger_with_info_sink() -> (QPDFLogger, Arc<Mutex<SinkState>>) {
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger.set_info(Some(PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    })));
    (logger, state)
}

fn logger_with_error_sink() -> (QPDFLogger, Arc<Mutex<SinkState>>) {
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger.set_error(Some(PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    })));
    (logger, state)
}

#[cfg(target_os = "linux")]
fn non_utf8_path(directory: &Path, filename: &[u8]) -> PathBuf {
    let mut bytes = directory.as_os_str().as_bytes().to_vec();
    bytes.push(b'/');
    bytes.extend_from_slice(filename);
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(target_os = "linux")]
fn job_json_with_paths(input: &Path, output: &Path) -> Vec<u8> {
    let mut json = b"{\"inputFile\":\"".to_vec();
    json.extend_from_slice(input.as_os_str().as_bytes());
    json.extend_from_slice(b"\",\"outputFile\":\"");
    json.extend_from_slice(output.as_os_str().as_bytes());
    json.extend_from_slice(b"\"}");
    json
}

fn xref_stream_with_extra_data() -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let xref_offset = bytes.len();
    let xref_header =
        b"1 0 obj\n<< /Type /XRef /Size 4 /Root 2 0 R /W [1 3 1] /Index [0 4] /Length 21 >>\nstream\n";
    let xref_tail = b"\nendstream\nendobj\n";
    let catalog_offset = xref_offset + xref_header.len() + 21 + xref_tail.len();
    let catalog = b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n";
    let pages_offset = catalog_offset + catalog.len();
    let pages = b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n";

    let mut entries = Vec::with_capacity(21);
    entries.extend_from_slice(&[0, 0, 0, 0xff, 0xff]);
    for offset in [xref_offset, catalog_offset, pages_offset] {
        entries.push(1);
        entries.extend_from_slice(&(offset as u32).to_be_bytes()[1..]);
        entries.push(0);
    }
    entries.push(0);

    bytes.extend_from_slice(xref_header);
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(xref_tail);
    bytes.extend_from_slice(catalog);
    bytes.extend_from_slice(pages);
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

#[test]
fn new_job_matches_qpdf_defaults() {
    let job = QPDFJob::default();

    assert_eq!(job.message_prefix(), "qpdf");
    assert_eq!(job.logger(), QPDFLogger::default_logger());
    assert!(!job.has_warnings());
    assert_eq!(job.complete(false).unwrap(), JobExitCode::Success);
    assert_eq!(JobExitCode::Error.as_i32(), 2);
    assert_eq!(JobExitCode::Success.as_i32(), 0);
    assert_eq!(JobExitCode::Warning.as_i32(), 3);
}

#[test]
fn job_json_byte_entry_point_accepts_literal_high_bit_password_bytes() {
    let mut json = br#"{"inputFile":"input.pdf","outputFile":"output.pdf","password":""}"#.to_vec();
    let password_end = json.len() - 2;
    json.insert(password_end, 0x80);

    let mut job = QPDFJob::new();
    job.initialize_from_json_bytes(&json)
        .expect("qpdf accepts raw high-bit bytes in a JSON string");

    let mut partial_json = br#"{"password":""}"#.to_vec();
    partial_json.insert(partial_json.len() - 2, 0x80);
    let mut partial_job = QPDFJob::new();
    partial_job
        .initialize_from_json_partial_bytes(&partial_json)
        .expect("partial byte entry point must preserve raw JSON bytes");
}

#[test]
fn qpdfjob_error_report_matches_the_qpdf_c_wrapper_boundary() {
    let (logger, state) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_message_prefix("qpdfjob json");

    job.report_job_error(&Error::Usage(flpdf::UsageError::new(
        "an output file name is required; use - for standard output",
    )))
    .unwrap();

    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdfjob json: an output file name is required; use - for standard output\n"
    );

    state.lock().unwrap().bytes.clear();
    job.report_job_error(&Error::SystemBytes(
        b"json-input-\xff: errors found in JSON".to_vec(),
    ))
    .unwrap();
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdfjob json: json-input-\xff: errors found in JSON\n"
    );
}

#[test]
fn keep_files_open_policy_counts_distinct_page_sources_and_honors_overrides() {
    let range = PageRange::parse("1").unwrap();
    let one_source = [
        PageSpecInput::new(1, range.clone()),
        PageSpecInput::new(1, range.clone()),
    ];
    let two_sources = [
        PageSpecInput::new(1, range.clone()),
        PageSpecInput::new(2, range),
    ];

    let mut job = QPDFJob::new();
    job.set_keep_files_open_threshold(1);
    assert!(job.keep_files_open_for_page_specs(&one_source));
    assert!(!job.keep_files_open_for_page_specs(&two_sources));

    job.set_keep_files_open(true);
    assert!(job.keep_files_open_for_page_specs(&two_sources));
    job.set_keep_files_open(false);
    assert!(!job.keep_files_open_for_page_specs(&one_source));

    assert_eq!(
        QPDFJob::parse_keep_files_open_threshold("+50junk").unwrap(),
        50
    );
}

#[test]
fn keep_files_open_policy_is_parsed_at_argv_and_json_job_boundaries() {
    let range = PageRange::parse("1").unwrap();
    let specs = [PageSpecInput::new(1, range.clone())];

    let mut argv_job = QPDFJob::new();
    argv_job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            "--keep-files-open=n".to_owned(),
            "--keep-files-open-threshold=+50junk".to_owned(),
        ])
        .unwrap();
    assert!(!argv_job.keep_files_open_for_page_specs(&specs));

    let json = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "output.pdf",
        "keepFilesOpen": "n",
        "keepFilesOpenThreshold": "50junk"
    })
    .to_string();
    let mut json_job = QPDFJob::new();
    json_job.initialize_from_json(&json).unwrap();
    assert!(!json_job.keep_files_open_for_page_specs(&specs));
}

#[test]
fn argv_keep_files_open_rejects_an_unknown_choice() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            "--keep-files-open=maybe".to_owned(),
        ])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "invalid value for --keep-files-open: maybe"
    ));
}

#[test]
fn argv_job_run_writes_output_and_reports_progress() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("argv-output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--deterministic-id".to_owned(),
        "--progress".to_owned(),
        "--password=unused".to_owned(),
        "--decrypt".to_owned(),
        "--object-streams=disable".to_owned(),
        "--".to_owned(),
    ];
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_for_job = Arc::clone(&progress);
    let mut job = QPDFJob::new();
    job.register_progress_reporter(move |percent| {
        progress_for_job.lock().unwrap().push(percent);
        Ok(())
    });
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
    let progress = progress.lock().unwrap();
    assert_eq!(progress.first(), Some(&0));
    assert_eq!(progress.last(), Some(&100));
}

#[test]
fn job_document_boundary_erases_file_empty_and_json_reader_kinds() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");

    let mut file_job = QPDFJob::new();
    let file_document: JobDocument = file_job
        .open_document(
            BufReader::new(File::open(&input).unwrap()),
            input.display().to_string(),
            PdfOpenOptions::default(),
        )
        .unwrap();

    let mut empty_job = QPDFJob::new();
    let empty_document: JobDocument = empty_job.create_empty_document().unwrap();

    let mut json_job = QPDFJob::new();
    let json_document: JobDocument = json_job
        .create_from_json_document(Cursor::new(COMPLETE_JSON), "input.json")
        .unwrap();

    assert_eq!(file_document.version(), "1.7");
    assert_eq!(empty_document.version(), "1.3");
    assert_eq!(json_document.version(), "1.3");
}

#[test]
fn json_job_empty_input_uses_the_job_document_boundary() {
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("empty.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": ""
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(pdf.version(), "1.3");
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 0);
}

#[test]
fn json_job_empty_encryption_status_returns_qpdf_exit_code() {
    for option in ["isEncrypted", "requiresPassword"] {
        let json = serde_json::json!({
            "empty": "",
            option: ""
        })
        .to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json(&json).unwrap();

        assert_eq!(job.run().unwrap(), JobExitCode::Error);
    }
}

#[test]
fn argv_job_run_returns_warning_status_for_repairable_input() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("warning-output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--static-id".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(output.exists());
}

#[test]
fn xref_stream_extra_data_produces_qpdf_warning_status() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("xref-extra.pdf");
    let output = tempdir.path().join("xref-extra-output.pdf");
    std::fs::write(&input, xref_stream_with_extra_data()).unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--static-id".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(output.exists());
}

#[test]
fn json_job_run_writes_output_with_static_id_and_generated_object_streams() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("json-output.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "staticId": "",
        "decrypt": "",
        "progress": "",
        "objectStreams": "generate"
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
    let bytes = std::fs::read(output).unwrap();
    assert!(bytes
        .windows(b"/Type /ObjStm".len())
        .any(|window| { window == b"/Type /ObjStm" }));
}

#[test]
fn json_job_run_applies_pages_and_attachments() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("pages-and-attachment.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "pages": [{"file": fixture}],
        "addAttachment": [{"file": fixture, "key": "fixture-key"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 1);
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"fixture-key")
        .unwrap()
        .is_some());
}

#[test]
fn json_job_nested_job_json_file_retains_outer_and_inner_attachments() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let nested = tempdir.path().join("nested.json");
    let output = tempdir.path().join("nested-attachments.pdf");
    std::fs::write(
        &nested,
        serde_json::json!({
            "addAttachment": [{"file": fixture, "key": "inner-key"}]
        })
        .to_string(),
    )
    .unwrap();
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "jobJsonFile": nested,
        "addAttachment": [{"file": fixture, "key": "outer-key"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"inner-key")
        .unwrap()
        .is_some());
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"outer-key")
        .unwrap()
        .is_some());
}

#[test]
fn json_job_nested_job_json_file_rejects_repeated_pages() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let nested = tempdir.path().join("nested-pages.json");
    let output = tempdir.path().join("nested-pages.pdf");
    std::fs::write(
        &nested,
        serde_json::json!({
            "pages": [{"file": fixture}]
        })
        .to_string(),
    )
    .unwrap();
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "jobJsonFile": nested,
        "pages": [{"file": fixture}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_json(&json)
        .expect_err("qpdf allows --pages only once across nested job JSON");
    assert!(matches!(
        error,
        Error::Usage(usage) if usage.to_string() == "--pages may only be specified one time"
    ));
}

/// qpdf's `handleRotations` resolves each `--rotate` range against the real
/// page count and then filters `0 <= pageno < npages` before touching
/// `pages`, so a `--collate=0`-produced empty document rotates nothing
/// without erroring (confirmed live: `--collate=0 --rotate=90` exits 0).
#[test]
fn json_job_run_applies_rotate_to_a_collate_zero_empty_page_selection() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("collate-zero-rotate.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "pages": [{"file": fixture}, {"file": fixture}],
        "collate": "0",
        "rotate": "90"
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 0);
}

/// qpdf keys its opened-source cache by filename alone
/// (`page_spec_qpdfs.count(page_spec.filename) == 0`, `QPDFJob.cc:2389`),
/// reusing the same already-open QPDF for a repeated literal path rather
/// than reopening it — so a page spec's own `password` is only consulted
/// the first time a filename is seen. Encode this as an observable pass/fail:
/// the first spec references an encrypted fixture with its correct (empty)
/// password; a second spec repeats the exact same path with a wrong
/// password. If flpdf reopened the file for the second spec (the pre-fix
/// bug), the wrong password would fail the job; deduplicating by path
/// (matching qpdf) never attempts that second open, so the wrong password
/// is never consulted and the job succeeds.
#[test]
fn json_job_run_reuses_an_already_opened_page_source_for_a_repeated_filename() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("repeated-filename.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "keepFilesOpenThreshold": "1",
        "pages": [
            {"file": &fixture, "range": "1"},
            {"file": &fixture, "password": "definitely-wrong", "range": "2"},
        ],
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(
        job.run().unwrap(),
        JobExitCode::Success,
        "a repeated filename must reuse the first spec's already-open \
         source and never consult a later spec's password"
    );
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 2);
}

/// 2-page document with an outline item pointing at page 2 (obj 4).
fn build_outline_fixture() -> Vec<u8> {
    use std::collections::BTreeMap;
    let mut objs: BTreeMap<u32, String> = BTreeMap::new();
    objs.insert(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".into(),
    );
    objs.insert(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into());
    for n in 3..=4 {
        objs.insert(
            n,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
    }
    objs.insert(
        10,
        "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 1 >>".into(),
    );
    objs.insert(
        20,
        "<< /Title (P2) /Parent 10 0 R /Dest [4 0 R /Fit] >>".into(),
    );

    let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in &objs {
        offs.insert(*n, raw.len());
        raw.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let max_num = *objs.keys().max().unwrap();
    let xref_pos = raw.len();
    raw.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        if let Some(&off) = offs.get(&i) {
            raw.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        } else {
            raw.extend_from_slice(b"0000000000 65535 f \n");
        }
    }
    raw.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );
    raw
}

/// A single-source `--pages . 1` job (qpdf's own-file page selection) takes
/// the in-place `QPDFJob::handle_page_specs` route
/// (`PageSpecJobOutput::InPlace`). Before this test, `QPDFJob::run`'s
/// in-place branch pruned the subset without first calling
/// `remap_outline_and_dests`, leaving the outline's `/Dest` pointing at the
/// now-removed, ungutted page 2 object instead of the null-and-kept-live
/// value qpdf produces (`libqpdf/QPDF_optimization.cc`'s outline/dest sweep,
/// matched by the merged-source CLI route's own
/// `remap_outline_and_dests` call in `flpdf-cli`).
#[test]
fn json_job_run_in_place_page_subset_remaps_outline_dests() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("outline-fixture.pdf");
    std::fs::write(&input, build_outline_fixture()).unwrap();
    let output = tempdir.path().join("in-place-subset.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "pages": [{"file": ".", "range": "1"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 1);

    // Writing renumbers objects, so locate the removed page through the
    // surviving outline item's `/Dest` rather than assuming the original
    // fixture's object number 4 persists in the output.
    let root = pdf.trailer_key_handle(b"Root");
    pdf.resolve(&root).unwrap();
    let outlines = root.as_dictionary().unwrap()[b"/Outlines".as_slice()].clone();
    pdf.resolve(&outlines).unwrap();
    let first_item = outlines.as_dictionary().unwrap()[b"/First".as_slice()].clone();
    pdf.resolve(&first_item).unwrap();
    let dest = first_item.as_dictionary().unwrap()[b"/Dest".as_slice()].clone();
    pdf.resolve(&dest).unwrap();
    let target = dest.as_array().unwrap()[0].clone();
    let target_ref = target
        .object_ref()
        .expect("the outline dest target must still be an indirect reference");
    pdf.resolve(&target).unwrap();

    assert!(
        target.is_null(),
        "the removed page kept alive by the outline dest must be nulled, not left as a \
         dangling /Type /Page dict"
    );
    assert!(
        pdf.live_object_refs().contains(&target_ref),
        "the nulled-but-referenced page must stay live"
    );
}

#[test]
fn json_job_run_applies_encryption_and_json_output_mode() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let pdf_output = tempdir.path().join("encrypted.pdf");
    let json_output = tempdir.path().join("encrypted.json");
    let json = serde_json::json!({
        "inputFile": fixture,
        "outputFile": pdf_output,
        "staticId": "",
        "encrypt": {
            "userPassword": "u",
            "ownerPassword": "o",
            "128bit": {"useAes": "y"}
        }
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let encrypted = Pdf::open_with_options(
        BufReader::new(File::open(pdf_output).unwrap()),
        PdfOpenOptions {
            password: b"u".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();
    assert!(encrypted.is_encrypted());

    let json = serde_json::json!({
        "inputFile": fixture,
        "outputFile": json_output,
        "jsonOutput": "2"
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let output = std::fs::read_to_string(json_output).unwrap();
    assert!(output.contains("\"jsonversion\": 2"));
    assert!(!output.contains("\"version\": 2"));
}

#[test]
fn json_job_partial_initialization_defers_missing_output_to_run() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input}).to_string();
    let mut job = QPDFJob::new();

    job.initialize_from_json_partial(&json).unwrap();
    let error = job.run().unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "an output file name is required; use - for standard output"
    ));
}

#[test]
fn json_job_partial_initialization_still_rejects_a_malformed_output_file() {
    // qpdf's JSONHandler dispatches `outputFile` to a string-only handler
    // (QPDFJob_json.cc:262-265) and rejects any other present type with a
    // usage error (JSONHandler.cc:186), regardless of partial-init mode: a
    // present-but-wrong-typed value is not the same as an absent key.
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input, "outputFile": 42}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json_partial(&json).unwrap_err();

    assert!(matches!(error, Error::Usage(ref message)
        if message.to_string().contains("outputFile") && message.to_string().contains("must be a string")));
}

#[test]
fn json_job_rejects_a_malformed_output_file_outside_partial_mode() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input, "outputFile": false}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json(&json).unwrap_err();

    assert!(matches!(error, Error::Usage(ref message)
        if message.to_string().contains("outputFile") && message.to_string().contains("must be a string")));
}

#[test]
fn json_job_progress_uses_the_qpdf_default_info_reporter() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("progress-output.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "progress": "",
    })
    .to_string();
    let (logger, state) = logger_with_info_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let info = state.lock().unwrap().bytes.clone();
    assert!(info
        .windows(b"write progress: 0%\n".len())
        .any(|window| { window == b"write progress: 0%\n" }));
    assert!(info
        .windows(b"write progress: 100%\n".len())
        .any(|window| { window == b"write progress: 100%\n" }));
}

#[test]
fn json_job_progress_logger_failures_abort_and_propagate_from_write() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("progress-output.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "progress": "",
    })
    .to_string();
    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn json_job_progress_labels_dash_output_as_standard_output() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "-",
        "progress": "",
    })
    .to_string();
    let (logger, info_state) = logger_with_info_sink();
    let save_state = Arc::new(Mutex::new(SinkState::default()));
    logger
        .set_save(
            Some(PipelineHandle::new(RecordingSink {
                state: Arc::clone(&save_state),
            })),
            false,
        )
        .unwrap();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let info = info_state.lock().unwrap().bytes.clone();
    let output = save_state.lock().unwrap().bytes.clone();
    assert!(info
        .windows(b"standard output: write progress: 0%\n".len())
        .any(|window| window == b"standard output: write progress: 0%\n"));
    assert!(output.starts_with(b"%PDF-"));
}

#[test]
fn json_job_rejects_same_input_output_before_truncating_a_hard_link() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("hard-link.pdf");
    std::fs::copy(fixture, &input).unwrap();
    std::fs::hard_link(&input, &output).unwrap();
    let before = std::fs::read(&input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
    })
    .to_string();

    let mut full_job = QPDFJob::new();
    let error = full_job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string()
                == "input file and output file are the same; use --replace-input to intentionally overwrite the input"
    ));

    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(&json).unwrap();

    let error = match job.create_qpdf() {
        Err(error) => error,
        Ok(_) => panic!("same-file configuration must fail before opening the input"),
    };
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string()
                == "input file and output file are the same; use --replace-input to intentionally overwrite the input"
    ));
    assert_eq!(std::fs::read(&input).unwrap(), before);
}

#[test]
fn create_qpdf_reports_an_ordinary_output_configuration_failure() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let (logger, error_state) = logger_with_error_sink();
    logger.info(Vec::<u8>::new()).unwrap();

    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "-",
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(&json).unwrap();

    assert!(job.create_qpdf().unwrap().is_none());
    assert!(error_state
        .lock()
        .unwrap()
        .bytes
        .windows(b"QPDFLogger: called setSave on standard output after standard output has already been used".len())
        .any(|window| {
            window
                == b"QPDFLogger: called setSave on standard output after standard output has already been used"
        }));
}

#[test]
fn json_job_dash_output_uses_the_job_save_pipeline() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    std::fs::copy(fixture, &input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "-",
    })
    .to_string();
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger
        .set_save(
            Some(PipelineHandle::new(RecordingSink {
                state: Arc::clone(&state),
            })),
            false,
        )
        .unwrap();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();
    let dash = Path::new("-");
    let _ = std::fs::remove_file(dash);

    let status = job.run().unwrap();
    let bytes = state.lock().unwrap().bytes.clone();
    let literal_dash_exists = dash.exists();
    let _ = std::fs::remove_file(dash);

    assert_eq!(status, JobExitCode::Success);
    assert!(
        bytes.starts_with(b"%PDF-"),
        "stdout sink must receive PDF bytes"
    );
    assert!(
        !literal_dash_exists,
        "dash must not be opened as a literal output path"
    );
}

#[test]
fn json_job_without_output_is_a_usage_error() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "an output file name is required; use - for standard output"
    ));

    let error = job.run().unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "an output file name is required; use - for standard output"
    ));
}

#[test]
fn json_job_accepts_linearize_configuration() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": tempdir.path().join("out.pdf"),
        "linearize": ""
    })
    .to_string();
    let mut job = QPDFJob::new();

    job.initialize_from_json(&json)
        .expect("linearize is part of the full job JSON schema");
}

#[test]
fn json_job_deterministic_id_repeats_identical_output() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let first = tempdir.path().join("first.pdf");
    let second = tempdir.path().join("second.pdf");

    for output in [&first, &second] {
        let json = serde_json::json!({
            "inputFile": input,
            "outputFile": output,
            "deterministicId": ""
        })
        .to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json(&json).unwrap();
        assert_eq!(job.run().unwrap(), JobExitCode::Success);
    }

    assert_eq!(
        std::fs::read(first).unwrap(),
        std::fs::read(second).unwrap()
    );
}

#[test]
fn create_qpdf_and_write_qpdf_are_separate_job_boundaries() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("separate-output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--static-id".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    assert_eq!(job.write_qpdf(&mut pdf).unwrap(), JobExitCode::Success);
    assert!(output.exists());
}

#[test]
fn write_qpdf_failure_returns_qpdf_error_status() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        tempdir.path().to_string_lossy().into_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    assert_eq!(job.write_qpdf(&mut pdf).unwrap(), JobExitCode::Error);
}

#[test]
fn write_qpdf_without_an_output_returns_qpdf_error_status() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let args = vec!["qpdfjob".to_owned(), input.to_string_lossy().into_owned()];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    assert_eq!(job.write_qpdf(&mut pdf).unwrap(), JobExitCode::Error);
}

#[test]
fn missing_input_returns_qpdf_error_status_without_panicking() {
    let tempdir = tempfile::tempdir().unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        tempdir
            .path()
            .join("missing.pdf")
            .to_string_lossy()
            .into_owned(),
        tempdir
            .path()
            .join("output.pdf")
            .to_string_lossy()
            .into_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn unknown_qpdf_job_argv_is_a_usage_error() {
    let args = vec!["qpdfjob".to_owned(), "--not-a-qpdf-option".to_owned()];
    let mut job = QPDFJob::new();

    let error = job.initialize_from_argv(&args).unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "unrecognized argument --not-a-qpdf-option"
    ));
}

#[test]
fn argv_usage_rejects_short_options_too_many_positionals_and_missing_input() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&["qpdfjob".to_owned(), "-x".to_owned()])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "unrecognized argument -x"
    ));

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "a.pdf".to_owned(),
            "b.pdf".to_owned(),
            "c.pdf".to_owned(),
        ])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "unknown argument c.pdf"
    ));

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&["qpdfjob".to_owned()])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "an input file name is required"
    ));
}

#[test]
fn create_qpdf_reports_unconfigured_and_malformed_inputs() {
    let mut job = QPDFJob::new();
    assert!(job.create_qpdf().unwrap().is_none());

    let tempdir = tempfile::tempdir().unwrap();
    let malformed = tempdir.path().join("malformed.pdf");
    std::fs::write(&malformed, b"not a PDF").unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        malformed.to_string_lossy().into_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    assert!(job.create_qpdf().unwrap().is_none());
}

#[test]
fn run_check_and_check_operation_failure_map_to_error_status() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        "--check".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_argv(&args).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn invalid_qpdf_job_json_is_rejected_at_initialization() {
    let mut job = QPDFJob::new();
    assert!(job.initialize_from_json("[]").is_err());
    assert!(job
        .initialize_from_json("{\"outputFile\":\"out.pdf\"}")
        .is_err());
    assert!(job
        .initialize_from_json("{\"inputFile\":\"input.pdf\",\"objectStreams\":\"unknown\"}")
        .is_err());
}

#[test]
fn job_json_rejects_v1_only_keys_under_json_version_2() {
    // qpdf validates jsonKey/version compatibility unconditionally in
    // checkConfiguration (`QPDFJob.cc:630-637`), confirmed against live
    // qpdf 11.9.0: `--json-key=objects` errors with this exact message even
    // without `--json` at all, since `m->json_version` defaults to 0, which
    // falls into the "not version 1" branch.
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.json");
    let json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": output,
        "json": "2",
        "jsonKey": ["objects"]
    })
    .to_string();
    let mut job = QPDFJob::new();
    let error = job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "json keys \"objects\" and \"objectinfo\" are only valid for json version 1"
    ));
}

#[test]
fn job_json_rejects_qpdf_key_under_json_version_1() {
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.json");
    let json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": output,
        "json": "1",
        "jsonKey": ["qpdf"]
    })
    .to_string();
    let mut job = QPDFJob::new();
    let error = job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "json key \"qpdf\" is only valid for json version > 1"
    ));
}

#[test]
fn job_json_password_file_reads_raw_bytes_and_keeps_only_the_first_line() {
    // The discarded second line contains a raw non-UTF-8 byte (0xFF is never
    // a valid UTF-8 lead byte on its own): `read_to_string` rejects the
    // whole file before authentication even gets a chance to run, while the
    // byte-preserving first-line split (`QUtil::read_lines_from_file` +
    // `lines.front()`, `QUtil.cc:1231-1286`) only ever looks at the first
    // line's bytes, so the password authenticates successfully. The first
    // line itself ends in `\r\n` to also exercise the trailing-`\r` strip.
    let encrypted_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let password_file = tempdir.path().join("password.txt");
    std::fs::write(
        &password_file,
        b"user-v4-aes\r\nignored line with \xffinvalid utf-8\n",
    )
    .unwrap();
    let output = tempdir.path().join("out.pdf");
    let json = serde_json::json!({
        "inputFile": encrypted_pdf,
        "outputFile": output,
        "passwordFile": password_file,
        "decrypt": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let written = std::fs::read(&output).unwrap();
    let mut opened = Pdf::open(Cursor::new(written))
        .expect("output must be decrypted and openable without a password");
    let _ = opened.root_handle().unwrap();
}

#[test]
fn job_json_no_warn_suppresses_document_open_warnings_not_just_the_summary() {
    // qpdf's noWarn (`Config::noWarn`, `QPDFJob_config.cc:407-410`) applies
    // `pdf.setSuppressWarnings(true)` to every QPDF the job opens
    // (`QPDFJob.cc:663-665`), not just the final completion summary.
    let (logger, state) = logger_with_warning_sink();
    let damaged = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.pdf");
    let json = serde_json::json!({
        "inputFile": damaged,
        "outputFile": output,
        "noWarn": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();
    // The repaired document still recorded a warning internally (qpdf's
    // `QPDF::warn` always pushes to `m->warnings` regardless of
    // `suppress_warnings`, which only gates whether it is printed), so the
    // job's own exit code is still `Warning` even though nothing is printed.
    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(
        state.lock().unwrap().bytes.is_empty(),
        "noWarn must suppress recovery warnings emitted while opening the document, not just \
         the final completion summary"
    );
}

#[test]
fn completion_emits_one_qpdf_warning_summary_and_warning_status() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_message_prefix("flpdf");
    job.record_warnings();

    assert_eq!(
        job.complete(true).unwrap(),
        JobExitCode::Warning,
        "qpdf uses status 3 for recoverable warnings"
    );
    assert_eq!(
        state.lock().unwrap().bytes,
        b"flpdf: operation succeeded with warnings; resulting file may have some problems\n"
    );
}

#[test]
fn warning_suppression_keeps_warning_status_but_suppresses_summary() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_suppress_warnings(true);
    job.record_warnings();

    assert_eq!(job.complete(false).unwrap(), JobExitCode::Warning);
    assert!(state.lock().unwrap().bytes.is_empty());
}

#[test]
fn warnings_exit_zero_changes_only_the_exit_status() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_warnings_exit_zero(true);
    job.record_warnings();

    assert_eq!(job.complete(false).unwrap(), JobExitCode::Success);
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: operation succeeded with warnings\n"
    );
}

#[test]
fn warning_sink_errors_are_returned_to_the_caller() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.record_warnings();

    assert!(matches!(
        job.complete(false),
        Err(Error::System(message)) if message == "warning sink failed"
    ));
}

#[test]
fn show_linearization_propagates_custom_info_sink_failure() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    logger.set_warn(Some(logger.discard()));
    logger.set_error(Some(logger.discard()));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let mut pdf = job
        .open(
            BufReader::new(File::open(&path).unwrap()),
            path.display().to_string(),
            PdfOpenOptions::default(),
        )
        .unwrap();

    let error = job
        .show_linearization(&mut pdf)
        .expect_err("custom info sink failure must propagate");
    assert!(matches!(
        error,
        Error::System(message) if message == "warning sink failed"
    ));
}

#[test]
fn show_linearization_propagates_custom_warning_sink_failure() {
    let mut bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
    )
    .unwrap();
    let marker = b"/O 6 /E";
    let replacement = b"/O 7 /E";
    let offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("linearized fixture should contain /O");
    bytes[offset..offset + marker.len()].copy_from_slice(replacement);

    let logger = QPDFLogger::create();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    logger.set_error(Some(logger.discard()));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let mut pdf = job
        .open(
            Cursor::new(bytes),
            "linearized-mismatch.pdf",
            PdfOpenOptions::default(),
        )
        .unwrap();

    let error = job
        .show_linearization(&mut pdf)
        .expect_err("custom warning sink failure must propagate");
    assert!(matches!(
        error,
        Error::System(message) if message == "warning sink failed"
    ));
}

/// `write_qpdf` emits the "supplied password looks like a Unicode password"
/// warning through the custom *error* sink (qpdf's `--password-mode=auto`
/// warning, `QPDF_encryption.cc`), not the warning sink used by the tests
/// above. This pins that its `?` propagates a custom error-sink failure the
/// same way.
#[test]
fn write_qpdf_propagates_custom_error_sink_failure_for_auto_password_warning() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();
    let output = directory.path().join("output.pdf");

    let logger = QPDFLogger::create();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(logger.discard()));
    logger.set_error(Some(PipelineHandle::new(FailingSink)));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "passwordMode": "auto",
        "encrypt": {
            "userPassword": "😀",
            "ownerPassword": "owner",
            "128bit": {"useAes": "y"}
        }
    })
    .to_string();
    job.initialize_from_json_partial(&json).unwrap();

    let mut pdf = job
        .open(
            BufReader::new(File::open(&input).unwrap()),
            input.display().to_string(),
            PdfOpenOptions::default(),
        )
        .unwrap();

    let error = job
        .write_qpdf(&mut pdf)
        .expect_err("custom error sink failure must propagate");
    assert!(matches!(
        error,
        Error::System(message) if message == "warning sink failed"
    ));
}

#[test]
fn document_repair_warnings_feed_the_shared_completion_state() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let pdf = Pdf::open_with_repair(BufReader::new(File::open(path).unwrap())).unwrap();
    assert!(!pdf.repair_diagnostics().entries().is_empty());

    let mut job = QPDFJob::new();
    job.record_document_warnings(&pdf);

    assert!(job.has_warnings());
}

#[test]
fn job_open_installs_logger_before_open_warnings() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let (job_logger, job_state) = logger_with_warning_sink();
    let (option_logger, option_state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(job_logger);

    let pdf = job
        .open(
            BufReader::new(File::open(path).unwrap()),
            "repairable.pdf",
            PdfOpenOptions {
                repair: true,
                logger: Some(option_logger),
                ..PdfOpenOptions::default()
            },
        )
        .expect("repairable input should open");

    assert!(!pdf.repair_diagnostics().entries().is_empty());
    assert!(job.has_warnings());
    assert!(!job_state.lock().unwrap().bytes.is_empty());
    assert!(
        option_state.lock().unwrap().bytes.is_empty(),
        "open diagnostics must use the job logger rather than caller options"
    );
}

#[test]
fn inspection_completes_through_the_shared_warning_boundary() {
    let mut job = QPDFJob::new();
    let (logger, state) = logger_with_warning_sink();
    job.set_logger(logger);
    let mut pdf = job
        .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
        .expect("complete JSON input");
    job.record_warnings();

    let status = job
        .inspect(&mut pdf, |document| -> flpdf::Result<()> {
            assert_eq!(document.root_ref(), Some(flpdf::ObjectRef::new(1, 0)));
            Ok(())
        })
        .expect("inspection completion");

    assert_eq!(status, JobExitCode::Warning);
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: operation succeeded with warnings\n"
    );
}

#[test]
fn registered_progress_reporter_is_attached_to_each_writer() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let mut job = QPDFJob::new();
    let progress_for_callback = Arc::clone(&progress);
    job.register_progress_reporter(move |percent| {
        progress_for_callback.lock().unwrap().push(percent);
        Ok(())
    });

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().unwrap();
    job.configure_writer_progress(&mut writer);
    writer.write().unwrap();
    assert!(!writer.get_buffer().unwrap().is_empty());

    let progress = progress.lock().unwrap();
    assert_eq!(progress.first(), Some(&0));
    assert_eq!(progress.last(), Some(&100));
}

#[test]
fn missing_progress_reporter_is_a_noop() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().unwrap();
    QPDFJob::new().configure_writer_progress(&mut writer);
    writer.write().unwrap();
    assert!(!writer.get_buffer().unwrap().is_empty());
}

#[test]
fn json_create_update_and_write_share_one_job_lifecycle() {
    let mut job = QPDFJob::new();
    let mut pdf = job
        .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
        .expect("complete JSON input");
    assert_eq!(pdf.root_ref(), Some(flpdf::ObjectRef::new(1, 0)));

    job.update_from_json(&mut pdf, Cursor::new(UPDATE_JSON), "update.json")
        .expect("partial JSON update");
    assert_eq!(
        pdf.get_object_handle(flpdf::ObjectRef::new(1, 0))
            .get_key(b"/Marker")
            .as_boolean(),
        Some(true)
    );

    let mut output = Vec::new();
    let status = job
        .write_json(
            &mut pdf,
            JsonJobOptions {
                decode_level: DecodeLevel::None,
                stream_data: JsonStreamData::None,
                stream_prefix: None,
                keys: &[],
                objects: &[],
            },
            JsonJobOutput::Stdout(&mut output),
        )
        .expect("JSON output");

    assert_eq!(status, JobExitCode::Success);
    assert!(String::from_utf8_lossy(&output).contains("\"jsonversion\": 2"));
}

#[test]
fn json_write_derives_file_completion_suffix_from_output_destination() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.record_warnings();
    let mut pdf = job
        .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
        .expect("complete JSON input");
    let mut output = Vec::new();
    let filename = Path::new("output.json");

    let status = job
        .write_json(
            &mut pdf,
            JsonJobOptions {
                decode_level: DecodeLevel::None,
                stream_data: JsonStreamData::None,
                stream_prefix: None,
                keys: &[],
                objects: &[],
            },
            JsonJobOutput::File {
                filename,
                writer: &mut output,
            },
        )
        .expect("JSON output");

    assert_eq!(status, JobExitCode::Warning);
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: operation succeeded with warnings; resulting file may have some problems\n"
    );
}

#[test]
fn json_write_reports_completion_sink_errors() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.record_warnings();
    let mut pdf = job
        .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
        .expect("complete JSON input");
    let mut output = Vec::new();

    let error = job
        .write_json(
            &mut pdf,
            JsonJobOptions {
                decode_level: DecodeLevel::None,
                stream_data: JsonStreamData::None,
                stream_prefix: None,
                keys: &[],
                objects: &[],
            },
            JsonJobOutput::Stdout(&mut output),
        )
        .expect_err("completion warning sink failure must be reported");

    assert!(matches!(
        error,
        flpdf::job::JsonJobError::Completion(Error::System(message))
            if message == "warning sink failed"
    ));
}

#[test]
fn json_create_installs_job_logger_before_import_warnings() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);

    let error = match job.create_from_json(Cursor::new(b"{}"), "input.json") {
        Ok(_) => panic!("invalid JSON must fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("errors found in JSON"));
    assert!(
        !state.lock().unwrap().bytes.is_empty(),
        "import-time diagnostic must use the job logger"
    );
}

#[test]
fn json_write_failure_does_not_emit_completion_summary() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.record_warnings();
    let mut pdf = Pdf::create_from_json(Cursor::new(ROOTLESS_JSON), "input.json")
        .expect("rootless JSON input");
    let mut output = Vec::new();

    let error = job
        .write_json(
            &mut pdf,
            JsonJobOptions {
                decode_level: DecodeLevel::None,
                stream_data: JsonStreamData::None,
                stream_prefix: None,
                keys: &[],
                objects: &[],
            },
            JsonJobOutput::Stdout(&mut output),
        )
        .expect_err("serializer failure must abort before completion");

    assert!(matches!(error, flpdf::job::JsonJobError::Output(_)));
    assert!(state.lock().unwrap().bytes.is_empty());
}

#[test]
fn json_job_output_matches_qpdf_11_9_json_input_route() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/complete.json");
    let expected = match Command::new("qpdf")
        .args(["--json-input", "--json=2"])
        .arg(&path)
        .arg("-")
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => panic!(
            "qpdf JSON route failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => {
            eprintln!("skipping qpdf differential: {error}");
            return;
        }
    };

    let mut job = QPDFJob::new();
    let mut pdf = job
        .create_from_json(
            File::open(&path).expect("complete JSON fixture"),
            path.display().to_string(),
        )
        .expect("complete JSON input");
    let mut actual = Vec::new();
    job.write_json(
        &mut pdf,
        JsonJobOptions {
            decode_level: DecodeLevel::Generalized,
            stream_data: JsonStreamData::None,
            stream_prefix: None,
            keys: &[],
            objects: &[],
        },
        JsonJobOutput::Stdout(&mut actual),
    )
    .expect("JSON output");

    assert_eq!(actual, expected);
}

#[test]
fn json_job_parser_accepts_all_covered_qpdf_handler_shapes() {
    let valid_documents = [
        r#"{"inputFile":""}"#,
        r#"{"streamData":"compress"}"#,
        r#"{"streamData":"preserve"}"#,
        r#"{"streamData":"uncompress"}"#,
        r#"{"jsonStreamData":"none"}"#,
        r#"{"jsonStreamData":"inline"}"#,
        r#"{"jsonStreamData":"file","jsonStreamPrefix":"streams"}"#,
        r#"{"removeUnreferencedResources":"auto"}"#,
        r#"{"removeUnreferencedResources":"yes"}"#,
        r#"{"removeUnreferencedResources":"no"}"#,
        r#"{"allowWeakCrypto":"","encrypt":{"userPassword":"u","ownerPassword":"o","128bit":{"modify":"all"}}}"#,
        r#"{"pages":{"file":"page.pdf","password":"p","range":"1-2"}}"#,
        r#"{"overlay":{"file":"overlay.pdf","from":"1","to":"1","repeat":"1"},"underlay":{"file":"underlay.pdf"}}"#,
        r#"{"addAttachment":{"file":"attachment.bin","filename":"shown.bin","key":"shown-key","replace":""}}"#,
        r#"{"copyAttachmentsFrom":{"file":"copy.pdf","password":"p","prefix":"copy-"}}"#,
        r#"{"removeAttachment":["old-key"],"setPageLabels":["1:D"]}"#,
        r#"{"jsonKey":["pages","qpdf"],"jsonObject":["trailer","1 0 R"]}"#,
    ];

    for json in valid_documents {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(json)
            .unwrap_or_else(|error| panic!("valid job JSON rejected: {json}: {error}"));
    }

    let invalid_documents = [
        r#"{"addAttachment":[{"file":"/"}]}"#,
        r#"{"overlay":[{}]}"#,
        r#"{"pages":[{}]}"#,
        r#"{"jsonKey":[1]}"#,
        r#"{"jsonKey":["unknown"]}"#,
        r#"{"jsonObject":[1]}"#,
        r#"{"jsonObject":["unknown"]}"#,
        r#"{"removeAttachment":[1]}"#,
        r#"{"setPageLabels":[1]}"#,
        r#"{"inputFile":"input.pdf","empty":""}"#,
    ];
    for json in invalid_documents {
        let mut job = QPDFJob::new();
        assert!(
            job.initialize_from_json_partial(json).is_err(),
            "invalid job JSON unexpectedly accepted: {json}"
        );
    }
}

#[test]
fn json_job_copies_attachments_from_every_donor_before_reporting_conflicts() {
    // qpdf's copyAttachments visits every configured donor and reports the
    // conflicting keys once after the last one (QPDFJob.cc:2089-2135), so a
    // conflict in the first donor must not stop the second from being
    // processed and listed.
    let tempdir = tempfile::tempdir().unwrap();
    let minimal = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
    )
    .unwrap();
    let write_with_attachment = |name: &str| -> std::path::PathBuf {
        let payload = tempdir.path().join(format!("{name}.bin"));
        std::fs::write(&payload, name.as_bytes()).unwrap();
        let mut job = QPDFJob::new();
        let mut pdf = job
            .open(
                Cursor::new(minimal.clone()),
                "fixture.pdf",
                PdfOpenOptions::default(),
            )
            .unwrap();
        job.add_attachments(
            &mut pdf,
            &[flpdf::job::AttachmentAddOptions {
                path: payload,
                key: b"shared".to_vec(),
                filename: b"shared".to_vec(),
                mimetype: None,
                description: None,
                creation_date: None,
                modification_date: None,
                replace: false,
                verbose: false,
            }],
        )
        .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        let path = tempdir.path().join(format!("{name}.pdf"));
        std::fs::write(&path, writer.get_buffer().unwrap()).unwrap();
        path
    };
    let target = write_with_attachment("target");
    let donor_a = write_with_attachment("donor-a");
    let donor_b = write_with_attachment("donor-b");
    let output = tempdir.path().join("output.pdf");

    let (logger, errors) = logger_with_error_sink();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(logger.discard()));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(&format!(
        r#"{{"inputFile":"{}","outputFile":"{}","copyAttachmentsFrom":[{{"file":"{}"}},{{"file":"{}"}}]}}"#,
        target.display(),
        output.display(),
        donor_a.display(),
        donor_b.display()
    ))
    .unwrap();

    assert_eq!(job.run().unwrap(), flpdf::job::JobExitCode::Error);
    let message = String::from_utf8_lossy(&errors.lock().unwrap().bytes).into_owned();
    assert!(
        message.contains("donor-a.pdf, key: shared")
            && message.contains("donor-b.pdf, key: shared"),
        "both donors must be processed before the aggregate error: {message}"
    );
    assert!(
        !output.exists(),
        "a conflicting copy must not write the output"
    );
}

#[test]
fn json_job_run_covers_update_page_labels_and_linearized_writer_stages() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/complete.json");
    let update = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/update.json");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    let pass1 = tempdir.path().join("pass1.pdf");
    std::fs::copy(fixture, &input).unwrap();

    let json = serde_json::json!({
        "inputFile": input,
        "jsonInput": "",
        "outputFile": output,
        "updateFromJson": update,
        "pages": [{"file": ".", "range": "1"}],
        "removePageLabels": "",
        "setPageLabels": ["1:D"],
        "linearize": "",
        "linearizePass1": pass1,
        "verbose": "",
        "staticId": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
}

#[test]
fn json_job_run_covers_overlay_attachment_and_copy_stages() {
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let attachment_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let overlay_output = tempdir.path().join("overlay.pdf");
    let overlay_json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": overlay_output,
        "overlay": {"file": one_page, "from": "1", "to": "1", "repeat": "1"},
        "underlay": [{"file": one_page}],
        "staticId": ""
    })
    .to_string();
    let mut overlay_job = QPDFJob::new();
    overlay_job.initialize_from_json(&overlay_json).unwrap();
    assert_eq!(overlay_job.run().unwrap(), JobExitCode::Success);

    let attachment_output = tempdir.path().join("attachments.pdf");
    let attachment_json = serde_json::json!({
        "inputFile": attachment_pdf,
        "outputFile": attachment_output,
        "removeAttachment": ["attachment.txt"],
        "copyAttachmentsFrom": [{"file": attachment_pdf, "prefix": "copy-"}],
        "verbose": "",
        "staticId": ""
    })
    .to_string();
    let mut attachment_job = QPDFJob::new();
    attachment_job
        .initialize_from_json(&attachment_json)
        .unwrap();
    assert_eq!(attachment_job.run().unwrap(), JobExitCode::Success);
    assert!(attachment_output.exists());
}

#[test]
fn json_job_output_covers_v1_sections_images_outlines_encryption_and_schema() {
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let image_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/shared-stream-objstm.pdf");
    let outline_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/json-diff/direct-outlines.pdf");
    let encrypted_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let v1_output = tempdir.path().join("v1.json");
    let v1_json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": v1_output,
        "json": "1",
        "jsonKey": ["objects", "objectinfo"],
        "staticId": ""
    })
    .to_string();
    let mut v1_job = QPDFJob::new();
    v1_job.initialize_from_json(&v1_json).unwrap();
    assert_eq!(v1_job.run().unwrap(), JobExitCode::Success);
    let v1_text = std::fs::read_to_string(v1_output).unwrap();
    assert!(v1_text.contains("\"objects\""));
    assert!(v1_text.contains("\"objectinfo\""));

    for (name, input) in [("images", image_pdf), ("outlines", outline_pdf.clone())] {
        let output = tempdir.path().join(format!("{name}.json"));
        let json = serde_json::json!({
            "inputFile": input,
            "outputFile": output,
            "json": "2",
            "jsonKey": ["pages"],
        })
        .to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json(&json).unwrap();
        assert_eq!(job.run().unwrap(), JobExitCode::Success);
        assert!(std::fs::metadata(output).unwrap().len() > 0);
    }

    let outline_output = tempdir.path().join("outline-section.json");
    let outline_json = serde_json::json!({
        "inputFile": outline_pdf,
        "outputFile": outline_output,
        "json": "2",
        "jsonKey": ["outlines"],
    })
    .to_string();
    let mut outline_job = QPDFJob::new();
    outline_job.initialize_from_json(&outline_json).unwrap();
    assert_eq!(outline_job.run().unwrap(), JobExitCode::Success);
    assert!(std::fs::metadata(outline_output).unwrap().len() > 0);

    let encrypted_output = tempdir.path().join("encrypted.json");
    let encrypted_json = serde_json::json!({
        "inputFile": encrypted_pdf,
        "outputFile": encrypted_output,
        "password": "user-v4-aes",
        "json": "2",
        "jsonKey": ["encrypt"],
        "showEncryptionKey": ""
    })
    .to_string();
    let mut encrypted_job = QPDFJob::new();
    encrypted_job.initialize_from_json(&encrypted_json).unwrap();
    assert_eq!(encrypted_job.run().unwrap(), JobExitCode::Success);

    let schema_output = tempdir.path().join("schema.json");
    let schema_json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": schema_output,
        "jsonOutput": "2",
        "testJsonSchema": ""
    })
    .to_string();
    let mut schema_job = QPDFJob::new();
    schema_job.initialize_from_json(&schema_json).unwrap();
    assert_eq!(schema_job.run().unwrap(), JobExitCode::Success);
}

#[test]
fn json_job_stdout_schema_uses_the_job_save_pipeline() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let (logger, state) = logger_with_info_sink();
    let save_state = Arc::new(Mutex::new(SinkState::default()));
    logger
        .set_save(
            Some(PipelineHandle::new(RecordingSink {
                state: Arc::clone(&save_state),
            })),
            false,
        )
        .unwrap();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let json = serde_json::json!({
        "inputFile": input,
        "jsonOutput": "2",
        "testJsonSchema": ""
    })
    .to_string();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(save_state.lock().unwrap().bytes.starts_with(b"{\n"));
    assert!(state.lock().unwrap().bytes.is_empty());
}

#[test]
fn json_job_json_input_and_replace_input_cover_success_and_failure_boundaries() {
    let minimal = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let json_input = tempdir.path().join("input.json");
    std::fs::write(&json_input, COMPLETE_JSON).unwrap();
    let created_output = tempdir.path().join("created.pdf");
    let create_json = serde_json::json!({
        "inputFile": json_input,
        "jsonInput": "",
        "outputFile": created_output,
        "staticId": ""
    })
    .to_string();
    let mut create_job = QPDFJob::new();
    create_job.initialize_from_json(&create_json).unwrap();
    assert_eq!(create_job.run().unwrap(), JobExitCode::Success);

    let bad_json_input = tempdir.path().join("bad.json");
    std::fs::write(&bad_json_input, b"{").unwrap();
    let bad_output = tempdir.path().join("bad.pdf");
    let bad_json = serde_json::json!({
        "inputFile": bad_json_input,
        "jsonInput": "",
        "outputFile": bad_output
    })
    .to_string();
    let mut bad_job = QPDFJob::new();
    bad_job.initialize_from_json(&bad_json).unwrap();
    assert_eq!(bad_job.run().unwrap(), JobExitCode::Error);

    let missing_update = tempdir.path().join("missing-update.json");
    let missing_update_output = tempdir.path().join("missing-update.pdf");
    let missing_update_json = serde_json::json!({
        "inputFile": minimal,
        "outputFile": missing_update_output,
        "updateFromJson": missing_update
    })
    .to_string();
    let mut missing_update_job = QPDFJob::new();
    missing_update_job
        .initialize_from_json(&missing_update_json)
        .unwrap();
    assert_eq!(missing_update_job.run().unwrap(), JobExitCode::Error);

    let replace_input = tempdir.path().join("replace.pdf");
    std::fs::copy(&minimal, &replace_input).unwrap();
    let replace_json = serde_json::json!({
        "inputFile": replace_input,
        "replaceInput": "",
        "staticId": ""
    })
    .to_string();
    let mut replace_job = QPDFJob::new();
    replace_job.initialize_from_json(&replace_json).unwrap();
    assert_eq!(replace_job.run().unwrap(), JobExitCode::Success);
    assert!(replace_input.exists());

    let failed_replace_input = tempdir.path().join("failed-replace.pdf");
    std::fs::copy(&minimal, &failed_replace_input).unwrap();
    let failed_replace_json = serde_json::json!({
        "inputFile": failed_replace_input,
        "replaceInput": "",
        "removeAttachment": ["missing"]
    })
    .to_string();
    let mut failed_replace_job = QPDFJob::new();
    failed_replace_job
        .initialize_from_json(&failed_replace_json)
        .unwrap();
    assert_eq!(failed_replace_job.run().unwrap(), JobExitCode::Error);
    assert!(failed_replace_input.exists());
}

#[test]
fn json_job_show_encryption_and_setter_boundaries_are_reachable() {
    let encrypted = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let (logger, info_state) = logger_with_info_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_input_file("placeholder.pdf").unwrap();
    assert!(job.set_input_file("second.pdf").is_err());
    job.set_output_file("placeholder-output.pdf").unwrap();
    assert!(job.set_output_file("second-output.pdf").is_err());
    job.set_password(b"placeholder-password".to_vec());

    let json = serde_json::json!({
        "inputFile": encrypted,
        "password": "user-v4-aes",
        "showEncryption": ""
    })
    .to_string();
    let mut inspection_job = QPDFJob::new();
    inspection_job.set_logger(job.logger());
    inspection_job.initialize_from_json(&json).unwrap();
    assert_eq!(inspection_job.run().unwrap(), JobExitCode::Success);
    assert!(info_state
        .lock()
        .unwrap()
        .bytes
        .windows(b"R = 4".len())
        .any(|window| { window == b"R = 4" }));
}

#[test]
fn json_job_rejects_conflicting_output_configuration_and_bad_page_labels() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let conflicting = [
        serde_json::json!({
            "inputFile": input,
            "outputFile": tempdir.path().join("out.pdf"),
            "replaceInput": ""
        }),
        serde_json::json!({"empty": "", "replaceInput": ""}),
        serde_json::json!({"inputFile": input, "replaceInput": "", "json": "2"}),
        serde_json::json!({
            "inputFile": input,
            "outputFile": tempdir.path().join("out.pdf"),
            "check": ""
        }),
    ];
    for value in conflicting {
        let mut job = QPDFJob::new();
        assert!(job.initialize_from_json(&value.to_string()).is_err());
    }

    let bad_labels = serde_json::json!({
        "inputFile": input,
        "outputFile": tempdir.path().join("bad-labels.pdf"),
        "setPageLabels": ["1:D", "2:D"]
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&bad_labels).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn json_job_replace_input_keeps_original_when_input_has_warnings() {
    let repairable = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("warning-replace.pdf");
    std::fs::copy(repairable, &input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "replaceInput": "",
        "staticId": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(input.exists());
    assert!(Path::new(&format!("{}.~qpdf-orig", input.display())).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_input_file_opens_a_literal_non_utf8_path() {
    let directory = tempfile::tempdir().unwrap();
    let input = non_utf8_path(directory.path(), b"input-\x80.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_json_bytes(&job_json_with_paths(&input, &output))
        .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists(), "qpdf job must open the exact input path");
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_output_file_creates_a_literal_non_utf8_path() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.pdf");
    let output = non_utf8_path(directory.path(), b"output-\x80.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_json_bytes(&job_json_with_paths(&input, &output))
        .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(
        output.exists(),
        "qpdf job must create the exact output path"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_replace_input_preserves_non_utf8_derived_backup_path() {
    let repairable = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let directory = tempfile::tempdir().unwrap();
    let input = non_utf8_path(directory.path(), b"warning-replace-\x80.pdf");
    std::fs::copy(repairable, &input).unwrap();

    let mut job = QPDFJob::new();
    let mut json = b"{\"inputFile\":\"".to_vec();
    json.extend_from_slice(input.as_os_str().as_bytes());
    json.extend_from_slice(b"\",\"replaceInput\":\"\"}");
    job.initialize_from_json_bytes(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    let mut backup = input.as_os_str().to_os_string();
    backup.push(".~qpdf-orig");
    assert!(PathBuf::from(backup).exists());
}
