use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{
    DecodeLevel, ObjectHandle, PageDocumentHelper, PageInput, PageObjectHelper, Pdf,
    PdfOpenOptions, PdfWriter,
};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};
use crate::output::write_bytes;

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
/// [`ObjectHandle::get_stream_data`] preserves the qpdf exception detail and
/// parsed source context at the canonical stream boundary. The driver catches
/// every error from this call, not only `Error::Unsupported`, exactly like
/// qpdf's `catch (std::exception&)`, then continues to the independent
/// all-decoded and raw reads.
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

    match qstream.get_stream_data(DecodeLevel::Generalized) {
        Ok(_) => writeln!(stdout, "oops -- didn't throw")?,
        Err(flpdf::Error::Unsupported(message)) => {
            writeln!(stdout, "get unfilterable stream: {message}")?
        }
        // qpdf's `catch (std::exception&)` (`qpdf/test_driver.cc:2373-2375`)
        // catches every accessor failure here, not only the unfilterable-
        // stream case, and always continues to the independent reads below.
        Err(error) => writeln!(stdout, "get unfilterable stream: {error}")?,
    }

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
/// `setFilterOnWrite(false)` forces `/S1` and `/S2` to retain their original
/// bytes while the writer still applies its requested specialized decode
/// policy to the other streams. This is the canonical ObjectHandle stream
/// state, not a test-driver-only output shortcut.
pub(crate) fn run_test_70<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer();
    trailer.get_key(b"/S1").set_filter_on_write(false)?;
    trailer.get_key(b"/S2").set_filter_on_write(false)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_decode_level(DecodeLevel::Specialized);
    writer.write()
}

/// qpdf source: `qpdf/test_driver.cc:2416-2457` (`test_71`).
///
/// Port qpdf's five XObject-enumeration calls through the canonical
/// `PageObjectHelper` surface (`libqpdf/QPDFPageObjectHelper.cc:318-395`).
/// The helper owns inherited-resource lookup, breadth-first recursive
/// traversal, canonical identity de-duplication, and the non-recursive map
/// helpers; this driver only formats qpdf's callback arguments.
fn write_xobject_line(
    output: &mut dyn Write,
    object: ObjectHandle,
    xobject_dict: ObjectHandle,
    key: Vec<u8>,
) -> flpdf::Result<()> {
    write_bytes(output, &xobject_dict.unparse())?;
    write!(output, " -> ")?;
    write_bytes(output, &key)?;
    write!(output, " -> ")?;
    write_bytes(output, &object.unparse())?;
    writeln!(output)?;
    Ok(())
}

fn write_xobject_map_line(
    output: &mut dyn Write,
    key: Vec<u8>,
    object: ObjectHandle,
) -> flpdf::Result<()> {
    write_bytes(output, &key)?;
    write!(output, " -> ")?;
    write_bytes(output, &object.unparse())?;
    writeln!(output)?;
    Ok(())
}
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
    let page_ref = pages[0];

    writeln!(stdout, "--- recursive, all ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.for_each_xobject(true, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- non-recursive, all ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.for_each_xobject(false, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- recursive, images ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.for_each_image(true, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- non-recursive, images ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.for_each_image(false, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- recursive, form XObjects ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.for_each_form_xobject(true, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- non-recursive, form XObjects ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.for_each_form_xobject(false, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }

    // qpdf obtains Fx1 directly from the page's resource dictionary after the
    // six page-level traversals, then constructs a helper over that Form.
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let resources = page.try_get_key(b"/Resources")?;
    pdf.resolve(&resources)?;
    let xobjects = resources.try_get_key(b"/XObject")?;
    pdf.resolve(&xobjects)?;
    let fx1 = xobjects.try_get_key(b"/Fx1")?;
    pdf.resolve(&fx1)?;

    writeln!(stdout, "--- recursive, all, from fx1 ---")?;
    {
        let mut form = PageObjectHelper::from_object_handle(fx1.clone(), pdf);
        form.for_each_xobject(true, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- non-recursive, all, from fx1 ---")?;
    {
        let mut form = PageObjectHelper::from_object_handle(fx1.clone(), pdf);
        form.for_each_xobject(false, |object, xobject_dict, key| {
            write_xobject_line(stdout, object, xobject_dict, key)
        })?;
    }
    writeln!(stdout, "--- get images, page ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        for (key, object) in page.get_images()? {
            write_xobject_map_line(stdout, key, object)?;
        }
    }
    writeln!(stdout, "--- get images, fx ---")?;
    {
        let mut form = PageObjectHelper::from_object_handle(fx1.clone(), pdf);
        for (key, object) in form.get_images()? {
            write_xobject_map_line(stdout, key, object)?;
        }
    }
    writeln!(stdout, "--- get form XObjects, page ---")?;
    {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        for (key, object) in page.get_form_xobjects()? {
            write_xobject_map_line(stdout, key, object)?;
        }
    }
    writeln!(stdout, "--- get form XObjects, fx ---")?;
    {
        let mut form = PageObjectHelper::from_object_handle(fx1, pdf);
        for (key, object) in form.get_form_xobjects()? {
            write_xobject_map_line(stdout, key, object)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_test_68, run_test_71};
    use flpdf::{Error, Pdf, PdfOpenOptions};
    use std::collections::BTreeMap;

    fn dct_qstream_pdf() -> Vec<u8> {
        let mut bytes =
            include_bytes!("../../../../tests/fixtures/test_driver/stream_dct.pdf").to_vec();
        let original = b"<< /Type /Catalog /Pages 2 0 R >>";
        let replacement = b"<< /Type /Catalog /Pages 2 0 R /QStream 6 0 R >>";
        let start = bytes
            .windows(original.len())
            .position(|window| window == original)
            .expect("catalog dictionary");
        bytes.splice(start..start + original.len(), replacement.iter().copied());
        bytes
    }

    fn non_stream_qstream_pdf() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /QStream 2 0 R >>\nendobj\n",
        );
        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
                .as_bytes(),
        );
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        bytes
    }

    fn unfiltered_qstream_pdf() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /QStream 3 0 R >>\nendobj\n",
        );
        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let off3 = bytes.len();
        bytes.extend_from_slice(b"3 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n");
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        bytes
    }

    #[test]
    fn test_68_catches_unfilterable_stream_and_continues_to_raw_data() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            dct_qstream_pdf(),
            PdfOpenOptions {
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open DCT stream fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_68(
            &mut pdf,
            b"stream_dct.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 68 should continue after the caught exception");

        assert!(stdout.starts_with(b"get unfilterable stream: "));
        assert!(stdout.ends_with(b"raw stream data okay\n"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_68_catches_every_accessor_error_like_qpdfs_broad_catch_boundary() {
        // qpdf's `catch (std::exception&)` (test_driver.cc:2373-2375) catches
        // ANY exception from the first `getStreamData()` call, not only the
        // unfilterable-stream case, and always continues to the next
        // (uncaught) read. A non-stream `/QStream` triggers a different
        // accessor error on the first call, which must still be printed
        // here; the second call (`get_stream_data(DecodeLevel::All)`, no
        // catch) then fails identically and propagates, matching qpdf's own
        // uncaught exception from that line.
        let mut pdf = Pdf::open_mem_owned_with_options(
            non_stream_qstream_pdf(),
            PdfOpenOptions {
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open non-stream fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        let error = run_test_68(
            &mut pdf,
            b"non-stream.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect_err("the second (uncaught) accessor call must still propagate");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "pipeStreamData called for non-stream"
        ));
        assert_eq!(
            stdout,
            b"get unfilterable stream: pipeStreamData called for non-stream\n"
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_68_reports_when_a_filterable_stream_does_not_throw() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            unfiltered_qstream_pdf(),
            PdfOpenOptions {
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open unfiltered fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_68(
            &mut pdf,
            b"unfiltered.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("filterable stream should complete");

        assert_eq!(stdout, b"oops -- didn't throw\n");
        assert!(stderr.is_empty());
    }

    fn nested_xobject_pdf() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        let objects = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources 4 0 R /Contents 5 0 R >>"
                    .as_slice(),
            ),
            (
                4,
                b"<< /XObject << /Fx1 6 0 R /Im1 8 0 R >> >>".as_slice(),
            ),
            (5, b"<< /Length 0 >>\nstream\n\nendstream".as_slice()),
            (
                6,
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 7 0 R /Length 0 >>\nstream\n\nendstream"
                    .as_slice(),
            ),
            (7, b"<< /XObject << /Fx2 9 0 R /Im2 8 0 R >> >>".as_slice()),
            (
                8,
                b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /Length 1 >>\nstream\nx\nendstream"
                    .as_slice(),
            ),
            (
                9,
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 10 0 R /Length 0 >>\nstream\n\nendstream"
                    .as_slice(),
            ),
            (10, b"<< /XObject << /Im3 8 0 R >> >>".as_slice()),
        ];
        for (number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 11\n0000000000 65535 f \n");
        for number in 1..=10 {
            bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 11 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn test_71_driver_emits_recursive_and_nonrecursive_xobject_callbacks() {
        let mut pdf = Pdf::open_mem_owned(nested_xobject_pdf()).expect("open XObject fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_71(
            &mut pdf,
            b"nested-xobject.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 71 should enumerate page and nested Form XObjects");

        assert!(stdout
            .windows(b" -> /Fx1 -> 6 0 R\n".len())
            .any(|window| { window == b" -> /Fx1 -> 6 0 R\n" }));
        assert!(stdout
            .windows(b" -> /Im1 -> 8 0 R\n".len())
            .any(|window| { window == b" -> /Im1 -> 8 0 R\n" }));
        assert!(stdout
            .windows(b" -> /Im2 -> 8 0 R\n".len())
            .any(|window| { window == b" -> /Im2 -> 8 0 R\n" }));
        assert!(stdout
            .windows(b"/Im1 -> 8 0 R\n".len())
            .any(|window| { window == b"/Im1 -> 8 0 R\n" }));
        assert!(stdout
            .windows(b"/Im2 -> 8 0 R\n".len())
            .any(|window| { window == b"/Im2 -> 8 0 R\n" }));
        assert!(stderr.is_empty());
    }
}
