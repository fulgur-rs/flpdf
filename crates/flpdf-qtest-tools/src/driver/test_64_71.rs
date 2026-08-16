use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{
    DecodeLevel, ObjectHandle, PageDocumentHelper, PageInput, Pdf, PdfOpenOptions, PdfWriter,
};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};

// This file ports qpdf's `test_64` through `test_71` (`qpdf/test_driver.cc:2303-2457`).

/// Open a second document from `path` for a test that overlays or copies
/// from a companion file, mirroring qpdf's own default-arguments
/// `QPDF::processFile(path)` (no password, full permissive-open recovery).
/// Repair diagnostics for `path` are drained through their own local
/// counter and `path`'s own diagnostic name, matching how qpdf's single
/// process-wide warning handler reports them: it does not know or care
/// which of several open `QPDF` instances a warning came from, it only
/// knows the filename baked into that instance's own `QPDFExc`/warning
/// text.
fn open_secondary_pdf(
    path: &OsStr,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Pdf<std::fs::File>> {
    let file = std::fs::File::open(path)?;
    let options = PdfOpenOptions {
        repair: true,
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

/// `ObjectHandle::get_key` never resolves its receiver (its own doc), unlike
/// qpdf's `QPDFObjectHandle` accessors, which all `dereference()` on entry
/// (`libqpdf/QPDFObjectHandle.cc`). `dict_key`/`resolve_handle` restore that
/// implicit dereference explicitly, matching the identically named helpers
/// already established for this crate's `test_02_09` file.
fn resolve_handle<R: Read + Seek>(pdf: &mut Pdf<R>, handle: &ObjectHandle) -> flpdf::Result<()> {
    pdf.resolve_object_handle(handle)
}

fn dict_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
) -> flpdf::Result<ObjectHandle> {
    resolve_handle(pdf, handle)?;
    Ok(handle.get_key(key))
}

/// Shared body for `test_64` through `test_67` (`qpdf/test_driver.cc:2303-2340`,
/// `test_64_67`): overlay each page of `arg2`'s document onto the matching
/// page of `pdf`, in place, via `QPDFPageObjectHelper::getFormXObjectForPage`
/// + `QPDFPageObjectHelper::placeFormXObject(fo, name, trimBox, false,
/// allow_shrink, allow_expand)`.
///
/// GAP(`QPDFPageObjectHelper::getFormXObjectForPage` /
/// `QPDFPageObjectHelper::placeFormXObject`): both exist inside flpdf --
/// [`crate`]-external code just cannot reach them. `page_form_xobject::
/// get_form_xobject_for_page` mirrors `getFormXObjectForPage` but its whole module
/// is absent from `flpdf::lib`'s `pub use` list (`lib.rs` has no
/// `page_form_xobject` line at all); `overlay::place_form_xobject` mirrors
/// `placeFormXObject`'s placement-fragment computation but is a private
/// (non-`pub(crate)`) fn even inside its own module, and the only public
/// entry point built on it, [`flpdf::apply_overlay_specs`], bakes in fixed
/// `allow_shrink`/`allow_expand` pairs for its own CLI-shaped
/// underlay/overlay/base-page roles (`overlay.rs`'s
/// `apply_overlays_to_page`) rather than exposing them as the free
/// parameters this test needs per call. This is an export-visibility gap,
/// not an unimplemented feature: were `get_form_xobject_for_page` and a
/// `pub` `place_form_xobject` (or equivalent) taking explicit
/// `allow_shrink`/`allow_expand` made reachable from this crate, the per-page
/// loop below would become a direct, gap-free port.
///
/// The pre-loop setup below -- opening `arg2` and computing `pages1`/
/// `pages2`/`npages` -- has no dependency on either missing primitive and is
/// real, faithful translation. Every loop iteration's first statement
/// (`ph2.getFormXObjectForPage()`) needs the first missing primitive, so
/// nothing inside the loop runs here. qpdf's final `QPDFWriter` write to
/// `a.pdf` (QDF mode, static ID) is unconditional in the source, but its
/// bytes would encode whatever the loop mutated `pdf` into; since the loop
/// never mutates `pdf` here, calling the writer would silently emit a
/// `pdf`-unchanged `a.pdf` instead of qpdf's real overlaid one -- fabricated
/// output a future diff could mistake for a real bug, which rule 5 forbids.
/// Matching `run_test_20`'s precedent for the same shape of gap (a write
/// whose faithfulness depends on a mutation this file cannot perform), the
/// writer is not invoked.
#[allow(clippy::too_many_arguments)]
fn test_64_67_body<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
    _allow_shrink: bool,
    _allow_expand: bool,
) -> flpdf::Result<()> {
    // qpdf: `assert(arg2);` (test_driver.cc:2313).
    let arg2 = arg2.expect("test 64-67 require arg2, matching qpdf's own assert(arg2)");
    let mut pdf2 = open_secondary_pdf(arg2, stdout, stderr)?;

    let pages1 = PageDocumentHelper::new(pdf).get_all_pages()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let pages2 = PageDocumentHelper::new(&mut pdf2).get_all_pages()?;
    let _npages = pages1.len().min(pages2.len());

    // GAP(QPDFPageObjectHelper::getFormXObjectForPage /
    // QPDFPageObjectHelper::placeFormXObject): see this function's own doc
    // above.
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:2342-2346` (`test_64`): `test_64_67`
/// with `allow_shrink=false, allow_expand=false`.
pub(crate) fn run_test_64<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_64_67_body(
        pdf,
        filename,
        arg2,
        stdout,
        stderr,
        diagnostics_written,
        false,
        false,
    )
}

/// qpdf source: `qpdf/test_driver.cc:2348-2352` (`test_65`): `test_64_67`
/// with `allow_shrink=true, allow_expand=false`.
pub(crate) fn run_test_65<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_64_67_body(
        pdf,
        filename,
        arg2,
        stdout,
        stderr,
        diagnostics_written,
        true,
        false,
    )
}

/// qpdf source: `qpdf/test_driver.cc:2354-2358` (`test_66`): `test_64_67`
/// with `allow_shrink=false, allow_expand=true`.
pub(crate) fn run_test_66<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_64_67_body(
        pdf,
        filename,
        arg2,
        stdout,
        stderr,
        diagnostics_written,
        false,
        true,
    )
}

/// qpdf source: `qpdf/test_driver.cc:2360-2364` (`test_67`): `test_64_67`
/// with `allow_shrink=true, allow_expand=true`.
pub(crate) fn run_test_67<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_64_67_body(
        pdf,
        filename,
        arg2,
        stdout,
        stderr,
        diagnostics_written,
        true,
        true,
    )
}

/// qpdf source: `qpdf/test_driver.cc:2366-2386` (`test_68`).
///
/// `qstream.getStreamData()` (no explicit decode level, so qpdf's own
/// default `qpdf_dl_generalized`) throws `QPDFExc` "getStreamData called on
/// unfilterable stream" (`libqpdf/QPDF_Stream.cc:344-360`) exactly when
/// `QPDF_Stream::pipeStreamData`'s `filtered` out-parameter comes back
/// `false` -- i.e. when a filter chain exists but the requested decode level
/// declined to apply it (as for this fixture's `/DCTDecode`-filtered stream,
/// which `qpdf_dl_generalized` treats as unfilterable the same way flpdf's
/// own [`DecodeLevel::Generalized`] does per `qpdf-port-design-patterns.md`
/// parity).
///
/// GAP(the `filtered` out-parameter of `QPDF_Stream::pipeStreamData`):
/// flpdf's public [`ObjectHandle::get_stream_data`] has no accessible
/// equivalent of that boolean -- its own internal `filtering_attempted` flag
/// (`object_handle.rs`'s `pipe_stream_data_inner`), set by the identical
/// rule, is never surfaced through the public `Result<Rc<Vec<u8>>>` return,
/// which instead reports only overall pipe I/O success. This exact
/// try/throw/catch pair therefore cannot be reproduced: neither
/// `"oops -- didn't throw"` nor `"get unfilterable stream: ..."` is printed.
/// The call is still made below, matching qpdf's own real evaluation of this
/// expression (its `try` block always calls it) and any internal state it
/// would leave behind, even though this file cannot observe or branch on
/// whether qpdf's real call would have thrown; any error here is discarded
/// rather than propagated with `?`, matching qpdf's `catch (std::exception&)`
/// swallowing it rather than letting it escape this function.
///
/// The two sections after it are independent of the missing flag --
/// [`DecodeLevel::All`] and [`ObjectHandle::get_raw_stream_data`] both exist
/// and are ported in full, including qpdf's own asymmetric guards (a 9-byte
/// compare gated on `getSize() > 10` for `b1`, a 10-byte compare under the
/// same `> 10` gate for `b2`; not "fixed" to be consistent).
pub(crate) fn run_test_68<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root = pdf.trailer_key_handle(b"Root");
    let qstream = dict_key(pdf, &root, b"/QStream")?;
    resolve_handle(pdf, &qstream)?;

    // GAP(the `filtered` out-parameter of `QPDF_Stream::pipeStreamData`): see
    // this function's own doc above.
    let _ = qstream.get_stream_data(DecodeLevel::Generalized);

    let b1 = qstream.get_stream_data(DecodeLevel::All)?;
    if b1.len() > 10 && b1.starts_with(b"wwwwwwwww") {
        writeln!(stdout, "filtered stream data okay")?;
    }

    let b2 = qstream.get_raw_stream_data()?;
    if b2.len() > 10 && b2.starts_with(b"\xff\xd8\xff\xe0\x00\x10\x4a\x46\x49\x46") {
        writeln!(stdout, "raw stream data okay")?;
    }
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:2388-2402` (`test_69`): with
/// `setImmediateCopyFrom(true)`, copy each of `pdf`'s pages into its own
/// fresh empty document and write it out as `auto-<i>.pdf` with a static
/// `/ID`. No missing primitive: [`Pdf::set_immediate_copy_from`],
/// [`Pdf::empty`], [`PageInput::foreign`] + [`PageDocumentHelper::add_page`],
/// and [`PdfWriter`] cover every step.
pub(crate) fn run_test_69<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    pdf.set_immediate_copy_from(true);
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    for (index, page_ref) in pages.into_iter().enumerate() {
        let mut out = Pdf::empty()?;
        PageDocumentHelper::new(&mut out).add_page(PageInput::foreign(pdf, page_ref), false)?;
        // qpdf: `QUtil::uint_to_string(i)` is a plain unsigned decimal
        // rendering (`libqpdf/QUtil.cc`'s `int_to_string_base`, base 10, no
        // padding), matching `{index}` here.
        let outname = format!("auto-{index}.pdf");
        let mut writer = PdfWriter::new(&mut out);
        writer.set_output_file(&outname)?;
        writer.set_static_id(true);
        writer.write()?;
    }
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:2404-2414` (`test_70`).
///
/// GAP(`QPDFObjectHandle::setFilterOnWrite`): qpdf's per-stream "always
/// write this stream's original bytes verbatim, ignoring the writer's decode
/// level / recompression settings" flag (consulted by
/// `QPDFWriter::unparseObject`'s stream-filtering decision) has no flpdf
/// equivalent -- confirmed: no `pub fn` on `ObjectHandle` or `Stream`
/// matching `*filter_on_write*` anywhere in `crates/flpdf/src`. The two
/// `getKey` lookups that would receive the (missing) call are real,
/// side-effect-free translations and are kept for line-for-line
/// correspondence with the source; without the mutation they would feed,
/// `PdfWriter::write()` below would filter `/S1` and `/S2` at the requested
/// decode level exactly like every other stream instead of forcing them raw,
/// producing an `a.pdf` that differs from qpdf's real one for those two
/// objects. Matching `run_test_20`'s precedent for a write whose
/// faithfulness depends on a missing mutation, the writer is not invoked.
pub(crate) fn run_test_70<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer_handle();
    let _s1 = trailer.get_key(b"/S1");
    let _s2 = trailer.get_key(b"/S2");

    // GAP(QPDFObjectHandle::setFilterOnWrite): see this function's own doc
    // above.
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:2416-2457` (`test_71`).
///
/// GAP(`QPDFPageObjectHelper::forEachXObject` / `::forEachImage` /
/// `::forEachFormXObject` / `::getImages` / `::getFormXObjects`): none of
/// qpdf's five XObject-enumeration operations
/// (`libqpdf/QPDFPageObjectHelper.cc`) have an flpdf equivalent -- confirmed:
/// no `for_each_x_object` / `for_each_image` / `for_each_form_x_object` /
/// `get_images` / `get_form_x_objects` `pub fn` anywhere in
/// `crates/flpdf/src`. `get_images` is the same missing primitive this
/// crate's `run_test_5` (`test_02_09.rs`) already documents its own
/// `GAP(QPDFPageObjectHelper::getImages)` against; this function extends
/// that same gap to its four siblings. Every one of this test's 12
/// `--- ... ---` section headers is printed directly by the driver itself
/// (qpdf's `show` lambda is only ever invoked *inside* the gapped calls), so
/// all 12 are real, faithful, independent output and are kept below; each
/// section's own per-item content, produced only by the missing calls, is
/// not. `fx1` (test_driver.cc:2435-2436, `page`'s `/Resources/XObject/Fx1`)
/// is real and cheaply fetchable but feeds only these same gapped calls, so
/// it is not constructed here.
pub(crate) fn run_test_71<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `QPDFPageDocumentHelper(pdf).getAllPages().at(0)`
    // (test_driver.cc:2422) -- `std::vector::at` throws `std::out_of_range`
    // uncaught on an empty page list; indexing `pages[0]` panics the same
    // way on the fixture this test is designed for, preserving crash parity
    // at this exact point even though nothing downstream can use the page.
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    let _page = pages[0];

    // GAP(QPDFPageObjectHelper::forEachXObject / ::forEachImage /
    // ::forEachFormXObject / ::getImages / ::getFormXObjects): see this
    // function's own doc above.
    writeln!(stdout, "--- recursive, all ---")?;
    writeln!(stdout, "--- non-recursive, all ---")?;
    writeln!(stdout, "--- recursive, images ---")?;
    writeln!(stdout, "--- non-recursive, images ---")?;
    writeln!(stdout, "--- recursive, form XObjects ---")?;
    writeln!(stdout, "--- non-recursive, form XObjects ---")?;
    writeln!(stdout, "--- recursive, all, from fx1 ---")?;
    writeln!(stdout, "--- non-recursive, all, from fx1 ---")?;
    writeln!(stdout, "--- get images, page ---")?;
    writeln!(stdout, "--- get images, fx ---")?;
    writeln!(stdout, "--- get form XObjects, page ---")?;
    writeln!(stdout, "--- get form XObjects, fx ---")?;
    Ok(())
}
