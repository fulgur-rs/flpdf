//! Ports qpdf's `test_72` through `test_79` (`qpdf/test_driver.cc:2460-2758`
//! in pinned qpdf 11.9.0). See `test_0_1.rs` for the house style this file
//! follows: how `flpdf::Pdf` is driven, how repair diagnostics are threaded
//! through `super::emit_new_diagnostics`, and how deep qpdf-source-line
//! comments justify each translation decision.
//!
//! Two tests here (`run_test_78`, `run_test_79`) hit the same trailer-write
//! primitive gap already established in `driver/test_18_25.rs` (see that
//! file's own module doc and `run_test_20`'s `GAP` comment):
//! `Pdf::trailer().replace_key(...)` compiles and returns `Ok`, but
//! `PdfWriter::write` reads `Pdf::trailer()` (a plain `&Dictionary` set once
//! at construction, confirmed by `crates/flpdf/src/writer.rs:2865` reading
//! `pdf.trailer_dictionary().clone()` directly), never the disconnected
//! `trailer()` clone graph -- so a *new* trailer entry installed that
//! way never reaches the written file. Each affected function's own `GAP`
//! comment marks the precise call this blocks; qpdf statements that do not
//! read the blocked trailer entry are still translated normally on either
//! side of it.

use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{
    DecodeLevel, EmbeddedFileDocumentHelper, EmbeddedFileStream, Error, FileSpec, NameTree,
    NumberTree, ObjectHandle, ObjectHandleParserCallbacks, PageDocumentHelper, ParseControl, Pdf,
    PdfWriter, TokenFilter, TokenFilterOutput,
};

use super::emit_new_diagnostics;
use crate::output::write_bytes;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Resolve one canonical handle hop, matching qpdf's lazy accessor
/// dereference without following the flpdf-only reference-as-value redirect.
fn resolve_once<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
) -> flpdf::Result<ObjectHandle> {
    pdf.resolve(handle)?;
    Ok(handle.clone())
}

/// `handle.getKey(key)` plus the implicit dereference of the *returned*
/// child that qpdf's next accessor call on it would perform
/// (`QPDFObjectHandle::getKey`, `libqpdf/QPDFObjectHandle.cc:979-988`).
/// Mirrors `chase_key` in `driver/test_42_49.rs` (module-private there, so
/// reimplemented here rather than imported).
fn chase_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
) -> flpdf::Result<ObjectHandle> {
    let chased = resolve_once(pdf, handle)?;
    let child = chased.get_key(key);
    resolve_once(pdf, &child)
}

/// `handle.getArrayItem(index)` plus the same implicit dereference as
/// [`chase_key`], for the next accessor call qpdf's own chained
/// `getArrayItem(i).getKey(...)` performs. A missing/out-of-range item
/// resolves the same way qpdf's own out-of-bounds `getArrayItem` does: as a
/// null handle (`QPDFObjectHandle::getArrayItem`,
/// `libqpdf/QPDFObjectHandle.cc:1091-1100`, which warns and returns
/// `newNull()`; the warning itself is not reproduced here).
fn chase_array_item<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    index: usize,
) -> flpdf::Result<ObjectHandle> {
    let chased = resolve_once(pdf, handle)?;
    let item = chased
        .as_array()
        .and_then(|items| items.get(index).cloned())
        .unwrap_or_else(ObjectHandle::null);
    resolve_once(pdf, &item)
}

/// `QUtil::hex_encode` (`libqpdf/QUtil.cc:720-731`): lowercase hex, two
/// characters per byte, no separators. Mirrors the identical local helper in
/// `driver/test_34_41.rs`.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// test_72 (`test_driver.cc:2460-2486`, `ParserCallbacks` at `:98-134`,
// `TokenFilter` at `:136-157`)
// ---------------------------------------------------------------------------

/// Ports qpdf's own top-level `ParserCallbacks` test class
/// (`test_driver.cc:98-134`). A second, independent copy of the identically
/// named/shaped struct already exists in `driver/test_34_41.rs` as
/// `ContentParserCallbacks` (module-private there); this is not the same
/// Rust type, but both are faithful translations of the same qpdf C++
/// class, ported once per file per this crate's own convention.
struct DriverParserCallbacks<'a> {
    stdout: &'a mut dyn Write,
}

impl<'a> ObjectHandleParserCallbacks for DriverParserCallbacks<'a> {
    fn content_size(&mut self, size: usize) -> flpdf::Result<()> {
        writeln!(self.stdout, "content size: {size}")?;
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: ObjectHandle,
        offset: usize,
        length: usize,
    ) -> flpdf::Result<ParseControl> {
        if object.as_name().as_deref() == Some(b"Abort".as_slice()) {
            writeln!(self.stdout, "test suite: terminating parsing")?;
            return Ok(ParseControl::Stop);
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
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        writeln!(self.stdout, "-EOF-")?;
        Ok(())
    }
}

/// Ports qpdf's own `TokenFilter` test class (`test_driver.cc:136-157`).
/// `flpdf::Token`/`ContentToken` has no public constructor, so the two
/// synthetic replacement tokens qpdf's version constructs
/// (`Token(tt_string, "Salad")`, `Token(tt_name, "/bye")`) are written here
/// as their already-known canonical raw spellings -- see the identical
/// reasoning on `driver/test_34_41.rs`'s own `PotatoSaladTokenFilter`, whose
/// exact shape this mirrors (a second, independent copy per this crate's
/// per-file convention, not a shared import).
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

pub(crate) fn run_test_72<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // Call some QPDFPageObjectHelper methods on form XObjects.
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    let page = pdf.get_object_handle(page_refs[0]);
    let resources = chase_key(pdf, &page, b"/Resources")?;
    let xobject = chase_key(pdf, &resources, b"/XObject")?;
    let fx1 = chase_key(pdf, &xobject, b"/Fx1")?;

    // qpdf's `QPDFPageObjectHelper::parseContents`/`pipeContents`/
    // `addContentTokenFilter` all dispatch on `oh.isFormXObject()`
    // (`QPDFPageObjectHelper.cc:485-536`): the form-XObject arm calls
    // `oh.parseAsContents`, `oh.pipeStreamData(p, 0, qpdf_dl_specialized)`,
    // and `oh.addTokenFilter` respectively (the page arm instead calls
    // `oh.parsePageContents`/`oh.pipePageContents`/
    // `oh.addContentTokenFilter`, which coalesces `/Contents` -- the wrong
    // path for a bare stream). `/Fx1` is always a form XObject in this
    // test's fixture, so the assert below encodes that branch condition
    // (matching this file's `chase_key`-based dereference discipline)
    // rather than silently hardcoding the form-XObject arm.
    assert!(
        fx1.is_form_xobject()?,
        "test 72's /Resources/XObject/Fx1 must be a form XObject"
    );

    writeln!(stdout, "--- parseContents ---")?;
    let mut callbacks = DriverParserCallbacks { stdout };
    fx1.parse_as_contents(&mut callbacks)?;

    // Do this once with addContentTokenFilter and once with addTokenFilter
    // to show that they are the same and to ensure that addTokenFilter is
    // directly exercised in testing. Per the dispatch note above, both
    // qpdf calls reach `oh.addTokenFilter` for this form XObject, so both
    // loop iterations make the identical flpdf call; `addTokenFilter`
    // accumulates onto the stream's filter list rather than replacing it
    // (`ObjectHandle::add_token_filter`'s own doc), matching qpdf's own
    // `std::vector`-backed token filter list -- both filters registered
    // across the two iterations remain active for the second iteration's
    // pipe.
    for _ in 0..2 {
        let filter: std::rc::Rc<std::cell::RefCell<dyn TokenFilter>> =
            std::rc::Rc::new(std::cell::RefCell::new(PotatoSaladTokenFilter));
        fx1.add_token_filter(filter)?;
        let data = fx1.get_stream_data(DecodeLevel::Specialized)?;
        assert!(
            data.windows(4).any(|window| window == b"/bye"),
            "filtered form XObject content must contain /bye"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_73 (`test_driver.cc:2488-2500`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_73<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // GAP(QPDF::QPDF() default constructor / unprocessed-file state): qpdf
    // constructs a second, wholly unprocessed `QPDF pdf2` and calls
    // `pdf2.getRoot()`. `QPDF::getRoot` reads `m->trailer.getKey("/Root")`
    // (`libqpdf/QPDF.cc:2354-2368`); on a default-constructed `QPDF`,
    // `m->trailer` is never assigned, so this resolves to a non-dictionary
    // and `getRoot` throws `damagedPDF("", 0, "unable to find /Root
    // dictionary")`, printed to stderr as "getRoot: <e.what()>". flpdf's
    // `Pdf<R>::open`/`open_mem*` constructors always parse an actual byte
    // source immediately; there is no "constructed but never processed"
    // state to reproduce this call on, so the try/catch is not ported and
    // its one stderr line is not emitted. This is test_73's only qpdf
    // output that depends on this gap -- the remainder below is real,
    // faithful translation of the rest of the function.

    // GAP(QPDF::closeInputSource): closes the underlying `InputSource`
    // while keeping already-parsed objects live in the object cache, so
    // the getRoot() call below is exercised without further disk access
    // (`libqpdf/QPDF.cc:278`; `include/qpdf/QPDF.hh:166`'s own doc: "may be
    // called ... after all processing has been done"). flpdf's `Pdf<R>`
    // has no equivalent operation to detach/close its reader while
    // retaining cached state, so this call is not ported. `pdf.getRoot()`
    // itself returns a `std::string` from `unparseResolved()` that is
    // immediately discarded by the C++ statement below, so this has no
    // observable output of its own in the success path either way -- only
    // its ability to *not* error is exercised, which the real call below
    // still exercises against the still-open reader.
    let root_seed = pdf.trailer_key_handle(b"Root");
    let root = resolve_once(pdf, &root_seed)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    if root.as_dictionary().is_none() {
        return Err(Error::System("unable to find /Root dictionary".to_string()));
    }
    let pages_seed = root.get_key(b"/Pages");
    let pages = resolve_once(pdf, &pages_seed)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let _ = pages.unparse_resolved();
    Ok(())
}

// ---------------------------------------------------------------------------
// test_74 (`test_driver.cc:2502-2545`) -- designed for split-nntree.pdf
// ---------------------------------------------------------------------------

pub(crate) fn run_test_74<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    writeln!(stdout, "/Split1")?;
    let mut split1 = NumberTree::new(pdf.trailer_key_handle(b"Split1"), true);
    split1.set_split_threshold(4);
    for key in [15_i64, 35, 125] {
        let value = ObjectHandle::string(key.to_string().into_bytes());
        let inserted = split1.insert(pdf, key, value)?;
        assert_eq!(
            inserted
                .current()
                .expect("insert returns a cursor at the inserted key")
                .0,
            key
        );
    }
    let mut cursor = split1.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, _value)) = cursor.current() {
        writeln!(stdout, "{key}")?;
        cursor.next(&mut split1, pdf)?;
    }

    writeln!(stdout, "/Split2")?;
    let mut split2 = NameTree::new(pdf.trailer_key_handle(b"Split2"), true);
    split2.set_split_threshold(4);
    let value = ObjectHandle::string(flpdf::pdf_string::new_unicode_string(b"C"));
    let inserted = split2.insert(pdf, b"C", value)?;
    assert_eq!(
        inserted
            .current()
            .expect("insert returns a cursor at the inserted key")
            .0
            .as_slice(),
        b"C"
    );
    let mut cursor = split2.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, _value)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut split2, pdf)?;
    }

    writeln!(stdout, "/Split3")?;
    let mut split3 = NameTree::new(pdf.trailer_key_handle(b"Split3"), true);
    split3.set_split_threshold(4);
    // "\xcf\x80" is the raw UTF-8 bytes qpdf's C++ string literal
    // holds -- the two-byte encoding of U+03C0 (pi) -- passed as-is as the
    // name-tree key, matching qpdf's `check_split2` lambda which never
    // transcodes `k` before using it as the tree key (only the *value*
    // goes through `newUnicodeString`).
    for key in [&b"P"[..], &b"\xcf\x80"[..]] {
        let value = ObjectHandle::string(flpdf::pdf_string::new_unicode_string(key));
        let inserted = split3.insert(pdf, key, value)?;
        assert_eq!(
            inserted
                .current()
                .expect("insert returns a cursor at the inserted key")
                .0
                .as_slice(),
            key
        );
    }
    let mut cursor = split3.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write_bytes(stdout, &key)?;
        write!(stdout, " ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut split3, pdf)?;
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_qdf_mode(true);
    writer.write()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_75 (`test_driver.cc:2547-2603`) -- designed for erase-nntree.pdf
// ---------------------------------------------------------------------------

pub(crate) fn run_test_75<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf's own function has no `std::cout` calls at all -- its entire
    // observable surface is assertions plus the closing `QPDFWriter` write.
    let mut erase1 = NameTree::new(pdf.trailer_key_handle(b"Erase1"), true);
    assert!(erase1.remove(pdf, b"1X")?.is_none());
    let removed = erase1.remove(pdf, b"1C")?.expect("1C must be present");
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let removed_text = removed
        .as_string()
        .as_deref()
        .map(flpdf::pdf_string::utf8_value)
        .unwrap_or_default();
    assert_eq!(removed_text, b"c");
    let mut iter1 = erase1.find(pdf, b"1B", false)?;
    iter1.remove(&mut erase1, pdf)?;
    assert_eq!(
        iter1.current().expect("cursor at 1D after removing 1B").0,
        b"1D"
    );
    iter1.remove(&mut erase1, pdf)?;
    assert!(iter1 == erase1.end());
    iter1.previous(&mut erase1, pdf)?;
    assert_eq!(
        iter1
            .current()
            .expect("cursor at 1A after stepping back from end")
            .0,
        b"1A"
    );
    iter1.remove(&mut erase1, pdf)?;
    assert!(iter1 == erase1.end());

    let erase2_handle = pdf.trailer_key_handle(b"Erase2");
    let mut erase2 = NumberTree::new(erase2_handle.clone(), true);
    let mut iter2 = erase2.find(pdf, 250, false)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    iter2.remove(&mut erase2, pdf)?;
    assert!(iter2 == erase2.end());
    iter2.previous(&mut erase2, pdf)?;
    assert_eq!(
        iter2
            .current()
            .expect("cursor at 240 after stepping back")
            .0,
        240
    );
    let kids = chase_key(pdf, &erase2_handle, b"/Kids")?;
    let kid1 = chase_array_item(pdf, &kids, 1)?;
    let limits1 = chase_key(pdf, &kid1, b"/Limits")?;
    let limits1_items = limits1.as_array().unwrap_or_default();
    let limit1_low = resolve_once(pdf, &limits1_items[0])?;
    let limit1_high = resolve_once(pdf, &limits1_items[1])?;
    assert_eq!(limit1_low.as_integer(), Some(230));
    assert_eq!(limit1_high.as_integer(), Some(240));

    let mut iter2b = erase2.find(pdf, 210, false)?;
    iter2b.remove(&mut erase2, pdf)?;
    assert_eq!(
        iter2b
            .current()
            .expect("cursor at 220 after removing 210")
            .0,
        220
    );
    let kids = chase_key(pdf, &erase2_handle, b"/Kids")?;
    let kid0 = chase_array_item(pdf, &kids, 0)?;
    let limits0 = chase_key(pdf, &kid0, b"/Limits")?;
    let limits0_items = limits0.as_array().unwrap_or_default();
    let limit0_low = resolve_once(pdf, &limits0_items[0])?;
    let limit0_high = resolve_once(pdf, &limits0_items[1])?;
    assert_eq!(limit0_low.as_integer(), Some(220));
    assert_eq!(limit0_high.as_integer(), Some(220));
    let kid0_kids = chase_key(pdf, &kid0, b"/Kids")?;
    assert_eq!(kid0_kids.as_array().unwrap_or_default().len(), 1);

    let mut erase3 = NumberTree::new(pdf.trailer_key_handle(b"Erase3"), true);
    let mut iter3 = erase3.find(pdf, 320, false)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    iter3.remove(&mut erase3, pdf)?;
    assert!(iter3 == erase3.end());
    erase3.remove(pdf, 310)?;
    assert!(erase3.begin(pdf)? == erase3.end());

    let mut erase4 = NumberTree::new(pdf.trailer_key_handle(b"Erase4"), true);
    let mut iter4 = erase4.find(pdf, 420, false)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    iter4.remove(&mut erase4, pdf)?;
    assert_eq!(
        iter4.current().expect("cursor at 430 after removing 420").0,
        430
    );

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_qdf_mode(true);
    writer.write()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_76 (`test_driver.cc:2605-2648`) -- arg2 is a file to attach
// ---------------------------------------------------------------------------

pub(crate) fn run_test_76<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let arg2 = arg2.expect("test 76 requires arg2 (a file to attach)");

    // `QPDFFileSpecObjectHelper::createFileSpec(pdf, "att1.txt", arg2)`
    // resolves to the (filename, fullpath) overload
    // (`include/qpdf/QPDFFileSpecObjectHelper.hh:71`), which reads `arg2`
    // from disk -- `FileSpec::create_file_spec_from_path` is that overload.
    let fs1_handle = FileSpec::create_file_spec_from_path(pdf, b"att1.txt", arg2)?;
    {
        let mut fs1 = FileSpec::new(fs1_handle.clone(), pdf)?;
        fs1.set_description(b"some text")?;
    }
    let efs1_stream = {
        let mut fs1 = FileSpec::new(fs1_handle.clone(), pdf)?;
        fs1.get_embedded_file_stream("")?
    };
    {
        let mut efs1 = EmbeddedFileStream::new(efs1_stream.clone(), pdf)?;
        efs1.set_subtype(b"text/plain")?;
        efs1.set_creation_date(b"D:20210207191121-05'00'")?;
        efs1.set_mod_date(b"D:20210208001122Z")?;
    }
    {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        efdh.replace_embedded_file(b"att1", fs1_handle.clone())?;
    }

    // qpdf's `createEFStream(pdf, "from string")` (the `std::string`
    // overload) and `createEFStream(pdf, p.getBufferSharedPointer())` (the
    // `shared_ptr<Buffer>` overload) both create a stream from
    // already-materialized bytes; `EmbeddedFileStream::create_ef_stream`
    // takes `impl AsRef<[u8]>` and covers both -- the `Pl_Buffer`
    // `writeCStr`/`finish`/`operator<<` ceremony qpdf's second call uses to
    // build "from buffer" is pipeline-API exercise with no effect on the
    // resulting bytes, so it is not reproduced; the literal byte content is
    // identical either way.
    let efs2_stream = EmbeddedFileStream::create_ef_stream(pdf, b"from string")?;
    {
        let mut efs2 = EmbeddedFileStream::new(efs2_stream.clone(), pdf)?;
        efs2.set_subtype(b"text/plain")?;
    }

    let efs3_stream = EmbeddedFileStream::create_ef_stream(pdf, b"from buffer")?;
    {
        let mut efs3 = EmbeddedFileStream::new(efs3_stream.clone(), pdf)?;
        efs3.set_subtype(b"text/plain")?;
    }

    let fs2_handle = FileSpec::create_file_spec(pdf, b"att2.txt", efs2_stream.clone())?;
    {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        efdh.replace_embedded_file(b"att2", fs2_handle)?;
    }

    let fs3_handle = FileSpec::create_file_spec(pdf, b"att3.txt", efs3_stream.clone())?;
    {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        efdh.replace_embedded_file(b"att3", fs3_handle.clone())?;
    }
    {
        let mut fs3 = FileSpec::new(fs3_handle, pdf)?;
        // "\xcf\x80.txt" is pi (U+03C0) + ".txt" in UTF-8, matching qpdf's
        // C++ string literal byte-for-byte.
        fs3.set_filename(b"\xcf\x80.txt", Some(b"att3.txt"))?;
    }

    {
        let efs1 = EmbeddedFileStream::new(efs1_stream, pdf)?;
        assert_eq!(efs1.get_creation_date()?, b"D:20210207191121-05'00'");
        assert_eq!(efs1.get_mod_date()?, b"D:20210208001122Z");
    }
    {
        let efs2 = EmbeddedFileStream::new(efs2_stream, pdf)?;
        assert_eq!(efs2.get_size()?, 11);
        assert_eq!(efs2.get_subtype()?, b"text/plain");
        assert_eq!(
            hex_encode(&efs2.get_checksum()?),
            "2fce9c8228e360ba9b04a1bd1bf63d6b"
        );
    }

    // qpdf's `efdh.getEmbeddedFiles()` returns `std::map<std::string,
    // shared_ptr<QPDFFileSpecObjectHelper>>`, so iteration is ascending-key
    // order (matching `BTreeMap`'s own order). Collect the handles first,
    // then drop `efdh` before wrapping each in its own `FileSpec` -- both
    // borrow `pdf` mutably and cannot be alive at once.
    let embedded_files = {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        efdh.get_embedded_files()?
    };
    for (key, handle) in &embedded_files {
        let mut fs = FileSpec::new(handle.clone(), pdf)?;
        let filename = fs.get_filename()?;
        write_bytes(stdout, key)?;
        write!(stdout, " -> ")?;
        write_bytes(stdout, &filename)?;
        writeln!(stdout)?;
    }

    let att1 = {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        efdh.get_embedded_file(b"att1")?
    };
    let att1 = att1.expect("efdh.getEmbeddedFile(\"att1\") must find the just-replaced entry");
    {
        let mut fs = FileSpec::new(att1, pdf)?;
        assert_eq!(fs.get_filename()?, b"att1.txt");
    }
    let potato = {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        efdh.get_embedded_file(b"potato")?
    };
    assert!(potato.is_none());

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_qdf_mode(true);
    writer.write()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_77 (`test_driver.cc:2650-2661`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_77<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    {
        let mut efdh = EmbeddedFileDocumentHelper::new(pdf);
        assert!(efdh.remove_embedded_file(b"att2")?);
        assert!(!efdh.remove_embedded_file(b"att2")?);
    }

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_qdf_mode(true);
    writer.write()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_78 (`test_driver.cc:2663-2702`) -- functional replaceStreamData
// ---------------------------------------------------------------------------

pub(crate) fn run_test_78<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // f1: the simple, non-retry-aware provider form.
    let s1 = pdf.new_stream()?;
    s1.replace_stream_data_with_callback(
        |pipeline| {
            pipeline.write(b"potato").map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            Ok(())
        },
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    )?;

    // f2: the retry-aware provider form. qpdf's lambda captures
    // `std::cerr` -- a process-global stream, unaffected by this test
    // driver's redirected diagnostic channel -- so this closure writes
    // directly to `std::io::stderr()` rather than to this function's own
    // `stderr: &mut dyn Write` parameter: `StreamDataProvider` callbacks
    // are `Fn(...) + 'static` (`object_handle.rs:4289`), which cannot
    // borrow this call's non-'static `stderr` reference. In the real
    // `flpdf-test-driver` binary (`driver.rs`) both resolve to the process's
    // real stderr, but this diverges from the injected-writer contract
    // other functions in this crate follow; see this file's own caveats.
    let s2 = pdf.new_stream()?;
    s2.replace_stream_data_with_retry_callback(
        |pipeline, suppress_warnings, will_retry| {
            let mut stderr = std::io::stderr();
            writeln!(stderr, "f2").map_err(Error::from)?;
            if will_retry {
                writeln!(stderr, "failing").map_err(Error::from)?;
                return Ok(false);
            }
            if !suppress_warnings {
                writeln!(stderr, "warning").map_err(Error::from)?;
            }
            pipeline.write(b"salad").map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            writeln!(stderr, "f2 done").map_err(Error::from)?;
            Ok(true)
        },
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    )?;

    // GAP(QPDFObjectHandle::replaceKey on the trailer): qpdf installs
    // `/Streams [s1 s2]` here as a *new* trailer entry so the closing
    // `QPDFWriter` serializes both streams. Per the established gap (see
    // this file's own module doc and `driver/test_18_25.rs`'s
    // `run_test_20`): `Pdf::trailer().replace_key(...)` has no
    // effect on what `PdfWriter::write` emits, so this installation is not
    // performed. `s1`/`s2` and their providers above are still real,
    // faithful translation -- only their reachability from the eventual
    // write is what this gap removes.
    writeln!(stdout, "piping with warning suppression")?;

    // GAP(QPDFObjectHandle::pipeStreamData with explicit suppress_warnings/
    // will_retry arguments): qpdf calls `s2.pipeStreamData(&d, nullptr, 0,
    // qpdf_dl_all, true, false)` to invoke `f2` directly with a
    // caller-chosen `suppress_warnings = true`, `will_retry = false`. The
    // only flpdf method that threads a per-call `suppress_warnings`
    // argument into a `StreamDataProvider` is
    // `ObjectHandle::pipe_stream_data` (`object_handle.rs:4472`), which is
    // `pub(crate)` and not reachable from this crate;
    // `Pdf::set_suppress_warnings` is an unrelated document-wide flag, not
    // this per-call argument. This explicit debug pipe is not performed, so
    // the "f2"/"warning"/"f2 done" stderr lines it alone would produce here
    // are not emitted by this call (independent of whatever `f2`'s own
    // invocation from a real `PdfWriter::write` -- were the trailer gap
    // above not also blocking that -- would separately produce).

    writeln!(stdout, "writing")?;
    // The `QPDFWriter` write itself is not performed: with `/Streams`
    // unreachable per the gap above, a write here would produce a file that
    // diverges from qpdf's own output, which this port must not fabricate.
    Ok(())
}

// ---------------------------------------------------------------------------
// test_79 (`test_driver.cc:2704-2758`) -- stream copier
// ---------------------------------------------------------------------------

pub(crate) fn run_test_79<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let copies = ObjectHandle::array(Vec::new());
    // GAP(QPDFObjectHandle::replaceKey on the trailer): qpdf installs
    // `copies` as `/Copies` here, and later builds
    // `QPDFObjectHandle::newArray(streams)` and installs that as
    // `/Originals` the same way (`test_driver.cc:2738`). Per the
    // established gap (see this file's own module doc and
    // `driver/test_18_25.rs`'s `run_test_20`): neither
    // `Pdf::trailer().replace_key(...)` call has any effect on what
    // `PdfWriter::write` emits, so neither installation (nor a closing
    // `QPDFWriter` write, which qpdf's own test_79 does not even reach
    // without them) is performed below. `copies` itself is still built and
    // populated by the loop below for its own sake -- every other statement
    // in this test operates on `pdf`'s canonical object graph directly and
    // is real, faithful, trailer-independent logic.

    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    let page = pdf.get_object_handle(page_refs[0]);
    let s1 = chase_key(pdf, &page, b"/Contents")?;

    let s2 = pdf.new_stream()?;
    s2.replace_stream_data(
        std::rc::Rc::new(b"from string".to_vec()),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );
    let indirect_16059 = pdf.make_indirect_object_handle(ObjectHandle::integer(16059))?;
    // qpdf builds this dictionary by string-concatenating
    // `pdf.makeIndirectObject(...).unparse()` into a literal and parsing it
    // through the two-argument `QPDFObjectHandle::parse(QPDF*, string)`,
    // which resolves embedded "N G R" syntax against `pdf`. flpdf's
    // single-argument `ObjectHandle::parse` explicitly rejects a nested
    // indirect reference (its own doc: "a nested indirect reference is
    // rejected because no document can canonicalize it"). Building the
    // dictionary directly from `indirect_16059` -- already an indirect
    // handle owned by `pdf` -- produces the identical object graph without
    // that primitive: an indirect `ObjectHandle` placed as a dictionary
    // value serializes as its own reference, not inlined
    // (`ObjectHandle::dictionary`'s own doc: "cloning or re-reading this
    // dictionary's entries never deep-copies their subtrees").
    let stuff = ObjectHandle::dictionary(vec![
        (b"/Direct".to_vec(), ObjectHandle::integer(3)),
        (b"/Indirect".to_vec(), indirect_16059),
    ]);
    let s2_dict = s2
        .as_stream_dict()
        .expect("Pdf::new_stream always yields a stream handle");
    s2_dict.replace_key(b"/Stuff", stuff)?;
    s2_dict.replace_key(b"/Other", ObjectHandle::string(b"other stuff".to_vec()))?;

    let s3 = pdf.new_stream()?;
    s3.replace_stream_data(
        std::rc::Rc::new(b"from buffer".to_vec()),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );

    let streams = [s1, s2, s3];
    for (index, orig) in streams.iter().enumerate() {
        let i = index + 1;
        let istr = i.to_string();
        let orig_data = orig.get_stream_data(DecodeLevel::Generalized)?;
        let copy = orig.copy_stream()?;
        let copy_dict = copy
            .as_stream_dict()
            .expect("ObjectHandle::copy_stream always yields a stream handle");
        copy_dict.replace_key(
            b"/Other",
            ObjectHandle::string(format!("other: {istr}").into_bytes()),
        )?;
        orig.replace_stream_data(
            std::rc::Rc::new(format!("something new {istr}").into_bytes()),
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        );
        let copy_data = copy.get_stream_data(DecodeLevel::Generalized)?;
        assert_eq!(orig_data.len(), copy_data.len());
        assert_eq!(orig_data.as_slice(), copy_data.as_slice());
        copies.append_array_item(copy)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{chase_array_item, chase_key, resolve_once, run_test_73};
    use flpdf::{ObjectHandle, Pdf, PdfOpenOptions};

    fn minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions::default(),
        )
        .expect("open minimal fixture")
    }

    #[test]
    fn canonical_helpers_resolve_one_hop_and_preserve_handles() {
        let mut pdf = minimal_pdf();
        let root = pdf.trailer_key_handle(b"Root");
        let resolved = resolve_once(&mut pdf, &root).expect("resolve root");
        assert_eq!(resolved.object_ref(), root.object_ref());

        let pages = chase_key(&mut pdf, &root, b"/Pages").expect("resolve /Pages");
        assert_eq!(
            pages.object_ref().map(|object_ref| object_ref.number),
            Some(2)
        );

        let array = ObjectHandle::array(vec![ObjectHandle::integer(7)]);
        assert_eq!(
            chase_array_item(&mut pdf, &array, 0)
                .expect("resolve array item")
                .as_integer(),
            Some(7)
        );
        assert!(chase_array_item(&mut pdf, &array, 1)
            .expect("resolve missing array item")
            .is_null());
    }

    #[test]
    fn test_73_resolves_root_and_pages_without_legacy_chasing() {
        let mut pdf = minimal_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_73(
            &mut pdf,
            b"minimal.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 73");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }
}
