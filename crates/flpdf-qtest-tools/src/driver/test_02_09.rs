use std::cell::Cell;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::{
    DecodeLevel, Error, ObjectHandle, PageDocumentHelper, Pdf, PdfWriter, Pipeline, PipelineError,
    PipelineResult, StreamDataMode, StreamDataProvider, STREAM_ENCODE_NORMALIZE,
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
// dereferences at every hop. `resolve_handle`/`dict_key` below restore that
// behavior explicitly: `dict_key` resolves its `handle` argument (the
// receiver) before reading `key` off it, mirroring qpdf's implicit
// dereference-before-use. `resolve_handle` alone is `dict_key`'s
// leaf-position twin, for a handle whose *own* value (not a further child)
// is about to be read or mutated.
//
// `Pdf::resolve`'s underlying `ObjectHandle::try_dereference`
// is a documented no-op for an already-direct or already-resolved handle,
// so calling either helper on a handle that happens to be resolved already
// (for example, one returned by `PageDocumentHelper::get_all_pages`, whose
// own repair walk may have already touched it) costs nothing.

fn resolve_handle<R: Read + Seek>(pdf: &mut Pdf<R>, handle: &ObjectHandle) -> flpdf::Result<()> {
    pdf.resolve(handle)
}

fn dict_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
) -> flpdf::Result<ObjectHandle> {
    resolve_handle(pdf, handle)?;
    Ok(handle.get_key(key))
}

/// qpdf source: `qpdf/test_driver.cc:286-308` (`test_2`).
///
/// "Encrypted file. This test case is designed for a specific PDF file."
/// (qpdf's own comment) -- every key read below is assumed present with the
/// expected type, matching that guarantee. A real type mismatch would hit
/// qpdf's `typeWarning` + documented-default fallback
/// (`libqpdf/QPDFObjectHandle.cc:2169-2189`); flpdf's equivalent `try_*`
/// accessor family that ports it (`type_warning`, `try_get_key`, ...) is
/// `pub(crate)`-only and unreachable from this crate, so this file uses the
/// plain, non-warning accessors and their own documented defaults
/// (`as_string` -> empty on mismatch) with no stderr warning text. See this
/// file's top-level caveats.
pub(crate) fn run_test_2<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();

    let info = dict_key(pdf, &trailer, b"/Info")?;
    let creation_date = dict_key(pdf, &info, b"/CreationDate")?;
    resolve_handle(pdf, &creation_date)?;
    write_bytes(stdout, &creation_date.as_string().unwrap_or_default())?;
    writeln!(stdout)?;

    let producer = dict_key(pdf, &info, b"/Producer")?;
    resolve_handle(pdf, &producer)?;
    write_bytes(stdout, &producer.as_string().unwrap_or_default())?;
    writeln!(stdout)?;

    let encrypt = dict_key(pdf, &trailer, b"/Encrypt")?;
    let o = dict_key(pdf, &encrypt, b"/O")?;
    resolve_handle(pdf, &o)?;
    write_bytes(stdout, &o.unparse())?;
    writeln!(stdout)?;
    let u = dict_key(pdf, &encrypt, b"/U")?;
    resolve_handle(pdf, &u)?;
    write_bytes(stdout, &u.unparse())?;
    writeln!(stdout)?;

    let root = dict_key(pdf, &trailer, b"/Root")?;
    let pages = dict_key(pdf, &root, b"/Pages")?;
    let kids = dict_key(pdf, &pages, b"/Kids")?;
    resolve_handle(pdf, &kids)?;
    // qpdf's `getArrayItem(1)` warns and returns null on an out-of-range
    // index (`libqpdf/QPDFObjectHandle.cc:762-777`); `.get(1)` below
    // silently defaults to a null handle for the same case, matching the
    // "returns null" half without the stderr warning (see this file's
    // top-level caveats).
    let page = kids
        .as_array()
        .and_then(|items| items.get(1).cloned())
        .unwrap_or_else(ObjectHandle::null);
    let contents = dict_key(pdf, &page, b"/Contents")?;
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
    let streams = dict_key(pdf, &trailer, b"/QStreams")?;
    resolve_handle(pdf, &streams)?;
    let items = streams.as_array().unwrap_or_default();
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
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    let mut qtest = trailer.get_key(b"/QTest");
    qtest.make_direct(false)?;
    qtest.remove_key(b"/Subject");
    qtest.replace_key(
        b"/Author",
        ObjectHandle::string(b"Mr. Potato Head".to_vec()),
    )?;

    let array = qtest.get_key(b"/A");
    if array
        .as_array()
        .and_then(|items| items.into_iter().next())
        .is_some_and(|item| item.as_integer() == Some(1))
    {
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

    let mut qtest2 = trailer.get_key(b"/QTest2");
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
/// GAP(`QPDFPageObjectHelper::getImages`): no flpdf equivalent exists
/// (confirmed: no `get_images`/`getImages` on any page helper or
/// `ObjectHandle`). The `page N:` and `  images:` lines print unconditionally
/// before the per-image loop in qpdf's own source, so they are kept as real
/// output; the image name/width/height lines are not emitted. The
/// `  content:`, `end page N`, `/QStrings`, and `/QNumbers` sections that
/// follow in qpdf's source are independent of `getImages` and are ported in
/// full.
pub(crate) fn run_test_5<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = {
        let mut helper = PageDocumentHelper::new(pdf);
        helper.get_all_pages()?
    };

    for (index, page_ref) in page_refs.iter().enumerate() {
        let pageno = index + 1;
        writeln!(stdout, "page {pageno}:")?;
        writeln!(stdout, "  images:")?;
        // GAP(QPDFPageObjectHelper::getImages): see this function's own doc
        // above.
        writeln!(stdout, "  content:")?;
        let page = pdf.get_object_handle(*page_ref);
        let content = page.get_page_contents()?;
        for item in &content {
            write!(stdout, "    ")?;
            write_bytes(stdout, &item.unparse())?;
            writeln!(stdout)?;
        }
        writeln!(stdout, "end page {pageno}")?;
    }

    let trailer = pdf.trailer();
    let root = dict_key(pdf, &trailer, b"/Root")?;

    let qstrings = dict_key(pdf, &root, b"/QStrings")?;
    resolve_handle(pdf, &qstrings)?;
    if let Some(items) = qstrings.as_array() {
        writeln!(stdout, "QStrings:")?;
        for item in items {
            resolve_handle(pdf, &item)?;
            let utf8 = item
                .as_string()
                .map(|value| flpdf::pdf_string::utf8_value(&value))
                .unwrap_or_default();
            write_bytes(stdout, &utf8)?;
            writeln!(stdout)?;
        }
    }

    let qnumbers = dict_key(pdf, &root, b"/QNumbers")?;
    resolve_handle(pdf, &qnumbers)?;
    if let Some(items) = qnumbers.as_array() {
        writeln!(stdout, "QNumbers:")?;
        for item in items {
            resolve_handle(pdf, &item)?;
            // qpdf's `getNumericValue()` handles integer and real values and
            // falls back to `0.0` (with a `typeWarning`) for anything else
            // (`libqpdf/QPDFObjectHandle.cc:377-389`); see this file's
            // top-level caveats for the missing warning text.
            let value = item
                .as_integer()
                .map(|value| value as f64)
                .or_else(|| item.as_real())
                .unwrap_or(0.0);
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
    let root = dict_key(pdf, &trailer, b"/Root")?;
    let metadata = dict_key(pdf, &root, b"/Metadata")?;
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
    let root = dict_key(pdf, &trailer, b"/Root")?;
    let qstream = dict_key(pdf, &root, b"/QStream")?;
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
    let root = dict_key(pdf, &trailer, b"/Root")?;
    let qstream = dict_key(pdf, &root, b"/QStream")?;
    resolve_handle(pdf, &qstream)?;
    if qstream.type_code()? != 10 {
        return Err(Error::Internal(
            "test 7 run on file with no QStream".to_string(),
        ));
    }

    let filter_dict = ObjectHandle::dictionary(vec![(
        b"/Filter".to_vec(),
        ObjectHandle::name(b"FlateDecode".to_vec()),
    )]);
    let compressed = flpdf::filters::encode_stream_data(&filter_dict, b"new data for stream\n")?;
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
    let root = dict_key(pdf, &trailer, b"/Root")?;
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
    use super::{run_test_3, StdoutPipeline};
    use flpdf::{ObjectHandle, Pdf, Pipeline};
    use std::io::{self, Write};
    use std::rc::Rc;

    struct FailAfterHeader;

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

    #[test]
    fn stdout_pipeline_reports_qpdf_tokenized_identifier() {
        let mut stdout = Vec::new();
        let pipeline = StdoutPipeline {
            stdout: &mut stdout,
        };
        assert_eq!(pipeline.identifier(), "tokenized stream");
    }
}
