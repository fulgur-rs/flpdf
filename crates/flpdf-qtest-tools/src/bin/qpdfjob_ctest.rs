//! Portable Rust consumer for qpdf's `qpdfjob-ctest.c`.
//!
//! The helper deliberately owns only the process-facing adapter: qpdf job
//! initialization, warning/status aggregation, document creation, and writing
//! remain in [`flpdf::job::QPDFJob`]. This keeps the qtest helper from growing
//! a second lifecycle or a legacy compatibility route.

use flpdf::job::{JobExitCode, QPDFJob};
use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, QPDFLogger, Result};
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

struct CustomErrorLog;

impl Pipeline for CustomErrorLog {
    fn identifier(&self) -> &str {
        "qpdfjob custom logger"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        let mut stderr = io::stderr().lock();
        stderr
            .write_all(b"|custom|")
            .and_then(|_| stderr.write_all(data))
            .and_then(|_| stderr.flush())
            .map_err(|error| PipelineError::runtime(error.to_string()))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().collect();
    let result = if args.get(1).is_some_and(|arg| arg == "wide") {
        run_wide(&args)
    } else {
        run_tests()
    };

    match result {
        Ok(()) => ExitCode::from(0),
        Err(error) => {
            eprintln!("qpdfjob-ctest: {error}");
            ExitCode::from(2)
        }
    }
}

fn run_wide(args: &[OsString]) -> Result<()> {
    if args.len() != 2 {
        return Err(Error::Unsupported(
            "qpdfjob-ctest wide accepts no arguments after wide".to_owned(),
        ));
    }
    let argv = vec![
        "qpdfjob".to_owned(),
        "minimal.pdf".to_owned(),
        "a.pdf".to_owned(),
        "--static-id".to_owned(),
    ];
    expect_status(run_argv(argv, None)?, JobExitCode::Success, "wide")?;
    print_line("wide test passed");
    Ok(())
}

fn run_tests() -> Result<()> {
    let argv = vec![
        "qpdfjob".to_owned(),
        "minimal.pdf".to_owned(),
        "a.pdf".to_owned(),
        "--deterministic-id".to_owned(),
        "--progress".to_owned(),
    ];
    expect_status(
        run_argv(argv, Some("potato"))?,
        JobExitCode::Success,
        "argv",
    )?;
    print_line("argv test passed");

    let json = r#"{
  "inputFile": "20-pages.pdf",
  "password": "user",
  "outputFile": "b.pdf",
  "staticId": "",
  "decrypt": "",
  "objectStreams": "generate"
}"#;
    expect_status(run_json(json)?, JobExitCode::Success, "json")?;
    print_line("json test passed");

    let warning_json = r#"{
  "inputFile": "xref-with-short-size.pdf",
  "outputFile": "c.pdf",
  "staticId": "",
  "decrypt": "",
  "objectStreams": "generate"
}"#;
    expect_status(run_json(warning_json)?, JobExitCode::Warning, "json warn")?;
    print_line("json warn test passed");

    let mut job = QPDFJob::new();
    let default_logger = job.logger();
    if default_logger != QPDFLogger::default_logger() {
        return Err(Error::Internal(
            "qpdfjob default logger identity changed".to_owned(),
        ));
    }
    let custom_logger = QPDFLogger::create();
    custom_logger.set_error(Some(PipelineHandle::new(CustomErrorLog)));
    job.set_logger(custom_logger.clone());
    if job.logger() != custom_logger {
        return Err(Error::Internal(
            "qpdfjob custom logger identity was not retained".to_owned(),
        ));
    }
    job.set_message_prefix("qpdfjob json");
    let _ = job.initialize_from_json(
        r#"{
  "inputFile": "nothing-there.pdf"
}"#,
    );
    expect_status(job.run()?, JobExitCode::Error, "json error")?;
    print_line("json error test passed");

    let argv = vec![
        "qpdfjob".to_owned(),
        "minimal.pdf".to_owned(),
        "d.pdf".to_owned(),
        "--deterministic-id".to_owned(),
        "--progress".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.register_progress_reporter(|percent| {
        print_line(format!("qpdfjob: d.pdf: write progress: {percent}%"));
        Ok(())
    });
    job.initialize_from_argv(&argv)?;
    let mut pdf = job
        .create_qpdf()?
        .ok_or_else(|| Error::Internal("qpdfjob createQPDF returned no document".to_owned()))?;
    expect_status(
        job.write_qpdf(&mut pdf)?,
        JobExitCode::Success,
        "create/write",
    )?;

    let missing_argv = vec![
        "qpdfjob".to_owned(),
        "m.pdf".to_owned(),
        "--check".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.set_message_prefix("qpdfjob");
    job.initialize_from_argv(&missing_argv)?;
    if job.create_qpdf()?.is_some() {
        return Err(Error::Internal(
            "qpdfjob createQPDF unexpectedly opened missing input".to_owned(),
        ));
    }
    print_line("qpdfjob_create_qpdf and qpdfjob_write_qpdf test passed");
    Ok(())
}

fn run_argv(argv: Vec<String>, progress_label: Option<&str>) -> Result<JobExitCode> {
    let mut job = QPDFJob::new();
    if let Some(label) = progress_label {
        let label = label.to_owned();
        job.register_progress_reporter(move |percent| {
            print_line(format!("{label}: write progress: {percent}%"));
            Ok(())
        });
    }
    job.initialize_from_argv(&argv)?;
    job.run()
}

fn run_json(json: &str) -> Result<JobExitCode> {
    let mut job = QPDFJob::new();
    job.initialize_from_json(json)?;
    job.run()
}

fn expect_status(actual: JobExitCode, expected: JobExitCode, stage: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Internal(format!(
            "qpdfjob {stage} returned status {}, expected {}",
            actual.as_i32(),
            expected.as_i32()
        )))
    }
}

fn print_line(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
    let _ = io::stdout().flush();
}
