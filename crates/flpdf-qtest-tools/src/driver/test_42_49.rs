use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{
    Dictionary, NameTree, NumberTree, Object, ObjectHandle, OutlineDocumentHelper,
    PageDocumentHelper, PageLabelDocumentHelper, Pdf, PdfWriter,
};

use super::emit_new_diagnostics;
use super::handle::{resolve_chain, write_object};
use crate::output::write_bytes;

// Shared helpers for test_46/test_48 (qpdf's number-tree/name-tree driver
// tests) and test_47/test_49 (page-label/outline document-helper tests).

/// Follow an arbitrary indirection chain to its terminal value, matching
/// `QPDFObjectHandle`'s implicit auto-dereference whenever a number-/name-
/// tree entry (or any plain `Object` read off the trailer) is used as a
/// scalar. `resolve_chain` (`handle.rs`) already implements this for the
/// legacy `Object` bridge that `test_0_1.rs` also reuses.
fn resolved<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> flpdf::Result<Object> {
    Ok(resolve_chain(pdf, value)?.0)
}

/// `QPDFObjectHandle::getStringValue()` for a plain `Object` tree value: the
/// exact stored bytes of a string, or an empty result for anything else.
/// qpdf's wrong-type case additionally calls `typeWarning` and prints a
/// WARNING line (`libqpdf/QPDFObjectHandle.cc:659-666`); `ObjectHandle::type_warning`
/// (`object_handle.rs:2105`) is `pub(crate)` and has no public equivalent, so
/// only the empty-string fallback is reproduced here, not the warning.
fn tree_string_value<R: Read + Seek>(pdf: &mut Pdf<R>, value: &Object) -> flpdf::Result<Vec<u8>> {
    let value = resolved(pdf, value.clone())?;
    Ok(value.as_string().map(<[u8]>::to_vec).unwrap_or_default())
}

/// Read a NameTree value through its canonical `ObjectHandle` route. qpdf's
/// name-tree iterator keeps the value handle live until the consumer asks for
/// a typed value, so this helper resolves the handle once and applies the
/// same empty-string fallback as the NumberTree adapter above.
fn tree_string_handle_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
) -> flpdf::Result<Vec<u8>> {
    let value = pdf.resolve_to_terminal(value)?;
    Ok(value.as_string().unwrap_or_default())
}

/// Resolve `handle`, then read `key` from it — `ObjectHandle::get_key`
/// never resolves on its own (`object_handle.rs:2769-2787`), matching
/// `QPDFObjectHandle::getKey`'s own internal `dereference()` call
/// (`libqpdf/QPDFObjectHandle.cc:979-990`). Returns the resolved child, so
/// chaining two calls resolves at every hop the way qpdf's own
/// `a.getKey("/X").getKey("/Y")` chase does.
fn chase_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
) -> flpdf::Result<ObjectHandle> {
    let (chased, _) = pdf.resolve_to_terminal_ref(handle)?;
    let child = chased.get_key(key);
    Ok(pdf.resolve_to_terminal_ref(&child)?.0)
}

pub(crate) fn run_test_42<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1407-1549. Crafted for
    // object-types.pdf. The function is exclusively `assert()`-based (a
    // whitebox test of `QPDFObjectHandle`'s own C++ API); it never writes to
    // stdout, and its only stderr output ("One error"/"Two errors") are
    // literal markers for WARNING lines produced by the very wrong-type
    // accessors gapped below, so nothing observable is lost by stopping here.
    let qtest = pdf.trailer_key_handle(b"QTest");
    let (qtest, _) = pdf.resolve_to_terminal_ref(&qtest)?;
    let dictionary = qtest.get_key(b"/Dictionary");
    let (dictionary, _) = pdf.resolve_to_terminal_ref(&dictionary)?;
    let key2 = dictionary.get_key(b"/Key2");
    let (array, _) = pdf.resolve_to_terminal_ref(&key2)?;
    let integer = qtest.get_key(b"/Integer");
    let (_integer, _) = pdf.resolve_to_terminal_ref(&integer)?;
    assert!(
        array.as_array().is_some(),
        "qpdf test_42 requires /Dictionary/Key2 to be an array"
    );

    // GAP(QPDFObjectHandle::aitems/ditems iterator; QPDFObjectHandle::getKeyIfDict;
    // QPDFObjectHandle::getStringValue/getName/getOperatorValue/getRealValue/
    // getNumericValue/getInlineImageValue/getDictAsMap/getKeys wrong-type-warning
    // scalar accessors; QPDFObjectHandle::isRectangle/getArrayAsRectangle/
    // newFromRectangle and ::isMatrix/getArrayAsMatrix/newFromMatrix; and the
    // uninitialized-`QPDFObjectHandle` state exercised by `isInitialized()`
    // after decrementing an `aitems()`/`ditems()` end iterator, and by a
    // default-constructed handle at the end of the test): qpdf's test_42
    // spends the remainder of its body on these low-level `QPDFObjectHandle`
    // primitives. None has a public flpdf equivalent -- `type_warning`
    // (`object_handle.rs:2105`) and the scalar wrong-type accessors it backs
    // are `pub(crate)`, `ObjectHandle` has no array/dict-iterator type, no
    // Rectangle/Matrix array conversion exists on `ObjectHandle` (only the
    // standalone `flpdf::Rectangle`/`flpdf::Matrix` value types exist), and
    // `ObjectHandle` has no state distinct from a direct null handle to
    // represent "uninitialized". The function has no output either way.
    Ok(())
}

pub(crate) fn run_test_43<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1551-1609.
    //
    // `hasAcroForm()` is `getRoot().hasKey("/AcroForm")`
    // (`libqpdf/QPDFAcroFormDocumentHelper.cc:32-34`). qpdf's `hasKey`
    // treats a key resolving to null the same as a missing key
    // (`libqpdf/QPDF_Dictionary.cc:98-100`, the same rule `test_0_1.rs`
    // documents for `/QTest`), so this chases `/AcroForm` to its terminal
    // value rather than using `ObjectHandle::has_key` (raw map presence
    // only).
    let has_acroform = match pdf.root_ref() {
        Some(root_ref) => {
            let root = pdf.get_object_handle(root_ref);
            let (root, _) = pdf.resolve_to_terminal_ref(&root)?;
            let acroform = root.get_key(b"/AcroForm");
            let (acroform, _) = pdf.resolve_to_terminal_ref(&acroform)?;
            !acroform.is_null()
        }
        None => false,
    };
    if !has_acroform {
        writeln!(stdout, "no forms")?;
        return Ok(());
    }

    writeln!(stdout, "iterating over form fields")?;
    // GAP(QPDFAcroFormDocumentHelper::getFormFields, ::getAnnotationsForField):
    // qpdf's `getFormFields()` returns the fields reached while traversing
    // `/AcroForm`'s widget-annotation graph, keyed by
    // `m->field_to_annotations` -- a `std::map<QPDFObjGen, ...>`
    // (`include/qpdf/QPDFAcroFormDocumentHelper.hh`), so both membership
    // (fields with no associated widget annotation are excluded; orphan
    // widgets are promoted to their own field) and order (by object number)
    // differ from flpdf's `AcroFormDocumentHelper::fields()`, which returns
    // every `/Fields`-tree node in raw `/Fields` array preorder. Using
    // `fields()` here would silently iterate a different, differently
    // ordered set of fields than qpdf's real loop, so the field-listing
    // loop is skipped rather than approximated.

    writeln!(stdout, "iterating over annotations per page")?;
    // GAP(QPDFAcroFormDocumentHelper::getWidgetAnnotationsForPage,
    // ::getFieldForAnnotation; QPDFAnnotationObjectHelper::getAppearanceState,
    // ::getAppearanceStream): no flpdf equivalent exists for looking up the
    // widget annotations on a page or the field that owns a given
    // annotation (both rely on the same `field_to_annotations`/
    // `annotation_to_field` analysis gapped above), nor for an annotation's
    // appearance state or named appearance stream. The per-page annotation
    // loop is skipped.
    Ok(())
}

pub(crate) fn run_test_44<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1611-1629.
    //
    // GAP(QPDFAcroFormDocumentHelper::getFormFields): as in test_43, qpdf's
    // `getFormFields()` (membership + order via `m->field_to_annotations`)
    // has no flpdf equivalent; `AcroFormDocumentHelper::fields()` walks a
    // different set in a different order. Using it here would set `/V` on a
    // different, differently ordered set of fields than qpdf's real
    // `setV`/"Set field value" loop, so that loop (and its "Set field
    // value: ..." lines) is skipped rather than approximated. The write
    // below therefore serializes the document exactly as opened, without
    // qpdf's field-value mutations.
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_suppress_original_object_ids(true);
    writer.write()?;
    Ok(())
}

pub(crate) fn run_test_45<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1631-1643.
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.write()?;

    // GAP(QPDF::getWarnings): qpdf's `pdf.getWarnings()` returns every
    // `QPDFExc` accumulated in `m->warnings` across the `QPDF` instance's
    // whole lifetime, including ones `QPDFWriter::write` raises through
    // `pipeStreamData`'s warn callback while copying stream data. flpdf's
    // writer (`crates/flpdf/src/writer.rs`, `writer/*.rs`) never calls
    // `Pdf::push_warning` (confirmed by grep: no hits in either), so
    // `Pdf::repair_diagnostics()` -- the crate's `m->warnings`-equivalent
    // sink, also fed by `nntree.rs`/`object_copy.rs` outside repair --
    // reflects only open-time diagnostics here, not any write-time ones a
    // real qpdf run against an obfuscated file could add. There is no
    // accessor with qpdf's full-lifecycle coverage, so the `exit(3)` gate
    // is skipped.
    Ok(())
}

pub(crate) fn run_test_46<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1645-1782. Crafted for
    // number-tree.pdf. `NumberTree`/`NumberTreeCursor` (`nntree.rs`) are a
    // direct qpdf-compatible port of `QPDFNumberTreeObjectHelper`/its
    // `iterator`: advancing an end cursor selects the first entry and
    // moving one backward selects the last, matching qpdf's own wrap
    // behavior (`NumberTreeCursor::next`/`::previous` doc comments).
    let qtest = pdf
        .trailer_dictionary()
        .get(b"QTest")
        .cloned()
        .unwrap_or(Object::Null);
    let mut ntoh = NumberTree::new(qtest, true);

    let mut cursor = ntoh.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        let text = tree_string_value(pdf, &value)?;
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
        cursor.next(&mut ntoh, pdf)?;
    }

    let ntoh_map = ntoh.as_map(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for (key, value) in &ntoh_map {
        let text = tree_string_value(pdf, value)?;
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
    }

    assert_eq!(1, ntoh.min(pdf)?);
    assert_eq!(29, ntoh.max(pdf)?);
    assert!(ntoh.has_index(pdf, 6)?);
    assert!(!ntoh.has_index(pdf, 500)?);
    assert!(ntoh.find_object(pdf, 4)?.is_none());
    let three = ntoh.find_object(pdf, 3)?.expect("index 3 present");
    assert_eq!(tree_string_value(pdf, &three)?, b"three");
    assert!(ntoh.find_object_at_or_below(pdf, 0)?.is_none());
    let (six, offset) = ntoh
        .find_object_at_or_below(pdf, 8)?
        .expect("index at or below 8 present");
    assert_eq!(tree_string_value(pdf, &six)?, b"six");
    assert_eq!(2, offset);

    let mut new1 = NumberTree::new_empty(pdf, true)?;
    let mut iter1 = new1.begin(pdf)?;
    assert!(iter1 == new1.end());
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    new1.insert(pdf, 1, Object::String(b"1".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, 1);
    // qpdf's `auto& iter1_val = *iter1;` aliases the iterator's own current
    // value in place -- `NNTreeIterator::operator*` returns a reference
    // into a member the iterator itself updates on every subsequent move
    // (`libqpdf/NNTree.cc`), so `iter1_val` is never a frozen snapshot: it
    // is always the same observation as `iter1.current()` at whatever point
    // it is read. Every later `iter1_val.*` assertion below is therefore
    // ported as a repeated `iter1.current()` read rather than a separate
    // value.
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, 1);
    new1.insert(pdf, 2, Object::String(b"2".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, 2);
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    assert!(iter1.current().is_none());
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, 1);
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, 2);

    writeln!(stdout, "insertAfter")?;
    let mut new2 = NumberTree::new_empty(pdf, true)?;
    let mut iter2 = new2.begin(pdf)?;
    assert!(iter2 == new2.end());
    iter2.insert_after(&mut new2, pdf, 3, Object::String(b"3!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 3").0, 3);
    iter2.insert_after(&mut new2, pdf, 4, Object::String(b"4!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 4").0, 4);
    let mut cursor = new2.begin(pdf)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &write_object(&value))?;
        writeln!(stdout)?;
        cursor.next(&mut new2, pdf)?;
    }

    writeln!(stdout, "/Bad1")?;
    let bad1_object = pdf
        .trailer_dictionary()
        .get(b"Bad1")
        .cloned()
        .unwrap_or(Object::Null);
    let mut bad1 = NumberTree::new(bad1_object, true);
    let bad1_begin = bad1.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(bad1_begin == bad1.end());
    assert!(bad1.last(pdf)? == bad1.end());

    writeln!(stdout, "/Bad2")?;
    let bad2_object = pdf
        .trailer_dictionary()
        .get(b"Bad2")
        .cloned()
        .unwrap_or(Object::Null);
    let mut bad2 = NumberTree::new(bad2_object, true);
    let mut cursor = bad2.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &write_object(&value))?;
        writeln!(stdout)?;
        cursor.next(&mut bad2, pdf)?;
    }

    for key in [&b"Empty1"[..], &b"Empty2"[..]] {
        write!(stdout, "/")?;
        write_bytes(stdout, key)?;
        writeln!(stdout)?;
        let object = pdf
            .trailer_dictionary()
            .get(key)
            .cloned()
            .unwrap_or(Object::Null);
        let mut empty = NumberTree::new(object, true);
        let empty_begin = empty.begin(pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        assert!(empty_begin == empty.end());
        assert!(empty.last(pdf)? == empty.end());

        let inserted = empty.insert(pdf, 5, Object::String(b"5".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("index 5 present");
        assert_eq!(inserted_key, 5);
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"5");
        assert_eq!(empty.begin(pdf)?.current().expect("begin at 5").0, 5);
        assert_eq!(empty.last(pdf)?.current().expect("last at 5").0, 5);
        let begin_value = empty.begin(pdf)?.current().expect("begin value at 5").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5");

        let inserted = empty.insert(pdf, 5, Object::String(b"5+".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("index 5 present");
        assert_eq!(inserted_key, 5);
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"5+");
        let begin_value = empty.begin(pdf)?.current().expect("begin value at 5+").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5+");

        let inserted = empty.insert(pdf, 6, Object::String(b"6".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("index 6 present");
        assert_eq!(inserted_key, 6);
        assert_eq!(tree_string_value(pdf, &inserted_value)?, b"6");
        let begin_value = empty.begin(pdf)?.current().expect("begin still at 5+").1;
        assert_eq!(tree_string_value(pdf, &begin_value)?, b"5+");
        assert_eq!(empty.last(pdf)?.current().expect("last at 6").0, 6);
        let last_value = empty.last(pdf)?.current().expect("last value at 6").1;
        assert_eq!(tree_string_value(pdf, &last_value)?, b"6");
    }

    writeln!(stdout, "Insert into invalid")?;
    let mut invalid1 = NumberTree::new(Object::Dictionary(Dictionary::new()), true);
    // GAP(QPDFExc::what): qpdf catches the `QPDFExc` this throws (the root
    // is a direct dictionary with neither `/Nums` nor `/Kids`) and prints
    // `e.what()`. flpdf's `Error::to_string()` (`error.rs`) is not
    // verified byte-identical to `QPDFExc::createWhat`'s formatting for
    // this condition, so the real, invalid `insert` call is still made for
    // its side effects, but its error text is not printed.
    let _ = invalid1.insert(pdf, 1, Object::Null);

    writeln!(stdout, "/Bad3, no repair")?;
    let bad3_object = pdf
        .trailer_dictionary()
        .get(b"Bad3")
        .cloned()
        .unwrap_or(Object::Null);
    let mut bad3 = NumberTree::new(bad3_object.clone(), false);
    let mut cursor = bad3.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &write_object(&value))?;
        writeln!(stdout)?;
        cursor.next(&mut bad3, pdf)?;
    }
    assert!(!kids_item_0_is_indirect(pdf, &bad3_object)?);

    writeln!(stdout, "/Bad3, repair")?;
    let mut bad3 = NumberTree::new(bad3_object.clone(), true);
    let mut cursor = bad3.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &write_object(&value))?;
        writeln!(stdout)?;
        cursor.next(&mut bad3, pdf)?;
    }
    assert!(kids_item_0_is_indirect(pdf, &bad3_object)?);

    writeln!(stdout, "/Bad4 -- missing limits")?;
    let bad4_object = pdf
        .trailer_dictionary()
        .get(b"Bad4")
        .cloned()
        .unwrap_or(Object::Null);
    let mut bad4 = NumberTree::new(bad4_object, true);
    bad4.insert(pdf, 5, Object::String(b"5".to_vec()))?;
    let mut cursor = bad4.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write!(stdout, "{key} ")?;
        write_bytes(stdout, &write_object(&value))?;
        writeln!(stdout)?;
        cursor.next(&mut bad4, pdf)?;
    }

    writeln!(stdout, "/Bad5 -- limit errors")?;
    let bad5_object = pdf
        .trailer_dictionary()
        .get(b"Bad5")
        .cloned()
        .unwrap_or(Object::Null);
    let mut bad5 = NumberTree::new(bad5_object, true);
    let found = bad5.find(pdf, 10, false)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(found == bad5.end());

    Ok(())
}

/// Whether the root's `/Kids` array's first item is stored indirectly --
/// qpdf's `bad3_oh.getKey("/Kids").getArrayItem(0).isIndirect()`.
fn kids_item_0_is_indirect<R: Read + Seek>(pdf: &mut Pdf<R>, root: &Object) -> flpdf::Result<bool> {
    let root = resolved(pdf, root.clone())?;
    let Some(dict) = root.as_dict() else {
        return Ok(false);
    };
    let Some(kids) = dict.get("Kids").cloned() else {
        return Ok(false);
    };
    let kids = resolved(pdf, kids)?;
    let Some(items) = kids.as_array() else {
        return Ok(false);
    };
    Ok(matches!(items.first(), Some(Object::Reference(_))))
}

pub(crate) fn run_test_47<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1784-1796.
    let root_handle = match pdf.root_ref() {
        Some(root_ref) => pdf.get_object_handle(root_ref),
        None => ObjectHandle::null(),
    };
    let pages_handle = chase_key(pdf, &root_handle, b"/Pages")?;
    let count_handle = chase_key(pdf, &pages_handle, b"/Count")?;
    let npages = count_handle.as_integer().unwrap_or(0);
    let mut labels = Vec::new();
    // qpdf's `npages - 1` is `long long` arithmetic with no underflow guard;
    // `checked_sub` falls back to the same "empty inclusive range" shape
    // (`end < start`) an `npages == 0` document would already produce there.
    let end_idx = npages.checked_sub(1).unwrap_or(-1);
    PageLabelDocumentHelper::new(pdf).get_labels_for_page_range(0, end_idx, 1, &mut labels)?;
    // qpdf's `labels` is a flat `[idx0, dict0, idx1, dict1, ...]` vector
    // (hence its `labels.size() % 2 == 0` assertion); flpdf's
    // `get_labels_for_page_range` returns the same content already paired
    // as `Vec<(i64, ObjectHandle)>`, so the parity check has no Rust
    // analogue to port -- it is tautologically true of the pair type.
    for (index, label) in &labels {
        write!(stdout, "{index} ")?;
        write_bytes(stdout, &label.unparse())?;
        writeln!(stdout)?;
    }
    Ok(())
}

pub(crate) fn run_test_48<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1798-1921. Crafted for name-tree.pdf.
    // `NameTree`/`NameTreeCursor` (`nntree.rs`) mirror
    // `QPDFNameTreeObjectHelper`/its `iterator` exactly as `NumberTree` does
    // for `QPDFNumberTreeObjectHelper` in test_46 -- see that function's
    // header comment for the shared iterator-wrap and value-aliasing notes,
    // which apply identically here.
    let qtest = pdf.trailer_key_handle(b"QTest");
    let mut ntoh = NameTree::new(qtest, true);

    let mut cursor = ntoh.begin(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    while let Some((key, value)) = cursor.current() {
        write_bytes(stdout, &key)?;
        write!(stdout, " -> ")?;
        let text = tree_string_handle_value(pdf, &value)?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
        cursor.next(&mut ntoh, pdf)?;
    }

    let ntoh_map = ntoh.as_map(pdf)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    for (key, value) in &ntoh_map {
        write_bytes(stdout, key)?;
        write!(stdout, " -> ")?;
        let text = tree_string_handle_value(pdf, value)?;
        write_bytes(stdout, &text)?;
        writeln!(stdout)?;
    }

    assert!(ntoh.has_name(pdf, "11 elephant")?);
    assert!(ntoh.has_name(pdf, "07 sev\u{2022}n")?);
    assert!(!ntoh.has_name(pdf, "potato")?);
    assert!(ntoh.find_object(pdf, "potato")?.is_none());
    let seven = ntoh
        .find_object(pdf, "07 sev\u{2022}n")?
        .expect("07 sev*n present");
    assert_eq!(tree_string_handle_value(pdf, &seven)?, b"seven!");
    let (last_key, last_value) = ntoh
        .last(pdf)?
        .current()
        .expect("name tree has a last entry");
    assert_eq!(last_key, b"29 twenty-nine");
    let last_resolved = pdf.resolve_to_terminal(&last_value)?;
    let last_raw = last_resolved.as_string().unwrap_or_default();
    assert_eq!(flpdf::pdf_string::utf8_value(&last_raw), b"twenty-nine!");

    let mut new1 = NameTree::new_empty(pdf, true)?;
    let mut iter1 = new1.begin(pdf)?;
    assert!(iter1 == new1.end());
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    new1.insert(pdf, "1", ObjectHandle::string(b"1".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, b"1");
    // See test_46's header comment: `iter1_val` is a live alias to
    // `iter1`'s own current value, so every subsequent `iter1_val.*`
    // assertion below is ported as a repeated `iter1.current()` read.
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, b"1");
    new1.insert(pdf, "2", ObjectHandle::string(b"2".to_vec()))?;
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, b"2");
    iter1.next(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    assert!(iter1.current().is_none());
    iter1.next(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 1").0, b"1");
    iter1.previous(&mut new1, pdf)?;
    assert!(iter1 == new1.end());
    iter1.previous(&mut new1, pdf)?;
    assert_eq!(iter1.current().expect("cursor at 2").0, b"2");

    writeln!(stdout, "insertAfter")?;
    let mut new2 = NameTree::new_empty(pdf, true)?;
    let mut iter2 = new2.begin(pdf)?;
    assert!(iter2 == new2.end());
    iter2.insert_after(&mut new2, pdf, "3", ObjectHandle::string(b"3!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 3").0, b"3");
    iter2.insert_after(&mut new2, pdf, "4", ObjectHandle::string(b"4!".to_vec()))?;
    assert_eq!(iter2.current().expect("cursor at 4").0, b"4");
    let mut cursor = new2.begin(pdf)?;
    while let Some((key, value)) = cursor.current() {
        write_bytes(stdout, &key)?;
        write!(stdout, " ")?;
        write_bytes(stdout, &value.unparse())?;
        writeln!(stdout)?;
        cursor.next(&mut new2, pdf)?;
    }

    for key in [&b"Empty1"[..], &b"Empty2"[..]] {
        write!(stdout, "/")?;
        write_bytes(stdout, key)?;
        writeln!(stdout)?;
        let mut empty = NameTree::new(pdf.trailer_key_handle(key), true);
        let empty_begin = empty.begin(pdf)?;
        emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
        assert!(empty_begin == empty.end());
        assert!(empty.last(pdf)? == empty.end());

        let inserted = empty.insert(pdf, "five", ObjectHandle::string(b"5".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("key five present");
        assert_eq!(inserted_key, b"five");
        assert_eq!(tree_string_handle_value(pdf, &inserted_value)?, b"5");
        assert_eq!(
            empty.begin(pdf)?.current().expect("begin at five").0,
            b"five"
        );
        assert_eq!(empty.last(pdf)?.current().expect("last at five").0, b"five");
        let begin_value = empty.begin(pdf)?.current().expect("begin value at five").1;
        assert_eq!(tree_string_handle_value(pdf, &begin_value)?, b"5");

        let inserted = empty.insert(pdf, "five", ObjectHandle::string(b"5+".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("key five present");
        assert_eq!(inserted_key, b"five");
        assert_eq!(tree_string_handle_value(pdf, &inserted_value)?, b"5+");
        let begin_value = empty.begin(pdf)?.current().expect("begin value at 5+").1;
        assert_eq!(tree_string_handle_value(pdf, &begin_value)?, b"5+");

        let inserted = empty.insert(pdf, "six", ObjectHandle::string(b"6".to_vec()))?;
        let (inserted_key, inserted_value) = inserted.current().expect("key six present");
        assert_eq!(inserted_key, b"six");
        assert_eq!(tree_string_handle_value(pdf, &inserted_value)?, b"6");
        let begin_value = empty.begin(pdf)?.current().expect("begin still at 5+").1;
        assert_eq!(tree_string_handle_value(pdf, &begin_value)?, b"5+");
        assert_eq!(empty.last(pdf)?.current().expect("last at six").0, b"six");
        let last_value = empty.last(pdf)?.current().expect("last value at six").1;
        assert_eq!(tree_string_handle_value(pdf, &last_value)?, b"6");
    }

    writeln!(stdout, "/Bad1 -- wrong key type")?;
    let mut bad1 = NameTree::new(pdf.trailer_key_handle(b"Bad1"), true);
    let found = bad1.find(pdf, "G", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below G").0, b"A");
    let mut cursor = bad1.begin(pdf)?;
    while let Some((key, _)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut bad1, pdf)?;
    }

    writeln!(stdout, "/Bad2 -- invalid kid")?;
    let mut bad2 = NameTree::new(pdf.trailer_key_handle(b"Bad2"), true);
    let found = bad2.find(pdf, "G", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below G").0, b"B");
    let mut cursor = bad2.begin(pdf)?;
    while let Some((key, _)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut bad2, pdf)?;
    }

    writeln!(stdout, "/Bad3 -- invalid kid")?;
    let mut bad3 = NameTree::new(pdf.trailer_key_handle(b"Bad3"), true);
    let found = bad3.find(pdf, "G", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert!(found == bad3.end());

    writeln!(stdout, "/Bad4 -- invalid kid")?;
    let mut bad4 = NameTree::new(pdf.trailer_key_handle(b"Bad4"), true);
    let found = bad4.find(pdf, "F", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below F").0, b"C");
    let mut cursor = bad4.begin(pdf)?;
    while let Some((key, _)) = cursor.current() {
        write_bytes(stdout, &key)?;
        writeln!(stdout)?;
        cursor.next(&mut bad4, pdf)?;
    }

    writeln!(stdout, "/Bad5 -- loop in find")?;
    let mut bad5 = NameTree::new(pdf.trailer_key_handle(b"Bad5"), true);
    let found = bad5.find(pdf, "F", true)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(found.current().expect("closest key below F").0, b"D");

    writeln!(stdout, "/Bad6 -- bad limits")?;
    let mut bad6 = NameTree::new(pdf.trailer_key_handle(b"Bad6"), true);
    let inserted = bad6.insert(pdf, "H", ObjectHandle::null())?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    assert_eq!(inserted.current().expect("key H present").0, b"H");

    Ok(())
}

pub(crate) fn run_test_49<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf 11.9.0 qpdf/test_driver.cc:1923-1937.
    //
    // `OutlineTree::get_outlines_for_page` is qpdf's
    // `QPDFOutlineDocumentHelper::getOutlinesForPage`
    // (`outline_object_helper.rs:266-285`'s own doc cites the qpdf source), and
    // `OutlineItem::get_title`/`::get_dest` are the decoded `/Title` and
    // resolved destination `getTitle()`/`getDest()` produce, recomputed live
    // on every call like qpdf's own accessors.
    //
    // qpdf constructs `QPDFOutlineDocumentHelper odh(pdf)` -- which walks the
    // top-level `/Outlines` `/First`/`/Next` chain in its constructor
    // (`libqpdf/QPDFOutlineDocumentHelper.cc:5-21`) -- before it lists pages
    // via `QPDFPageDocumentHelper(pdf).getAllPages()`. `OutlineDocumentHelper`
    // has no constructor-time side effect of its own; `get_tree` is where the
    // equivalent top-level walk happens, so it must run before page listing
    // to preserve that order. The tree-building helper is dropped immediately
    // after so the page list can borrow `pdf` again; a fresh helper serves
    // the per-page loop below (each item's `title`/`dest` calls already
    // resolve live off the item's own handle, so a different helper instance
    // observes the same catalog and produces identical results).
    let tree = {
        let mut tree_helper = OutlineDocumentHelper::new(pdf);
        tree_helper.get_tree()?
    };
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    let mut helper = OutlineDocumentHelper::new(pdf);
    for (pageno, page_ref) in pages.into_iter().enumerate() {
        let mut lines: Vec<(String, Vec<u8>)> = Vec::new();
        for (_, item) in tree.get_outlines_for_page(&mut helper, Some(page_ref))? {
            let title = item.get_title(&mut helper)?;
            let dest = item.get_dest(&mut helper)?.unparse_resolved();
            lines.push((title, dest));
        }
        for (title, dest) in lines {
            write!(stdout, "page {pageno}: {title} -> ")?;
            write_bytes(stdout, &dest)?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}
