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
    document_json, Error, ObjectHandle, ObjectRef, Pdf, Pipeline, PipelineError, PipelineResult,
};

use super::emit_new_diagnostics;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// qpdf's `QPDF::getRoot` (`libqpdf/QPDF.cc:2355-2368`) additionally throws
/// `damagedPDF` when `/Root` does not resolve to a dictionary and, under
/// `check_mode`, repairs a missing/invalid `/Type`. This port only reaches
/// for the handle itself; a caller that needs the dictionary type checked
/// resolves it through [`resolved_key`]/`Pdf::resolve_to_terminal`
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
    let child = parent.get_key(key);
    pdf.resolve_to_terminal(&child)
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

/// `QPDFObjectHandle::replaceKeyAndGetNew`
/// (`libqpdf/QPDFObjectHandle.cc:1213-1217`): `replaceKey(key, value);
/// return value;`.
fn replace_key_and_get_new(
    dict: &ObjectHandle,
    key: &[u8],
    value: ObjectHandle,
) -> flpdf::Result<ObjectHandle> {
    dict.replace_key(key, value.clone())?;
    Ok(value)
}

/// `QPDFObjectHandle::removeKeyAndGetOld`
/// (`libqpdf/QPDFObjectHandle.cc:1240-1248`): read `key`'s current value
/// from `asDictionary()`, defaulting to a fresh null when `dict` is not a
/// dictionary or `key` is absent, then `removeKey(key)`.
fn remove_key_and_get_old(dict: &ObjectHandle, key: &[u8]) -> ObjectHandle {
    let old = dict
        .as_dictionary()
        .and_then(|entries| entries.get(key).cloned())
        .unwrap_or_else(ObjectHandle::null);
    dict.remove_key(key);
    old
}

/// `QPDFObjectHandle::replaceKeyAndGetOld`
/// (`libqpdf/QPDFObjectHandle.cc:1219-1225`): `old =
/// removeKeyAndGetOld(key); replaceKey(key, value); return old;`.
fn replace_key_and_get_old(
    dict: &ObjectHandle,
    key: &[u8],
    value: ObjectHandle,
) -> flpdf::Result<ObjectHandle> {
    let old = remove_key_and_get_old(dict, key);
    dict.replace_key(key, value)?;
    Ok(old)
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
    let three = replace_key_and_get_new(&dict, b"/Three", ObjectHandle::array(vec![]))?;
    three.append_array_item(ObjectHandle::parse(b"(a)")?)?;
    three.append_array_item(ObjectHandle::parse(b"(b)")?)?;
    let newdict = three.append_array_item_and_get_new(ObjectHandle::dictionary(vec![]))?;
    newdict.replace_key(b"/Z", ObjectHandle::parse(b"/Y")?)?;
    newdict.replace_key(b"/X", ObjectHandle::parse(b"/W")?)?;
    dict.replace_key(b"/Quack", ObjectHandle::parse(b"[1 2 3]")?)?;
    let quack = replace_key_and_get_old(&dict, b"/Quack", ObjectHandle::parse(b"/Moo")?)?;
    assert_eq!(quack.unparse(), b"[ 1 2 3 ]");
    let nothing = replace_key_and_get_old(&dict, b"/NotThere", ObjectHandle::null())?;
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

    assert!(remove_key_and_get_old(&new_dict, b"/M").is_null());
    assert_eq!(remove_key_and_get_old(&new_dict, b"/P").unparse(), b"/Q");
    assert_eq!(
        new_dict.unparse(),
        ObjectHandle::parse(b"<< /T /U >>")?.unparse()
    );

    // Test errors (test_driver.cc:3155-3159).
    let root = root_handle(pdf);
    pdf.resolve(&root)?;
    let root = root.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    let arr2 = replace_key_and_get_new(&root, b"/QTest", ObjectHandle::parse(b"[1 2]")?)?;
    // GAP(QPDFObjectHandle::setObjectDescription): no public equivalent
    // attaches explicit `pdf` + description warning context to a handle
    // (test_driver.cc:3157, `arr2.setObjectDescription(&pdf, "test
    // array")`), so `arr2` below carries only whatever context
    // `replace_key`'s own child-attachment already gives a value installed
    // into the live object graph, not the description qpdf's test
    // installs. If that is insufficient for `erase_array_item_and_get_old`
    // to route its warning instead of erroring (see that method's own
    // doc), the call below surfaces that honestly as a propagated
    // `flpdf::Error::System` via `?` rather than a silently wrong null.
    assert!(arr2.erase_array_item_and_get_old(50)?.is_null());
    assert!(root.erase_array_item_and_get_old(0)?.is_null());
    Ok(())
}

// ---------------------------------------------------------------------------
// test_89 (test_driver.cc:3162-3172)
// ---------------------------------------------------------------------------

/// Generate object warnings via type-mismatched mutations. Crafted to work
/// with `manual-qpdf-json.json` -- object 5 is not assumed to genuinely be
/// a dictionary/array at either mutation site below, matching qpdf's own
/// point of exercising the mismatch-warning path.
///
/// Not wired into `driver::run`'s dispatch: qpdf builds test 89's `pdf` via
/// `QPDF::createFromJSON`, which has no flpdf equivalent (`driver::run`
/// short-circuits `n == 89` before it would ever reach a dispatch call).
/// Kept for when that primitive lands.
#[allow(dead_code)]
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

    let root = root_handle(pdf);
    pdf.resolve(&root)?;
    let root = root.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    root.append_array_item(null.clone())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    let object5_ref = pdf.get_object_handle(ObjectRef::new(5, 0));
    pdf.resolve(&object5_ref)?;
    let object5 = object5_ref.clone();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    object5.replace_key(b"/X", null.clone())?;

    // GAP(QPDFObjectHandle::getArrayItem): no public flpdf accessor reads a
    // single array item by index with qpdf's own dereference-then-
    // `typeWarning("array", ...)`-on-mismatch contract; `ObjectHandle::as_array`
    // never dereferences and never warns on a type mismatch (its own doc:
    // "never performs resolution itself"). Re-resolving `object5` above
    // covers the dereference half; the read below substitutes
    // `as_array().first()` for the index read itself, but a genuinely
    // non-array object 5 here falls back to a direct null instead of also
    // raising qpdf's warning the way the three mutations above do.
    let item0 = object5
        .as_array()
        .and_then(|items| items.first().cloned())
        .unwrap_or_else(ObjectHandle::null);
    item0.replace_key(b"/X", null)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_90 (test_driver.cc:3174-3185)
// ---------------------------------------------------------------------------

/// Generate an object warning via `QPDF::updateFromJSON`. Crafted to work
/// with `good13.pdf` and `various-updates.json` (the JSON file is `arg2`).
pub(crate) fn run_test_90<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = arg2;
    // GAP(QPDF::updateFromJSON): test_90's entire body
    // (test_driver.cc:3179-3184) opens with `pdf.updateFromJSON(arg2)`,
    // applying a qpdf-JSON-v2 update document (`arg2` is its path) to the
    // live object graph. flpdf has no JSON-update entry point anywhere in
    // `crates/flpdf/src/` -- `document_json.rs`'s own module doc states
    // plainly: "the input side (JSONReactor, createFromJSON,
    // updateFromJSON, importJSON) has no counterpart here" -- so nothing in
    // this body, including the two unconditional trailer mutations and the
    // `/QTest/strings` integer read that follow the JSON update in qpdf's
    // own source, can be attempted.
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
    let root = qpdf.resolve_to_terminal(&root_h)?;
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
    let page1 = qpdf.resolve_to_terminal(&first_kid)?;
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
    // `Object::Reference` lifting always resolves through
    // `Pdf::get_object_handle`'s own canonical registry entry
    // (`reader.rs`'s `lift_to_handle_bounded_with_options`), so `root1`
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

    // GAP(QPDF::makeIndirectObject): `Pdf::make_indirect_object_handle`
    // clones `oh1`'s direct value onto a *new*, separately-registered
    // indirect slot rather than promoting the existing shared allocation
    // in place (`Pdf::make_indirect_object_handle`'s own doc: "materializing
    // here would duplicate a direct stream's payload into the legacy
    // Object cache"), so the returned handle does not share identity with
    // `oh1` the way qpdf's in-place promotion does. `assert(oh1.isSameObjectAs(oh4))`,
    // `assert(oh1.isIndirect())`, and `assert(oh4.isIndirect())`
    // (test_driver.cc:3263-3266) are not ported; the `/Potato` assertions
    // below do not depend on `oh4` and are.
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
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // GAP(QPDFPageObjectHelper::getMediaBox / getCropBox / getBleedBox /
    // getTrimBox / getArtBox): every assertion in this test
    // (test_driver.cc:3271-3371) is an `isSameObjectAs`/`isIndirect`/
    // copy-on-fallback identity check over `QPDFObjectHandle`-returning box
    // getters that take `copy_if_shared`/`copy_if_fallback` flags. flpdf's
    // box helpers (`PageObjectHelper::media_box`/`crop_box`/`bleed_box`/
    // `trim_box`/`art_box`, `page_object_helper.rs:475-616`) return a
    // numeric `PageBox` value struct with no handle identity and no
    // copy-on-fallback flags at all, so none of this test's identity/sharing
    // assertions has any primitive to port against; a numeric-only
    // comparison would silently test something else entirely.
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
    let container = pdf.resolve_to_terminal(&container)?;
    let container_items = container
        .as_array()
        .expect("many-nulls.pdf's /Nulls trailer entry is an array");
    // GAP(QPDFObjectHandle::getArrayItem): see `run_test_89`'s own GAP
    // comment for the missing single-item read-with-dereference-and-warning
    // accessor; `container_items.first()` substitutes the whole-array read
    // this crate does provide, resolved to terminal below the same way
    // qpdf's own `getArrayItem` dereferences internally.
    let first_item = container_items
        .first()
        .cloned()
        .expect("many-nulls.pdf's /Nulls trailer array has at least one element");
    let nulls = pdf.resolve_to_terminal(&first_item)?;
    let items = nulls
        .as_array()
        .expect("many-nulls.pdf's /Nulls[0] is a large direct array of nulls");
    assert!(items.len() > 10000);
    let nulls2 = nulls.shallow_copy()?;
    assert_eq!(nulls.unparse(), nulls2.unparse());
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
mod tests {
    use super::{run_test_88, run_test_89};
    use flpdf::{Pdf, PdfOpenOptions};

    fn minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open_mem_owned_with_options(
            include_bytes!("../../../../tests/fixtures/minimal.pdf").to_vec(),
            PdfOpenOptions::default(),
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
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_89_resolves_root_and_unknown_object_handles_once() {
        let mut pdf = minimal_pdf();
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
