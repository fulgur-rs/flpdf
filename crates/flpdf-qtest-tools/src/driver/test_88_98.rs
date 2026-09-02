//! test_88 through test_98 (`qpdf/test_driver.cc:3106-3454`).
//!
//! Several bodies in this range open on a qpdf primitive with no flpdf
//! counterpart at any visibility (`QPDF::createFromJSON`/`updateFromJSON`,
//! `QPDFObjectHandle::getJSON`/`writeJSON`/`getStreamJSON`,
//! `QPDFPageObjectHelper`'s identity-preserving box getters). Those bodies
//! are `// GAP(...)` stubs per this crate's translation contract; every
//! `GAP` comment below names the missing qpdf symbol and stops at the exact
//! line that needs it.

use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::json_inspect::{DecodeLevel, StreamDataMode};
use flpdf::{
    document_json, Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf,
    Pipeline, PipelineError, PipelineResult,
};

use super::{
    crt_open_error_message, emit_new_diagnostics, open_error_bytes, os_str_diagnostic_bytes,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// qpdf's `QPDF::getRoot` (`libqpdf/QPDF.cc:2355-2368`) additionally throws
/// `damagedPDF` when `/Root` does not resolve to a dictionary and, under
/// `check_mode`, repairs a missing/invalid `/Type`. This port only reaches
/// for the handle itself; a caller that needs the dictionary type checked
/// resolves it through [`resolved_key`]/`Pdf::resolve_handle`
/// and inspects it directly -- the same substitute this crate's own
/// `test_34_41.rs` uses via its own (non-shared, so not reused here) private
/// `root_handle`.
fn root_handle<R: Read + Seek>(pdf: &mut Pdf<R>) -> ObjectHandle {
    pdf.trailer_key_handle(b"Root")
}

/// qpdf's `getKey` dereferences its result internally -- every
/// `QPDFObjectHandle.cc` dictionary accessor reaches `asDictionary()` /
/// `dereference()` before inspecting a child's type -- while
/// `ObjectHandle::get_key` explicitly does not ("never performs resolution
/// itself", `get_key`'s own doc). Every multi-hop qpdf
/// `getKey(...).getKey(...)` chain ported in this file resolves each hop
/// through this helper instead of a bare `get_key`.
fn resolved_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    parent: &ObjectHandle,
    key: &[u8],
) -> flpdf::Result<ObjectHandle> {
    pdf.resolve(parent)?;
    let child = parent.get_key(key);
    pdf.resolve(&child)?;
    Ok(child)
}

/// `QPDFObjectHandle::isScalar` (`libqpdf/QPDFObjectHandle.cc:450-453`):
/// `isBool() || isInteger() || isName() || isNull() || isReal() ||
/// isString()`, i.e. ordinals `2..=7` in [`ObjectHandle::type_code`]'s own
/// table (`null`, `boolean`, `integer`, `real`, `string`, `name`).
fn is_scalar(handle: &ObjectHandle) -> flpdf::Result<bool> {
    Ok(matches!(handle.type_code()?, 2..=7))
}

/// `QPDFObjectHandle::isDestroyed` (`libqpdf/QPDFObjectHandle.cc:333-336`):
/// `dereference() && obj->getTypeCode() == ot_destroyed`. `type_code()`'s
/// own doc documents ordinal `14` as this port's `Destroyed` state -- the
/// value a resolved indirect handle is left with once its owning [`Pdf`] is
/// dropped (`Pdf`'s own `Drop` impl, `pdf.rs:194-208`).
fn is_destroyed(handle: &ObjectHandle) -> flpdf::Result<bool> {
    Ok(handle.type_code()? == 14)
}

// ---------------------------------------------------------------------------
// test_88 (test_driver.cc:3106-3160)
// ---------------------------------------------------------------------------

/// Exercise the mutate/get accessor family added in qpdf 11
/// (`replaceKeyAndGetNew`/`Old`, `appendItemAndGetNew`,
/// `insertItemAndGetNew`, `eraseItemAndGetOld`, `removeKeyAndGetOld`) over
/// an entirely direct, in-memory object graph, then two error-path
/// array-erase calls against `pdf`'s own root.
pub(crate) fn run_test_88<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let dict = ObjectHandle::dictionary(vec![]);
    dict.replace_key(b"/One", ObjectHandle::integer(1))?;
    dict.replace_key(b"/Two", ObjectHandle::integer(2))?;
    let three = dict.replace_key_and_get_new(b"/Three", ObjectHandle::array(vec![]))?;
    three.append_array_item(ObjectHandle::parse(b"(a)")?)?;
    three.append_array_item(ObjectHandle::parse(b"(b)")?)?;
    let newdict = three.append_array_item_and_get_new(ObjectHandle::dictionary(vec![]))?;
    newdict.replace_key(b"/Z", ObjectHandle::parse(b"/Y")?)?;
    newdict.replace_key(b"/X", ObjectHandle::parse(b"/W")?)?;
    dict.replace_key(b"/Quack", ObjectHandle::parse(b"[1 2 3]")?)?;
    let quack = dict.replace_key_and_get_old(b"/Quack", ObjectHandle::parse(b"/Moo")?)?;
    assert_eq!(quack.unparse(), b"[ 1 2 3 ]");
    let nothing = dict.replace_key_and_get_old(b"/NotThere", ObjectHandle::null())?;
    assert!(nothing.is_null());
    assert_eq!(
        dict.unparse(),
        ObjectHandle::parse(
            b"<< /One 1 /Quack /Moo /Two 2 /Three [ (a) (b) << /Z /Y /X /W >> ] >>"
        )?
        .unparse()
    );

    let arr = dict.get_key(b"/Three");
    arr.insert_array_item(0, ObjectHandle::string(b"0".to_vec()))?;
    arr.insert_array_item(0, ObjectHandle::string(b"00".to_vec()))?;
    assert_eq!(
        arr.unparse(),
        ObjectHandle::parse(b"[ (00) (0) (a) (b) << /Z /Y /X /W >> ]")?.unparse()
    );
    let new_dict =
        arr.insert_array_item_and_get_new(1, ObjectHandle::parse(b"<< /P /Q /R /S >>")?)?;
    arr.erase_array_item(2)?;
    arr.erase_array_item(0)?;
    assert_eq!(
        arr.unparse(),
        ObjectHandle::parse(b"[ << /P /Q /R /S >> (a) (b) << /Z /Y /X /W >> ]")?.unparse()
    );

    // `new_dict` shares internals with the same element in `arr` --
    // qpdf's own comment at test_driver.cc:3140-3143 -- so mutating
    // `new_dict` below is observable through `arr.unparse()`.
    new_dict.remove_key(b"/R");
    new_dict.replace_key(b"/T", ObjectHandle::parse(b"/U")?)?;
    assert_eq!(
        arr.unparse(),
        ObjectHandle::parse(b"[ << /P /Q /T /U >> (a) (b) << /Z /Y /X /W >> ]")?.unparse()
    );
    let s = arr.erase_array_item_and_get_old(1)?;
    assert_eq!(s.unparse(), b"(a)");
    assert_eq!(
        arr.unparse(),
        ObjectHandle::parse(b"[ << /P /Q /T /U >> (b) << /Z /Y /X /W >> ]")?.unparse()
    );

    assert!(new_dict.remove_key_and_get_old(b"/M")?.is_null());
    assert_eq!(new_dict.remove_key_and_get_old(b"/P")?.unparse(), b"/Q");
    assert_eq!(
        new_dict.unparse(),
        ObjectHandle::parse(b"<< /T /U >>")?.unparse()
    );

    // Test errors (test_driver.cc:3155-3159).
    let root = root_handle(pdf);
    pdf.resolve(&root)?;
    let root = root.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let arr2 = root.replace_key_and_get_new(b"/QTest", ObjectHandle::parse(b"[1 2]")?)?;
    arr2.set_object_description(pdf, "test array")?;
    assert!(arr2.erase_array_item_and_get_old(50)?.is_null());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(root.erase_array_item_and_get_old(0)?.is_null());
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_89 (test_driver.cc:3162-3172)
// ---------------------------------------------------------------------------

/// Generate object warnings via type-mismatched mutations after qpdf's
/// `QPDF::createFromJSON` document construction (`qpdf/test_driver.cc:3162-3172`).
/// The caller supplies the newly-created live JSON document, so all mutation
/// and warning operations remain on the canonical ObjectHandle graph.
pub(crate) fn run_test_89<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let null = ObjectHandle::null();

    // `getTrailer()`/`getRoot()` (test_driver.cc:3168-3169) dereference
    // internally. `Pdf::trailer()` already returns a direct,
    // resolved dictionary value, so `append_array_item` observes the real
    // type without an extra resolve step; `appendItem` on a dictionary
    // warns `typeWarning("array", "ignoring attempt to append item")` and
    // is a no-op (`QPDFObjectHandle.cc:916-925`), which
    // `ObjectHandle::append_array_item`'s own `prepare_array_mutation`
    // reproduces through this crate's warning pipeline.
    let trailer = pdf.trailer();
    trailer.append_array_item(null.clone())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let root = pdf.root_handle()?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    root.append_array_item(null.clone())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let object5_ref = pdf.get_object_handle(ObjectRef::new(5, 0));
    pdf.resolve(&object5_ref)?;
    let object5 = object5_ref.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    object5.replace_key(b"/X", null.clone())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    // qpdf's getArrayItem(0) dereferences the receiver and uses its warning
    // boundary on a non-array or invalid index. The canonical signed-index
    // accessor has the same contract and returns the live child handle.
    let item0 = object5.try_get_array_item(0)?;
    item0.replace_key(b"/X", null)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_90 (test_driver.cc:3174-3185)
// ---------------------------------------------------------------------------

/// Generate an object warning via `QPDF::updateFromJSON`. Crafted to work
/// with `good13.pdf` and `various-updates.json` (the JSON file is `arg2`).
pub(crate) fn run_test_90<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let arg2 = arg2.expect("test 90 requires an update JSON filename");
    let arg2_diagnostic = os_str_diagnostic_bytes(arg2);

    // qpdf's `FileInputSource(arg2)` (behind `updateFromJSON(std::string
    // const&)`, `QPDF.cc:809-811`) reports a failed open as
    // `"open " + filename + ": " + strerror(errno)`
    // (`QUtil::safe_fopen`/`QPDFSystemError::createWhat`,
    // `QUtil.cc:490-518`, `QPDFSystemError.cc:12-28`). Open the file at this
    // driver boundary and translate through the same CRT-message route the
    // ordinary driver file opens use, instead of surfacing
    // `update_from_json_file`'s own `Error::FileIo` text verbatim.
    let source = std::fs::File::open(arg2).map_err(|error| {
        let crt_message = crt_open_error_message(arg2);
        let message = open_error_bytes(&arg2_diagnostic, crt_message.as_deref(), &error);
        Error::System(String::from_utf8_lossy(&message).into_owned())
    })?;
    let input_name = String::from_utf8_lossy(arg2_diagnostic.as_ref()).into_owned();
    // qpdf's own warning callback prints each `importJSON` validation
    // warning synchronously as the reactor records it, before the
    // "errors found in JSON" exception unwinds (`QPDF_json.cc`). Drain the
    // retained diagnostics before propagating a soft-validation failure so
    // this driver does not silently drop them behind the generic terminal
    // message.
    if let Err(error) = pdf.update_from_json(source, input_name) {
        emit_new_diagnostics(pdf, diagnostics_written, &arg2_diagnostic, stdout, stderr)?;
        return Err(error);
    }
    emit_new_diagnostics(pdf, diagnostics_written, &arg2_diagnostic, stdout, stderr)?;

    // Keep the update source as the diagnostic name for mutations whose live
    // values were installed by JSON; qpdf's final root mutation belongs to
    // the original PDF and uses filename instead.
    let null = ObjectHandle::null();
    let trailer = pdf.trailer();
    trailer.append_array_item(null.clone())?;
    emit_new_diagnostics(pdf, diagnostics_written, &arg2_diagnostic, stdout, stderr)?;

    let qtest = resolved_key(pdf, &trailer, b"/QTest")?;
    qtest.append_array_item(null.clone())?;
    emit_new_diagnostics(pdf, diagnostics_written, &arg2_diagnostic, stdout, stderr)?;

    let strings = resolved_key(pdf, &qtest, b"/strings")?;
    strings.try_get_int_value()?;
    emit_new_diagnostics(pdf, diagnostics_written, &arg2_diagnostic, stdout, stderr)?;

    let root = pdf.root_handle()?;
    root.append_array_item(null)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_91 (test_driver.cc:3187-3193)
// ---------------------------------------------------------------------------

/// A `Pl_StdioFile`-shaped [`Pipeline`] over this driver's own `stdout`
/// writer. qpdf's `Pl_StdioFile` wraps a `FILE*` the caller keeps owning
/// and open past this pipeline's own lifetime; borrowing `stdout` for the
/// duration of one `write_json` call is the same relationship expressed
/// through Rust borrowing instead of a raw pointer -- a container
/// substitution, not a missing primitive (CLAUDE.md deviation class (B)).
struct StdoutPipeline<'a> {
    stdout: &'a mut dyn Write,
}

impl Pipeline for StdoutPipeline<'_> {
    fn identifier(&self) -> &str {
        "stdout"
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

/// Exercise the simpler, single-call overload of `QPDF::writeJSON`:
/// `pdf.writeJSON(2, &p, qpdf_dl_none, qpdf_sj_inline, "", {})`. qpdf's
/// declaration order (`include/qpdf/QPDF.hh:136-142`) is `(version,
/// pipeline, decode_level, stream_data_mode, file_prefix, wanted_objects)`,
/// matching [`document_json::write_json`]'s own parameter order; the empty
/// `file_prefix` string has no effect (and no flpdf parameter) outside
/// [`StreamDataMode::File`], so it is not carried through.
///
/// `QPDF::writeJSON` walks every object via `getAllObjects()`
/// (`libqpdf/QPDF_json.cc:900-925`) and serializes each one, dereferencing it
/// for the first time if it had not been resolved yet. A malformed object
/// resolved lazily this way still goes through the same `QPDF::warn` call any
/// other first resolution would (`libqpdf/QPDF.cc:487-493`), which -- unlike
/// this driver's own suppressed-then-manually-drained diagnostics -- writes
/// straight to the real process's warn logger the moment it fires, with no
/// separate drain step required. `document_json::write_json` triggers the
/// same lazy-resolution warnings through this crate's own
/// [`flpdf::Pdf::repair_diagnostics`] collection, so they must be drained
/// with [`emit_new_diagnostics`] once the call returns, matching every other
/// test in this file that performs a resolution-triggering operation.
pub(crate) fn run_test_91<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    {
        let mut sink = StdoutPipeline { stdout };
        document_json::write_json(
            pdf,
            2,
            &mut sink,
            DecodeLevel::None,
            &StreamDataMode::Inline,
            &[],
        )
        .map_err(|error| Error::System(error.to_string()))?;
    }
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_92 (test_driver.cc:3195-3242)
// ---------------------------------------------------------------------------

/// Exercise indirect objects owned by a destroyed [`Pdf`]. qpdf opens its
/// own, second `QPDF` instance here (`auto qpdf = QPDF::create();
/// qpdf->processFile("minimal.pdf");`, test_driver.cc:3199-3200),
/// independent of this function's own `pdf` parameter -- confirmed by
/// test_92's presence in `runtest`'s own `ignore_filename` set
/// (test_driver.cc:3463) -- so `pdf` is unused here exactly as qpdf's own
/// `test_92(QPDF& pdf, ...)` never touches its `pdf` parameter either.
pub(crate) fn run_test_92<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let file = std::fs::File::open("minimal.pdf")?;
    let mut qpdf = Pdf::open(std::io::BufReader::new(file))?;

    // GAP(QPDFObjectHandle::getOwningQPDF): no public accessor exposes a
    // handle's owning-document identity (`ObjectHandle::owning_pdf_unique_id`
    // is `pub(crate)`), so the three `assert(*.getOwningQPDF() ==
    // qpdf.get())` checks below (test_driver.cc:3202,3206,3210) and the
    // later `assert(oh.getOwningQPDF() == nullptr)` (test_driver.cc:3218)
    // are not ported; every other assertion in this test is.
    let root_h = root_handle(&mut qpdf);
    qpdf.resolve(&root_h)?;
    let root = root_h.clone();
    assert!(root.is_indirect());
    assert!(root.as_dictionary().is_some());

    let page1 = resolved_key(&mut qpdf, &root, b"/Pages")?;
    let kids = resolved_key(&mut qpdf, &page1, b"/Kids")?;
    let kid_items = kids
        .as_array()
        .expect("minimal.pdf's /Pages/Kids is a direct array");
    let first_kid = kid_items
        .first()
        .cloned()
        .expect("minimal.pdf's /Kids has at least one page");
    qpdf.resolve(&first_kid)?;
    let page1 = first_kid.clone();
    assert!(page1.is_indirect());
    assert!(page1.as_dictionary().is_some());

    let resources = resolved_key(&mut qpdf, &page1, b"/Resources")?;
    assert!(resources.as_dictionary().is_some());
    assert!(!resources.is_indirect());

    let contents = resolved_key(&mut qpdf, &page1, b"/Contents")?;
    assert!(!is_scalar(&contents)?);
    let contents_dict = contents
        .as_stream_dict()
        .expect("minimal.pdf's page /Contents is a stream");

    drop(qpdf);

    // All objects should no longer be indirect (`check`, test_driver.cc:3217-3220).
    assert!(!root.is_indirect());
    assert!(!page1.is_indirect());
    assert!(!resources.is_indirect());
    assert!(!contents.is_indirect());
    assert!(!contents_dict.is_indirect());

    // Objects that were originally indirect are destroyed; direct children
    // (`resources`, `contents_dict`) retain their old values instead
    // (test_driver.cc:3227-3235).
    assert!(is_destroyed(&root)?);
    assert!(!is_scalar(&root)?);
    assert!(is_destroyed(&page1)?);
    assert!(is_destroyed(&contents)?);
    assert!(resources.as_dictionary().is_some());
    assert!(contents_dict.as_dictionary().is_some());

    // GAP(QPDFObjectHandle::unparse, throwing `std::logic_error` for a
    // destroyed handle, `libqpdf/QPDF_Destroyed.cc:24-29`):
    // `ObjectHandle::unparse` returns `Vec<u8>`, not `Result`, and its own
    // doc documents falling back to a `null` unparse for a `Destroyed`
    // handle rather than raising an error -- there is no exception channel
    // here to assert against, so the `try { root.unparse(); assert(false);
    // } catch (std::logic_error&) {}` block (test_driver.cc:3236-3241) is
    // not ported.
    Ok(())
}

// ---------------------------------------------------------------------------
// test_93 (test_driver.cc:3244-3269)
// ---------------------------------------------------------------------------

/// Test `QPDFObjectHandle` equality: two handles are equal if they point to
/// the same underlying object.
pub(crate) fn run_test_93<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // An indirect JSON/PDF value resolves through `Pdf::get_object_handle`'s
    // own canonical registry entry, so `root1`
    // below and `root_handle`'s own reference share the same underlying
    // `Rc` -- `is_same_object_as` needs no resolution to observe that.
    let trailer = pdf.trailer();
    let root1 = trailer.get_key(b"/Root");
    let root2 = root_handle(pdf);
    assert!(root1.is_same_object_as(&root2));

    let oh1 = ObjectHandle::parse(b"<< /One /Two >>")?;
    let oh2 = oh1.clone();
    assert!(oh1.is_same_object_as(&oh2));
    let oh3 = ObjectHandle::parse(b"<< /One /Two >>")?;
    assert!(!oh1.is_same_object_as(&oh3));
    oh2.replace_key(b"/One", ObjectHandle::parse(b"/Three")?)?;
    assert!(oh1.is_same_object_as(&oh2));
    assert_eq!(oh2.unparse(), b"<< /One /Three >>");
    assert!(!oh1.is_indirect());

    let oh4 = pdf.make_indirect_from_object_handle(oh1.clone())?;
    assert!(oh1.is_same_object_as(&oh4));
    assert!(oh1.is_indirect());
    assert!(oh4.is_indirect());
    trailer.replace_key(b"/Potato", oh1.clone())?;
    let potato = trailer.get_key(b"/Potato");
    assert!(potato.is_same_object_as(&oh2));
    Ok(())
}

// ---------------------------------------------------------------------------
// test_94 (test_driver.cc:3271-3371)
// ---------------------------------------------------------------------------

/// Exercise methods to get page boxes. Built for `boxes2.pdf`.
pub(crate) fn run_test_94<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:3271-3371. The handle-returning
    // PageObjectHelper methods below preserve the same live identity and
    // copy-on-fallback semantics as qpdf; the numeric PageBox convenience
    // methods are intentionally not used here.
    let root = root_handle(pdf);
    let pages_root = resolved_key(pdf, &root, b"/Pages")?;
    let root_media = resolved_key(pdf, &pages_root, b"/MediaBox")?;
    let root_media_unparse = root_media.unparse();

    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 5);
    let p1_ref = pages[0];
    let p2_ref = pages[1];
    let p3_ref = pages[2];
    let p4_ref = pages[3];
    let p5_ref = pages[4];

    let p1 = pdf.get_object_handle(p1_ref);
    assert!(p1.try_get_key(b"/MediaBox")?.is_null());
    {
        let mut page = PageObjectHelper::new(p1_ref, pdf);
        assert!(page.get_media_box(false)?.is_same_object_as(&root_media));
        assert!(page
            .get_crop_box(false, false)?
            .is_same_object_as(&root_media));
        assert!(page
            .get_bleed_box(false, false)?
            .is_same_object_as(&root_media));
        assert!(page
            .get_trim_box(false, false)?
            .is_same_object_as(&root_media));
        assert!(page
            .get_art_box(false, false)?
            .is_same_object_as(&root_media));

        let p1_new_art = page.get_art_box(false, true)?;
        assert_eq!(p1_new_art.unparse(), root_media_unparse);
        assert!(!p1_new_art.is_same_object_as(&root_media));

        let p1_new_crop = page.get_crop_box(false, false)?;
        assert!(!p1_new_crop.is_same_object_as(&root_media));
        assert!(!p1_new_crop.is_same_object_as(&p1_new_art));
        assert_eq!(p1_new_crop.unparse(), root_media_unparse);

        assert!(page.get_media_box(false)?.is_same_object_as(&root_media));
        assert!(page
            .get_trim_box(false, false)?
            .is_same_object_as(&p1_new_crop));

        let p1_effective_media = page.get_media_box(true)?;
        assert_eq!(p1_effective_media.unparse(), root_media_unparse);
        assert!(!p1_effective_media.is_same_object_as(&root_media));
    }

    {
        let mut page = PageObjectHelper::new(p2_ref, pdf);
        assert!(page.get_media_box(false)?.is_same_object_as(&root_media));
        let p2_crop = page.get_crop_box(false, false)?;
        let p2_new_trim = page.get_trim_box(false, true)?;
        assert_eq!(p2_new_trim.unparse(), p2_crop.unparse());
        assert!(!p2_new_trim.is_same_object_as(&p2_crop));
        assert!(page.get_media_box(false)?.is_same_object_as(&root_media));
    }

    {
        let mut page = PageObjectHelper::new(p3_ref, pdf);
        let p3_media = page.get_media_box(false)?;
        let p3_crop = page.get_crop_box(false, false)?;
        assert!(page.get_media_box(true)?.is_same_object_as(&p3_media));
        assert!(page.get_crop_box(true, true)?.is_same_object_as(&p3_crop));
    }

    {
        let p4 = pdf.get_object_handle(p4_ref);
        let p4_orig_crop = p4.try_get_key(b"/CropBox")?;
        let mut page = PageObjectHelper::new(p4_ref, pdf);
        let p4_crop = page.get_crop_box(false, false)?;
        assert!(p4_orig_crop.is_same_object_as(&p4_crop));
        let p4_bleed1 = page.get_bleed_box(false, false)?;
        let p4_bleed2 = page.get_bleed_box(false, true)?;
        assert!(!p4_bleed1.is_same_object_as(&p4_crop));
        assert!(p4_bleed1.is_same_object_as(&p4_bleed2));
        let p4_art1 = page.get_art_box(false, false)?;
        assert!(p4_art1.is_same_object_as(&p4_crop));
        let p4_art2 = page.get_art_box(false, true)?;
        assert!(!p4_art2.is_same_object_as(&p4_crop));
        let p4_new_crop = page.get_crop_box(true, false)?;
        assert!(!p4_new_crop.is_same_object_as(&p4_orig_crop));
        assert!(p4_orig_crop.is_indirect());
        assert!(!p4_new_crop.is_indirect());
        assert_eq!(p4_new_crop.unparse(), p4_orig_crop.unparse_resolved());
    }

    {
        let mut page = PageObjectHelper::new(p5_ref, pdf);
        assert!(page.get_media_box(false)?.is_same_object_as(&root_media));
        assert!(page
            .get_crop_box(false, false)?
            .is_same_object_as(&root_media));
        assert!(page
            .get_bleed_box(false, false)?
            .is_same_object_as(&root_media));
        let p5_new_bleed = page.get_bleed_box(true, true)?;
        let p5_new_media = page.get_media_box(false)?;
        let p5_new_crop = page.get_crop_box(false, false)?;
        assert!(!p5_new_media.is_same_object_as(&root_media));
        assert!(!p5_new_crop.is_same_object_as(&root_media));
        assert!(!p5_new_crop.is_same_object_as(&p5_new_media));
        assert!(!p5_new_bleed.is_same_object_as(&root_media));
        assert!(!p5_new_bleed.is_same_object_as(&p5_new_media));
        assert!(!p5_new_bleed.is_same_object_as(&p5_new_crop));
        assert_eq!(p5_new_media.unparse(), root_media_unparse);
        assert_eq!(p5_new_crop.unparse(), root_media_unparse);
        assert_eq!(p5_new_bleed.unparse(), root_media_unparse);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// test_95 (test_driver.cc:3373-3397)
// ---------------------------------------------------------------------------

/// Test `QPDFObjectHandle::isScalar`.
pub(crate) fn run_test_95<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let oh_b = ObjectHandle::boolean(false);
    let oh_i = ObjectHandle::integer(1);
    let oh_r = ObjectHandle::real(42.0);
    let oh_n = ObjectHandle::name(b"Test".to_vec());
    let oh_s = ObjectHandle::string(b"/Test".to_vec());
    let oh_o = ObjectHandle::operator(b"/Test".to_vec());
    let oh_ii = ObjectHandle::inline_image(b"/Test".to_vec());
    let oh_a = ObjectHandle::array(vec![]);
    let oh_d = ObjectHandle::dictionary(vec![]);

    assert!(is_scalar(&oh_b)?);
    assert!(is_scalar(&oh_i)?);
    assert!(is_scalar(&oh_r)?);
    assert!(is_scalar(&oh_n)?);
    assert!(is_scalar(&oh_s)?);
    assert!(!is_scalar(&oh_o)?);
    assert!(!is_scalar(&oh_ii)?);
    assert!(!is_scalar(&oh_a)?);
    assert!(!is_scalar(&oh_d)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// test_96 (test_driver.cc:3399-3412)
// ---------------------------------------------------------------------------

/// Test edge cases with quoted characters and string parsing.
pub(crate) fn run_test_96<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let s = ObjectHandle::parse(b"(\\48\\418\\121\\4)")?;
    let stored = s
        .as_string()
        .expect("qpdf string-literal syntax parses to a String value");
    assert_eq!(
        flpdf::pdf_string::unparse_binary(&stored),
        b"<043821385104>"
    );

    let s = ObjectHandle::parse(b"(\\48\\418\\121\\41)")?;
    let stored = s
        .as_string()
        .expect("qpdf string-literal syntax parses to a String value");
    assert_eq!(
        flpdf::pdf_string::unparse_binary(&stored),
        b"<043821385121>"
    );

    let s = ObjectHandle::parse(b"<a>")?;
    let stored = s
        .as_string()
        .expect("qpdf hex-string syntax parses to a String value");
    assert_eq!(flpdf::pdf_string::unparse_binary(&stored), b"<a0>");

    let s = ObjectHandle::parse(b"<abc>")?;
    let stored = s
        .as_string()
        .expect("qpdf hex-string syntax parses to a String value");
    assert_eq!(flpdf::pdf_string::unparse_binary(&stored), b"<abc0>");
    Ok(())
}

// ---------------------------------------------------------------------------
// test_97 (test_driver.cc:3414-3422)
// ---------------------------------------------------------------------------

/// Shallow array copy. Built for `many-nulls.pdf`.
pub(crate) fn run_test_97<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let container = pdf.trailer_key_handle(b"Nulls");
    pdf.resolve(&container)?;
    let container_items = container
        .as_array()
        .expect("many-nulls.pdf's /Nulls trailer entry is an array");
    // GAP(QPDFObjectHandle::getArrayItem): see `run_test_89`'s own GAP
    // comment for the missing single-item read-with-dereference-and-warning
    // accessor; `container_items.first()` substitutes the whole-array read
    // this crate does provide, and the canonical resolver below performs the
    // same one-hop dereference qpdf's own `getArrayItem` performs internally.
    let first_item = container_items
        .first()
        .cloned()
        .expect("many-nulls.pdf's /Nulls trailer array has at least one element");
    pdf.resolve(&first_item)?;
    let items = first_item
        .as_array()
        .expect("many-nulls.pdf's /Nulls[0] is a large direct array of nulls");
    assert!(items.len() > 10000);
    let nulls2 = first_item.shallow_copy()?;
    assert_eq!(first_item.unparse(), nulls2.unparse());
    Ok(())
}

// ---------------------------------------------------------------------------
// test_98 (test_driver.cc:3424-3454)
// ---------------------------------------------------------------------------

/// Test methods no longer used by qpdf as a result of
/// `QPDFObjectHandle::writeJSON`. Built for `minimal.pdf`.
pub(crate) fn run_test_98<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // GAP(QPDFObjectHandle::getJSON / QPDFObjectHandle::writeJSON
    // (per-object overload) / QPDFObjectHandle::getStreamJSON): test_98
    // exists specifically to compare `ObjectHandle::write_json`/`get_json`/
    // `write_stream_json` against each other -- but all three are
    // `pub(crate)` in `object_handle.rs` (`:5283`, `:5307`, `:4560`), not
    // reachable from this separate crate. qpdf's own first loop already
    // only compares two of *its own* outputs against each other rather
    // than against an independent oracle, so even a partial port here would
    // be a self-comparison on top of being unreachable.
    Ok(())
}

#[cfg(test)]
mod test_97_tests {
    use super::run_test_97;
    use flpdf::{ObjectHandle, Pdf};

    fn many_nulls_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut pdf = Pdf::empty().expect("create empty PDF");
        let inner = ObjectHandle::array(
            (0..=10_000)
                .map(|_| ObjectHandle::null())
                .collect::<Vec<_>>(),
        );
        let top = ObjectHandle::array(vec![inner]);
        let top = pdf
            .make_indirect_from_object_handle(top)
            .expect("promote /Nulls container");
        pdf.trailer()
            .replace_key(b"/Nulls", top)
            .expect("install /Nulls trailer entry");
        pdf
    }

    #[test]
    fn test_97_shallow_copies_the_large_array_through_canonical_handles() {
        let mut pdf = many_nulls_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_97(
            &mut pdf,
            b"many-nulls.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 97");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }
}

#[cfg(test)]
mod test_92_tests {
    use super::run_test_92;
    use flpdf::{Pdf, PdfOpenOptions};

    struct CurrentDirGuard(std::path::PathBuf);

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore current directory");
        }
    }

    #[test]
    fn test_92_resolves_the_destroyed_document_fixture_through_handles() {
        let _lock = super::super::CURRENT_DIR_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("acquire current-directory test lock");
        let directory = tempfile::tempdir().expect("create test directory");
        std::fs::write(
            directory.path().join("minimal.pdf"),
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf"),
        )
        .expect("write minimal fixture");
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(directory.path()).expect("enter test directory");
        let _restore = CurrentDirGuard(previous);

        let mut pdf = Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
            PdfOpenOptions::default(),
        )
        .expect("open source fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_92(
            &mut pdf,
            b"source.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 92");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::{run_test_88, run_test_89};
    use flpdf::{ObjectHandle, ObjectRef, Pdf, PdfOpenOptions};

    fn minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let options = PdfOpenOptions {
            description: "minimal.pdf".to_owned(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        };
        Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            options,
        )
        .expect("open minimal fixture")
    }

    #[test]
    fn test_88_resolves_the_root_before_qpdf_mutations() {
        let mut pdf = minimal_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_88(
            &mut pdf,
            b"minimal.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 88");

        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            b"WARNING: test array: ignoring attempt to erase out of bounds array item\n\
              WARNING: minimal.pdf, object 1 0 at offset 19: operation for array attempted on object of type dictionary: ignoring attempt to erase item\n"
        );
    }

    #[test]
    fn test_89_resolves_root_and_unknown_object_handles_once() {
        let mut pdf = minimal_pdf();
        pdf.replace_object(
            ObjectRef::new(5, 0),
            ObjectHandle::array(vec![ObjectHandle::dictionary(vec![])]),
        )
        .expect("install a contextful object-5 array for the test fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_89(
            &mut pdf,
            b"minimal.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 89");
    }
}

#[cfg(test)]
mod test_94_tests {
    use super::run_test_94;
    use flpdf::{PageDocumentHelper, Pdf};
    use std::collections::BTreeMap;

    fn boxes2_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let objects = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 5 /MediaBox [0 0 612 792] >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /CropBox [1 2 3 4] >>",
            ),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [5 6 7 8] /CropBox [1 2 3 4] >>",
            ),
            (
                6,
                "<< /Type /Page /Parent 2 0 R /MediaBox [5 6 7 8] /CropBox 8 0 R /TrimBox [1 2 3 4] /BleedBox [5 6 7 8] >>",
            ),
            (
                7,
                "<< /Type /Page /Parent 2 0 R /TrimBox [1 2 3 4] /ArtBox [5 6 7 8] >>",
            ),
            (8, "[10 20 30 40]"),
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        for (number, body) in objects {
            offsets.insert(number, bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
        for number in 1..=8 {
            bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        Pdf::open_mem_owned(bytes).expect("boxes2 fixture should parse")
    }

    #[test]
    fn test_94_executes_box_assertions_and_copy_side_effects() {
        let mut pdf = boxes2_pdf();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_94(
            &mut pdf,
            b"boxes2.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("test 94 should execute the page-box assertion matrix");

        let pages = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("boxes2 fixture should retain five pages");
        assert_eq!(pages.len(), 5);

        for key in [b"/MediaBox".as_slice(), b"/CropBox", b"/ArtBox"] {
            assert!(
                pdf.get_object_handle(pages[0])
                    .try_has_key(key)
                    .expect("page 1 key lookup"),
                "test 94 should copy {key:?} onto page 1"
            );
        }
        assert!(pdf
            .get_object_handle(pages[1])
            .try_has_key(b"/TrimBox")
            .unwrap());
        assert!(pdf
            .get_object_handle(pages[3])
            .try_has_key(b"/BleedBox")
            .unwrap());
        for key in [b"/MediaBox".as_slice(), b"/CropBox", b"/BleedBox"] {
            assert!(
                pdf.get_object_handle(pages[4])
                    .try_has_key(key)
                    .expect("page 5 key lookup"),
                "test 94 should copy {key:?} onto page 5"
            );
        }
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }
}
