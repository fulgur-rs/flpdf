use flpdf::job::{JobExitCode, JsonJobOptions, JsonJobOutput, JsonStreamData, QPDFJob};
use flpdf::json_inspect::DecodeLevel;
use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, Pdf, PdfOpenOptions, PdfWriter, QPDFLogger};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

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
    });
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
    let progress = progress.lock().unwrap();
    assert_eq!(progress.first(), Some(&0));
    assert_eq!(progress.last(), Some(&100));
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
fn json_job_partial_initialization_defers_missing_output_to_run() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input}).to_string();
    let mut job = QPDFJob::new();

    job.initialize_from_json_partial(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Error);
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

    assert!(matches!(error, Error::Unsupported(ref message)
        if message.contains("outputFile") && message.contains("must be a string")));
}

#[test]
fn json_job_rejects_a_malformed_output_file_outside_partial_mode() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input, "outputFile": false}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json(&json).unwrap_err();

    assert!(matches!(error, Error::Unsupported(ref message)
        if message.contains("outputFile") && message.contains("must be a string")));
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

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
    assert_eq!(std::fs::read(&input).unwrap(), before);
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

    assert!(job.initialize_from_json(&json).is_err());

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn json_job_rejects_a_key_outside_the_implemented_schema_subset() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "out.pdf",
        "linearize": ""
    })
    .to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json(&json).unwrap_err();

    assert!(matches!(error, Error::Unsupported(_)));
    assert!(error.to_string().contains("linearize"));
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

    assert!(job.initialize_from_argv(&args).is_err());
}

#[test]
fn argv_usage_rejects_short_options_too_many_positionals_and_missing_input() {
    let mut job = QPDFJob::new();
    assert!(job
        .initialize_from_argv(&["qpdfjob".to_owned(), "-x".to_owned()])
        .is_err());

    let mut job = QPDFJob::new();
    assert!(job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "a.pdf".to_owned(),
            "b.pdf".to_owned(),
            "c.pdf".to_owned(),
        ])
        .is_err());

    let mut job = QPDFJob::new();
    assert!(job.initialize_from_argv(&["qpdfjob".to_owned()]).is_err());
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
