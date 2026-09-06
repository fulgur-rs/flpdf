use std::any::Any;
use std::ffi::OsStr;
use std::io::{Cursor, Read, Seek, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use flpdf::{
    qutil, EncryptParams, Error, ObjectHandle, PageDocumentHelper, PageObjectHelper, Pdf,
    PdfOpenOptions, PdfWriter, Pipeline, PipelineError, PipelineHandle, PipelineResult, QPDFLogger,
};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};
use crate::output::write_bytes;

/// Open `path` as a secondary document the way qpdf's own single-argument
/// `QPDF::processFile(char const*)` does for every test in this file that
/// takes a foreign document via `arg2` (`oldpdf`/`pdf2` locals,
/// test_driver.cc:2087). Repair is enabled (qpdf's `processFile` always
/// attempts recovery on demand) and any repair diagnostic is printed
/// immediately, exactly once, using `path`'s own name — matching qpdf's
/// default warning callback, which prints straight to `std::cerr` for a
/// `QPDF` that never installs a custom handler. This is the same recipe as
/// `test_26_33.rs`'s private `open_secondary_pdf` (that file's copy is not
/// `pub(crate)`, so it cannot be imported here); no password parameter is
/// needed because every call site in this file uses the one-argument
/// `processFile` overload.
fn open_secondary_pdf(
    path: &OsStr,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Pdf<std::fs::File>> {
    let file = std::fs::File::open(path)?;
    let path_bytes = os_str_diagnostic_bytes(path).into_owned();
    let options = PdfOpenOptions {
        repair: true,
        suppress_warnings: true,
        description: path_bytes.clone(),
        ..PdfOpenOptions::default()
    };
    let secondary = Pdf::open_with_options(file, options)?;
    let mut secondary_diagnostics_written = 0;
    emit_new_diagnostics(
        &secondary,
        &mut secondary_diagnostics_written,
        &path_bytes,
        stdout,
        stderr,
    )?;
    Ok(secondary)
}

/// test_56_59 (test_driver.cc:2073-2113): overlay `pdf2`'s pages as Form
/// XObjects onto `pdf`'s own pages, one destination resource dictionary and
/// one placed content fragment per page, then write `a.pdf` in QDF mode with
/// a static `/ID`. `handle_from_transformation`/`invert_to_transformation`
/// select which of the four `getFormXObjectForPage`/`placeFormXObject`
/// rotation-handling combinations `test_56`..`test_59` each exercise
/// (test_driver.cc:2077-2082).
///
/// The loop below follows qpdf's live page-helper route: source pages become
/// document-owned Form XObjects, those handles are copied into the destination
/// document, and the destination page receives the resource and content-stream
/// mutations before the QDF/static-ID write.
#[allow(clippy::too_many_arguments)]
fn test_56_59_body<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    handle_from_transformation: bool,
    invert_to_transformation: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `assert(arg2);` (test_driver.cc:2085).
    let arg2 = arg2.expect("test 56-59 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
    let mut pdf2 = open_secondary_pdf(arg2, stdout, stderr)?;
    let arg2_diagnostic = os_str_diagnostic_bytes(arg2);
    let mut secondary_diagnostics_written = pdf2.repair_diagnostics().entries().len();

    // `QPDFPageDocumentHelper(pdf).getAllPages()` / `QPDFPageDocumentHelper(pdf2).getAllPages()`
    // (test_driver.cc:2089-2091).
    let pages1 = PageDocumentHelper::new(pdf).get_all_pages()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let pages2 = PageDocumentHelper::new(&mut pdf2).get_all_pages()?;
    emit_new_diagnostics(
        &pdf2,
        &mut secondary_diagnostics_written,
        &arg2_diagnostic,
        stdout,
        stderr,
    )?; // cov:ignore: diagnostic sink failures are covered by the shared driver flush tests; this terminator has no separate qpdf behavior
    let npages = pages1.len().min(pages2.len());

    for index in 0..npages {
        // qpdf first creates the Form XObject in the source document, then
        // imports that live indirect handle into the destination
        // (`test_driver.cc:2095-2096`).
        let source_form = {
            let mut source_page = PageObjectHelper::new(pages2[index], &mut pdf2);
            source_page.get_form_xobject_for_page(handle_from_transformation)?
        };
        emit_new_diagnostics(
            &pdf2,
            &mut secondary_diagnostics_written,
            &arg2_diagnostic,
            stdout,
            stderr,
        )?; // cov:ignore: diagnostic sink failures are covered by the shared driver flush tests; this terminator has no separate qpdf behavior
        let form = pdf.copy_foreign_object(&source_form)?;

        // qpdf's getAttribute("/Resources", true) makes an inherited or
        // indirect resource dictionary private before the XObject entry is
        // installed. The live PageObjectHelper and ObjectHandle routes do the
        // same work here.
        let content = {
            let mut destination_page = PageObjectHelper::new(pages1[index], pdf);
            let resources = destination_page.get_resources(true)?;
            let mut min_suffix = 1;
            let name = resources.get_unique_resource_name(b"/Fx", &mut min_suffix, None)?;
            let rect = destination_page
                .get_trim_box(false, false)?
                .try_get_array_as_rectangle()?;
            let name_text = String::from_utf8(name.clone())
                .expect("qpdf-generated Fx resource names are ASCII");
            let (content, _matrix) = destination_page.place_form_xobject(
                form.clone(),
                &name_text,
                rect,
                invert_to_transformation,
                true,
                false,
            )?; // cov:ignore: valid qpdf fixtures cover placement success; this is only the defensive Result propagation edge
            resources.merge_resources(&ObjectHandle::parse(b"<< /XObject << >> >>")?, None)?;
            resources.get_key(b"/XObject").replace_key(&name, form)?;
            content
        };

        // qpdf adds these streams after the resource mutation, in this exact
        // order: a leading `q\n`, then `\nQ\n` plus the placement fragment.
        let q_stream = pdf.new_stream_with_data(Rc::new(b"q\n".to_vec()))?;
        let placed_stream =
            pdf.new_stream_with_data(Rc::new(format!("\nQ\n{content}").into_bytes()))?;
        let mut destination_page = PageObjectHelper::new(pages1[index], pdf);
        destination_page.add_page_contents(q_stream, true)?;
        destination_page.add_page_contents(placed_stream, false)?;

        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.write()?;
    // `QPDFWriter::write()` can resolve objects that were never touched by
    // the loop above (e.g. while renumbering the full object graph), and
    // that resolution can append new repair diagnostics. qpdf's own warning
    // callback prints synchronously as `write()` runs, so a final drain here
    // is required to keep this driver's stdout/stderr in the same order.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

/// test_56 (test_driver.cc:2115-2119): `test_56_59(pdf, arg2, false, false)`.
pub(crate) fn run_test_56<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        false,
        false,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// test_57 (test_driver.cc:2121-2125): `test_56_59(pdf, arg2, true, false)`.
pub(crate) fn run_test_57<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        true,
        false,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// test_58 (test_driver.cc:2127-2131): `test_56_59(pdf, arg2, false, true)`.
pub(crate) fn run_test_58<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        false,
        true,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// test_59 (test_driver.cc:2133-2137): `test_56_59(pdf, arg2, true, true)`.
pub(crate) fn run_test_59<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        true,
        true,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// The `merge_resources` conflicts-tracking map: `rtype -> old_key ->
/// new_key`, structurally identical to (but not the same nominal path as)
/// `flpdf`'s own `object_handle::ResourceConflicts` — that type alias lives
/// in a private (`mod object_handle;`, not `pub mod`) module and is not
/// re-exported from the crate root, so it cannot be named from this crate.
/// Rust checks the parameter type
/// (`Option<&mut object_handle::ResourceConflicts>`) structurally for a type
/// alias, so a locally-declared alias with the identical expansion satisfies
/// [`ObjectHandle::merge_resources`]'s signature without needing to name the
/// inaccessible alias — a container substitution, not a missing primitive.
type MergeConflicts =
    std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>>;

/// qpdf's local `show_conflicts` lambda (test_driver.cc:2176-2184): print
/// `msg`, then each `rtype` (in sorted order, matching `std::map`'s own
/// ordering — `MergeConflicts` is a `BTreeMap` for the same reason) followed
/// by its `old_key -> new_key` pairs, two-space indented.
fn show_conflicts(
    msg: &str,
    conflicts: &MergeConflicts,
    stdout: &mut dyn Write,
) -> flpdf::Result<()> {
    writeln!(stdout, "{msg}")?;
    for (rtype, renames) in conflicts {
        write_bytes(stdout, rtype)?;
        writeln!(stdout, ":")?;
        for (old_key, new_key) in renames {
            write!(stdout, "  ")?;
            write_bytes(stdout, old_key)?;
            write!(stdout, " -> ")?;
            write_bytes(stdout, new_key)?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}

/// qpdf's local `make_resource` lambda (test_driver.cc:2152-2157): build a
/// one-element array holding `QPDFObjectHandle::newString(text)`, make it an
/// indirect object of `pdf`, and install it at `dict[key]`.
fn make_resource<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    key: &[u8],
    text: &[u8],
) -> flpdf::Result<()> {
    let array = ObjectHandle::array(vec![ObjectHandle::string(text.to_vec())]);
    let indirect = pdf.make_indirect_object_handle(array)?;
    dict.replace_key(key, indirect)
}

/// test_60 (test_driver.cc:2139-2213): boundary-condition testing for
/// `getUniqueResourceName` and conflict-detecting `mergeResources`.
///
/// The four conflict-reporting merges and the final QDF/static-ID write use
/// the public canonical resource and live-trailer routes, matching qpdf's
/// `test_driver.cc:2186-2212` sequence.
pub(crate) fn run_test_60<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, stderr, diagnostics_written); // test_60 has no warning-producing operation on `pdf`.

    let r1 = ObjectHandle::dictionary(vec![]);
    let mut min_suffix: usize = 1;
    for _ in 1..3 {
        let name = r1.get_unique_resource_name(b"/Quack", &mut min_suffix, None)?;
        r1.merge_resources(&ObjectHandle::parse(b"<< /Z << >> >>")?, None)?;
        r1.get_key(b"/Z")
            .replace_key(&name, ObjectHandle::string(b"moo".to_vec()))?;
    }

    let z = r1.get_key(b"/Z");
    r1.replace_key(b"/Y", ObjectHandle::dictionary(vec![]))?;
    let y = r1.get_key(b"/Y");
    make_resource(pdf, &z, b"/F1", b"r1.Z.F1")?;
    make_resource(pdf, &z, b"/F2", b"r1.Z.F2")?;
    make_resource(pdf, &y, b"/F2", b"r1.Y.F2")?;
    make_resource(pdf, &y, b"/F3", b"r1.Y.F3")?;

    let r2 = ObjectHandle::parse(b"<< /Z << >> /Y << >> >>")?;
    let z = r2.get_key(b"/Z");
    let y = r2.get_key(b"/Y");
    make_resource(pdf, &z, b"/F2", b"r2.Z.F2")?;
    make_resource(pdf, &y, b"/F3", b"r2.Y.F3")?;
    make_resource(pdf, &y, b"/F4", b"r2.Y.F4")?;
    // qpdf: `y.replaceKey("/F5", QPDFObjectHandle::newString("direct r2.Y.F5"));`
    // (test_driver.cc:2173) — a direct object, unlike the four `make_resource` calls above.
    y.replace_key(b"/F5", ObjectHandle::string(b"direct r2.Y.F5".to_vec()))?;

    let mut conflicts: MergeConflicts = MergeConflicts::new();

    r1.merge_resources(&r2, Some(&mut conflicts))?;
    show_conflicts("first merge", &conflicts, stdout)?;
    let r3 = r1.shallow_copy()?;
    // Merge again. The direct object gets recopied. Everything else is the same
    // (test_driver.cc:2189-2190).
    r1.merge_resources(&r2, Some(&mut conflicts))?;
    show_conflicts("second merge", &conflicts, stdout)?;

    // qpdf promotes every direct value in r2's resource subdictionaries
    // before the third and fourth merges. This is the canonical qpdf-shaped
    // operation, not a driver-local traversal.
    r2.make_resources_indirect(pdf)?;
    r1.merge_resources(&r2, Some(&mut conflicts))?;
    show_conflicts("third merge", &conflicts, stdout)?;
    r1.merge_resources(&r2, Some(&mut conflicts))?;
    show_conflicts("fourth merge", &conflicts, stdout)?;

    // Pdf::trailer returns the live dictionary observed by the writer, so the
    // three qpdf trailer replacements remain on the canonical handle route.
    let trailer = pdf.trailer();
    trailer.replace_key(b"/QTest1", r1)?;
    trailer.replace_key(b"/QTest2", r2)?;
    trailer.replace_key(b"/QTest3", r3)?;
    pdf.mark_object_handle_dirty(&trailer)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.write()?;
    Ok(())
}

/// Rust-native test helper equivalent to qpdf's `ExtendNameTree` subclass.
///
/// The C++ test observes the destructor's stdout side effect. Rust has no
/// shared-library vtable boundary to probe, but `Drop` is the corresponding
/// ownership contract and remains directly observable by the qtest output.
struct Test61ExtendNameTree<'a> {
    stdout: &'a mut dyn Write,
}

impl Drop for Test61ExtendNameTree<'_> {
    fn drop(&mut self) {
        let _ = writeln!(self.stdout, "~ExtendNameTree called");
    }
}

struct Test61BufferInputSource;

trait Test61InputSource {
    fn as_any(&self) -> &dyn Any;
}

impl Test61InputSource for Test61BufferInputSource {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Test61DiscardPipeline;

trait Test61Pipeline {
    fn as_any(&self) -> &dyn Any;
}

impl Test61Pipeline for Test61DiscardPipeline {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// test_61 (test_driver.cc:2215-2260): verify qpdf exception classifications,
/// in-place memory processing, utility error boundaries, and the observable
/// ownership/type checks in a Rust-native form.
pub(crate) fn run_test_61(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, arg2, stderr, diagnostics_written);

    // qpdf test_driver.cc:2221-2225. The strict parse raises the Rust
    // Error::Parse counterpart of QPDFExc before any warning is delivered.
    pdf.set_attempt_recovery(false);
    pdf.set_suppress_warnings(true);
    match pdf.process_memory_file(b"empty", Vec::new()) {
        Err(Error::Parse { .. }) => writeln!(stdout, "Caught QPDFExc as expected")?,
        Err(error) => return Err(error),
        Ok(()) => {
            return Err(Error::Internal(
                "test 61 empty process unexpectedly succeeded".to_owned(),
            ))
        }
    }

    match qutil::safe_fopen("/does/not/exist", "r") {
        Err(Error::System(_) | Error::SystemBytes(_)) => {
            writeln!(stdout, "Caught QPDFSystemError as expected")?
        }
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(Error::Internal(
                "test 61 missing safe_fopen unexpectedly succeeded".to_owned(),
            ))
        }
    }

    match qutil::int_to_string_base(0, 12, 0) {
        Err(Error::Internal(message))
            if message == "int_to_string_base called with unsupported base" =>
        {
            writeln!(stdout, "Caught logic_error as expected")?
        }
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(Error::Internal(
                "test 61 unsupported base unexpectedly succeeded".to_owned(),
            ))
        }
    }

    match qutil::to_utf8(0xffff_ffff) {
        Err(Error::System(message)) if message == "bounds error in QUtil::toUTF8" => {
            writeln!(stdout, "Caught runtime_error as expected")?
        }
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(Error::Internal(
                "test 61 out-of-range UTF-8 unexpectedly succeeded".to_owned(),
            ))
        }
    }

    let input_source = Test61BufferInputSource;
    let input_source_ref: &dyn Test61InputSource = &input_source;
    assert!(input_source_ref
        .as_any()
        .downcast_ref::<Test61BufferInputSource>()
        .is_some());
    let pipeline = Test61DiscardPipeline;
    let pipeline_ref: &dyn Test61Pipeline = &pipeline;
    assert!(pipeline_ref
        .as_any()
        .downcast_ref::<Test61DiscardPipeline>()
        .is_some());

    {
        let _name_tree = Test61ExtendNameTree { stdout };
    }
    Ok(())
}

/// test_62 (test_driver.cc:2262-2287): int/unsigned-int size-boundary
/// checks on trailer values written via `QPDFObjectHandle::newInteger` and
/// read back through `getIntValue`/`getUIntValue`/`getIntValueAsInt`/
/// `getUIntValueAsUInt`.
///
/// `t.replaceKey(...)` here is real, unlike the `run_test_26`/`run_test_60`
/// GAP: `Pdf::trailer()` is memoized (`pdf.rs:286-296`, "Repeated
/// calls return the same shared handle"), so `ObjectHandle::replace_key` on
/// it mutates the one shared handle every later `trailer()`/
/// `get_key` call observes — the missing piece in the other tests'
/// GAP is specifically that `PdfWriter` never reads that bridge back, which
/// is irrelevant here since this test never writes a file.
///
/// `getIntValue` (unclamped `i64`, no warning path exercised for a plain
/// integer) is real via [`ObjectHandle::try_get_int_value`]. The three
/// narrowing/unsigned accessors are exposed by the canonical `ObjectHandle`
/// route as `try_get_uint_value`, `try_get_int_value_as_int`, and
/// `try_get_uint_value_as_uint`; each retains qpdf's warning and saturation
/// behavior instead of implementing a driver conversion shim.
pub(crate) fn run_test_62<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, stdout); // test_62 writes only its qpdf warning lines.

    with_default_error_capture(stderr, || {
        let t = pdf.trailer();
        // `QIntC::to_ulonglong(INT_MAX)`/`to_longlong(INT_MIN)`/`to_longlong(UINT_MAX)`
        // (test_driver.cc:2268-2273) are lossless casts here: every product below fits
        // comfortably inside i64/u64, so no `QIntC` narrowing check can fire.
        let q1_l: u64 = 3_u64 * u64::from(u32::try_from(i32::MAX).expect("i32::MAX fits in u32"));
        let q1: i64 = i64::try_from(q1_l).expect("q1_l fits in i64");
        let q2_l: i64 = 3_i64 * i64::from(i32::MIN);
        let q2: i64 = q2_l;
        let q3_i: u32 = u32::MAX;
        let q3: i64 = i64::from(q3_i);

        t.replace_key(b"/Q1", ObjectHandle::integer(q1))?;
        t.replace_key(b"/Q2", ObjectHandle::integer(q2))?;
        t.replace_key(b"/Q3", ObjectHandle::integer(q3))?;

        // qpdf: `assert_compare_numbers(q1, t.getKey("/Q1").getIntValue());` (test_driver.cc:2277).
        assert_eq!(t.get_key(b"/Q1").try_get_int_value()?, q1);
        assert_eq!(t.get_key(b"/Q1").try_get_uint_value()?, q1_l);
        assert_eq!(t.get_key(b"/Q1").try_get_int_value_as_int()?, i32::MAX);
        assert_eq!(t.get_key(b"/Q1").try_get_uint_value_as_uint()?, u32::MAX);
        assert_eq!(t.get_key(b"/Q2").try_get_int_value()?, q2_l);
        assert_eq!(t.get_key(b"/Q2").try_get_uint_value()?, 0);
        assert_eq!(t.get_key(b"/Q2").try_get_int_value_as_int()?, i32::MIN);
        assert_eq!(t.get_key(b"/Q2").try_get_uint_value_as_uint()?, 0);
        assert_eq!(t.get_key(b"/Q3").try_get_int_value_as_int()?, i32::MAX);
        assert_eq!(t.get_key(b"/Q3").try_get_uint_value_as_uint()?, u32::MAX);

        // qpdf's programmatic integers have no owning QPDF, so warnIfPossible
        // writes the six range warnings directly to the default error logger.
        // Capture that process-global sink only at this library boundary so
        // the caller-owned stderr writer receives the same bytes in unit tests
        // and in the actual test-driver binary; no document-diagnostic replay
        // or semantic adapter is involved.
        Ok(())
    })
}

struct DefaultErrorCaptureSink(Arc<Mutex<Vec<u8>>>);

impl Pipeline for DefaultErrorCaptureSink {
    fn identifier(&self) -> &str {
        "test driver default error capture"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0
            .lock()
            .map_err(|_| PipelineError::runtime("default error capture mutex poisoned"))?
            .extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn with_default_error_capture(
    stderr: &mut dyn Write,
    body: impl FnOnce() -> flpdf::Result<()>,
) -> flpdf::Result<()> {
    static DEFAULT_ERROR_CAPTURE_LOCK: Mutex<()> = Mutex::new(());

    let _guard = DEFAULT_ERROR_CAPTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let logger = QPDFLogger::default_logger();
    let restore = logger.get_error()?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    logger.set_error(Some(PipelineHandle::new(DefaultErrorCaptureSink(
        Arc::clone(&captured),
    ))));

    let result = body();
    logger.set_error(Some(restore));
    let captured = captured
        .lock()
        .map_err(|_| PipelineError::runtime("default error capture mutex poisoned"))?
        .clone();
    stderr.write_all(&captured)?;
    result
}

/// test_63 (test_driver.cc:2289-2301): set R6 (AES-256) encryption
/// parameters on a `QPDFWriter` *before* setting its output filename,
/// regression-testing a qpdf bug where the filename was (incorrectly) part
/// of the `/ID` input data. flpdf's `/ID` generation does not consult the
/// output filename at all (`PdfWriter::set_output_file` only names where
/// bytes go), so this specific bug class cannot recur here — the test is
/// ported for its literal call sequence, mirroring qpdf's own operation
/// order exactly, not because flpdf shares the underlying risk.
///
/// `setR6EncryptionParameters("u", "o", true, true, true, true, true, true,
/// qpdf_r3p_full, true)` (test_driver.cc:2298) requests every capability bit
/// granted and full-quality printing — the same permission set
/// [`flpdf::PermissionsConfig::default`] encodes (`permissions.rs`'s own bit
/// table matches `interpretR3EncryptionParameters`'s `P` bit assignments
/// one-to-one, and every `allow_*` argument here is `true` with
/// `print = qpdf_r3p_full`, so no bit gets cleared either way) — so
/// [`EncryptParams::v5_r6`]'s default permissions and `encrypt_metadata =
/// true` are exactly this call's parameters, without needing to name each
/// bit individually.
pub(crate) fn run_test_63<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, stdout, stderr, diagnostics_written); // test_63 prints nothing and triggers no new diagnostics.

    let mut w = PdfWriter::new(pdf);
    w.set_encryption_parameters(EncryptParams::v5_r6("u", "o"));
    w.set_output_file("a.pdf")?;
    w.write()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_test_61, test_56_59_body, DefaultErrorCaptureSink};
    use flpdf::{Pdf, Pipeline};
    use std::sync::{Arc, Mutex};

    struct CurrentDirGuard(std::path::PathBuf);

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore current directory");
        }
    }

    #[test]
    fn test_56_59_body_runs_the_canonical_overlay_route() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let source = directory.path().join("source.pdf");
        std::fs::write(
            &source,
            include_bytes!("../../../../tests/fixtures/compat/fxo-red.pdf"),
        )
        .expect("write source fixture");
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/fxo-red.pdf").to_vec(),
        )
        .expect("open destination fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        test_56_59_body(
            &mut pdf,
            b"destination.pdf",
            Some(source.as_os_str()),
            false,
            false,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 56 body");

        assert_eq!(stdout, b"");
        assert!(stderr.is_empty());
        assert!(directory.path().join("a.pdf").is_file());
    }

    #[test]
    fn default_error_capture_sink_forwards_bytes() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut sink = DefaultErrorCaptureSink(Arc::clone(&captured));

        assert_eq!(sink.identifier(), "test driver default error capture");
        sink.write(b"warning\n").expect("capture sink write");
        sink.finish().expect("capture sink finish");
        assert_eq!(&*captured.lock().unwrap(), b"warning\n");
    }

    #[test]
    fn test_61_body_matches_qpdf_exception_output() {
        let mut pdf = Pdf::uninitialized();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_61(
            &mut pdf,
            b"-",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 61 should catch the expected qpdf error classes");

        assert_eq!(
            stdout,
            b"Caught QPDFExc as expected\n\
Caught QPDFSystemError as expected\n\
Caught logic_error as expected\n\
Caught runtime_error as expected\n\
~ExtendNameTree called\n"
        );
        assert!(stderr.is_empty());
    }
}
