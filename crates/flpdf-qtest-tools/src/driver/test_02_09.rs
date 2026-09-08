use std::cell::Cell;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::pipeline::{FlateAction, PlFlate};
use flpdf::{
    DecodeLevel, Error, ObjectHandle, PageDocumentHelper, PageObjectHelper, Pdf, PdfWriter,
    Pipeline, PipelineError, PipelineResult, StreamDataMode, StreamDataProvider,
    STREAM_ENCODE_NORMALIZE,
};

use crate::driver::emit_new_diagnostics;
use crate::output::write_bytes;

// This file ports qpdf's `test_2` through `test_9` (`qpdf/test_driver.cc:287-519`).
//
// `ObjectHandle::get_key` never resolves its receiver -- it returns a
// direct null handle for a not-yet-resolved indirect handle, the same as
// for a genuinely missing key (`ObjectHandle::get_key`'s own doc). qpdf's
// `QPDFObjectHandle` methods, by contrast, all call `dereference()` on
// entry (`libqpdf/QPDFObjectHandle.cc`'s accessor bodies), so a chain like
// `trailer.getKey("/Info").getKey("/CreationDate")` transparently
// dereferences at every hop. `ObjectHandle::try_get_key` (`pub`) already
// resolves its own receiver before reading `key` off it, so dictionary-key
// chases below call it directly instead of through a local wrapper.
//
// qpdf's own type accessors for a handle's *value* split into two publicly
// reachable families: a silent, non-throwing "as" family used only from
// C++-internal call sites (`asInteger`/`asDictionary`/`asArray`/`asName`/
// `asReal`/`asString`; all `private` in `include/qpdf/QPDFObjectHandle.hh`,
// directly under its `private:` label) and a warning-emitting "get value"
// family that IS public and is what qpdf's own `test_driver.cc` actually
// calls (`getStringValue`/`getUTF8Value`/`getIntValue`/`getNumericValue`/
// `getArrayItem`/`getArrayNItems`, `libqpdf/QPDFObjectHandle.cc`). The
// second family's flpdf port -- `try_get_string_value`/`try_get_utf8_value`/
// `try_get_int_value`/`try_get_numeric_value`/`try_get_array_item`/
// `try_get_array_as_vector` (`crates/flpdf/src/object_handle.rs`) -- is
// already `pub` and already resolves its own receiver, so this file calls
// those directly wherever qpdf's test driver calls their qpdf counterpart.
// `resolve_handle` below is still needed for the handful of sites that read
// a handle through an accessor with no warning-emitting counterpart at all
// (`unparse`, `type_code`, `pipe_stream_data`) or that merely gate on a
// non-throwing `isX()`-shaped check (`is_null`), where qpdf's own
// dereference-before-use has no separate warning to reproduce.
//
// `Pdf::resolve`'s underlying `ObjectHandle::try_dereference`
// is a documented no-op for an already-direct or already-resolved handle,
// so calling `resolve_handle` on one that happens to be resolved already
// (for example, one returned by `PageDocumentHelper::get_all_pages`, whose
// own repair walk may have already touched it) costs nothing.

fn resolve_handle<R: Read + Seek>(pdf: &mut Pdf<R>, handle: &ObjectHandle) -> flpdf::Result<()> {
    pdf.resolve(handle)
}

/// qpdf source: `qpdf/test_driver.cc:286-308` (`test_2`).
///
/// "Encrypted file. This test case is designed for a specific PDF file."
/// (qpdf's own comment) -- every key read below is assumed present with the
/// expected type, matching that guarantee. A real type mismatch would hit
/// qpdf's `typeWarning` + documented-default fallback
/// (`libqpdf/QPDFObjectHandle.cc:2169-2189`), which the `try_get_*_value`/
/// `try_get_array_item` calls below reproduce directly (see this file's
/// top-level caveats for the two sites, `/O` and `/U`, that call `unparse`
/// instead and so have no warning-emitting counterpart to reach).
pub(crate) fn run_test_2<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();

    let info = trailer.try_get_key(b"/Info")?;
    let creation_date = info.try_get_key(b"/CreationDate")?;
    let creation_date_value = creation_date.try_get_string_value()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    write_bytes(stdout, &creation_date_value)?;
    writeln!(stdout)?;

    let producer = info.try_get_key(b"/Producer")?;
    let producer_value = producer.try_get_string_value()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    write_bytes(stdout, &producer_value)?;
    writeln!(stdout)?;

    let encrypt = trailer.try_get_key(b"/Encrypt")?;
    let o = encrypt.try_get_key(b"/O")?;
    resolve_handle(pdf, &o)?;
    // qpdf delivers a lazy-resolution warning the instant `warn()` records it
    // (`libqpdf/QPDF.cc:487-494`), so it belongs before this value's own line
    // rather than after both encrypted-string lines.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    write_bytes(stdout, &o.unparse())?;
    writeln!(stdout)?;
    let u = encrypt.try_get_key(b"/U")?;
    resolve_handle(pdf, &u)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    write_bytes(stdout, &u.unparse())?;
    writeln!(stdout)?;

    let root = trailer.try_get_key(b"/Root")?;
    let pages = root.try_get_key(b"/Pages")?;
    let kids = pages.try_get_key(b"/Kids")?;
    let page = kids.try_get_array_item(1)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    let contents = page.try_get_key(b"/Contents")?;
    resolve_handle(pdf, &contents)?;
    let data = contents.get_stream_data(DecodeLevel::Generalized)?;
    write_bytes(stdout, &data)?;
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:310-322` (`test_3`).
///
/// qpdf source: `qpdf/test_driver.cc:311-322` (`test_3`).
///
/// qpdf flushes each stream header, then pipes the corresponding `/QStreams`
/// member through `qpdf_ef_normalize` and `qpdf_dl_generalized`. The canonical
/// ObjectHandle pipe owns both the decode chain and the ContentNormalizer; the
/// driver only supplies qpdf's output pipeline and drains diagnostics after the
/// pipe returns so warning bytes appear after the already-written stream data.
pub(crate) fn run_test_3<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let streams = trailer.try_get_key(b"/QStreams")?;
    let items = streams.try_get_array_as_vector()?;
    // qpdf's `getArrayNItems()` (`libqpdf/QPDFObjectHandle.cc:758-767`) warns
    // for a non-array receiver before any stream output, matching this
    // drain's position ahead of the per-stream loop below.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    for (index, stream) in items.iter().enumerate() {
        writeln!(stdout, "-- stream {index} --")?;
        stdout.flush()?;
        {
            let mut sink = StdoutPipeline { stdout };
            let mut filtering_attempted = false;
            let _ = stream.pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                STREAM_ENCODE_NORMALIZE,
                DecodeLevel::Generalized,
                false,
                false,
            )?; // cov:ignore: llvm-cov maps the tested pipeline-error continuation to this terminator
        }
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
            .map_err(Error::from)?;
    }
    Ok(())
}

/// A `Pl_StdioFile`-shaped pipeline over the test driver's injected stdout.
struct StdoutPipeline<'a> {
    stdout: &'a mut dyn Write,
}

impl Pipeline for StdoutPipeline<'_> {
    fn identifier(&self) -> &str {
        "tokenized stream"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.stdout
            .write_all(data)
            .map_err(|error| PipelineError::runtime(error.to_string()))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

/// qpdf source: `qpdf/test_driver.cc:324-372` (`test_4`).
///
/// `ObjectHandle::make_direct` is the canonical port of qpdf's recursive
/// `QPDFObjectHandle::makeDirect` (`libqpdf/QPDFObjectHandle.cc:2091-2160`).
/// The driver owns only qpdf's call order and writer configuration; graph
/// copying, cycle detection, stream stopping, and indirect promotion remain
/// in the core ObjectHandle/Pdf APIs.
pub(crate) fn run_test_4<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let mut qtest = trailer.try_get_key(b"/QTest")?;
    qtest.make_direct(false)?;
    qtest.remove_key(b"/Subject");
    qtest.replace_key(
        b"/Author",
        ObjectHandle::string(b"Mr. Potato Head".to_vec()),
    )?;

    let array = qtest.try_get_key(b"/A")?;
    let first_item = array.try_get_array_item(0)?.try_get_int_value()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    if first_item == 1 {
        array.set_array_item(1, ObjectHandle::integer(5))?;
        array.insert_array_item(2, ObjectHandle::integer(10))?;
        array.append_array_item(ObjectHandle::integer(12))?;
        array.erase_array_item(3)?;
        array.insert_array_item(4, ObjectHandle::integer(6))?;
        array.insert_array_item(0, ObjectHandle::integer(9))?;
    } else {
        array.set_array_items(vec![
            ObjectHandle::integer(14),
            ObjectHandle::integer(15),
            ObjectHandle::integer(9),
        ])?;
    }

    let mut qtest2 = trailer.try_get_key(b"/QTest2")?;
    // qpdf's `isNull()` dereferences before checking
    // (`libqpdf/QPDFObjectHandle.cc:240-249`); `try_get_key` above resolves
    // only its receiver, so `qtest2` itself is resolved explicitly here --
    // otherwise an unresolved indirect `/QTest2` would read as non-null
    // regardless of what it actually resolves to (`ObjectHandle::is_null`'s
    // own doc).
    resolve_handle(pdf, &qtest2)?;
    // A lazy resolution here can raise a recoverable repair warning. qpdf
    // delivers it the instant `warn()` records it (`libqpdf/QPDF.cc:487-494`),
    // so drain before the next output rather than letting it surface after a
    // later accessor's drain -- or, for `run_test_4`, never.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    if !qtest2.is_null() {
        qtest2.make_direct(true)?;
        trailer.replace_key(b"/QTest2", qtest2)?;
    }

    let info = pdf.make_indirect_from_object_handle(qtest)?;
    trailer.replace_key(b"/Info", info.clone())?;
    pdf.mark_object_handle_dirty(&info)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory()?;
    writer.write()?;
    write_bytes(stdout, &writer.get_buffer()?)?;
    Ok(())
}

/// Port of `QUtil::double_to_string(num, decimal_places, trim_trailing_zeroes)`
/// (`libqpdf/QUtil.cc:349-370`), restricted to `test_5`'s one call shape
/// (`decimal_places = 3`, `trim_trailing_zeroes = false`, so no trimming
/// branch is reachable). qpdf formats with `std::ostringstream` under
/// `std::fixed` + `std::setprecision(3)`; Rust's `{:.3}` float formatting
/// matches that digit-for-digit for finite values, modulo the exact
/// tie-breaking rule at the discarded digit (see this file's top-level
/// caveats).
fn double_to_string_3(value: f64) -> String {
    format!("{value:.3}")
}

/// qpdf source: `qpdf/test_driver.cc:374-420` (`test_5`).
///
/// `QPDFPageObjectHelper::getImages` is the canonical non-recursive image
/// enumeration route (`libqpdf/QPDFPageObjectHelper.cc:370-384`). The
/// `PageObjectHelper` call resolves each returned image through the same live
/// handle graph before the driver reads `/Width` and `/Height`, matching the
/// qpdf `getDict().getKey(...).getIntValue()` sequence.
pub(crate) fn run_test_5<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = {
        let mut helper = PageDocumentHelper::new(pdf);
        helper.get_all_pages()?
    };

    for (index, page_ref) in page_refs.iter().enumerate() {
        let pageno = index + 1;
        writeln!(stdout, "page {pageno}:")?;
        writeln!(stdout, "  images:")?;
        // Read the dimensions while the helper holds `pdf`, then release that
        // borrow before writing so the type warnings `try_get_int_value` can
        // raise are drained ahead of their own line. qpdf delivers a warning
        // the instant `warn()` records it (`libqpdf/QPDF.cc:487-494`), so a
        // deferred drain would print it after the line it belongs to -- or
        // after a later section's output entirely.
        // Collect the handles while the helper holds `pdf`, then release that
        // borrow so each image's own type warnings can drain immediately
        // before its line. qpdf processes the entries sequentially and
        // delivers a warning the instant `warn()` records it
        // (`libqpdf/QPDF.cc:487-494`), so a later image's warning must follow
        // the earlier images' lines, not precede all of them.
        let images = PageObjectHelper::new(*page_ref, pdf).get_images()?;
        for (name, image) in images {
            let image_dict = image
                .as_stream_dict()
                .expect("get_images only returns image stream handles");
            let width = image_dict.try_get_key(b"/Width")?.try_get_int_value()?;
            let height = image_dict.try_get_key(b"/Height")?.try_get_int_value()?;
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
                .map_err(Error::from)?;
            write!(stdout, "    ")?;
            write_bytes(stdout, &name)?;
            writeln!(stdout, ": {width} x {height}")?;
        }
        writeln!(stdout, "  content:")?;
        let mut page_helper = PageObjectHelper::new(*page_ref, pdf);
        let content = page_helper.get_page_contents()?;
        for item in &content {
            write!(stdout, "    ")?;
            write_bytes(stdout, &item.unparse())?;
            writeln!(stdout)?;
        }
        writeln!(stdout, "end page {pageno}")?;
    }

    let trailer = pdf.trailer();
    let root = trailer.try_get_key(b"/Root")?;

    let qstrings = root.try_get_key(b"/QStrings")?;
    resolve_handle(pdf, &qstrings)?;
    // The container's own resolution can warn; qpdf raises it during the
    // array check, before the section header, and it must still appear
    // when the array turns out to be empty.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    if let Some(items) = qstrings.as_array() {
        writeln!(stdout, "QStrings:")?;
        for item in items {
            let utf8 = item.try_get_utf8_value()?;
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
                .map_err(Error::from)?;
            write_bytes(stdout, &utf8)?;
            writeln!(stdout)?;
        }
    }

    let qnumbers = root.try_get_key(b"/QNumbers")?;
    resolve_handle(pdf, &qnumbers)?;
    // The container's own resolution can warn; qpdf raises it during the
    // array check, before the section header, and it must still appear
    // when the array turns out to be empty.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
        .map_err(Error::from)?;
    if let Some(items) = qnumbers.as_array() {
        writeln!(stdout, "QNumbers:")?;
        for item in items {
            let value = item.try_get_numeric_value()?;
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)
                .map_err(Error::from)?;
            writeln!(stdout, "{}", double_to_string_3(value))?;
        }
    }
    Ok(())
}

/// A `Pl_Buffer`-shaped [`Pipeline`] accumulator, for `run_test_6`'s direct
/// [`ObjectHandle::pipe_stream_data`] call below.
#[derive(Default)]
struct ByteSink {
    bytes: Vec<u8>,
}

impl Pipeline for ByteSink {
    fn identifier(&self) -> &str {
        "test 6 metadata stream"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

/// Pipe a payload through qpdf's direct `Pl_Flate(a_deflate)` stage.
///
/// `qpdf/test_driver.cc:test_8` constructs this stage itself instead of
/// asking a stream dictionary to encode a complete buffer. Keeping that
/// boundary here preserves the stage's write/finish calls and its exception
/// contract for the provider fixture.
fn deflate_with_pipeline(data: &[u8]) -> flpdf::Result<Vec<u8>> {
    let mut sink = ByteSink::default();
    {
        let mut flate = PlFlate::new("compress", &mut sink, FlateAction::Deflate)?;
        flate.write(data)?;
        flate.finish()?;
    }
    Ok(sink.bytes)
}

/// qpdf source: `qpdf/test_driver.cc:422-439` (`test_6`).
///
/// `metadata.pipeStreamData(&bufpl, 0, qpdf_dl_none)` decrypts (decode level
/// and decryption are independent in qpdf) but requests no content filter,
/// so `filtering_attempted` is unconditionally false and the overall-success
/// return is discarded (`test_driver.cc:431` reads neither). This is the
/// direct [`ObjectHandle::pipe_stream_data`] call, not
/// [`ObjectHandle::get_stream_data`]: `get_stream_data` mirrors qpdf's
/// `getStreamData`, which throws when `filtering_attempted` is false
/// (`libqpdf/QPDF_Stream.cc:345-359`) -- a call shape qpdf's own test suite
/// never uses with [`DecodeLevel::None`] for exactly that reason.
/// `get_raw_stream_data` would be the *wrong* substitution here since it
/// skips decryption, inverting this test's cleartext-vs-encrypted detection
/// on an encrypted-metadata fixture.
pub(crate) fn run_test_6<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let root = trailer.try_get_key(b"/Root")?;
    let metadata = root.try_get_key(b"/Metadata")?;
    resolve_handle(pdf, &metadata)?;
    if metadata.type_code()? != 10 {
        return Err(Error::Internal(
            "test 6 run on file with no metadata".to_string(),
        ));
    }
    let mut sink = ByteSink::default();
    let mut filtering_attempted = false;
    metadata.pipe_stream_data(
        &mut sink,
        &mut filtering_attempted,
        0,
        DecodeLevel::None,
        false,
        false,
    )?;
    let data = sink.bytes;
    let cleartext = data.starts_with(b"<?xpacket");
    writeln!(
        stdout,
        "encrypted={}; cleartext={}",
        u8::from(pdf.is_encrypted()),
        u8::from(cleartext)
    )?;
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:441-455` (`test_7`).
///
/// `QPDFObjectHandle::newNull()` passed as `filter`/`decode_parms` is an
/// *initialized*, direct null handle -- distinct from omitting the
/// argument -- and qpdf's `replaceStreamData` buffer overload installs
/// exactly the keys it is given
/// (`libqpdf/QPDFObjectHandle.cc:1345-1350`, `libqpdf/QPDF_Stream.cc:637-649`).
/// [`ObjectHandle::replace_stream_data`]'s `Some(ObjectHandle::null())`
/// reproduces that: its own `replace_key_unchecked` removes a key given an
/// explicit direct null value, matching qpdf's `/Filter`/`/DecodeParms`
/// removal for this call.
pub(crate) fn run_test_7<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let root = trailer.try_get_key(b"/Root")?;
    let qstream = root.try_get_key(b"/QStream")?;
    resolve_handle(pdf, &qstream)?;
    if qstream.type_code()? != 10 {
        return Err(Error::Internal(
            "test 7 run on file with no QStream".to_string(),
        ));
    }
    qstream.replace_stream_data(
        Rc::new(b"new data for stream\n".to_vec()),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );
    pdf.mark_object_handle_dirty(&qstream)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.write()?;
    Ok(())
}

/// Port of qpdf's local `Provider` class (`qpdf/test_driver.cc:67-96`), a
/// `QPDFObjectHandle::StreamDataProvider` that pipes a fixed buffer and,
/// when `bad_length` is set, one extra byte beyond it. `bad_length` is a
/// `Cell` (qpdf mutates the same `Provider*` instance in place after
/// registration via `provider->badLength(...)`, which flpdf's
/// `Rc<dyn StreamDataProvider>` registration also requires interior
/// mutability for).
struct LengthBugProvider {
    data: Rc<Vec<u8>>,
    bad_length: Cell<bool>,
}

impl LengthBugProvider {
    fn set_bad_length(&self, value: bool) {
        self.bad_length.set(value);
    }
}

impl StreamDataProvider for LengthBugProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        pipeline
            .write(&self.data)
            .map_err(|error| Error::System(error.to_string()))?;
        if self.bad_length.get() {
            pipeline
                .write(b" ")
                .map_err(|error| Error::System(error.to_string()))?;
        }
        pipeline
            .finish()
            .map_err(|error| Error::System(error.to_string()))?;
        Ok(())
    }
}

/// qpdf source: `qpdf/test_driver.cc:457-493` (`test_8`).
///
/// The "test 7" text in the no-`/QStream` error message below is qpdf's own
/// copy-paste typo from `test_7`, preserved verbatim.
pub(crate) fn run_test_8<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let root = trailer.try_get_key(b"/Root")?;
    let qstream = root.try_get_key(b"/QStream")?;
    resolve_handle(pdf, &qstream)?;
    if qstream.type_code()? != 10 {
        return Err(Error::Internal(
            "test 7 run on file with no QStream".to_string(),
        ));
    }

    let compressed = deflate_with_pipeline(b"new data for stream\n")?;
    let provider = Rc::new(LengthBugProvider {
        data: Rc::new(compressed),
        bad_length: Cell::new(false),
    });
    qstream.replace_stream_data_provider(
        provider.clone(),
        Some(ObjectHandle::name(b"FlateDecode".to_vec())),
        Some(ObjectHandle::null()),
    )?;
    provider.set_bad_length(false);
    pdf.mark_object_handle_dirty(&qstream)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    // Linearize to force the provider to be called multiple times.
    writer.set_linearization(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.write()?;

    // Every time a provider pipes stream data, it has to provide the same
    // amount of data.
    provider.set_bad_length(true);
    match qstream.get_stream_data(DecodeLevel::Generalized) {
        Ok(_) => writeln!(stdout, "oops -- getStreamData didn't throw")?,
        Err(error) => writeln!(stdout, "exception: {error}")?,
    }
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:495-519` (`test_9`).
///
/// qpdf's 20-byte fixed buffer holds exactly `"data for new stream\n"` (20
/// bytes with no NUL terminator, per the source's own "no null!" comment);
/// the literal byte string below is that same 20-byte payload.
pub(crate) fn run_test_9<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let root = trailer.try_get_key(b"/Root")?;
    resolve_handle(pdf, &root)?;

    let qstream = pdf.new_stream_with_data(Rc::new(b"data for new stream\n".to_vec()))?;
    let rstream = pdf.new_stream()?;

    match rstream.get_stream_data(DecodeLevel::Generalized) {
        Ok(_) => writeln!(stdout, "oops -- getStreamData didn't throw")?,
        Err(error) => writeln!(stdout, "exception: {error}")?,
    }

    rstream.replace_stream_data(
        Rc::new(b"data for other stream\n".to_vec()),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );
    pdf.mark_object_handle_dirty(&rstream)?;

    root.replace_key(b"/QStream", qstream)?;
    root.replace_key(b"/RStream", rstream)?;
    pdf.mark_object_handle_dirty(&root)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.write()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{deflate_with_pipeline, run_test_3, run_test_5, StdoutPipeline};
    use flpdf::{ObjectHandle, Pdf, Pipeline};
    use std::collections::BTreeMap;
    use std::io::{self, Write};
    use std::rc::Rc;

    struct FailAfterHeader;

    #[test]
    fn test_8_deflate_stage_matches_qpdf_pipeline_bytes() {
        let compressed = deflate_with_pipeline(b"new data for stream\n")
            .expect("qpdf-shaped Pl_Flate deflate stage");
        assert_eq!(
            compressed,
            [
                0x78, 0x9c, 0xcb, 0x4b, 0x2d, 0x57, 0x48, 0x49, 0x2c, 0x49, 0x54, 0x48, 0xcb, 0x2f,
                0x52, 0x28, 0x2e, 0x29, 0x4a, 0x4d, 0xcc, 0xe5, 0x02, 0x00, 0x4c, 0xc9, 0x07, 0x22,
            ]
        );
    }

    impl Write for FailAfterHeader {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if data.starts_with(b"-- stream") {
                Ok(data.len())
            } else {
                Err(io::Error::other("synthetic stream-output failure"))
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_3_pipes_and_normalizes_each_qstreams_member() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let stream = pdf
            .new_stream_with_data(Rc::new(b"A\rB".to_vec()))
            .expect("QStreams member");
        pdf.trailer()
            .replace_key(b"/QStreams", ObjectHandle::array(vec![stream]))
            .expect("install QStreams");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        run_test_3(
            &mut pdf,
            b"fixture.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test_3 should pipe QStreams");

        assert_eq!(stdout, b"-- stream 0 --\nA\nB");
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_3_propagates_stream_pipeline_failures() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let stream = pdf
            .new_stream_with_data(Rc::new(b"payload".to_vec()))
            .expect("QStreams member");
        pdf.trailer()
            .replace_key(b"/QStreams", ObjectHandle::array(vec![stream]))
            .expect("install QStreams");

        let mut stdout = FailAfterHeader;
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        assert!(run_test_3(
            &mut pdf,
            b"fixture.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .is_err());
        assert!(stdout.flush().is_ok());
    }

    /// A minimal valid PDF (empty catalog/pages tree) whose trailer carries
    /// `/QStreams 5` -- a direct, non-array value -- so opening it through
    /// the ordinary `Pdf::open_mem_owned` path gives the resulting handle a
    /// real document/logger context, matching how a genuine qpdf warning
    /// reaches its output stream (a bare `Pdf::empty()` trailer key has no
    /// such context and reports a contextless warning as an error instead).
    fn pdf_with_non_array_qstreams() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        let objects = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".as_slice()),
        ];
        for (number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        for number in 1..=2 {
            bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 3 /Root 1 0 R /QStreams 5 >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn test_3_warns_and_treats_a_non_array_qstreams_as_empty() {
        // qpdf's unguarded `getArrayNItems()` call at the top of `test_3`
        // (`qpdf/test_driver.cc:311`) warns and yields `0` for a non-array
        // receiver (`libqpdf/QPDFObjectHandle.cc:758-767`), producing zero
        // loop iterations rather than a crash or silent divergence.
        let mut pdf =
            Pdf::open_mem_owned(pdf_with_non_array_qstreams()).expect("open non-array fixture");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        run_test_3(
            &mut pdf,
            b"fixture.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("a non-array QStreams warns rather than failing");

        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            b"WARNING: operation for array attempted on object of type integer: treating as empty\n"
        );
    }

    #[test]
    fn stdout_pipeline_reports_qpdf_tokenized_identifier() {
        let mut stdout = Vec::new();
        let pipeline = StdoutPipeline {
            stdout: &mut stdout,
        };
        assert_eq!(pipeline.identifier(), "tokenized stream");
    }

    fn image_page_pdf() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        let objects = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources 4 0 R /Contents 6 0 R >>"
                    .as_slice(),
            ),
            (4, b"<< /XObject << /Im1 5 0 R >> >>".as_slice()),
            (
                5,
                b"<< /Type /XObject /Subtype /Image /Width 7 /Height 11 /Length 1 >>\nstream\nx\nendstream"
                    .as_slice(),
            ),
            (6, b"<< /Length 0 >>\nstream\n\nendstream".as_slice()),
        ];
        for (number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        for number in 1..=6 {
            bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn test_5_driver_emits_image_dimensions_from_canonical_page_helper() {
        let mut pdf = Pdf::open_mem_owned(image_page_pdf()).expect("open image page fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_5(
            &mut pdf,
            b"image-page.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 5 should enumerate the image");

        assert_eq!(
            stdout,
            b"page 1:\n  images:\n    /Im1: 7 x 11\n  content:\n    6 0 R\nend page 1\n"
        );
        assert!(stderr.is_empty());
    }
}
