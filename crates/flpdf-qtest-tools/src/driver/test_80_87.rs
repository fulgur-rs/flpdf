use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{Read, Seek, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use super::{
    crt_open_error_message, emit_new_diagnostics, open_error_bytes, os_str_diagnostic_bytes,
};
use flpdf::{
    job::{JobExitCode, QPDFJob},
    AcroFormDocumentHelper, Error, Matrix, ObjectHandle, ObjectRef, PageObjectHelper, Pdf,
    PdfOpenOptions, PdfWriter, Pipeline, PipelineError, PipelineHandle, PipelineResult,
};

struct CapturedPipeline {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Pipeline for CapturedPipeline {
    fn identifier(&self) -> &str {
        "qpdf job test capture"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes
            .lock()
            .map_err(|_| PipelineError::runtime("qpdf job capture mutex poisoned"))?
            .extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

/// Open test 80's second document through qpdf's `processFile(path)` boundary.
/// Keep warnings suppressed during parsing so the driver can emit the retained
/// diagnostics exactly once with `arg2`'s filename, and translate a file-open
/// failure through qpdf's path-aware `QPDFSystemError` wording.
fn open_test_80_secondary(
    path: &OsStr,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Pdf<std::fs::File>> {
    let path_bytes = os_str_diagnostic_bytes(path).into_owned();
    let file = std::fs::File::open(path).map_err(|error| {
        let crt_message = crt_open_error_message(path);
        let message = open_error_bytes(&path_bytes, crt_message.as_deref(), &error);
        Error::System(String::from_utf8_lossy(&message).into_owned())
    })?;
    let secondary = Pdf::open_with_options(
        file,
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: path_bytes.clone(),
            ..PdfOpenOptions::default()
        },
    )?;
    let mut diagnostics_written = 0;
    emit_new_diagnostics(
        &secondary,
        &mut diagnostics_written,
        &path_bytes,
        stdout,
        stderr,
    )?;
    Ok(secondary)
}

/// qpdf's test_80 (`test_driver.cc:2761-2805`) exercises
/// `QPDFAcroFormDocumentHelper::transformAnnotations` (transform the main
/// file's page-1 annotations in place and add the resulting form fields via
/// `addAndRenameFormFields`) and `QPDFPageObjectHelper::copyAnnotations`
/// (copy the same annotations, mirrored, onto page 1 of a second document
/// opened from `arg2`), then writes both documents with
/// `QPDFWriter::setQDFMode(true)`/`setStaticID(true)` to `a.pdf`/`b.pdf`. No
/// stdout is printed by this test; its only externally observable effect is
/// those two written files.
pub(crate) fn run_test_80<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 `test_driver.cc:2761-2805`. The qpdf public
    // `transformAnnotations` and `addAndRenameFormFields` responsibilities
    // remain in AcroFormDocumentHelper; this driver only sequences their live
    // handles and delegates the foreign-page copy to PageObjectHelper.
    let arg2 = arg2.ok_or_else(|| Error::Internal("test 80 requires arg2".to_owned()))?;
    let page1_ref = flpdf::pages::page_refs(pdf)?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Internal("test 80 requires a first page".to_owned()))?;
    // qpdf's `page1.getKey("/Annots")` (test_driver.cc:2775) resolves page1
    // as `getKey`'s own first step, with no separate explicit resolve
    // beforehand. `try_get_key` below does the same, and
    // `transform_annotations`'s own resolving `try_as_array` step resolves
    // `old_annots` in turn, so no standalone resolve is needed for either
    // handle.
    let page1 = pdf.get_object_handle(page1_ref);
    let old_annots = page1.try_get_key(b"/Annots")?;

    let mut first_matrix = Matrix::default();
    first_matrix.translate(306.0, 396.0);
    first_matrix.scale(0.4, 0.4);
    {
        let mut acroform = AcroFormDocumentHelper::new(pdf)?;
        let transformed = acroform.transform_annotations(old_annots.clone(), first_matrix)?;
        for annotation in &transformed.new_annotations {
            old_annots.append_array_item(annotation.clone())?;
        }
        acroform.add_and_rename_form_fields(transformed.new_fields.clone())?;
    }
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let secondary_filename = os_str_diagnostic_bytes(arg2).into_owned();
    let mut pdf2 = open_test_80_secondary(arg2, stdout, stderr)?;
    let mut secondary_diagnostics = pdf2.repair_diagnostics().entries().len();
    let page2_ref = flpdf::pages::page_refs(&mut pdf2)?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Internal("test 80 requires a second page".to_owned()))?;
    let mut second_matrix = Matrix::default();
    second_matrix.translate(612.0, 0.0);
    second_matrix.scale(-1.0, 1.0);
    {
        let mut page2 = PageObjectHelper::new(page2_ref, &mut pdf2);
        page2.copy_annotations_from(page1.clone(), second_matrix, pdf)?;
    }
    emit_new_diagnostics(
        &pdf2,
        &mut secondary_diagnostics,
        &secondary_filename,
        stdout,
        stderr,
    )?;

    {
        let mut writer = PdfWriter::new(pdf);
        writer.set_output_file("a.pdf")?;
        writer.set_static_id(true);
        writer.set_qdf_mode(true);
        writer.write()?;
    }
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    {
        let mut writer = PdfWriter::new(&mut pdf2);
        writer.set_output_file("b.pdf")?;
        writer.set_static_id(true);
        writer.set_qdf_mode(true);
        writer.write()?;
    }
    emit_new_diagnostics(
        &pdf2,
        &mut secondary_diagnostics,
        &secondary_filename,
        stdout,
        stderr,
    )?;
    Ok(())
}

/// qpdf's test_81 (`test_driver.cc:2807-2817`) checks that a type-mismatched
/// accessor on a handle with no owning document -- `newNull().getIntValue()`
/// -- throws `QPDFExc` with error code `qpdf_e_object`, rather than the
/// ordinary warn-and-return-default behavior `getIntValue` uses when a
/// document *is* reachable to receive the warning
/// (`libqpdf/QPDFObjectHandle.cc:502-513`, `typeWarning`,
/// `libqpdf/QPDFObjectHandle.cc:2168-2189`). No stdout is printed.
pub(crate) fn run_test_81<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 `test_driver.cc:2807-2817`: a type error on a direct
    // handle with no owning document is caught as `QPDFExc(qpdf_e_object)`.
    // `try_get_int_value` is the canonical fallible Rust boundary for that
    // same qpdf type-warning operation; do not print or synthesize the
    // warning because qpdf catches it before the driver footer is emitted.
    let error = ObjectHandle::null()
        .try_get_int_value()
        .expect_err("test 81 integer accessor must raise the qpdf object error");
    assert!(matches!(
        error,
        Error::QpdfExc(warning)
            if warning.get_error_code() == flpdf::QpdfErrorCode::Object
    ));
    Ok(())
}

/// qpdf's test_82 (`test_driver.cc:2819-2861`) exercises the compound
/// predicates `QPDFObjectHandle::isNameAndEquals`, `isDictionaryOfType`,
/// `isStreamOfType`, and `isOrHasName`. No stdout is printed; only internal
/// assertions, all of which need one of these predicates from their very
/// first line.
pub(crate) fn run_test_82<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 `test_driver.cc:2819-2861`. Keep the consumer's
    // assertion order and let the canonical ObjectHandle methods perform
    // lazy resolution; qpdf's C++ methods return bool while flpdf exposes
    // the same operation through its fallible resolver boundary.
    let name = ObjectHandle::name(b"Marvin".to_vec());
    let string = ObjectHandle::string(b"/Marvin".to_vec());
    assert!(name.try_is_name_and_equals(b"Marvin")?);
    assert!(!name.try_is_name_and_equals(b"/Marvin")?);
    assert!(!string.try_is_name_and_equals(b"Marvin")?);

    let mut dictionary = ObjectHandle::parse(b"<</A 1 /Type /Test /Subtype /Marvin>>")?;
    assert!(dictionary.try_is_dictionary_of_type(b"Test", b"")?);
    assert!(dictionary.try_is_dictionary_of_type(b"Test", b"")?);
    assert!(dictionary.try_is_dictionary_of_type(b"Test", b"Marvin")?);
    assert!(dictionary.try_is_dictionary_of_type(b"", b"Marvin")?);
    assert!(dictionary.try_is_dictionary_of_type(b"", b"")?);
    assert!(!dictionary.try_is_dictionary_of_type(b"Test2", b"")?);
    assert!(!dictionary.try_is_dictionary_of_type(b"Test2", b"Marvin")?);
    assert!(!dictionary.try_is_dictionary_of_type(b"Test", b"M")?);
    assert!(!name.try_is_dictionary_of_type(b"", b"")?);

    dictionary = ObjectHandle::parse(b"<</A 1 /Type null /Subtype /Marvin>>")?;
    assert!(!dictionary.try_is_dictionary_of_type(b"Test", b"")?);
    dictionary = ObjectHandle::parse(b"<</A 1 /Type (Test) /Subtype /Marvin>>")?;
    assert!(!dictionary.try_is_dictionary_of_type(b"/Test", b"")?);
    dictionary = ObjectHandle::parse(b"<</A 1 /Type /Test /Subtype (Marvin)>>")?;
    assert!(!dictionary.try_is_dictionary_of_type(b"/Test", b"")?);
    dictionary = ObjectHandle::parse(b"<</A 1 /Subtype /Marvin>>")?;
    assert!(!dictionary.try_is_dictionary_of_type(b"Test", b"/Marvin")?);

    let stream = pdf.get_object_handle(ObjectRef::new(1, 0));
    assert!(stream.try_is_stream_of_type(b"ObjStm", b"")?);
    assert!(!stream.try_is_stream_of_type(b"Test", b"")?);
    assert!(!pdf
        .get_object_handle(ObjectRef::new(2, 0))
        .try_is_stream_of_type(b"Pages", b"")?);

    let mut array = ObjectHandle::parse(b"[/Blah /Blaah /Blaaah]")?;
    assert!(array.try_is_or_has_name(b"Blah")?);
    assert!(array.try_is_or_has_name(b"Blaaah")?);
    assert!(!array.try_is_or_has_name(b"Blaaaah")?);
    assert!(array.try_get_array_item(0)?.try_is_or_has_name(b"Blah")?);
    assert!(!array.try_get_array_item(1)?.try_is_or_has_name(b"Blah")?);
    array = ObjectHandle::parse(b"[]")?;
    assert!(!array.try_is_or_has_name(b"Blah")?);
    assert!(!string.try_is_or_has_name(b"Marvin")?);
    Ok(())
}

/// qpdf's test_83 (`test_driver.cc:2863-2882`) reads `arg2` as a JSON job
/// file and calls `QPDFJob::initializeFromJson` (a `partial=false`
/// job-config parse), reporting success or a caught usage/exception message.
/// Real qpdf prints "calling initializeFromJson" unconditionally immediately
/// before that call, once `arg2` has been read into memory.
pub(crate) fn run_test_83<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let arg2 =
        arg2.ok_or_else(|| Error::Internal("test 83 requires a job JSON path".to_owned()))?;
    let arg2_diagnostic = os_str_diagnostic_bytes(arg2);
    // qpdf's `QUtil::read_file_into_memory` (`test_driver.cc:2871-2873`) opens
    // through `safe_fopen`, the same CRT-backed open path `FileInputSource`
    // uses, and reports a failed open as `"open " + filename + ": " +
    // strerror(errno)` (`QUtil.cc:490-518`). Translate through the same
    // CRT-message route the other driver file opens use (test 90's update-JSON
    // open, `test_88_98.rs::run_test_90`) instead of a bare `Error::Io`.
    let bytes = std::fs::read(arg2).map_err(|error| {
        let crt_message = crt_open_error_message(arg2);
        let message = open_error_bytes(&arg2_diagnostic, crt_message.as_deref(), &error);
        Error::System(String::from_utf8_lossy(&message).into_owned())
    })?;

    writeln!(stdout, "calling initializeFromJson")?;
    // qpdf reads the file into a raw `std::string` (`QUtil::read_file_into_memory`,
    // `test_driver.cc:2871-2873`) with no UTF-8 validation. Keep the same raw
    // bytes through the public byte-taking QPDFJob boundary so valid high-bit
    // string values are handled by the qpdf-compatible parser rather than
    // rejected by an adapter-level UTF-8 conversion.
    let result = QPDFJob::new().initialize_from_json_bytes(&bytes);
    match result {
        Ok(()) => writeln!(stdout, "called initializeFromJson")?,
        Err(Error::Usage(error)) => writeln!(stderr, "usage: {error}")?,
        Err(error) => writeln!(stderr, "exception: {error}")?,
    }
    Ok(())
}

/// qpdf's test_84 (`test_driver.cc:2884-2971`) exercises the full `QPDFJob`
/// API surface across five scenarios: the fluent `config()` builder,
/// `run()`, `checkConfiguration()`, `registerProgressReporter()`, and
/// `setOutputStreams()`. The very first line of the C++ body prints "normal"
/// unconditionally before touching any of that surface.
pub(crate) fn run_test_84<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    writeln!(stdout, "normal")?;

    {
        let mut job = QPDFJob::new();
        {
            let mut config = job.config();
            config.input_file("minimal.pdf")?;
            config.output_file("a.pdf")?;
            config.qdf().deterministic_id();
            config.object_streams("preserve")?.progress();
            config.check_configuration()?;
        }
        let status = job.run()?;
        assert_eq!(status, JobExitCode::Success);
        assert!(!job.has_warnings());
    }

    writeln!(stdout, "custom progress reporter")?;
    {
        let progress = Rc::new(RefCell::new(Vec::new()));
        let progress_for_job = Rc::clone(&progress);
        let mut job = QPDFJob::new();
        job.register_progress_reporter(move |percent| {
            progress_for_job
                .borrow_mut()
                .extend_from_slice(format!("custom write progress: {percent}%\n").as_bytes());
            Ok(())
        });
        {
            let mut config = job.config();
            config.input_file("minimal.pdf")?;
            config.output_file("a.pdf")?;
            config.qdf().deterministic_id();
            config.object_streams("preserve")?.progress();
            config.check_configuration()?;
        }
        let status = job.run()?;
        assert_eq!(status, JobExitCode::Success);
        assert!(!job.has_warnings());
        stdout.write_all(&progress.borrow())?;
    }

    writeln!(stdout, "error caught by check")?;
    {
        let mut job = QPDFJob::new();
        {
            let mut config = job.config();
            config.output_file("a.pdf")?;
            config.qdf();
        }
        writeln!(stdout, "finished config")?;
        match job.check_configuration() {
            Ok(()) => {
                return Err(Error::Internal(
                    "test 84 check unexpectedly succeeded".to_owned(),
                ))
            }
            Err(Error::Usage(error)) => writeln!(stdout, "usage: {error}")?,
            Err(error) => return Err(error),
        }
    }

    writeln!(stdout, "error caught by run")?;
    {
        let mut job = QPDFJob::new();
        {
            let mut config = job.config();
            config.output_file("a.pdf")?;
            config.qdf();
        }
        writeln!(stdout, "finished config")?;
        match job.run() {
            Ok(_) => {
                return Err(Error::Internal(
                    "test 84 run unexpectedly succeeded".to_owned(),
                ))
            }
            Err(Error::Usage(error)) => writeln!(stdout, "usage: {error}")?,
            Err(error) => return Err(error),
        }
    }

    writeln!(stdout, "output capture")?;
    {
        let captured_stdout = Arc::new(Mutex::new(Vec::new()));
        let captured_stderr = Arc::new(Mutex::new(Vec::new()));
        let mut job = QPDFJob::new();
        job.set_output_streams(
            Some(PipelineHandle::new(CapturedPipeline {
                bytes: Arc::clone(&captured_stdout),
            })),
            Some(PipelineHandle::new(CapturedPipeline {
                bytes: Arc::clone(&captured_stderr),
            })),
        );
        {
            let mut config = job.config();
            config.input_file("bad2.pdf")?;
            config.show_object("4,0")?;
            config.check_configuration()?;
        }
        writeln!(stdout, "calling run")?;
        let _ = job.run()?;
        writeln!(stdout, "captured stdout")?;
        let captured_stdout = captured_stdout
            .lock()
            .map_err(|_| PipelineError::runtime("qpdf job stdout capture mutex poisoned"))?;
        stdout.write_all(&captured_stdout)?;
        writeln!(stdout, "captured stderr")?;
        let captured_stderr = captured_stderr
            .lock()
            .map_err(|_| PipelineError::runtime("qpdf job stderr capture mutex poisoned"))?;
        stdout.write_all(&captured_stderr)?;
    }
    Ok(())
}

/// qpdf's test_85 (`test_driver.cc:2973-3062`) exercises the
/// `QPDFObjectHandle::getValueAs...` family in qpdf call order. The canonical
/// Rust accessors resolve the receiver, return `None` without a type warning
/// for a wrong type, and retain qpdf's integer clamp warnings.
pub(crate) fn run_test_85<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let oh_b = ObjectHandle::boolean(false);
    let oh_i = ObjectHandle::integer(1);
    let oh_i_maxplus = ObjectHandle::integer(i64::from(i32::MAX) + 1);
    let oh_i_umaxplus = ObjectHandle::integer(i64::from(u32::MAX) + 1);
    let oh_i_minminus = ObjectHandle::integer(i64::from(i32::MIN) - 1);
    let oh_i_neg = ObjectHandle::integer(-1);
    let oh_r = ObjectHandle::real_literal(42.0, b"42.0".to_vec());
    let oh_n = ObjectHandle::name(b"Test".to_vec());
    let oh_s = ObjectHandle::string(b"/Test".to_vec());
    let oh_o = ObjectHandle::operator(b"/Test".to_vec());
    let oh_ii = ObjectHandle::inline_image(b"/Test".to_vec());

    assert_eq!(oh_b.try_get_value_as_bool()?, Some(false));
    assert_eq!(oh_i.try_get_value_as_bool()?, None);
    assert_eq!(oh_i.try_get_value_as_int()?, Some(1));
    assert_eq!(oh_b.try_get_value_as_int()?, None);
    assert_eq!(oh_i.try_get_value_as_int_as_int()?, Some(1));
    assert_eq!(oh_b.try_get_value_as_int_as_int()?, None);
    assert_eq!(oh_i_maxplus.try_get_value_as_int_as_int()?, Some(i32::MAX));
    assert_eq!(oh_i_minminus.try_get_value_as_int_as_int()?, Some(i32::MIN));

    assert_eq!(oh_i.try_get_value_as_uint()?, Some(1));
    assert_eq!(oh_b.try_get_value_as_uint()?, None);
    assert_eq!(oh_i_neg.try_get_value_as_uint()?, Some(0));
    assert_eq!(oh_i.try_get_value_as_uint_as_uint()?, Some(1));
    assert_eq!(oh_b.try_get_value_as_uint_as_uint()?, None);
    assert_eq!(oh_i_neg.try_get_value_as_uint_as_uint()?, Some(0));
    assert_eq!(
        oh_i_umaxplus.try_get_value_as_uint_as_uint()?,
        Some(u32::MAX)
    );

    assert_eq!(oh_r.try_get_value_as_real()?, Some(b"42.0".to_vec()));
    assert_eq!(oh_i.try_get_value_as_real()?, None);

    let num = oh_i.try_get_value_as_number()?.expect("integer number");
    assert!((num - 1.0) < 1e-6 && (num - 1.0) > -1e-6);
    let num = oh_r.try_get_value_as_number()?.expect("real number");
    assert!((num - 42.0) < 1e-6 && (num - 42.0) > -1e-6);
    assert_eq!(oh_b.try_get_value_as_number()?, None);

    assert_eq!(oh_n.try_get_value_as_name()?, Some(b"/Test".to_vec()));
    assert_eq!(oh_r.try_get_value_as_name()?, None);
    assert_eq!(oh_s.try_get_value_as_utf8()?, Some(b"/Test".to_vec()));
    assert_eq!(oh_r.try_get_value_as_utf8()?, None);

    // qpdf's own source repeats this exact `getValueAsUTF8` block twice in a
    // row (`test_driver.cc:3047-3051`, identical to the block immediately
    // above it); preserve the call order rather than de-duplicating it.
    assert_eq!(oh_s.try_get_value_as_utf8()?, Some(b"/Test".to_vec()));
    assert_eq!(oh_r.try_get_value_as_utf8()?, None);

    assert_eq!(oh_o.try_get_value_as_operator()?, Some(b"/Test".to_vec()));
    assert_eq!(oh_r.try_get_value_as_operator()?, None);
    assert_eq!(
        oh_ii.try_get_value_as_inline_image()?,
        Some(b"/Test".to_vec())
    );
    assert_eq!(oh_r.try_get_value_as_inline_image()?, None);

    Ok(())
}

/// qpdf's test_86 (`test_driver.cc:3064-3083`) checks symmetry between
/// `QPDFObjectHandle::newUnicodeString` and `getUTF8Value` for a string that
/// round-trips through UTF-16BE with a BOM (because PDFDocEncoding cannot
/// represent it, `U+001F` falling in the range PDFDocEncoding instead maps to
/// accent glyphs) but has no codepoint above `U+00FF`. No stdout is printed
/// anywhere in this test; only internal assertions.
pub(crate) fn run_test_86<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let utf8_val: &[u8] = b"\x1f";
    let utf16_val: &[u8] = b"\xfe\xff\x00\x1f";

    // QUtil::utf8_to_ascii/utf8_to_pdf_doc, the `bool`-returning overloads
    // qpdf's own test asserts on (`test_driver.cc:3074-3077`,
    // `libqpdf/QUtil.cc:1651-1673`). U+001F is plain ASCII, so
    // `utf8_to_ascii` round-trips it and reports success; PDFDocEncoding
    // reassigns `0x18`-`0x1f` to accent glyphs
    // (`libqpdf/QUtil.cc:1575-1580`), so `utf8_to_pdf_doc` substitutes `?`
    // and reports failure. Comparing only the bytes would not separate that
    // from a literal `?` in the input, which converts successfully.
    let (ascii, ascii_ok) = flpdf::qutil::utf8_to_ascii_checked(utf8_val);
    assert!(ascii_ok);
    assert_eq!(ascii, utf8_val);
    let (pdf_doc, pdf_doc_ok) = flpdf::qutil::utf8_to_pdf_doc_checked(utf8_val);
    assert!(!pdf_doc_ok);
    assert_eq!(pdf_doc, b"?");

    // QUtil::utf8_to_utf16 (`libqpdf/QUtil.cc:1621-1625`) is BOM + UTF-16BE
    // code units for every codepoint, which for this ASCII-range input is
    // exactly `filespec_helper::encode_utf16be`'s own contract.
    let utf16_from_utf8 = flpdf::filespec_helper::encode_utf16be(
        std::str::from_utf8(utf8_val).expect("utf8_val is valid UTF-8 by construction"),
    );
    assert_eq!(utf16_from_utf8, utf16_val);

    // QUtil::utf16_to_utf8 (`libqpdf/QUtil.cc:1693-...`) strips a leading
    // BOM and decodes the rest as UTF-16(BE or LE); `pdf_string::utf8_value`
    // is the matching byte-level transcoder for this direct probe.
    let utf8_from_utf16 = flpdf::pdf_string::utf8_value(utf16_val);
    assert_eq!(utf8_from_utf16, utf8_val);

    let stored = flpdf::pdf_string::new_unicode_string(utf8_val);
    assert_eq!(stored, utf16_val);

    // qpdf's newUnicodeString result is read through the public object-handle
    // accessors (`test_driver.cc:3080-3082`), not through QPDF_String's
    // implementation-level value. Keep the same assertion order: stored
    // bytes first, then the UTF-8 view.
    let stored_handle = ObjectHandle::string(stored);
    let stored_value = stored_handle.try_get_string_value()?;
    assert_eq!(stored_value, utf16_val);
    let utf8_of_stored = stored_handle.try_get_utf8_value()?;
    assert_eq!(utf8_of_stored, utf8_val);

    Ok(())
}

/// qpdf's test_87 (`test_driver.cc:3085-3103`) demonstrates that a
/// dictionary entry resolving to null is equivalent to a missing key across
/// `unparse()`, `getKeys()`, and `getJSON()`. No stdout is printed; only
/// internal assertions.
pub(crate) fn run_test_87<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let dict = ObjectHandle::parse(b"<< /A 1 /B null >>")?;
    assert_eq!(dict.unparse(), b"<< /A 1 >>");
    assert_eq!(dict.try_get_keys()?, BTreeSet::from([b"/A".to_vec()]));

    dict.replace_key(b"/A", ObjectHandle::null())?;
    assert_eq!(dict.unparse(), b"<< >>");
    assert_eq!(dict.try_get_keys()?, BTreeSet::new());

    let dict = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::parse(b"2")?),
        (b"B".to_vec(), ObjectHandle::null()),
    ]);
    assert_eq!(dict.unparse(), b"<< /A 2 >>");
    assert_eq!(dict.try_get_keys()?, BTreeSet::from([b"/A".to_vec()]));

    // `dict.getJSON(JSON::LATEST)` (qpdf 11.9.0's latest schema is v2,
    // matching `json_inspect::QPDF_JSON_VERSION`) followed by `JSON::unparse`
    // -- ported publicly as `json_inspect::pdf_object_to_json` (which itself
    // documents `QPDF_Dictionary::writeJSON`'s identical null-omission rule,
    // `libqpdf/QPDF_Dictionary.cc:75-76`) and `Json::unparse`.
    let json = flpdf::json_inspect::pdf_object_to_json(&dict)
        .map_err(|error| Error::Internal(error.to_string()))?;
    let unparsed = json
        .unparse()
        .map_err(|error| Error::Internal(error.to_string()))?;
    assert_eq!(unparsed, b"{\n  \"/A\": 2\n}");

    Ok(())
}
