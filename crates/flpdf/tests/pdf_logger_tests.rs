mod common;
use common::PdfCanonicalTestExt;

use flpdf::job::QPDFJob;
use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{DecodeLevel, Error, ObjectRef, Pdf, PdfOpenOptions, QPDFLogger};
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

fn unknown_xref_entry_type_bytes() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 >>\nstream\nabcd\nendstream\nendobj\nstartxref\n9\n%%EOF\n".to_vec()
}

/// Two entries: a valid type-1 row followed by an unknown type `a` (97).
fn unknown_second_xref_entry_type_bytes() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 2 /Length 4 >>\nstream\n\x01\x00a\x00\nendstream\nendobj\nstartxref\n9\n%%EOF\n".to_vec()
}

fn indirect_xref_filter_bytes(array_value: bool) -> Vec<u8> {
    let filter = if array_value {
        "[ /ASCIIHexDecode ]"
    } else {
        "/ASCIIHexDecode"
    };
    format!(
        "%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Filter 2 0 R /Length 9 >>\nstream\n00000000>\nendstream\nendobj\n2 0 obj\n{filter}\nstartxref\n9\n%%EOF\n"
    )
    .into_bytes()
}

/// A damaged file (`startxref 0`, matching qpdf's immediate-reconstruction
/// special case) whose line-scan recovers two objects: object 1's body has
/// no `endobj` before object 2's own header starts, and object 2 is a valid
/// `/Type /XRef` stream that reconstruction accepts as its candidate. Object
/// 1's malformed body is resolved live, through the canonical owner, while
/// `find_xref_stream_trailer_candidate_canonical` scans every recovered
/// entry looking for the candidate (`libqpdf/QPDF.cc:585-589`; every entry is
/// resolved regardless of whether it turns out to be the winning candidate).
fn candidate_discovery_live_warning_bytes() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Foo 1 >>\n2 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Length 3 >>\nstream\n\x01\x00\x00\nendstream\nendobj\nstartxref\n0\n%%EOF\n".to_vec()
}

/// The sole candidate's own body is missing `endobj` before `startxref`
/// (mirroring `candidate_discovery_live_warning_bytes`'s object 1, but as
/// the candidate itself rather than a filler entry read during discovery),
/// so its *read* warns live ("expected endobj") when re-entered. `/Index [10
/// 2]` keeps the stream's two decoded entries (object numbers 10 and 11)
/// from colliding with the line scan's own registration of this object under
/// its real number (1) -- re-entry seeds `XrefRegistration` from that same
/// line-scan table, and a colliding object number is silently skipped before
/// its type is ever checked (`registration.entries.contains_key`, mirroring
/// qpdf's `try_emplace`, `QPDF.cc:1158-1169`), which would hide the type-97
/// entry entirely. `/Length 5` (one byte more than the two entries need)
/// makes `build_xref_stream` push a stream-length diagnostic and continue,
/// then object 11's decoded type (97) is unrecognized and `build_xref_stream`
/// fails outright -- both on the *same* re-entry read.
fn candidate_reentry_build_failure_after_live_warning_bytes() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Index [10 2] /Size 12 /Length 5 >>\nstream\n\x01\x00a\x00X\nendstream\nstartxref\n0\n%%EOF\n".to_vec()
}

/// Two `/Type /XRef` objects at distinct offsets, deliberately numbered so
/// the *lower*-offset one (the eventual `/Prev` target) has the *higher*
/// object number: `find_xref_stream_trailer_candidate_canonical` visits
/// entries in ascending object-number order and only ever assigns the
/// winning candidate on the first entry whose offset exceeds every offset
/// seen so far, so object 1 (the higher-offset, later object) is visited
/// first and wins; object 2 (the lower-offset `/Prev` target) is visited
/// second and never displaces it. Object 1's own re-entry is clean (no
/// warning of its own); its `/Prev` points at object 2, whose body is
/// missing `endobj` before the next line (mirroring
/// `candidate_discovery_live_warning_bytes`'s technique), so reading it
/// during the `/Prev` walk warns live ("expected endobj").
fn previous_xref_section_live_warning_bytes() -> Vec<u8> {
    let prefix = b"%PDF-1.4\n".to_vec();
    let object_2 = b"2 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Length 3 >>\nstream\n\x01\x00\x00\nendstream\n".to_vec();
    let previous_offset = prefix.len();
    let mut bytes = prefix;
    bytes.extend_from_slice(&object_2);
    bytes.extend_from_slice(
        format!(
            "1 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Prev {previous_offset} /Length 3 >>\nstream\n\x01\x00\x00\nendstream\nendobj\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(b"startxref\n0\n%%EOF\n");
    bytes
}

fn previous_classic_trailer_then_hybrid_live_warning_bytes() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let hybrid_offset = bytes.len() + b"junk ".len();
    bytes.extend_from_slice(
        b"junk 3 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Filter 1 /Length 3 >>\nstream\n",
    );
    bytes.extend_from_slice(b"\x01\x00\x00\nendstream\nendobj\n");

    let previous_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n0000000000 65535 f \n0000000000 65535 f \n{hybrid_offset:010} 00000 n \ntrailer\n<< /Size 4 /XRefStm {hybrid_offset} >> stream\n"
        )
        .as_bytes(),
    );
    let candidate_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "1 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Prev {previous_offset} /Length 3 >>\nstream\n\x01\x00\x00\nendstream\nendobj\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(format!("startxref\n{candidate_offset}\n%%EOF\n").as_bytes());
    bytes
}

fn indirect_previous_offset_live_warning_bytes() -> Vec<u8> {
    // The candidate's `/Prev` is an indirect reference, so resolving the
    // previous section's offset dereferences object 3 0 -- a read that warns
    // live through the owner, outside the section parse the deferral window
    // used to cover.
    let prefix = b"%PDF-1.4\n".to_vec();
    let object_2 = b"2 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Length 3 >>\nstream\n\x01\x00\x00\nendstream\n".to_vec();
    let previous_offset = prefix.len();
    let mut bytes = prefix;
    bytes.extend_from_slice(&object_2);
    // Object 3 0 is missing its `endobj`, so dereferencing it raises the same
    // `expected endobj` repair warning the section-parse test relies on.
    bytes.extend_from_slice(format!("3 1 obj\n{previous_offset}\n").as_bytes());
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Prev 3 1 R /Length 3 >>\nstream\n\x01\x00\x00\nendstream\nendobj\n",
    );
    bytes.extend_from_slice(b"startxref\n0\n%%EOF\n");
    bytes
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

fn malformed_filter_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>",
        ),
        (4, b"<< /Length 3 /Filter 7 >>\nstream\nabc\nendstream"),
    ] {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
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
        description: b"input.pdf".to_vec(),
        ..PdfOpenOptions::default()
    };

    assert_eq!(options, options.clone());
    let pdf = Pdf::open_with_options(Cursor::new(MINIMAL_PDF), options).unwrap();
    assert_eq!(pdf.logger(), logger);
}

#[cfg(target_os = "linux")]
#[test]
fn object_warning_preserves_a_non_utf8_file_description() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let directory = tempfile::tempdir().expect("temporary source directory");
    let path = directory.path().join(std::ffi::OsString::from_vec(
        b"object-warning-\xff.pdf".to_vec(),
    ));
    std::fs::write(&path, malformed_filter_pdf()).expect("write malformed filter PDF");
    let (logger, output) = recording_logger();
    let description = path.as_os_str().as_bytes().to_vec();
    let mut pdf = Pdf::open_file_with_options(
        &path,
        PdfOpenOptions {
            logger: Some(logger),
            description: description.clone(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("open malformed filter PDF");

    let stream = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve(&stream).expect("resolve content stream");
    let _ = stream.get_stream_data(DecodeLevel::All);

    let output = output.lock().expect("capture output");
    assert!(
        output
            .windows(description.len())
            .any(|window| window == description),
        "object warning must retain the raw source description: {output:?}"
    );
    assert!(
        !output.windows(3).any(|window| window == b"\xef\xbf\xbd"),
        "object warning must not contain U+FFFD: {output:?}"
    );
    let diagnostics = pdf.repair_diagnostics();
    let diagnostic = diagnostics
        .entries()
        .last()
        .expect("object warning diagnostic");
    assert!(
        diagnostic
            .what_bytes()
            .windows(description.len())
            .any(|window| window == description),
        "diagnostic must retain the raw object warning bytes: {:?}",
        diagnostic.what_bytes()
    );
}

#[test]
fn warning_delivers_initial_repair_diagnostics_once_in_original_order() {
    let (logger, output) = recording_logger();
    let (bytes, xref_start) = warnings_only_corrupt_xref_bytes();
    let pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        output.lock().unwrap().as_slice(),
        format!(
            "WARNING: input.pdf: file is damaged\n\
             WARNING: input.pdf (offset {xref_start}): xref not found\n\
             WARNING: input.pdf: Attempting to reconstruct cross-reference table\n"
        )
        .as_bytes()
    );
    assert_eq!(
        pdf.repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| String::from_utf8_lossy(entry.get_message_detail()).into_owned())
            .collect::<Vec<_>>(),
        [
            "file is damaged",
            "xref not found",
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
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(output.lock().unwrap().is_empty());
    assert_eq!(pdf.repair_diagnostics().entries().len(), 3);
}

#[test]
fn warning_delivery_failure_is_returned_by_open() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let (bytes, _) = warnings_only_corrupt_xref_bytes();

    assert!(matches!(
        Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
        description: b"input.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}

#[test]
fn job_open_applies_warning_suppression_before_parsing() {
    let (logger, output) = recording_logger();
    let (bytes, _) = warnings_only_corrupt_xref_bytes();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_suppress_warnings(true);

    let pdf = job
        .open(
            Cursor::new(bytes),
            "suppressed.pdf",
            PdfOpenOptions::default(),
        )
        .expect("warning-bearing PDF should still open");

    assert!(pdf.suppress_warnings());
    assert!(output.lock().unwrap().is_empty());
    assert_eq!(pdf.repair_diagnostics().entries().len(), 3);
    assert!(job.has_warnings());
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
        description: b"check.pdf".to_vec(),
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
            description: b"broken.pdf".to_vec(),
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
             WARNING: broken.pdf (offset {xref_start}): xref not found\n\
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
        description: b"broken.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}

#[test]
fn strict_open_retains_qpdf_header_warning_before_startxref_error() {
    let (logger, output) = recording_logger();
    let error = match Pdf::open_with_options(
        Cursor::new(b"oops\n".to_vec()),
        PdfOpenOptions {
            repair: false,
            logger: Some(logger),
            description: b"bad1.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("a non-PDF input must fail after qpdf records its header warning"),
        Err(error) => error,
    };

    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: bad1.pdf: can't find PDF header\n"
    );
    let (source, diagnostics) = error
        .open_failure()
        .expect("strict open must retain the warning collection");
    assert_eq!(source.to_string(), "bad1.pdf: can't find startxref");
    assert_eq!(diagnostics.entries().len(), 1);
    assert_eq!(
        diagnostics.entries()[0].what_bytes(),
        b"bad1.pdf: can't find PDF header"
    );
}

#[test]
fn unknown_xref_entry_type_matches_qpdf_after_reconstruction() {
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(unknown_xref_entry_type_bytes()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf-compatible reconstruction should return the candidate trailer");

    let error = pdf
        .root_handle()
        .expect_err("the recovered candidate has no /Root dictionary");
    assert!(matches!(
        error,
        Error::QpdfExc(warning) if warning.get_message_detail() == b"unable to find /Root dictionary"
    ));
    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: input.pdf (xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 2; actual = 4\n\
         WARNING: input.pdf: file is damaged\n\
         WARNING: input.pdf (xref stream, offset 71): unknown xref stream entry type 97\n\
         WARNING: input.pdf: Attempting to reconstruct cross-reference table\n\
         WARNING: input.pdf (xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 2; actual = 4\n\
         WARNING: input.pdf: reported number of objects (1) is not one plus the highest object number (1)\n"
    );
}

#[test]
fn reconstruction_orders_a_live_candidate_discovery_warning_after_the_trio() {
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(candidate_discovery_live_warning_bytes()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf-compatible reconstruction should return the xref-stream candidate");

    let error = pdf
        .root_handle()
        .expect_err("the recovered candidate has no /Root dictionary");
    assert!(matches!(
        error,
        Error::QpdfExc(warning) if warning.get_message_detail() == b"unable to find /Root dictionary"
    ));
    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: input.pdf: file is damaged\n\
         WARNING: input.pdf: can't find startxref\n\
         WARNING: input.pdf: Attempting to reconstruct cross-reference table\n\
         WARNING: input.pdf (object 1 0, offset 30): expected endobj\n\
         WARNING: input.pdf: reported number of objects (1) is not one plus the highest object number (2)\n",
        "the trio and line-scan diagnostics must print before the live warning \
         candidate discovery raises while resolving object 1 0"
    );
}

#[test]
fn unknown_second_xref_entry_type_reports_the_payload_offset_like_qpdf() {
    // qpdf's `damagedPDF("xref stream", ...)` reports the input's last read
    // offset, which is the stream payload start after `pipeStreamData`'s
    // single read (QPDF.cc:2496-2498, 2625-2628), so the second malformed
    // entry is still reported at offset 71, not 73.
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(unknown_second_xref_entry_type_bytes()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf-compatible reconstruction should return the candidate trailer");

    let error = pdf
        .root_handle()
        .expect_err("the recovered candidate has no /Root dictionary");
    assert!(matches!(
        error,
        Error::QpdfExc(warning) if warning.get_message_detail() == b"unable to find /Root dictionary"
    ));
    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: input.pdf: file is damaged\n\
         WARNING: input.pdf (xref stream, offset 71): unknown xref stream entry type 97\n\
         WARNING: input.pdf: Attempting to reconstruct cross-reference table\n"
    );
}

#[test]
fn indirect_xref_filter_keeps_cached_null_through_reconstruction() {
    for array_value in [false, true] {
        let (logger, output) = recording_logger();
        let mut pdf = Pdf::open_with_options(
            Cursor::new(indirect_xref_filter_bytes(array_value)),
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
                description: b"input.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("qpdf-compatible reconstruction should return the candidate trailer");

        let error = pdf
            .root_handle()
            .expect_err("the recovered candidate has no /Root dictionary");
        assert!(matches!(
            error,
            Error::QpdfExc(warning) if warning.get_message_detail() == b"unable to find /Root dictionary"
        ));
        let expected = b"WARNING: input.pdf (xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 2; actual = 9\n\
             WARNING: input.pdf: file is damaged\n\
             WARNING: input.pdf (xref stream, offset 85): unknown xref stream entry type 48\n\
             WARNING: input.pdf: Attempting to reconstruct cross-reference table\n\
             WARNING: input.pdf (xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 2; actual = 9\n\
             WARNING: input.pdf: reported number of objects (1) is not one plus the highest object number (2)\n";
        assert_eq!(
            output.lock().unwrap().as_slice(),
            expected,
            "array_value={array_value}: an unresolved indirect filter must remain a cached null"
        );
    }
}

#[test]
fn warning_routes_lazy_resolution_immediately_and_only_once() {
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(LAZY_WARNING_PDF),
        PdfOpenOptions {
            logger: Some(logger),
            description: b"lazy.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    pdf.resolve_canonical_object(ObjectRef::new(5, 0)).unwrap();
    pdf.resolve_canonical_object(ObjectRef::new(5, 0)).unwrap();

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
            description: b"lazy.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(matches!(
        pdf.resolve_canonical_object(ObjectRef::new(5, 0)),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
    assert_eq!(
        pdf.repair_diagnostics().entries()[0].get_message_detail(),
        b"expected endobj"
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
            description: b"live.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    pdf.set_logger(replacement.clone());
    assert_eq!(pdf.logger(), replacement);
    pdf.resolve_canonical_object(ObjectRef::new(5, 0)).unwrap();

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
            description: b"live.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    pdf.set_suppress_warnings(true);
    assert!(pdf.suppress_warnings());
    pdf.resolve_canonical_object(ObjectRef::new(4, 0)).unwrap();
    assert!(output.lock().unwrap().is_empty());

    pdf.set_suppress_warnings(false);
    assert!(!pdf.suppress_warnings());
    pdf.resolve_canonical_object(ObjectRef::new(5, 0)).unwrap();

    let diagnostics = pdf.repair_diagnostics();
    assert_eq!(diagnostics.entries().len(), 2);
    assert!(diagnostics.entries()[0]
        .what_bytes()
        .windows(b"object 4 0".len())
        .any(|w| w == b"object 4 0"));
    assert!(diagnostics.entries()[1]
        .what_bytes()
        .windows(b"object 5 0".len())
        .any(|w| w == b"object 5 0"));
    let output = output.lock().unwrap();
    assert!(!output
        .windows(b"object 4 0".len())
        .any(|w| w == b"object 4 0"));
    assert!(output
        .windows(b"object 5 0".len())
        .any(|w| w == b"object 5 0"));
}

#[test]
fn candidate_reentry_orders_its_own_read_warning_before_its_build_diagnostic() {
    let (logger, output) = recording_logger();
    let error = match Pdf::open_with_options(
        Cursor::new(candidate_reentry_build_failure_after_live_warning_bytes()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("an unrecognized xref stream entry type must fail the candidate re-entry"),
        Err(error) => error,
    };

    assert!(error.open_failure().is_some());
    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: input.pdf: file is damaged\n\
         WARNING: input.pdf: can't find startxref\n\
         WARNING: input.pdf: Attempting to reconstruct cross-reference table\n\
         WARNING: input.pdf (object 1 0, offset 102): expected endobj\n\
         WARNING: input.pdf (xref stream: object 1 0, offset 102): expected endobj\n\
         WARNING: input.pdf (xref stream, offset 9): Cross-reference stream data has the wrong size; expected = 4; actual = 5\n",
        "the re-entry's own read warning must print before the build diagnostic \
         parse_xref_stream_with_canonical_owner writes to its sink afterward, \
         since the read happens first in real call order"
    );
}

#[test]
fn previous_xref_section_defers_a_live_read_warning_through_the_prev_walk() {
    let (logger, output) = recording_logger();
    let mut pdf = Pdf::open_with_options(
        Cursor::new(previous_xref_section_live_warning_bytes()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf-compatible reconstruction should return the candidate trailer");

    let error = pdf
        .root_handle()
        .expect_err("the recovered candidate has no /Root dictionary");
    assert!(matches!(
        error,
        Error::QpdfExc(warning) if warning.get_message_detail() == b"unable to find /Root dictionary"
    ));
    assert_eq!(
        output.lock().unwrap().as_slice(),
        b"WARNING: input.pdf: file is damaged\n\
         WARNING: input.pdf: can't find startxref\n\
         WARNING: input.pdf: Attempting to reconstruct cross-reference table\n\
         WARNING: input.pdf (object 2 0, offset 85): expected endobj\n\
         WARNING: input.pdf (xref stream: object 2 0, offset 85): expected endobj\n\
         WARNING: input.pdf: reported number of objects (1) is not one plus the highest object number (2)\n",
        "the /Prev target's own read warning, raised while merge_previous_xref_sections \
         follows the candidate's /Prev chain, must print after the trio and after \
         discovery's own resolution of the same object -- not live, ahead of both"
    );
}

#[test]
fn previous_trailer_warning_precedes_a_hybrid_live_warning_within_one_hop() {
    let (logger, output) = recording_logger();
    let bytes = previous_classic_trailer_then_hybrid_live_warning_bytes();
    let _pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("the synthetic classic and hybrid xref chain is recoverable");

    let output = output.lock().unwrap().clone();
    let trailer_warning = output
        .windows(b"stream keyword found in trailer".len())
        .position(|window| window == b"stream keyword found in trailer")
        .expect("the previous trailer warning is emitted");
    let hybrid_warning = output
        .windows(b"stream filter type is not name or array".len())
        .position(|window| window == b"stream filter type is not name or array")
        .unwrap_or_else(|| {
            panic!(
                "the hybrid stream warning is emitted:\n{}",
                String::from_utf8_lossy(&output)
            )
        });
    assert!(
        trailer_warning < hybrid_warning,
        "qpdf emits the trailer warning before the hybrid read warning:\n{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn indirect_previous_offset_keeps_the_trio_first() {
    // Invariant guard for the `/Prev` *offset resolution* (as opposed to the
    // previous section's parse, covered by the test above). Resolving an
    // indirect `/Prev` dereferences its target, and that read can warn live
    // through the owner; qpdf has one warnings deque delivered strictly in
    // call order (`libqpdf/QPDF.cc:487-494`), so the reconstruction trio must
    // still come first.
    //
    // This fixture does not by itself demonstrate an inversion: object 3 is
    // already reached by the line scan, so discovery resolves (and warns
    // about) it before the `/Prev` walk runs, and the order is correct with
    // or without the surrounding deferral. The test pins the invariant so a
    // later change to either path cannot silently reorder these lines.
    let (logger, output) = recording_logger();
    let _ = Pdf::open_with_options(
        Cursor::new(indirect_previous_offset_live_warning_bytes()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: b"input.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    );
    let recorded = output.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&recorded).to_string();
    let trio_end = text
        .find("Attempting to reconstruct cross-reference table")
        .expect("the reconstruction trio is emitted");
    assert!(
        text.contains("expected endobj"),
        "the fixture must actually raise the live read warning, otherwise this \
         test is vacuous:\n{text}"
    );
    for live in ["expected endobj"] {
        if let Some(position) = text.find(live) {
            assert!(
                position > trio_end,
                "a live read warning raised while resolving an indirect /Prev must not \
                 precede the buffered reconstruction trio:\n{text}"
            );
        }
    }
}
