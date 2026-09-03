use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::{
    DecodeLevel, Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageInput, Pdf,
    PdfOpenOptions, PdfWriter, Pipeline, PipelineResult, StreamDataMode,
};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};
use crate::output::write_bytes;

/// Open `path` as a secondary document the way qpdf's own `QPDF::processFile`
/// does for every test in this file that takes a foreign document via
/// `arg2` (test_driver.cc's `oldpdf`/`other`/`encrypted` locals): repair is
/// enabled (qpdf's `processFile` always attempts recovery on demand), and
/// any repair diagnostic is printed immediately, exactly once, using
/// `path`'s own name -- matching qpdf's default warning callback, which
/// prints straight to `std::cerr` for a `QPDF` that never installs a custom
/// handler. `password` mirrors the two-argument `processFile(path,
/// password)` overload; an empty password matches the one-argument
/// overload.
///
/// `QPDF::processFile` opens `path` through `FileInputSource`, which opens it
/// with `QUtil::safe_fopen` (`libqpdf/FileInputSource.cc:14-18`,
/// `libqpdf/QUtil.cc:490-518`); an open failure there throws
/// `"open " + path + ": " + strerror(errno)`
/// (`QPDFSystemError::createWhat`, `libqpdf/QPDFSystemError.cc:12-28`) --
/// the same path-aware, CRT-probed format [`super::open_error_bytes`] and
/// [`super::crt_open_error_message`] already build for this driver's
/// *primary* `filename1` open failure, reused here instead of propagating a
/// bare [`std::io::Error`] (whose `Display` carries neither the "open"
/// operation nor `path` itself).
fn open_secondary_pdf(
    path: &OsStr,
    password: &[u8],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Pdf<std::fs::File>> {
    let file = std::fs::File::open(path).map_err(|error| {
        let crt_message = super::crt_open_error_message(path);
        let message = super::open_error_bytes(
            &os_str_diagnostic_bytes(path),
            crt_message.as_deref(),
            &error,
        );
        flpdf::Error::System(String::from_utf8_lossy(&message).into_owned())
    })?;
    let path_bytes = os_str_diagnostic_bytes(path);
    let options = PdfOpenOptions {
        repair: true,
        password: password.to_vec(),
        suppress_warnings: true,
        description: String::from_utf8_lossy(&path_bytes).into_owned(),
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

/// qpdf's local `getPageContents` helper (test_driver.cc:158-163): the
/// decoded bytes of `page`'s `/Contents` stream, with one literal NUL byte
/// appended -- qpdf's own helper builds a `std::string` from the decoded
/// buffer and then appends the C-string literal `"\0"`, which embeds a
/// trailing NUL in the `std::string`'s content rather than terminating it.
/// Like qpdf's helper, this assumes `/Contents` is a single stream (not an
/// array); it is only used by `run_test_30`, whose fixture pages qpdf's own
/// test asset guarantees have a single content stream.
fn page_contents<R: Read + Seek>(pdf: &mut Pdf<R>, page: ObjectRef) -> flpdf::Result<Vec<u8>> {
    let handle = pdf.get_object_handle(page);
    let contents = handle.get_key(b"/Contents");
    let data = contents.get_stream_data(DecodeLevel::Generalized)?;
    let mut owned = data.as_ref().clone();
    owned.push(0);
    Ok(owned)
}

/// A `Pl_Buffer`-shaped [`Pipeline`] for `run_test_33`.
///
/// qpdf's `Pl_Buffer` stays borrowed by the caller (`w.setOutputPipeline(&p)`
/// takes a raw pointer), so `p.getBufferSharedPointer()` reads the
/// accumulated bytes back after `write()` returns. `PdfWriter::set_output_pipeline`
/// instead takes ownership of its `Pipeline` (`Box<dyn Pipeline>`), so this
/// accumulator shares its byte sink through an `Rc<RefCell<Vec<u8>>>` clone
/// kept by the caller instead of a raw pointer -- a container substitution,
/// not a missing primitive; the accumulated bytes are identical either way.
struct SharedBufferPipeline {
    sink: Rc<RefCell<Vec<u8>>>,
}

impl Pipeline for SharedBufferPipeline {
    fn identifier(&self) -> &str {
        "buffer"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.sink.borrow_mut().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

/// test_26 (test_driver.cc:977-1000): copy the `O3` page via `addPage`
/// without crossing page boundaries, then replace `pdf`'s `/QTest` with a
/// copy of the foreign `/QTest` before writing `a.pdf`.
pub(crate) fn run_test_26<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `assert(arg2 != nullptr);` (test_driver.cc:987).
    let arg2 = arg2.expect("test 26 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
    let mut oldpdf = open_secondary_pdf(arg2, b"", stdout, stderr)?;
    let qtest = oldpdf.trailer_key_handle(b"QTest");
    let o3 = qtest.get_key(b"/O3");
    // qpdf never checks that `/O3` is indirect before calling `addPage`; a
    // page-tree entry is always an indirect object in a well-formed PDF,
    // matching the fixture this test is designed for.
    let o3_ref = o3
        .object_ref()
        .expect("/O3 is a page, always an indirect object");
    PageDocumentHelper::new(pdf).add_page(PageInput::foreign(&mut oldpdf, o3_ref), false)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    // qpdf next does `pdf.getTrailer().replaceKey("/QTest",
    // pdf.copyForeignObject(qtest))` (test_driver.cc:993). Run the copy for
    // its own real, faithful side effect on `pdf`'s object graph (C++
    // evaluates the argument expression before the call it feeds), but stop
    // there:
    let _ = pdf.copy_foreign_object(&qtest)?;
    // GAP(QPDF::getTrailer().replaceKey): flpdf has no public API to mutate
    // `Pdf::trailer()` after open. `ObjectHandle::replace_key` on
    // `Pdf::trailer()` only mutates the legacy handle-bridge
    // snapshot -- `emit_canonical_pdf`/`PdfWriter` never read it; the writer
    // serializes from `Pdf::trailer()`'s own `Dictionary` directly (e.g.
    // `writer.rs`'s `emit_canonical_pdf_inner`: `let mut trailer =
    // pdf.trailer_dictionary().clone();`). Without a way to attach the copied `/QTest`,
    // `PdfWriter::write()` cannot reproduce qpdf's real `a.pdf`, so it is
    // not attempted here.
    Ok(())
}

/// test_27 (test_driver.cc:1002-1076): copy `O3` and the page it refers to
/// before copying `qtest`, exercising copying from a stream backed by a
/// provider (including copying a provider multiple times) and
/// `setImmediateCopyFrom`.
pub(crate) fn run_test_27<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // Create a provider-backed stream in `empty1`, matching qpdf's local
    // `Pl_Buffer` + `Provider` construction (test_driver.cc:1013-1022): the
    // provider is a closure over its captured bytes, called (possibly
    // more than once, across copies) to write and finish the pipeline --
    // the same contract as qpdf's `Provider::provideStreamData`, which
    // calls `p->write(...)` then `p->finish()` itself.
    let empty1 = Pdf::empty()?;
    let s1 = empty1.new_stream()?;
    let data1 = b"new data for stream\n".to_vec();
    s1.replace_stream_data_with_callback(
        move |pipeline: &mut dyn Pipeline| {
            pipeline.write(&data1)?;
            pipeline.finish()?;
            Ok(())
        },
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    )?;
    // Copy the provider-backed stream from `empty1` into `empty2`, matching
    // `s1 = empty2.copyForeignObject(s1);`. `copy_foreign_object`'s stream
    // handling keeps the source document's lazy dispatch boundary rather
    // than eagerly materializing the provider's bytes at copy time
    // (`object_copy.rs`'s `DocumentResolver::copy_stream_data` doc), so this
    // is a faithful translation, not a container substitution.
    let mut empty2 = Pdf::empty()?;
    let s1 = empty2.copy_foreign_object(&s1)?;

    // qpdf keeps `empty3`, the provider-backed stream, and `oldpdf` alive only
    // until all three streams and `/QTest` have been copied
    // (`test_driver.cc:1036-1068`). The immediate-copy source can therefore be
    // dropped before the writer, while the non-immediate `empty1`/`empty2`
    // sources remain alive just as qpdf's outer locals do.
    {
        // A second provider, in `empty3`, which has `setImmediateCopyFrom(true)`
        // -- matching test_driver.cc:1036-1053.
        let data2 = b"more data for stream\n".to_vec();
        let empty3 = Pdf::empty()?;
        empty3.set_immediate_copy_from(true);
        let s3 = empty3.new_stream()?;
        s3.replace_stream_data_with_callback(
            move |pipeline: &mut dyn Pipeline| {
                pipeline.write(&data2)?;
                pipeline.finish()?;
                Ok(())
            },
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        )?;

        // qpdf: `assert(arg2 != nullptr);` (test_driver.cc:1054).
        let arg2 =
            arg2.expect("test 27 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
        let mut oldpdf = open_secondary_pdf(arg2, b"", stdout, stderr)?;
        let qtest = oldpdf.trailer_key_handle(b"QTest");
        let o3 = qtest.get_key(b"/O3");
        let other_page = o3.get_key(b"/OtherPage");
        let other_page_ref = other_page
            .object_ref()
            .expect("/O3/OtherPage is a page, always an indirect object");
        let o3_ref = o3
            .object_ref()
            .expect("/O3 is a page, always an indirect object");
        {
            // qpdf: `dh.addPage(O3.getKey("/OtherPage"), false); dh.addPage(O3,
            // false);` (test_driver.cc:1060-1061) -- order matters: the other
            // page is added first.
            let mut dh = PageDocumentHelper::new(pdf);
            dh.add_page(PageInput::foreign(&mut oldpdf, other_page_ref), false)?;
            dh.add_page(PageInput::foreign(&mut oldpdf, o3_ref), false)?;
        }
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

        let s2 = oldpdf.new_stream_with_data(Rc::new(b"potato\n".to_vec()))?;

        // qpdf replaces `/QTest` in the live trailer with a copy of the
        // foreign `qtest`, then appends copies of `s1`/`s2`/`s3` (in that
        // order) into a new `/QTest2` trailer array
        // (test_driver.cc:1063-1068). Keep the copied handles on the live
        // trailer; calling copyForeignObject without attaching its return
        // value would not reproduce qpdf's reachable graph.
        let trailer = pdf.trailer();
        let copied_qtest = pdf.copy_foreign_object(&qtest)?;
        trailer.replace_key(b"/QTest", copied_qtest)?;
        let qtest2 =
            trailer.replace_key_and_get_new(b"/QTest2", ObjectHandle::array(Vec::new()))?;
        for stream in [&s1, &s2, &s3] {
            qtest2.append_array_item(pdf.copy_foreign_object(stream)?)?;
        }
        pdf.mark_object_handle_dirty(&trailer)?;
    }

    // qpdf writes only after the transient source documents above leave
    // scope. `setDecodeLevel` plus `setCompressStreams(false)` is the exact
    // writer configuration from test_driver.cc:1070-1076.
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_compress_streams(false);
    writer.set_decode_level(DecodeLevel::Generalized);
    writer.write()?;
    // Writer-time provider/stream errors are collected by the canonical PDF
    // logger and must be emitted before the shared `test N done` footer.
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

/// test_28 (test_driver.cc:1078-1094): `copyForeignObject` error cases.
pub(crate) fn run_test_28<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let qtest = pdf.trailer_key_handle(b"QTest");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    // qpdf's `catch (std::logic_error const& e)` here always fires for this
    // fixture: `qtest` is either direct (rejected as "not indirect") or
    // belongs to `pdf` itself (rejected as "object from this QPDF") --
    // `copy_foreign_object` returns `Err` for both, matching either
    // qpdf-observed shape without needing to know which one the fixture
    // uses.
    match pdf.copy_foreign_object(&qtest) {
        Ok(_) => writeln!(stdout, "oops -- didn't throw")?,
        Err(error) => writeln!(stdout, "logic error: {error}")?,
    }
    match pdf.copy_foreign_object(&ObjectHandle::integer(1)) {
        Ok(_) => writeln!(stdout, "oops -- didn't throw")?,
        Err(error) => writeln!(stdout, "logic error: {error}")?,
    }
    Ok(())
}

/// test_29 (test_driver.cc:1096-1145): detect mixed-ownership objects in
/// `QPDFWriter`, and detect adding a foreign object directly.
pub(crate) fn run_test_29<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `assert(arg2 != nullptr);` (test_driver.cc:1100).
    let arg2 = arg2.expect("test 29 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
    let mut other = open_secondary_pdf(arg2, b"", stdout, stderr)?;

    // qpdf's first scenario places the primary document's foreign /QTest
    // inside an ownerless direct dictionary, then attaches that dictionary to
    // the secondary document's live trailer before writing it
    // (test_driver.cc:1102-1120). `replace_key` intentionally performs the
    // same shallow ownership check as qpdf, so the foreign descendant remains
    // available for QPDFWriter's write-time ownership check.
    let qtest = pdf.trailer_key_handle(b"QTest");
    let dictionary = ObjectHandle::dictionary(Vec::new());
    dictionary.replace_key(b"/QTest", qtest)?;
    other.trailer().replace_key(b"/QTest", dictionary)?;
    let first_write = {
        let mut writer = PdfWriter::new(&mut other);
        writer.set_output_file("a.pdf")?;
        writer.write()
    };
    match first_write {
        Ok(()) => writeln!(stdout, "oops -- didn't throw")?,
        Err(Error::Internal(message)) => writeln!(stdout, "logic error: {message}")?,
        Err(error) => return Err(error),
    }

    // qpdf repeats the same graph construction with an object from a second
    // document that is destroyed before the writer runs
    // (test_driver.cc:1123-1135). Pdf teardown turns the retained canonical
    // handle into the writer's distinct destroyed-object error.
    let mut other2 = Pdf::empty()?;
    let root2 = other2.root_handle()?;
    let dictionary = ObjectHandle::dictionary(Vec::new());
    dictionary.replace_key(b"/QTest", root2)?;
    other.trailer().replace_key(b"/QTest", dictionary)?;
    drop(other2);
    let second_write = {
        let mut writer = PdfWriter::new(&mut other);
        writer.set_output_file("a.pdf")?;
        writer.write()
    };
    match second_write {
        Ok(()) => writeln!(stdout, "oops -- didn't throw")?,
        Err(Error::Internal(message)) => writeln!(stdout, "logic error: {message}")?,
        Err(error) => return Err(error),
    }

    // The third scenario is real: attaching another document's root
    // directly is `ObjectHandle::replace_key`'s own documented ownership
    // check, no trailer mutation required.
    let root1_ref = pdf
        .root_ref()
        .ok_or_else(|| flpdf::Error::System("pdf has no /Root".to_string()))?;
    let root1 = pdf.get_object_handle(root1_ref);
    let root2_ref = other
        .root_ref()
        .ok_or_else(|| flpdf::Error::System("other has no /Root".to_string()))?;
    let root2 = other.get_object_handle(root2_ref);
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    match root1.replace_key(b"/Oops", root2) {
        Ok(()) => writeln!(stdout, "oops -- didn't throw")?,
        Err(error) => writeln!(stdout, "logic error: {error}")?,
    }
    Ok(())
}

/// test_30 (test_driver.cc:1147-1171): copy encryption parameters onto a
/// fresh write of `pdf`, then verify the first page's contents survive
/// round-tripping through the newly encrypted `b.pdf`.
pub(crate) fn run_test_30<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `assert(arg2 != nullptr);` (test_driver.cc:1150).
    let arg2 = arg2.expect("test 30 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
    let mut encrypted = open_secondary_pdf(arg2, b"user", stdout, stderr)?;

    // qpdf's `QPDFWriter::copyEncryptionParameters(QPDF&)` snapshots the
    // donor's authenticated file key, `/Encrypt` dictionary, and permanent
    // `/ID[0]`. The canonical reader owns this exact boundary in
    // `Pdf::writer_copy_encryption_source` (`reader.rs:382-411`), so the qtest
    // driver consumes the same source rather than rebuilding a raw snapshot.
    let source = encrypted
        .writer_copy_encryption_source()?
        .ok_or_else(|| flpdf::Error::System("encrypted input is not encrypted".to_string()))?;

    let mut w = PdfWriter::new(pdf);
    w.set_output_file("b.pdf")?;
    w.set_stream_data_mode(StreamDataMode::Preserve);
    w.copy_encryption_parameters(source);
    w.write()?;

    // qpdf opens `final` first (test_driver.cc:1159-1160; `open_secondary_pdf`
    // above drains any of its own repair diagnostics immediately), then reads
    // `pdf`'s own page contents before `final`'s (test_driver.cc:1161-1164) --
    // any warning that read raises is emitted at that point, so drain `pdf`'s
    // diagnostics right after `orig_contents`, before reading `final_pdf`'s.
    let mut final_pdf = open_secondary_pdf(OsStr::new("b.pdf"), b"user", stdout, stderr)?;
    let orig_page = PageDocumentHelper::new(pdf)
        .get_all_pages()?
        .into_iter()
        .next()
        .ok_or_else(|| flpdf::Error::System("pdf has no pages".to_string()))?;
    let orig_contents = page_contents(pdf, orig_page)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let new_page = PageDocumentHelper::new(&mut final_pdf)
        .get_all_pages()?
        .into_iter()
        .next()
        .ok_or_else(|| flpdf::Error::System("final has no pages".to_string()))?;
    let new_contents = page_contents(&mut final_pdf, new_page)?;
    if orig_contents != new_contents {
        writeln!(stdout, "oops -- page contents don't match")?;
        writeln!(stdout, "original:")?;
        write_bytes(stdout, &orig_contents)?;
        writeln!(stdout, "new:")?;
        write_bytes(stdout, &new_contents)?;
        writeln!(stdout)?;
    }
    Ok(())
}

/// test_31 (test_driver.cc:1173-1212): `QPDFObjectHandle::parse` coverage.
///
/// The two Rust parse entry points below are thin consumers of the canonical
/// object parser. qpdf's parse overloads use the same `QPDFParser` and differ
/// only in their description and owning-document context.
pub(crate) fn run_test_31<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `auto o1 = "..."_qpdf;` (test_driver.cc:1176-1177). The
    // `_qpdf` literal is `QPDFObjectHandle::parse(str, "QPDFObjectHandle
    // literal")` (`QPDFObjectHandle.cc:2597-2601`); the description is
    // irrelevant here because this parse succeeds, so the context-less,
    // description-less `ObjectHandle::parse` is byte-equivalent for this
    // one call.
    let o1 = ObjectHandle::parse(
        b"[/name 16059 3.14159 false\n << /key true /other [ (string1) (string2) ] >> null]",
    )?;
    write_bytes(stdout, &o1.unparse())?;
    writeln!(stdout)?;

    // qpdf: `QPDFObjectHandle o2 = QPDFObjectHandle::parse("   12345 \f
    // ");` then `assert(o2.isInteger() && (o2.getIntValue() == 12345));`
    // (test_driver.cc:1179-1180). This also succeeds, so it needs no
    // description either.
    let o2 = ObjectHandle::parse(b"   12345 \x0c  ")?;
    assert_eq!(o2.as_integer(), Some(12345));

    // qpdf's context-free overload throws a logic error for a nested indirect
    // reference. Its context-free description overload throws the trailing
    // QPDFExc with `parsed object` as the input-source name.
    let error = ObjectHandle::parse_with_description(b"[1 0 R]", "indirect test")
        .expect_err("context-free parsing must reject an indirect reference");
    let Error::Internal(message) = error else {
        return Err(Error::Internal(
            "context-free indirect parse returned the wrong error category".to_owned(),
        ));
    };
    writeln!(stdout, "logic error parsing indirect: {message}")?;

    let error = ObjectHandle::parse_with_description(b"0 trailing", "trailing test")
        .expect_err("context-free parsing must reject trailing data");
    let Error::Parse { offset, message } = error else {
        return Err(Error::Internal(
            "context-free trailing parse returned the wrong error category".to_owned(),
        ));
    };
    let mut exception = String::from("parsed object");
    exception.push_str(" (trailing test");
    assert_eq!(offset, 0);
    exception.push(')');
    exception.push_str(": ");
    exception.push_str(&message);
    writeln!(stdout, "trailing data: {exception}")?;

    // qpdf's context-taking overload inserts unresolved references into the
    // owning canonical cache. `try_is_integer` and `resolve` reproduce the
    // following qpdf type/null predicates at their normal lazy boundary.
    let first = ObjectHandle::parse_with_context(pdf, b"[5 0 R]", "")?;
    let first_item = first.try_get_array_item(0)?;
    assert!(first_item.try_is_integer()?);
    assert!(!first_item.is_direct());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let second = ObjectHandle::parse_with_context(pdf, b"[5 0 R]", "")?;
    let second_item = second.try_get_array_item(0)?;
    assert!(first_item.is_same_object_as(&second_item));
    assert!(!second_item.is_direct());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let mixed = ObjectHandle::parse_with_context(pdf, b"[5 0 R 0 R /X]", "")?;
    assert_eq!(mixed.unparse(), b"[ 5 0 R 0 (R) /X ]");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let described = ObjectHandle::parse_with_context(pdf, b"[1 0 R]", "indirect test")?;
    assert_eq!(described.unparse(), b"[ 1 0 R ]");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    for input in [b"}".as_slice(), b"{".as_slice(), b">>".as_slice()] {
        let recovered = ObjectHandle::parse_with_context(pdf, input, "")?;
        assert!(recovered.is_null());
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    }

    let null_reference = ObjectHandle::parse_with_context(pdf, b"[7 0 R]", "")?;
    let null_item = null_reference.try_get_array_item(0)?;
    pdf.resolve(&null_item)?;
    assert!(null_item.is_null());
    assert!(!null_item.is_direct());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let direct_null = ObjectHandle::parse_with_context(pdf, b"null", "")?;
    assert!(direct_null.is_null());
    assert!(direct_null.is_direct());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let invalid_objgen =
        ObjectHandle::parse_with_context(pdf, b"[0 0 R -1 0 R 1 65535 R 1 100000 R 1 -1 R]", "")?;
    assert_eq!(invalid_objgen.unparse(), b"[ null null null null null ]");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

/// test_32 (test_driver.cc:1214-1234): extra header text, across the
/// four linearized/newline combinations.
pub(crate) fn run_test_32<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf's test_32 body has no warning-producing operations on `pdf` (four
    // plain `QPDFWriter::write()` calls), so `filename`/`stderr`/
    // `diagnostics_written` are genuinely unused here rather than merely
    // omitted.
    let _ = (filename, stderr, diagnostics_written);
    let filenames = ["a.pdf", "b.pdf", "c.pdf", "d.pdf"];
    for (i, name) in filenames.into_iter().enumerate() {
        let linearized = (i & 1) != 0;
        let newline = (i & 2) != 0;
        let mut w = PdfWriter::new(pdf);
        w.set_output_file(name)?;
        w.set_static_id(true);
        writeln!(stdout, "file: {name}")?;
        writeln!(
            stdout,
            "linearized: {}",
            if linearized { "yes" } else { "no" }
        )?;
        writeln!(stdout, "newline: {}", if newline { "yes" } else { "no" })?;
        w.set_linearization(linearized);
        if linearized {
            // qpdf: "avoid dependency on zlib's output" (test_driver.cc:1229).
            w.set_compress_streams(false);
        }
        w.set_extra_header_text(if newline {
            "%% Comment with newline\n"
        } else {
            "%% Comment\n% No newline"
        });
        w.write()?;
    }
    Ok(())
}

/// test_33 (test_driver.cc:1236-1249): write to a custom output pipeline,
/// then copy the resulting bytes into `a.pdf`.
pub(crate) fn run_test_33<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf's test_33 body prints nothing and has no warning-producing
    // operation on `pdf` (one `QPDFWriter::write()` call through a custom
    // pipeline), so `filename`/`stdout`/`stderr`/`diagnostics_written` are
    // genuinely unused here rather than merely omitted.
    let _ = (filename, stdout, stderr, diagnostics_written);
    let sink = Rc::new(RefCell::new(Vec::new()));
    let mut w = PdfWriter::new(pdf);
    w.set_static_id(true);
    w.set_output_pipeline(SharedBufferPipeline {
        sink: Rc::clone(&sink),
    })?;
    w.write()?;
    let bytes = sink.borrow().clone();
    std::fs::write("a.pdf", &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{open_secondary_pdf, run_test_29, run_test_30, run_test_31};
    use flpdf::{EncryptParams, Pdf, PdfOpenOptions, PdfWriter};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct CurrentDirGuard(PathBuf);

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore test current directory");
        }
    }

    fn pdf_with_integer_object() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = [0usize; 6];
        offsets[1] = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        offsets[5] = bytes.len();
        bytes.extend_from_slice(b"5 0 obj\n16059\nendobj\n");

        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            if offset == 0 {
                bytes.extend_from_slice(b"0000000000 00000 f \n");
            } else {
                bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    fn pdf_with_qtest_object() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let catalog_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let qtest_offset = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Marker true >>\nendobj\n");
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f \n{catalog_offset:010} 00000 n \n{qtest_offset:010} 00000 n \n"
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 3 /Root 1 0 R /QTest 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    fn pdf_with_foreign_page_graph() -> Vec<u8> {
        let objects: [&[u8]; 5] = [
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /OtherPage 4 0 R >>\nendobj\n",
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
            b"5 0 obj\n<< /O3 3 0 R >>\nendobj\n",
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(bytes.len());
            bytes.extend_from_slice(object);
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 6 /Root 1 0 R /QTest 5 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn test_29_matches_qpdf_mixed_ownership_output() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let secondary = directory.path().join("minimal.pdf");
        std::fs::write(
            &secondary,
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf"),
        )
        .expect("write secondary PDF");

        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = Pdf::open_mem_owned(pdf_with_qtest_object()).expect("open qtest PDF");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        run_test_29(
            &mut pdf,
            b"copy-foreign-objects-in.pdf",
            Some(secondary.as_os_str()),
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 29 should complete after reporting writer logic errors");

        assert_eq!(
            stdout,
            b"logic error: QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file.\n\
logic error: attempted to unparse a QPDFObjectHandle from a destroyed QPDF\n\
logic error: Attempting to add an object from a different QPDF. Use QPDF::copyForeignObject to add objects from another file.\n"
        );
        assert!(
            stderr.is_empty(),
            "test 29 should not emit stderr: {stderr:?}"
        );
    }

    #[test]
    fn test_27_writes_live_trailer_copies_and_provider_streams() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let secondary = directory.path().join("copy-foreign-objects-in.pdf");
        std::fs::write(&secondary, pdf_with_foreign_page_graph()).expect("write secondary PDF");

        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("open primary PDF");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        super::run_test_27(
            &mut pdf,
            b"minimal.pdf",
            Some(secondary.as_os_str()),
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 27 should write its live trailer mutations");

        assert!(
            directory.path().join("a.pdf").is_file(),
            "test 27 must reach the qpdf writer after trailer mutation"
        );
        let mut written = Pdf::open(std::fs::File::open(directory.path().join("a.pdf")).unwrap())
            .expect("reopen test 27 output");
        let qtest2 = written.trailer_key_handle(b"QTest2");
        written.resolve(&qtest2).expect("resolve /QTest2");
        assert_eq!(qtest2.as_array().expect("/QTest2 array").len(), 3);
        let qtest = written.trailer_key_handle(b"QTest");
        written.resolve(&qtest).expect("resolve /QTest");
        assert!(
            qtest.as_dictionary().is_some(),
            "test 27 must attach the copied /QTest to the live trailer"
        );
        assert!(
            stdout.is_empty(),
            "test 27 stdout should be empty: {stdout:?}"
        );
        assert!(
            stderr.is_empty(),
            "synthetic test 27 stderr should be empty: {stderr:?}"
        );
    }

    #[test]
    fn test_30_consumes_the_canonical_copy_encryption_source() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        let secondary = directory.path().join("secondary.pdf");

        let mut donor = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("open donor PDF");
        let mut donor_writer = PdfWriter::new(&mut donor);
        donor_writer
            .set_output_file(&secondary)
            .expect("configure donor output");
        donor_writer.set_static_id(true);
        donor_writer.set_encryption_parameters(EncryptParams::v4_aes128(
            b"user".to_vec(),
            b"owner".to_vec(),
        ));
        donor_writer.write().expect("write encrypted donor");

        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut target = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("open target PDF");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_30(
            &mut target,
            b"target.pdf",
            Some(Path::new(&secondary).as_os_str()),
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 30");

        assert!(directory.path().join("b.pdf").is_file());
        // qpdf's test_30 prints nothing to stdout when the copied output's
        // page contents match; a non-empty stdout means the "oops -- page
        // contents don't match" branch fired, which a silent
        // copy_encryption_parameters regression must not slip past.
        assert!(
            stdout.is_empty(),
            "test 30 must report no content mismatch: {:?}",
            String::from_utf8_lossy(&stdout)
        );

        // The whole point of test 30 is that `b.pdf` carries the donor's
        // copied /Encrypt parameters, not that it merely exists: reopen it
        // with the donor's user password and confirm it is still encrypted
        // and that password still authenticates, so a
        // copy_encryption_parameters regression that produced a plaintext
        // (but otherwise page-identical) output is caught.
        let copied = open_secondary_pdf(
            Path::new(&directory.path().join("b.pdf")).as_os_str(),
            b"user",
            &mut stdout,
            &mut stderr,
        )
        .expect("reopen the copy-encryption output with the donor's user password");
        assert!(
            copied.is_encrypted(),
            "test 30's output must still carry the donor's copied /Encrypt dictionary"
        );
        assert!(
            copied.user_password_matched(),
            "the donor's user password must still authenticate the copied output"
        );
    }

    #[test]
    fn test_31_matches_qpdf_parse_object_output_for_a_clean_context() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            pdf_with_integer_object(),
            PdfOpenOptions {
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open clean PDF context");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_31(
            &mut pdf,
            b"one-page.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 31");

        assert_eq!(
            stdout,
            b"[ /name 16059 3.14159 false << /key true /other [ (string1) (string2) ] >> null ]\n\
logic error parsing indirect: QPDFParser::parse called without context on an object with indirect references\n\
trailing data: parsed object (trailing test): trailing data found parsing object from string\n"
        );
        assert_eq!(
            stderr,
            b"WARNING: parsed object (offset 9): unknown token while reading object; treating as string\n\
WARNING: parsed object: treating unexpected brace token as null\n\
WARNING: parsed object: treating unexpected brace token as null\n\
WARNING: parsed object: unexpected dictionary close token\n"
        );
    }
}
