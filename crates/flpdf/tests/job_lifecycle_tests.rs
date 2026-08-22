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

#[test]
fn new_job_matches_qpdf_defaults() {
    let job = QPDFJob::default();

    assert_eq!(job.message_prefix(), "qpdf");
    assert_eq!(job.logger(), QPDFLogger::default_logger());
    assert!(!job.has_warnings());
    assert_eq!(job.complete(false).unwrap(), JobExitCode::Success);
    assert_eq!(JobExitCode::Success.as_i32(), 0);
    assert_eq!(JobExitCode::Warning.as_i32(), 3);
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
