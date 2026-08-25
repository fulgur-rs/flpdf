// Port of qpdf's `test_18` through `test_25` (`qpdf/test_driver.cc:796-974`
// in pinned qpdf 11.9.0). See `test_0_1.rs` for the house style this file
// follows: how `flpdf::Pdf` is driven, how repair diagnostics are threaded
// through `super::emit_new_diagnostics`, and how deep qpdf-source-line
// comments justify each translation decision.
//
// Two of these eight tests (`run_test_20`, `run_test_25`) hit the same
// primitive gap and stop short of qpdf's own `QPDFWriter` call: flpdf has no
// public API that mutates `Pdf`'s own trailer in a way `PdfWriter` observes.
// `Pdf::trailer` returns `&Dictionary` with no setter; `Pdf::trailer_handle`
// lifts a *clone* of that dictionary into a separate `ObjectHandle` graph
// (`trailer_handle_memo`, `crates/flpdf/src/pdf.rs:263-296`) that is never
// written back; and every `PdfWriter` output path reads `pdf.trailer_dictionary()`
// directly (e.g. `crates/flpdf/src/writer.rs:2725`, `:3774`), never the
// handle graph. `self.trailer` itself is set once at construction and never
// reassigned anywhere in the crate (`pdf.rs:270-271`'s own doc). Concretely:
// `pdf.trailer().replace_key(...)` compiles and returns `Ok`, but has
// no effect whatsoever on what `PdfWriter::write` emits. See each function's
// own `GAP` comment for the exact stop point.
//
// Five functions here (`run_test_18`, `19`, `21`, `22`, `23`) bound their
// generic reader as `R: Read + Seek + 'static` rather than the bare
// `R: Read + Seek` used elsewhere in this crate: both `PageDocumentHelper`
// and `PdfWriter` are themselves defined as `<R: Read + Seek + 'static>`
// (`page_document_helper.rs`'s and `writer.rs`'s own struct
// definitions), so a function generic over an unqualified `R: Read + Seek`
// cannot construct either -- this is a Rust generic-bounds requirement of
// those two existing flpdf types, orthogonal to qpdf parity, not a
// behavioral choice. Every caller in this crate instantiates `R` as a
// concrete, owned (hence `'static`) reader type in practice.

use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{
    ObjectHandle, PageDocumentHelper, PageInput, Pdf, PdfOpenOptions, PdfWriter, StreamDataMode,
};

use super::emit_new_diagnostics;

pub(crate) fn run_test_18<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:796-815 -- "Remove a page and re-insert it in the same
    // file." qpdf's `pdf.getAllPages()` returns a reference into QPDF's own
    // live, internally-cached page vector that `addPage`/`removePage` keep
    // updated in place (`include/qpdf/QPDF.hh:673-680`), so the C++ test
    // re-reads through the *same* `pages` variable after each mutation.
    // `PageDocumentHelper::get_all_pages` intentionally returns an owned
    // snapshot instead (its own doc: "a later page insertion or removal
    // requires a fresh call") -- there is no live-updating container to
    // alias here, so this re-fetches explicitly after each mutation. Every
    // assertion qpdf makes (page count, and the final page's identity) is
    // preserved by doing so.
    let mut helper = PageDocumentHelper::new(pdf);
    let mut pages = helper.get_all_pages()?;
    assert_eq!(pages.len(), 10);
    let page5 = pages[5];
    helper.remove_page(page5)?;
    pages = helper.get_all_pages()?;
    assert_eq!(pages.len(), 9);
    let page5_input: PageInput<'_, std::io::Cursor<Vec<u8>>> = PageInput::existing(page5);
    helper.add_page(page5_input, false)?;
    pages = helper.get_all_pages()?;
    assert_eq!(pages.len(), 10);
    // `page5` was removed just above, so it is absent from the page list
    // when `add_page` re-inserts it: `PageDocumentHelper::insert_page`'s
    // already-present branch (which would instead install a shallow-copied
    // duplicate under a fresh ref -- the case `run_test_19` exercises from
    // the opposite side, by never removing the page first) does not
    // trigger here. The re-added page keeps `page5`'s own identity,
    // matching qpdf's `pages.back().getObjGen() == page5.getObjGen()`.
    assert_eq!(pages.last(), Some(&page5));

    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.write()
}

pub(crate) fn run_test_19<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:817-832 -- "Remove a page and re-insert it in the same
    // file. Try to insert a page that's already there. A shallow copy gets
    // inserted instead." Same re-fetch substitution as `run_test_18`'s own
    // comment documents for qpdf's live `getAllPages()` vector; `count`
    // must be captured from the *pre*-`add_page` snapshot to match qpdf's
    // `count = pages.size()` before the mutation.
    let mut helper = PageDocumentHelper::new(pdf);
    let pages = helper.get_all_pages()?;
    let newpage = pages[5];
    let count = pages.len();
    let newpage_input: PageInput<'_, std::io::Cursor<Vec<u8>>> = PageInput::existing(newpage);
    helper.add_page(newpage_input, false)?;
    let pages = helper.get_all_pages()?;
    let last = *pages
        .last()
        .expect("add_page leaves at least one more page than before");
    assert_eq!(pages.len(), count + 1);
    // `newpage` was still present in the page list (unlike `run_test_18`,
    // which removes the page first), so `PageDocumentHelper::insert_page`'s
    // already-present branch installs a fresh, shallow-copied duplicate
    // object for this re-added occurrence -- the opposite side of the same
    // branch `run_test_18` exercises.
    assert_ne!(last, newpage);

    let last_handle = pdf.get_object_handle(last);
    let newpage_handle = pdf.get_object_handle(newpage);
    // The shallow copy duplicates the page dictionary itself but keeps its
    // indirect `/Contents` reference shared with the original, matching
    // qpdf's `last.getKey("/Contents").getObjGen() ==
    // newpage.getKey("/Contents").getObjGen()`.
    assert_eq!(
        last_handle.get_key(b"/Contents").object_ref(),
        newpage_handle.get_key(b"/Contents").object_ref()
    );

    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

pub(crate) fn run_test_20<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:834-849 -- "Shallow copy an array". The read half is a
    // real, faithful translation: `getKey`/`shallowCopy`/`appendItem` map
    // directly onto `ObjectHandle::get_key`/`shallow_copy`/
    // `append_array_item`, none of which resolve or repair anything (their
    // own docs), so no new diagnostics can appear here.
    let trailer = pdf.trailer();
    let qtest = trailer.get_key(b"/QTest");
    let copy = qtest.shallow_copy()?;
    let size = trailer.get_key(b"/Size").shallow_copy()?;
    copy.append_array_item(size)?;

    // GAP(QPDFObjectHandle::replaceKey on the trailer): qpdf's
    // `trailer.replaceKey("/QTest2", copy)` installs `copy` as a *new*
    // trailer entry that the subsequent `QPDFWriter` then serializes. See
    // this file's module doc for why flpdf has no equivalent: there is no
    // public mutator for `Pdf`'s own trailer that a `PdfWriter` output
    // actually observes. `pdf.trailer().replace_key(b"/QTest2",
    // copy)` would compile and return `Ok` while writing nothing through to
    // the output, so it is not called here rather than being called and
    // silently doing nothing. The remainder of qpdf's test_20 -- that
    // `replaceKey` call and the final `QPDFWriter` write -- is not
    // translated below this comment.
    Ok(())
}

pub(crate) fn run_test_21<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:851-860 -- "Try to shallow copy a stream". qpdf's
    // `QPDF_Stream::copy` unconditionally throws
    // `std::runtime_error("stream objects cannot be cloned")`
    // (`libqpdf/QPDF_Stream.cc:140-145`), so the driver's own
    // `std::cout << "you can't see this"` line is dead code left in the
    // qpdf source to document that the throw propagates uncaught out of
    // this function (caught only by `main`'s top-level handler,
    // `test_driver.cc:3585-3593`). Mirrored here by `shallow_copy`'s own
    // `Err(Error::System("stream objects cannot be cloned"))`
    // (`object_handle.rs:3509-3524`'s doc) and `?` -- the `writeln!` below
    // it is real translated source text, but the preceding `?` is where
    // qpdf's exception leaves this function, so it never executes.
    let mut helper = PageDocumentHelper::new(pdf);
    let pages = helper.get_all_pages()?;
    let page = pages[0];
    let page_handle = pdf.get_object_handle(page);
    let contents = page_handle.get_key(b"/Contents");

    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    contents.shallow_copy()?;
    writeln!(stdout, "you can't see this")?;
    Ok(())
}

pub(crate) fn run_test_22<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:862-872 -- "Try to remove a page we don't have". The
    // first `removePage` succeeds; the second, on the same already-removed
    // page, fails with `Error::Missing("page is not in the document")`
    // (`PageDocumentHelper::remove_page`'s own doc), propagated by `?` the
    // same way run_test_21's stream error is -- leaving the following
    // `std::cout << "you can't see this"` line dead code again.
    let mut helper = PageDocumentHelper::new(pdf);
    let pages = helper.get_all_pages()?;
    let page = pages[0];
    helper.remove_page(page)?;

    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    // Re-borrow: `helper`'s first mutable borrow of `pdf` must end before
    // `emit_new_diagnostics` above can immutably borrow `pdf`.
    let mut helper = PageDocumentHelper::new(pdf);
    helper.remove_page(page)?;
    writeln!(stdout, "you can't see this")?;
    Ok(())
}

pub(crate) fn run_test_23<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:874-880 -- removes the last page and returns. qpdf's
    // own body has neither a `QPDFWriter` call nor any output of its own
    // (the shared "test N done" footer belongs to `runtest`/this crate's
    // `run`, not to the individual test function).
    let mut helper = PageDocumentHelper::new(pdf);
    let pages = helper.get_all_pages()?;
    let last = *pages
        .last()
        .expect("a page-manipulation fixture has at least one page");
    helper.remove_page(last)?;

    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

pub(crate) fn run_test_24<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:882-945 -- "Test behavior of reserved objects". None
    // of `new_reserved`/`trailer_handle`/`replace_key`/`array`/
    // `append_array_item`/`as_array`/`is_reserved`/`set_object`/
    // `materialize` here triggers file I/O or repair (their own docs), so
    // no new diagnostics can appear and `emit_new_diagnostics` is not
    // called.
    let res1 = pdf.new_reserved()?;
    let res2 = pdf.new_reserved()?;
    let trailer = pdf.trailer();
    // qpdf's own literal keys omit the leading `/` here ("Array1"/"Array2",
    // unlike "/QTest2" in `run_test_20`) -- `QPDF_Dictionary` stores
    // whatever literal string it is given with no normalization
    // (`libqpdf/QPDF_Dictionary.cc`), and `ObjectHandle::replace_key`
    // matches that (its own doc: "this API does not normalize slashless
    // input"), so this reproduces the qpdf source's own key text exactly
    // rather than "fixing" it to "/Array1"/"/Array2". Per this file's
    // module doc, neither entry ever reaches a `PdfWriter` in this port
    // regardless (the `replaceReserved`/writer gap below stops this test before qpdf's
    // own `QPDFWriter` call) -- unlike `run_test_20`/`run_test_25`'s
    // trailer mutations, this pair is genuinely inert either way, but is
    // translated here because it precedes the remaining replacement gap.
    trailer.replace_key(b"Array1", res1.clone())?;
    trailer.replace_key(b"Array2", res2.clone())?;

    let array1 = ObjectHandle::array(Vec::new());
    array1.append_array_item(res2.clone())?;
    array1.append_array_item(ObjectHandle::integer(1))?;
    let array2 = ObjectHandle::array(Vec::new());
    array2.append_array_item(res1.clone())?;
    array2.append_array_item(ObjectHandle::integer(2))?;

    // Make sure trying to ask questions about a reserved object doesn't
    // break it.
    if res1.as_array().is_some() {
        writeln!(stdout, "oops -- res1 is an array")?;
    }
    if res1.is_reserved() {
        writeln!(stdout, "res1 is still reserved after checking if array")?;
    }

    // `QPDF::replaceReserved` is exactly `replaceObject(reserved.getObjGen(),
    // replacement)` (`libqpdf/QPDF.cc:2008-2015`); `Pdf::set_object` is that
    // same canonical cache-replacement boundary (its own doc). `array1`'s
    // indirect child (`res2`) materializes to a plain `Object::Reference`
    // without dereferencing it (`ObjectHandle::materialize`'s own
    // `materialize_child` helper "never recurses" into an indirect child,
    // matching qpdf's `QPDFWriter::unparseChild`), so this does not require
    // `res2` to already be resolved -- `array1` itself is not reserved, so
    // `materialize` does not hit its own `is_reserved` rejection either.
    let res1_ref = res1
        .object_ref()
        .expect("new_reserved always allocates an indirect object");
    pdf.set_object(res1_ref, array1.materialize()?);
    if res1.is_reserved() {
        writeln!(stdout, "oops -- res1 is still reserved")?;
    } else {
        writeln!(stdout, "res1 is no longer reserved")?;
    }
    assert!(res1.as_array().is_some());
    writeln!(stdout, "res1 is an array")?;

    // `res2.unparseResolved()` reaches the reserved throw through
    // `QPDFObjectHandle::unparseResolved`'s own dereference
    // (`libqpdf/QPDFObjectHandle.cc:1586-1592`) exactly the way
    // `ObjectHandle::materialize`'s own top-level `is_reserved` check does
    // -- that method's own doc draws this correspondence explicitly, "only
    // when the reserved handle *is* the value being dereferenced there,
    // mirroring where qpdf's own throw is actually reached". The message
    // text matches `QPDF_Reserved::unparse`'s throw verbatim
    // (`libqpdf/QPDF_Reserved.cc:22-26`:
    // `"QPDFObjectHandle: attempting to unparse a reserved object"`).
    match res2.materialize() {
        Ok(_) => writeln!(stdout, "oops -- didn't throw")?,
        Err(error) => writeln!(stdout, "logic error: {error}")?,
    }

    // GAP(QPDF::replaceReserved): `ObjectHandle::make_direct` now ports the
    // recursive conversion and reserved-handle error above
    // (`libqpdf/QPDFObjectHandle.cc:2091-2131`). The remaining qpdf sequence
    // after `res2.makeDirect()` needs the document-owned
    // `replaceReserved(res2, array2)` transition and its writer-visible
    // reserved-slot identity. Per the port's stop-at-gap rule, the
    // `res2.makeDirect()` try/catch's own print, `res2.assertArray()`, the
    // circular `i1`/`i2` resolution check, and the final `QPDFWriter` write
    // remain outside this helper slice. `array2` above is real,
    // faithfully-translated qpdf source that precedes the replacement gap.
    Ok(())
}

pub(crate) fn run_test_25<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // test_driver.cc:947-974 -- "The copy object tests are designed to work
    // with a specific file. ... Copy qtest without crossing page
    // boundaries. Should get O1 and O2 and their streams but not O3 or any
    // other pages. Also verify that attempts to copy /Pages objects return
    // null."
    assert!(arg2.is_some());
    let arg2 = arg2.expect("checked by the assert above");

    // qpdf's fresh `QPDF oldpdf; oldpdf.processFile(arg2);` uses the class's
    // own default `attemptRecovery == true` -- only the *outer* `pdf`'s
    // `n == 0` special case in `runtest` disables recovery, and only for
    // `pdf` itself (`test_driver.cc:3491-3493`), never for a separately
    // constructed `QPDF` like `oldpdf` here. This opens with repair enabled
    // unconditionally, independent of whatever test number dispatched here.
    let bytes = std::fs::read(arg2)?;
    let options = PdfOpenOptions {
        repair: true,
        ..PdfOpenOptions::default()
    };
    let mut oldpdf = Pdf::open_mem_owned_with_options(bytes, options)?;

    // qpdf's `oldpdf.getTrailer().getKey("/QTest")`; `Pdf::trailer_handle`
    // is the direct equivalent of `QPDF::getTrailer` (both own docs).
    let oldpdf_trailer = oldpdf.trailer();
    let qtest = oldpdf_trailer.get_key(b"/QTest");
    let copied = pdf.copy_foreign_object(&qtest)?;

    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    // GAP(QPDFObjectHandle::replaceKey on the trailer): same root cause as
    // `run_test_20`'s gap -- see this file's module doc. qpdf's
    // `pdf.getTrailer().replaceKey("/QTest", pdf.copyForeignObject(qtest))`
    // installs `copied` as the trailer's own `/QTest` entry, which the
    // subsequent `QPDFWriter` then serializes; flpdf has no public mutator
    // for `Pdf`'s own trailer that a `PdfWriter` output actually observes.
    // `pdf.trailer().replace_key(b"/QTest", copied)` would compile
    // and return `Ok` while writing nothing through to the output, so it is
    // not called here. The remainder of qpdf's test_25 -- the
    // `copyForeignObject(oldpdf.getRoot().getKey("/Pages")).isNull()`
    // assertion and the final `QPDFWriter` write -- sits after this
    // `replaceKey` in qpdf's own order and is not translated below this
    // comment.
    let _copied = copied;
    Ok(())
}
