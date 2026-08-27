use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::{
    DecodeLevel, ObjectHandle, ObjectRef, PageDocumentHelper, PageInput, Pdf, PdfOpenOptions,
    PdfWriter, Pipeline, PipelineResult, StreamDataMode,
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
    let options = PdfOpenOptions {
        repair: true,
        password: password.to_vec(),
        suppress_warnings: true,
        ..PdfOpenOptions::default()
    };
    let secondary = Pdf::open_with_options(file, options)?;
    let mut secondary_diagnostics_written = 0;
    let path_bytes = os_str_diagnostic_bytes(path);
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
    let arg2 = arg2.expect("test 27 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
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

    // qpdf next replaces `/QTest` in `pdf`'s trailer with a copy of the
    // foreign `qtest`, then appends copies of `s1`/`s2`/`s3` (in that order)
    // into a new `/QTest2` trailer array (test_driver.cc:1063-1068). Run
    // each copy for its own real, faithful side effect on `pdf`'s object
    // graph, but stop there:
    let _ = pdf.copy_foreign_object(&qtest)?;
    let _ = pdf.copy_foreign_object(&s1)?;
    let _ = pdf.copy_foreign_object(&s2)?;
    let _ = pdf.copy_foreign_object(&s3)?;
    // GAP(QPDF::getTrailer().replaceKey / replaceKeyAndGetNew): the same
    // missing primitive as `run_test_26` -- flpdf has no public API to
    // mutate `Pdf::trailer()` after open, so `/QTest` and `/QTest2` cannot
    // be attached and `PdfWriter::write()` is not attempted.
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

    // GAP(QPDFObjectHandle::replaceKey on a foreign trailer): qpdf's first
    // scenario sneaks `pdf`'s own `/QTest` value into a fresh direct
    // dictionary, then attaches that dictionary to `other`'s trailer via
    // `other->getTrailer().replaceKey("/QTest", dict)` to build a
    // mixed-ownership object graph, writes it, and expects `QPDFWriter`'s
    // ownership check to reject it (test_driver.cc:1110-1120). flpdf has no
    // public API to mutate a `Pdf`'s trailer after open (same missing
    // primitive as `run_test_26`'s GAP), so the mixed-ownership state cannot
    // be constructed and this scenario's "logic error: ..." / "oops --
    // didn't throw" line is not attempted.

    // GAP(QPDFObjectHandle::replaceKey on a foreign trailer): qpdf's second
    // scenario repeats the construction with a dangling source document
    // (`other2`, freed before the write) to prove deletion does not defeat
    // the ownership check (test_driver.cc:1123-1135). Same missing
    // primitive as above; not attempted.

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
/// `pdf`, `filename`, `stderr`, and `diagnostics_written` are unused here:
/// every real qpdf line past the first two parses needs either
/// `QPDFObjectHandle::parse(string, description)` (a two-argument,
/// still-context-less overload that embeds `description` into its thrown
/// message text) or `QPDFObjectHandle::parse(&pdf, string[, description])`
/// (the context-taking overload that allows indirect references). flpdf's
/// public `ObjectHandle::parse(&[u8])` has neither a description parameter
/// nor a document-context form, so this file stops at the first line that
/// needs either.
pub(crate) fn run_test_31<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (pdf, filename, stderr, diagnostics_written);

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

    // GAP(QPDFObjectHandle::parse(std::string const&, std::string const&)):
    // qpdf's next line, `QPDFObjectHandle::parse("[1 0 R]", "indirect
    // test")`, is expected to throw a `std::logic_error` whose message
    // embeds the literal description "indirect test" (no context is passed,
    // so an indirect reference cannot be canonicalized --
    // `QPDFObjectHandle::parse(nullptr, ...)`,
    // `QPDFObjectHandle.cc:1672-1699`). flpdf's `ObjectHandle::parse` takes
    // no description parameter, so this exact message cannot be reproduced,
    // and every subsequent line in this test needs either that description
    // parameter or the context-taking `QPDFObjectHandle::parse(&pdf, ...)`
    // overload (to parse `[5 0 R]`, `[1 0 R]`, etc. with real indirect
    // references) -- neither exists in flpdf's public API. The remainder of
    // this test (through test_driver.cc:1211) is not attempted.
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
