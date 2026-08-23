use flpdf::job::QPDFJob;
use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, ObjectRef, Pdf, PdfOpenOptions, QPDFLogger};
use std::io::Cursor;
use std::sync::{Arc, Mutex};

const MINIMAL_PDF: &[u8] = include_bytes!("../../../tests/fixtures/minimal.pdf");
const LAZY_WARNING_PDF: &[u8] =
    include_bytes!("../../../tests/fixtures/compat/chained-indirect-contents.pdf");

struct RecordingSink(Arc<Mutex<Vec<u8>>>);

impl Pipeline for RecordingSink {
    fn identifier(&self) -> &str {
        "pdf warning recording sink"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

struct FailingSink;

impl Pipeline for FailingSink {
    fn identifier(&self) -> &str {
        "pdf warning failing sink"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Err(PipelineError::runtime("warning sink failed"))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn recording_logger() -> (QPDFLogger, Arc<Mutex<Vec<u8>>>) {
    let logger = QPDFLogger::create();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    logger.set_warn(Some(PipelineHandle::new(RecordingSink(Arc::clone(&bytes)))));
    (logger, bytes)
}

fn warnings_only_corrupt_xref_bytes() -> (Vec<u8>, usize) {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".as_slice(),
    ] {
        offsets.push(pdf.len());
        pdf.extend_from_slice(object);
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    pdf[xref_start + 2] = b'z';
    (pdf, xref_start)
}

fn terminal_repair_failure_bytes() -> (Vec<u8>, usize) {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    pdf.extend_from_slice(
        format!("traile_\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    (pdf, xref_start)
}

fn two_lazy_warning_objects() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n".as_slice(),
        b"3 0 obj\nnull\nendobj\n".as_slice(),
        b"4 0 obj\n40\n".as_slice(),
        b"5 0 obj\n50\n".as_slice(),
    ] {
        offsets.push(pdf.len());
        pdf.extend_from_slice(object);
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn open_options_default_to_the_process_logger_unsuppressed_and_unnamed() {
    let options = PdfOpenOptions::default();

    assert!(options.logger.is_none());
    assert!(!options.suppress_warnings);
    assert!(options.description.is_empty());

    let pdf = Pdf::open_with_options(Cursor::new(MINIMAL_PDF), options).unwrap();
    assert_eq!(pdf.logger(), QPDFLogger::default_logger());
}

#[test]
fn open_options_clone_and_compare_an_explicit_logger_by_identity() {
    let logger = QPDFLogger::create();
    let options = PdfOpenOptions {
        logger: Some(logger.clone()),
        suppress_warnings: true,
        description: "input.pdf".to_owned(),
        ..PdfOpenOptions::default()
    };

    assert_eq!(options, options.clone());
    let pdf = Pdf::open_with_options(Cursor::new(MINIMAL_PDF), options).unwrap();
    assert_eq!(pdf.logger(), logger);
}

#[test]
fn warning_replays_initial_repair_diagnostics_once_in_original_order() {
    let (logger, output) = recording_logger();
    let (bytes, xref_start) = warnings_only_corrupt_xref_bytes();
    let pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: "input.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        output.lock().unwrap().as_slice(),
        format!(
            "WARNING: input.pdf: file is damaged\n\
             WARNING: input.pdf (offset {xref_start}): expected integer\n\
             WARNING: input.pdf: Attempting to reconstruct cross-reference table\n"
        )
        .as_bytes()
    );
    assert_eq!(
        pdf.repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>(),
        [
            "file is damaged",
            "expected integer",
            "Attempting to reconstruct cross-reference table",
        ]
    );
}

#[test]
fn warning_suppression_keeps_initial_repair_diagnostics() {
    let (logger, output) = recording_logger();
    let (bytes, _) = warnings_only_corrupt_xref_bytes();
    let pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            suppress_warnings: true,
            description: "input.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(output.lock().unwrap().is_empty());
    assert_eq!(pdf.repair_diagnostics().entries().len(), 3);
}

#[test]
fn warning_initial_replay_failure_is_returned_by_open() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let (bytes, _) = warnings_only_corrupt_xref_bytes();

    assert!(matches!(
        Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
                description: "input.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}

#[test]
fn check_with_repair_propagates_warning_delivery_failure() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let (bytes, _) = warnings_only_corrupt_xref_bytes();

    let mut job = QPDFJob::new();
    job.set_logger(logger.clone());
    assert!(matches!(
        job.open(
            Cursor::new(bytes),
            "check.pdf",
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
                description: "check.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}

#[test]
fn terminal_open_failure_delivers_accumulated_repair_warnings_first() {
    let (logger, output) = recording_logger();
    let (bytes, xref_start) = terminal_repair_failure_bytes();
    let error = match Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: "broken.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("repair must still fail without a trailer keyword"),
        Err(error) => error,
    };

    assert!(error.open_failure().is_some());
    assert_eq!(
        output.lock().unwrap().as_slice(),
        format!(
            "WARNING: broken.pdf: file is damaged\n\
             WARNING: broken.pdf (offset {xref_start}): expected integer\n\
             WARNING: broken.pdf: Attempting to reconstruct cross-reference table\n"
        )
        .as_bytes()
    );
}

#[test]
fn terminal_open_failure_returns_warning_delivery_failure() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let (bytes, _) = terminal_repair_failure_bytes();

    assert!(matches!(
        Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
                description: "broken.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}

#[test]
fn warning_routes_lazy_resolution_immediately_and_only_once() {
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(LAZY_WARNING_PDF),
        PdfOpenOptions {
            logger: Some(logger),
            description: "lazy.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    pdf.resolve_object(ObjectRef::new(5, 0)).unwrap();
    pdf.resolve_object(ObjectRef::new(5, 0)).unwrap();

    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: lazy.pdf (object 5 0, offset 232): expected endobj\n"
    );
    assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
}

#[test]
fn warning_delivery_failure_is_returned_after_the_diagnostic_is_appended() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let mut pdf = Pdf::open_with_options(
        Cursor::new(LAZY_WARNING_PDF),
        PdfOpenOptions {
            logger: Some(logger),
            description: "lazy.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(matches!(
        pdf.resolve_object(ObjectRef::new(5, 0)),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
    assert_eq!(
        pdf.repair_diagnostics().entries()[0].message,
        "(object 5 0, offset 232): expected endobj"
    );
}

#[test]
fn live_logger_replacement_routes_only_to_the_replacement() {
    let (original, original_output) = recording_logger();
    let (replacement, replacement_output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(LAZY_WARNING_PDF),
        PdfOpenOptions {
            logger: Some(original),
            description: "live.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    pdf.set_logger(replacement.clone());
    assert_eq!(pdf.logger(), replacement);
    pdf.resolve_object(ObjectRef::new(5, 0)).unwrap();

    assert!(original_output.lock().unwrap().is_empty());
    assert_eq!(
        replacement_output.lock().unwrap().as_slice(),
        b"WARNING: live.pdf (object 5 0, offset 232): expected endobj\n"
    );
}

#[test]
fn live_suppression_toggle_only_changes_delivery_not_collection() {
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(two_lazy_warning_objects()),
        PdfOpenOptions {
            logger: Some(logger),
            description: "live.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    pdf.set_suppress_warnings(true);
    assert!(pdf.suppress_warnings());
    pdf.resolve_object(ObjectRef::new(4, 0)).unwrap();
    assert!(output.lock().unwrap().is_empty());

    pdf.set_suppress_warnings(false);
    assert!(!pdf.suppress_warnings());
    pdf.resolve_object(ObjectRef::new(5, 0)).unwrap();

    let diagnostics = pdf.repair_diagnostics();
    assert_eq!(diagnostics.entries().len(), 2);
    assert!(diagnostics.entries()[0].message.starts_with("(object 4 0,"));
    assert!(diagnostics.entries()[1].message.starts_with("(object 5 0,"));
    let output = output.lock().unwrap();
    assert!(!output
        .windows(b"object 4 0".len())
        .any(|w| w == b"object 4 0"));
    assert!(output
        .windows(b"object 5 0".len())
        .any(|w| w == b"object 5 0"));
}
