//! Ports qpdf's `test_34` through `test_41` (`qpdf/test_driver.cc:1252-1404`
//! in pinned qpdf 11.9.0).
//!
//! House style, shared helpers (`resolve_chain`, `write_object`,
//! `write_qpdf_object`), and the repair-diagnostics threading convention
//! all follow `driver/test_0_1.rs` -- read that file first.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::pipeline::{FlateAction, PlFlate};
use flpdf::{
    DecodeLevel, ObjectHandle, PageDocumentHelper, PageObjectHelper, Pdf, PdfWriter, Pipeline,
    PipelineResult, TokenFilter, TokenFilterOutput,
};

use super::emit_new_diagnostics;
use crate::output::write_bytes;

// ---------------------------------------------------------------------------
// Shared helpers
//
// qpdf's object accessors dereference their receiver (`QPDFObjectHandle::dereference`,
// `libqpdf/QPDFObjectHandle.cc:2376-2383`). Translate each operation through the
// corresponding fallible `ObjectHandle::try_*` accessor and drain diagnostics at
// that operation boundary; do not route resolution through the qpdf-less
// `Pdf::resolve` compatibility facade.
// ---------------------------------------------------------------------------

/// Drain warnings produced by one qpdf-shaped accessor before the next
/// operation, matching qpdf's synchronous `QPDF::warn` logger.
fn after_qpdf_call<'a, T, R: Read + Seek>(
    pdf: &'a Pdf<R>,
    filename: &'a [u8],
    diagnostics_written: &'a mut usize,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
) -> impl FnOnce(flpdf::Result<T>) -> flpdf::Result<T> + 'a {
    move |result| {
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        result
    }
}

/// `QPDFObjectHandle::getKey` (`libqpdf/QPDFObjectHandle.cc:979-988`): resolve
/// the receiver and return the child without resolving it yet.
#[allow(clippy::too_many_arguments)]
fn qpdf_get_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<ObjectHandle> {
    let result = handle.try_get_key(key);
    after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(result)
}

/// `QUtil::hex_encode` (`libqpdf/QUtil.cc:720-731`): lowercase hex, two
/// characters per byte, no separators.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// test_34 (`test_driver.cc:1251-1263`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_34<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    writeln!(stdout, "version: {}", pdf.version())?;

    // qpdf evaluates getExtensionLevel first, then independently evaluates
    // getRoot().getKey("/Extensions").unparse(), and finally calls
    // getVersionAsPDFVersion (`qpdf/test_driver.cc:1252-1263`). Keep those
    // observable boundaries in that order while delegating each responsibility
    // to the production Pdf API. Drain diagnostics before the corresponding
    // output line because qpdf's warning logger is synchronous.
    // qpdf's warning logger is synchronous, so a call that records a repair
    // warning and *then* fails must still have that warning printed first.
    // Hold the result, drain diagnostics, and only then propagate.
    let extension_level = pdf.get_extension_level();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let extension_level = extension_level?;
    writeln!(stdout, "extension level: {extension_level}")?;

    // `getKey("/Extensions").unparse()` never dereferences: an indirect
    // result always prints its own `N G R` regardless of resolution state,
    // and a direct result's own nested children print the same way
    // (`ObjectHandle::unparse`'s own doc) -- no extra resolve step here.
    let root = pdf.root_handle();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let root = root?;
    let extensions = root.try_get_key(b"/Extensions");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let extensions = extensions?;
    write_bytes(stdout, &extensions.unparse())?;
    writeln!(stdout)?;

    // `getVersionAsPDFVersion` calls getExtensionLevel again, as qpdf does;
    // the canonical resolver cache prevents already-observed warnings from
    // being emitted a second time.
    let version = pdf.get_version_as_pdf_version();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let version = version?;
    writeln!(
        stdout,
        "As PDFVersion: {}.{}/{extension_level}",
        version.major(),
        version.minor()
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_35 / test_36 (`test_driver.cc:1265-1338`)
//
// qpdf itself writes these as two fully independent functions -- no shared
// private helper, unlike e.g. its own `test_56_59` -- so `run_test_35` and
// `run_test_36` below stay two separate walks with the array/loop structure
// duplicated between them, matching that. `matching_filespec_ef_f_stream`
// below *is* a shared helper, but it factors out only the one five-clause
// boolean predicate the two loop bodies both evaluate byte-for-byte
// identically (`item.isDictionary() && ... .isStream()`, present verbatim
// in both `test_driver.cc:1277-1279` and `:1323-1325`) -- a "same order, no
// behavior change" consolidation (CLAUDE.md's (B) class of deviation, not a
// restructuring of qpdf's own two-function shape).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FlateBuffer {
    bytes: Vec<u8>,
}

impl Pipeline for FlateBuffer {
    fn identifier(&self) -> &str {
        "buffer"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

/// Pipe raw bytes through qpdf's direct `Pl_Flate(a_inflate)` stage.
///
/// The qpdf test-driver deliberately passes `qpdf_dl_none` to
/// `pipeStreamData` and supplies a bare codec, so the stream's own filter
/// dictionary and decode parameters are not consulted. This helper keeps that
/// raw `write`/`finish` pipeline boundary instead of constructing a synthetic
/// dictionary for a whole-buffer compatibility decoder.
fn inflate_with_pipeline(raw: &[u8]) -> flpdf::Result<Vec<u8>> {
    let mut sink = FlateBuffer::default();
    {
        let mut flate = PlFlate::new("compress", &mut sink, FlateAction::Inflate)?;
        flate.write(raw)?;
        flate.finish()?;
    }
    Ok(sink.bytes)
}

/// `item.isDictionary() && item.getKey("/Type").isName() &&
/// (item.getKey("/Type").getName() == "/Filespec") &&
/// item.getKey("/EF").isDictionary() && item.getKey("/EF").getKey("/F").isStream()`
/// (`test_driver.cc:1277-1279`, `1323-1325`), returning the resolved `/EF /F`
/// stream handle on a match. `try_get_name` returns qpdf's slash-prefixed
/// name spelling, so compare with `b"/Filespec"`.
#[allow(clippy::too_many_arguments)]
fn matching_filespec_ef_f_stream<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    item: &ObjectHandle,
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Option<ObjectHandle>> {
    let item_is_dictionary = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        item.try_is_dictionary(),
    )?;
    if !item_is_dictionary {
        return Ok(None);
    }
    let type_value = qpdf_get_key(
        pdf,
        item,
        b"/Type",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let type_is_name = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        type_value.try_is_name(),
    )?;
    if !type_is_name {
        return Ok(None);
    }
    let type_name = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        type_value.try_get_name(),
    )?;
    if type_name.as_slice() != b"/Filespec" {
        return Ok(None);
    }
    let ef = qpdf_get_key(
        pdf,
        item,
        b"/EF",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let ef_is_dictionary = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        ef.try_is_dictionary(),
    )?;
    if !ef_is_dictionary {
        return Ok(None);
    }
    let ef_f = qpdf_get_key(
        pdf,
        &ef,
        b"/F",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let ef_f_is_stream = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        ef_f.try_is_stream_of_type(b"", b""),
    )?;
    if !ef_f_is_stream {
        return Ok(None);
    }
    Ok(Some(ef_f))
}

pub(crate) fn run_test_35<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root_result = pdf.root_handle();
    let root = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(root_result)?;
    let names = qpdf_get_key(
        pdf,
        &root,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let embedded_files = qpdf_get_key(
        pdf,
        &names,
        b"/EmbeddedFiles",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let names = qpdf_get_key(
        pdf,
        &embedded_files,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;

    // qpdf collects into `std::map<std::string, shared_ptr<Buffer>>`
    // (`test_driver.cc:1270`), so the print loop below iterates in ascending
    // filename byte order regardless of array position, and a repeated
    // filename keeps only the *last* array entry that produced it (plain
    // `operator[]` assignment, `test_driver.cc:1282`) -- both of which
    // `BTreeMap::insert` reproduces directly.
    let mut attachments: BTreeMap<Vec<u8>, Rc<Vec<u8>>> = BTreeMap::new();
    let array_count = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        names.try_get_array_n_items(),
    )?;
    for index in 0..array_count {
        let item = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            names.try_get_array_item(i64::try_from(index).unwrap_or(i64::MAX)),
        )?;
        let Some(ef_f) = matching_filespec_ef_f_stream(
            pdf,
            &item,
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?
        else {
            continue;
        };
        let filename_handle = qpdf_get_key(
            pdf,
            &item,
            b"/F",
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?;
        let filename_value = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            filename_handle.try_get_string_value(),
        )?;
        let data = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            ef_f.get_stream_data(DecodeLevel::Generalized),
        )?;
        attachments.insert(filename_value, data);
    }

    for (attachment_name, data) in attachments {
        write_bytes(stdout, &attachment_name)?;
        stdout.write_all(b":\n")?;
        // qpdf's `data.at(i) < 0` reads a `char` as signed (the typical
        // platform default this crate targets), so combined with
        // `data.at(i) > 126` the condition is exactly "byte outside
        // 0..=126" -- i.e. byte >= 127 read as `u8` (`test_driver.cc:1290-1295`).
        let is_binary = data.iter().any(|&byte| byte > 126);
        if is_binary {
            let mut summary = Vec::new();
            for &byte in data.iter().take(20) {
                if (32..=126).contains(&byte) {
                    summary.push(byte);
                } else {
                    summary.push(b'.');
                }
            }
            summary.extend_from_slice(format!(" ({} bytes)", data.len()).as_bytes());
            write_bytes(stdout, &summary)?;
        } else {
            write_bytes(stdout, &data)?;
        }
        stdout.write_all(b"--END--\n")?;
    }
    Ok(())
}

pub(crate) fn run_test_36<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root_result = pdf.root_handle();
    let root = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(root_result)?;
    let names = qpdf_get_key(
        pdf,
        &root,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let embedded_files = qpdf_get_key(
        pdf,
        &names,
        b"/EmbeddedFiles",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let names = qpdf_get_key(
        pdf,
        &embedded_files,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;

    let array_count = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
        names.try_get_array_n_items(),
    )?;
    for index in 0..array_count {
        let item = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            names.try_get_array_item(i64::try_from(index).unwrap_or(i64::MAX)),
        )?;
        let Some(ef_f) = matching_filespec_ef_f_stream(
            pdf,
            &item,
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?
        else {
            continue;
        };
        let filename_handle = qpdf_get_key(
            pdf,
            &item,
            b"/F",
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?;
        let filename_value = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            filename_handle.try_get_string_value(),
        )?;
        if filename_value.as_slice() != b"attachment1.txt" {
            continue;
        }
        let attachment_name = filename_value;

        // `stream.pipeStreamData(&p2, 0, qpdf_dl_none)` pipes the raw,
        // undecoded stream bytes through a bare `Pl_Flate` inflate stage
        // (`test_driver.cc:1329-1331`), bypassing the object's own
        // `/DecodeParms` (in particular any predictor) entirely.
        // `get_raw_stream_data` is `QPDF_Stream::getRawStreamData`, the same
        // undecoded source `pipeStreamData(dl_none)` reads
        // (`ObjectHandle::get_raw_stream_data`'s own doc).
        let raw = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            ef_f.get_raw_stream_data(),
        )?;
        let data = inflate_with_pipeline(raw.as_ref())?;

        let dict_handle = after_qpdf_call(pdf, filename, diagnostics_written, stdout, stderr)(
            ef_f.try_get_stream_dict(),
        )?;
        write_bytes(stdout, &dict_handle.unparse())?;
        write_bytes(stdout, &attachment_name)?;
        stdout.write_all(b":\n")?;
        write_bytes(stdout, &data)?;
        stdout.write_all(b"--END--\n")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_37 (`test_driver.cc:1340-1348`, `ParserCallbacks` at `:98-134`)
// ---------------------------------------------------------------------------

struct ContentParserCallbacks<'a, R: Read + Seek + 'static> {
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
    pdf: &'a Pdf<R>,
    filename: &'a [u8],
    diagnostics_written: &'a mut usize,
}

impl<'a, R: Read + Seek + 'static> flpdf::ObjectHandleParserCallbacks
    for ContentParserCallbacks<'a, R>
{
    fn content_size(&mut self, size: usize) -> flpdf::Result<()> {
        writeln!(self.stdout, "content size: {size}")?;
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: ObjectHandle,
        offset: usize,
        length: usize,
    ) -> flpdf::Result<flpdf::ParseControl> {
        if object.as_name().as_deref() == Some(b"Abort".as_slice()) {
            writeln!(self.stdout, "test suite: terminating parsing")?;
            // `terminateParsing()` throws immediately
            // (`test_driver.cc:116-119`), so the type/offset/length line
            // below never executes for the `/Abort` token itself.
            return Ok(flpdf::ParseControl::Stop);
        }
        let type_name = object.type_name()?;
        write!(
            self.stdout,
            "{}, offset={offset}, length={length}: ",
            type_name
        )?;
        if object.type_code()? == 12 {
            // ot_inlineimage
            let value = object.as_inline_image().unwrap_or_default();
            writeln!(self.stdout, "{}", hex_encode(&value))?;
        } else {
            write_bytes(self.stdout, &object.unparse())?;
            writeln!(self.stdout)?;
        }
        Ok(flpdf::ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        emit_new_diagnostics(
            self.pdf,
            self.diagnostics_written,
            self.filename,
            self.stdout,
            self.stderr,
        )?;
        writeln!(self.stdout, "-EOF-")?;
        Ok(())
    }
}

pub(crate) fn run_test_37<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for page_ref in page_refs {
        let page = pdf.get_object_handle(page_ref);
        let result = {
            let mut callbacks = ContentParserCallbacks {
                stdout,
                stderr,
                pdf: &*pdf,
                filename,
                diagnostics_written,
            };
            page.parse_page_contents(&mut callbacks)
        };
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        result?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_38 (`test_driver.cc:1350-1358`) -- designed for override-compressed-object.pdf
// ---------------------------------------------------------------------------

pub(crate) fn run_test_38<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf resolves the Catalog, QTest array, each array item, and the final
    // unparse at their respective public accessor boundaries
    // (`qpdf/test_driver.cc:1351-1358`). Keep the same order with the
    // canonical ObjectHandle accessors instead of the driver-local resolution
    // helpers.
    let root = pdf.root_handle()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let qtest = root.try_get_key(b"/QTest")?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let count = qtest.try_get_array_n_items()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for index in 0..count {
        let item = qtest.try_get_array_item(index as i64)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        let rendered = item.try_unparse_resolved()?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        write_bytes(stdout, &rendered)?;
        writeln!(stdout)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_39 (`test_driver.cc:1360-1375`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_39<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for (index, page_ref) in page_refs.into_iter().enumerate() {
        writeln!(stdout, "page {}", index + 1)?;

        // qpdf's `getImages` owns the inherited resource/XObject walk and
        // returns direct image handles in resource-name order
        // (`libqpdf/QPDFPageObjectHelper.cc:318-383`). The canonical flpdf
        // helper has the same boundary; keep resolution at the stream-dict,
        // key, and unparseResolved accessors rather than rebuilding that walk
        // in the qtest consumer.
        let images = PageObjectHelper::new(page_ref, pdf).get_images()?;
        for (_key, image) in images {
            let dict = image.try_get_stream_dict()?;
            let filter = dict.try_get_key(b"/Filter")?.try_unparse_resolved()?;
            let color_space = dict.try_get_key(b"/ColorSpace")?.try_unparse_resolved()?;
            write!(stdout, "filter: ")?;
            write_bytes(stdout, &filter)?;
            write!(stdout, ", color space: ")?;
            write_bytes(stdout, &color_space)?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_40 (`test_driver.cc:1377-1389`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_40<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    assert!(
        arg2.is_some(),
        "test 40 requires arg2 (qpdf's own `assert(arg2 != nullptr)`, test_driver.cc:1384)"
    );
    let arg2 = arg2.expect("checked above");
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file(arg2)?;
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.write()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_41 (`test_driver.cc:1391-1404`, `TokenFilter` at `:136-157`)
// ---------------------------------------------------------------------------

/// Ports qpdf's own `TokenFilter` test class (`test_driver.cc:136-157`).
/// This crate's `flpdf::Token` (`libqpdf/QPDFTokenizer.hh`'s `Token`
/// equivalent) has no public constructor -- only its accompanying parser can
/// build one -- so the two synthetic replacement tokens qpdf's version
/// constructs (`Token(tt_string, "Salad")`, `Token(tt_name, "/bye")`) are
/// written here as their already-known canonical raw spellings instead of
/// being built through the (private) `Token` constructor. Both literals are
/// plain ASCII with no characters this crate's own canonical-raw rules
/// (`tokenizer.rs`'s `canonical_string_raw`/`canonical_name_raw`, which this
/// comment's byte sequences were checked against) would ever escape, so
/// `(Salad)` and `/bye` are exactly the bytes `writeToken` would emit for
/// them -- this is not a fabricated value, only a hand-computed one for a
/// fixed, already-known literal.
struct PotatoSaladTokenFilter;

impl TokenFilter for PotatoSaladTokenFilter {
    fn handle_token(
        &mut self,
        token: &flpdf::ContentToken,
        output: &mut TokenFilterOutput<'_>,
    ) -> flpdf::PipelineResult<()> {
        if token.token_type == flpdf::ContentTokenType::String && token.value == b"Potato" {
            output.write(b"(Salad)")?;
        } else {
            output.write_token(token)?;
        }
        Ok(())
    }

    fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> flpdf::PipelineResult<()> {
        output.write(b"/bye")?;
        output.write(b"\n")?;
        Ok(())
    }
}

pub(crate) fn run_test_41<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for page_ref in page_refs {
        let page = pdf.get_object_handle(page_ref);
        let filter: Rc<RefCell<dyn TokenFilter>> = Rc::new(RefCell::new(PotatoSaladTokenFilter));
        page.add_content_token_filter(filter)?;
    }
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.write()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        inflate_with_pipeline, qpdf_get_key, run_test_34, run_test_37, run_test_38, run_test_39,
    };
    use flpdf::{Pdf, PdfOpenOptions};

    #[test]
    fn test_36_inflate_stage_matches_qpdf_pipeline_output() {
        let compressed = [
            0x78, 0x9c, 0xcb, 0x4b, 0x2d, 0x57, 0x48, 0x49, 0x2c, 0x49, 0x54, 0x48, 0xcb, 0x2f,
            0x52, 0x28, 0x2e, 0x29, 0x4a, 0x4d, 0xcc, 0xe5, 0x02, 0x00, 0x4c, 0xc9, 0x07, 0x22,
        ];
        assert_eq!(
            inflate_with_pipeline(&compressed).expect("qpdf-shaped Pl_Flate inflate stage"),
            b"new data for stream\n"
        );
    }

    fn pdf_with_image_xobject() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources 4 0 R /Contents 6 0 R >>",
            ),
            (4, b"<< /XObject << /Im1 5 0 R >> >>"),
            (
                5,
                b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\n\0\0\0\nendstream",
            ),
            (6, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = [0usize; 7];
        for &(number, body) in objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    fn pdf_with_content_stream(data: &[u8]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let objects = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 612 792] >>"
                    .as_slice(),
            ),
        ];
        let mut offsets = [0usize; 5];
        for &(number, body) in &objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        offsets[4] = bytes.len();
        bytes.extend_from_slice(b"4 0 obj\n<< /Length ");
        bytes.extend_from_slice(data.len().to_string().as_bytes());
        bytes.extend_from_slice(b" >>\nstream\n");
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    fn pdf_with_lazy_qtest_item() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let objects = [
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /QTest [4 0 R] >>".as_slice(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".as_slice(),
            ),
        ];
        let mut offsets = [0usize; 5];
        for &(number, body) in &objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        offsets[4] = bytes.len();
        bytes.extend_from_slice(b"4 0 obj\n42\nendobx\n");

        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    fn pdf_with_large_extension_level() -> Vec<u8> {
        let objects = [
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>".as_slice(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".as_slice(),
            ),
            (4, b"<< /ADBE 5 0 R >>".as_slice()),
            (
                5,
                b"<< /BaseVersion /1.7 /ExtensionLevel 5000000000 >>".as_slice(),
            ),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = [0usize; 6];
        for &(number, body) in &objects {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn qpdf_get_key_uses_the_canonical_accessor_and_keeps_child_lazy() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions::default(),
        )
        .expect("open minimal fixture");
        let root = pdf.root_handle().expect("resolve catalog root");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        let child = qpdf_get_key(
            &mut pdf,
            &root,
            b"/Pages",
            b"minimal.pdf",
            &mut diagnostics_written,
            &mut stdout,
            &mut stderr,
        )
        .expect("read qpdf getKey child");

        assert!(root.is_resolved());
        assert!(child.is_indirect());
        assert!(
            !child.is_resolved(),
            "qpdf getKey resolves its receiver but leaves its returned child lazy"
        );
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn image_enumeration_resolves_resources_and_image_children_once() {
        let mut pdf =
            Pdf::open_mem_owned_with_options(pdf_with_image_xobject(), PdfOpenOptions::default())
                .expect("open image fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_39(
            &mut pdf,
            b"image.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 39");

        assert_eq!(stdout, b"page 1\nfilter: null, color space: /DeviceRGB\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_38_flushes_warnings_from_lazy_array_item_resolution() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_lazy_qtest_item(),
            PdfOpenOptions {
                description: b"lazy-item.pdf".to_vec(),
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open lazy QTest fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_38(
            &mut pdf,
            b"lazy-item.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 38");

        assert_eq!(stdout, b"42\n");
        assert_eq!(
            stderr,
            b"WARNING: lazy-item.pdf (object 4 0, offset 212): expected endobj\n"
        );
    }

    #[test]
    fn test_34_emits_qpdf_extension_level_clamp_warning() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_large_extension_level(),
            PdfOpenOptions {
                description: b"extension-level.pdf".to_vec(),
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open extension-level fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_34(
            &mut pdf,
            b"extension-level.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 34");

        assert_eq!(
            stdout,
            b"version: 1.7\n\
extension level: 2147483647\n\
4 0 R\n\
As PDFVersion: 1.7/2147483647\n"
        );
        assert!(!stderr.is_empty(), "qpdf emits the integer clamp warning");
        assert!(
            String::from_utf8_lossy(&stderr)
                .contains("requested value of integer is too big; returning INT_MAX"),
            "stderr must retain qpdf's clamp warning: {:?}",
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn test_37_inline_image_eof_keeps_qpdf_stream_context_and_end_offset() {
        let content = b"BI /W 1 ID \0";
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_content_stream(content),
            PdfOpenOptions::default(),
        )
        .expect("open inline-image EOF fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_37(
            &mut pdf,
            b"inline-image-eof.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 37");

        assert_eq!(
            stderr,
            format!(
                "WARNING: page object 3 0 stream 4 0 (stream data, offset {}): \
                 EOF found while reading inline image\n",
                content.len()
            )
            .as_bytes()
        );
    }
}
