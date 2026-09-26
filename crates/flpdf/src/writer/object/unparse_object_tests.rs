//! qpdf correspondence: tests for `QPDFWriter::unparseObject`, `unparseChild`, and `writeTrailer` writer emission.

use crate::object_handle::identity_tests::{error_resolving_handle, resolver_bearing_handle};
use crate::object_handle::{ObjectHandle, ObjectValue, StreamValue};
use crate::writer::object::{
    dict_is_sig_with_byte_range, unparse_child as unparse_child_to_sink, visible_dict_entries,
    ObjectWriterEmissionVecTestExt,
};
use crate::{ObjectRef, Result};
use std::collections::BTreeSet;
use std::rc::Rc;

fn unparse_child(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    crate::writer::output::with_buffer_sink(out, |out| unparse_child_to_sink(handle, out))
}

fn compact_string_hook(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    out.extend_from_slice(b"<hook:");
    out.extend_from_slice(value);
    out.extend_from_slice(b">");
    Ok(())
}

fn qdf_string_hook(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    out.extend_from_slice(b"{hook:");
    out.extend_from_slice(value);
    out.extend_from_slice(b"}");
    Ok(())
}

#[test]
fn visible_dict_entries_keeps_non_null_and_drops_direct_null() {
    let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![
        (b"Zulu".to_vec(), ObjectHandle::integer(26)),
        (b"DirectNull".to_vec(), ObjectHandle::null()),
    ];
    let visible = visible_dict_entries(&entries).expect("no resolver needed");
    let keys: Vec<&[u8]> = visible.iter().map(|(k, _)| k.as_slice()).collect();
    assert_eq!(keys, [b"Zulu".as_slice()]);
}

#[test]
fn visible_dict_entries_resolves_and_drops_an_indirect_null() {
    let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
    let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![(b"RefNull".to_vec(), indirect_null)];
    let visible = visible_dict_entries(&entries).unwrap();
    assert!(visible.is_empty());
}

#[test]
fn visible_dict_entries_propagates_a_dropped_document_error() {
    let (indirect_null, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![(b"RefNull".to_vec(), indirect_null)];
    assert!(visible_dict_entries(&entries).is_err());
}

#[test]
fn dict_is_sig_with_byte_range_true_when_both_present() {
    let entries = vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
    ];
    assert!(dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_false_when_byte_range_is_a_direct_null() {
    // `QPDF_Dictionary::hasKey` (QPDF_Dictionary.cc:98-101) is
    // `items.count(key) > 0 && !items[key].isNull()` -- a null
    // `/ByteRange` value counts as *absent*, the same way a null-valued
    // entry is excluded from `visible_dict_entries`'s own output.
    let entries = vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"/ByteRange".to_vec(), ObjectHandle::null()),
    ];
    assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_false_when_byte_range_resolves_to_an_indirect_null() {
    // Same null-exclusion rule as the direct case above, but through
    // `isNull()`'s own dereference (QPDFObjectHandle.cc:353-356) --
    // `hasKey` resolves an indirect `/ByteRange` to decide this too.
    let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
    let entries = vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"/ByteRange".to_vec(), indirect_null),
    ];
    assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_propagates_a_dropped_document_error_from_byte_range() {
    // `/Type` is a direct `/Sig`, so the short-circuit passes it and
    // reaches `/ByteRange`'s own forced resolution, which must
    // propagate a dropped-document error the same way `/Type`'s own
    // resolution does.
    let (indirect_byte_range, resolver) = error_resolving_handle(ObjectRef::new(32, 0));
    drop(resolver);
    let entries = vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"/ByteRange".to_vec(), indirect_byte_range),
    ];
    assert!(dict_is_sig_with_byte_range(&entries).is_err());
}

#[test]
fn dict_is_sig_with_byte_range_false_without_byte_range_key() {
    let entries = vec![(b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec()))];
    assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_false_without_type_key() {
    let entries = vec![(b"/ByteRange".to_vec(), ObjectHandle::array(vec![]))];
    assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_false_when_type_is_not_sig() {
    let entries = vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
    ];
    assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_resolves_an_indirect_type_that_is_sig() {
    // Mirrors qpdf's own `getKey("/Type").isNameAndEquals("/Sig")`,
    // which dereferences through `isName()` -- an indirect `/Type` must
    // be force-resolved to decide this, not conservatively treated as
    // "not Sig".
    let (indirect_type, _resolver) = resolver_bearing_handle(ObjectValue::Name(b"Sig".to_vec()));
    let entries = vec![
        (b"/Type".to_vec(), indirect_type),
        (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
    ];
    assert!(dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn dict_is_sig_with_byte_range_propagates_a_dropped_document_error_from_type() {
    let (indirect_type, resolver) = error_resolving_handle(ObjectRef::new(30, 0));
    drop(resolver);
    let entries = vec![
        (b"/Type".to_vec(), indirect_type),
        (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
    ];
    assert!(dict_is_sig_with_byte_range(&entries).is_err());
}

#[test]
fn dict_is_sig_with_byte_range_does_not_resolve_byte_range_when_type_is_not_sig() {
    // Mirrors qpdf's own `&&` short-circuit
    // (`isDictionaryOfType("/Sig") && hasKey("/ByteRange")`,
    // QPDFWriter.cc:1497-1498): `/ByteRange`'s resolver must never run
    // at all once `/Type` is confirmed not `/Sig` -- an indirect
    // `/ByteRange` whose resolver would error must not surface that
    // error here.
    let (indirect_byte_range, _resolver) = error_resolving_handle(ObjectRef::new(31, 0));
    let entries = vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (b"/ByteRange".to_vec(), indirect_byte_range),
    ];
    assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
}

#[test]
fn unparse_object_writes_sig_contents_as_a_hex_string() {
    // `/Contents` is a printable-ASCII direct String, which the ordinary
    // `write_string_value` path (via `use_hex_string`) would write as a
    // literal string `(hi)` -- confirms the Sig+ByteRange special case
    // overrides that choice with `write_hex_string`'s own byte shape
    // (`<` + one lowercase hex pair per byte + `>`: `h` = 0x68, `i` =
    // 0x69).
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
    ]);
    let mut out = Vec::new();
    dict.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /ByteRange [ ] /Contents <6869> /Type /Sig >>");
}

#[test]
fn unparse_object_leaves_contents_literal_without_sig_type() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
    ]);
    let mut out = Vec::new();
    dict.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /ByteRange [ ] /Contents (hi) /Type /Page >>");
}

#[test]
fn unparse_object_leaves_contents_literal_without_byte_range() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
    ]);
    let mut out = Vec::new();
    dict.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /Contents (hi) /Type /Sig >>");
}

#[test]
fn unparse_object_writes_an_indirect_sig_contents_as_reference_form_not_hex() {
    // Mirrors `unparseChild`'s own indirect-first short-circuit
    // (QPDFWriter.cc:1149-1156): an indirect `/Contents` value writes
    // as its own "N G R" reference form regardless of the Sig+ByteRange
    // condition -- qpdf's flags are only consulted inside
    // `unparseObject`, which an indirect child never reaches.
    let (indirect_contents, _resolver) =
        resolver_bearing_handle(ObjectValue::String(b"hi".to_vec()));
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), indirect_contents),
    ]);
    let mut out = Vec::new();
    dict.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /ByteRange [ ] /Contents 20 0 R /Type /Sig >>");
}

#[test]
fn unparse_object_leaves_a_non_string_sig_contents_unaffected() {
    // The Sig+ByteRange special case only affects a child whose
    // resolved value is itself a String (QPDFWriter.cc's `f_hex_string`
    // handling lives inside the `ot_string` arm alone) -- a non-String
    // direct `/Contents` (unusual in practice, but not structurally
    // ruled out) falls through to the ordinary child-writer unaffected.
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::integer(7)),
    ]);
    let mut out = Vec::new();
    dict.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /ByteRange [ ] /Contents 7 /Type /Sig >>");
}

#[test]
fn unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
    // `dict_is_sig_with_byte_range`
    // was hoisted to run once, unconditionally, before the per-entry loop
    // even started -- rather than lazily, only when the loop's own
    // iteration actually reaches a surviving `/Contents` key -- for the
    // Sig+ByteRange hex-string special case that same commit added. This
    // reintroduced an unnecessary-force-resolution bug. The
    // `refiltered`-key exclusion test (see
    // `unparse_stream_dict_entries`'s own doc) covers
    // `/Filter`/`/DecodeParms`: `/Type` (and, conditionally,
    // `/ByteRange`) now gets force-resolved even for a dict with no
    // `/Contents` key at all to apply the special case to -- see
    // `try_write_sig_contents_hex_string`'s own doc for why the operand
    // order matters, not just the call site's existence.
    //
    // This dict has no `/Contents` key, so a correctly lazy
    // implementation never even calls `dict_is_sig_with_byte_range` --
    // `/Type`'s own resolution must be driven solely by
    // `visible_dict_entries`'s ordinary per-key null-suppression pass,
    // which runs in the dict's own (`BTreeMap`) alphabetical key order.
    // `/AAA` sorts before `/Type`; both are dropped-document handles at
    // distinct object refs, so the surfaced error text -- which embeds
    // the failing ref, see
    // `try_dereference_reports_a_dropped_document_without_reconnecting`
    // -- pins exactly which one was actually touched first: with the
    // eager bug, `/Type` (object 30) is resolved before the
    // null-suppression loop even starts, so its error surfaces even
    // though `/AAA` (object 99) sorts first in qpdf's own single-pass
    // loop order; with the fix, `/AAA`'s error surfaces instead, and
    // `/Type`'s handle is never dereferenced at all. (A plain
    // success-vs-error assertion cannot discriminate this bug on its
    // own: `visible_dict_entries` itself already force-resolves every
    // surviving entry -- including `/Type`/`/ByteRange` whenever they
    // are present as dict keys -- for the ordinary null-suppression
    // check, matching qpdf's own `isNull()` call inside the identical
    // per-item loop, `QPDFWriter.cc:1488-1491`; a dict with a genuinely
    // erroring `/Type` errors either way. The bug is about *which*
    // resolution happens first among several, not whether the overall
    // call succeeds.)
    let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
    drop(aaa_resolver);
    let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
    drop(type_resolver);
    let dict = ObjectHandle::dictionary(vec![
        (b"AAA".to_vec(), aaa),
        (b"Type".to_vec(), sig_type),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
    ]);
    let mut out = Vec::new();
    let error = dict.unparse_object(&mut out).unwrap_err();
    assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
}

#[test]
fn unparse_object_qdf_writes_sig_contents_as_a_hex_string() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
    ]);
    let mut out = Vec::new();
    dict.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(
        out,
        b"<<\n  /ByteRange [\n  ]\n  /Contents <6869>\n  /Type /Sig\n>>"
    );
}

#[test]
fn unparse_object_qdf_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
    // QDF sibling of
    // `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
    // above -- `unparse_dict_entries_qdf` needed the identical fix at
    // its own call site. See that test's own doc for the full
    // eager-vs-lazy rationale and why a plain success-vs-error
    // assertion cannot discriminate this bug.
    let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
    drop(aaa_resolver);
    let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
    drop(type_resolver);
    let dict = ObjectHandle::dictionary(vec![
        (b"AAA".to_vec(), aaa),
        (b"Type".to_vec(), sig_type),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
    ]);
    let mut out = Vec::new();
    let error = dict.unparse_object_qdf(&mut out, 0).unwrap_err();
    assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
}

#[test]
fn unparse_stream_body_writes_sig_contents_as_a_hex_string() {
    // The Sig+ByteRange special case has no `f_stream` guard in real
    // qpdf either (QPDFWriter.cc:1490-1504 is the same shared loop the
    // stream-dictionary branch falls into) -- a stream whose dict
    // happens to be `/Type /Sig` with `/ByteRange` is unusual but not
    // structurally ruled out.
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(2)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(
        out,
        b"<< /ByteRange [ ] /Contents <6869> /Type /Sig /Length 2 >>"
    );
}

#[test]
fn unparse_stream_body_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
    // Stream sibling of
    // `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
    // above -- `unparse_stream_dict_entries` needed the identical fix at
    // its own call site. See that test's own doc for the full
    // eager-vs-lazy rationale and why a plain success-vs-error
    // assertion cannot discriminate this bug. `/Length` is a harmless
    // direct value here -- this test is not about the `refiltered`
    // dimension Finding 2 already fixed, so `refiltered == false`.
    let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
    drop(aaa_resolver);
    let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
    drop(type_resolver);
    let dict = ObjectHandle::dictionary(vec![
        (b"AAA".to_vec(), aaa),
        (b"Type".to_vec(), sig_type),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Length".to_vec(), ObjectHandle::integer(0)),
    ]);
    let mut out = Vec::new();
    let error = dict.unparse_stream_body(&mut out, false).unwrap_err();
    assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
}

#[test]
fn unparse_child_writes_indirect_handle_as_reference_form() {
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let mut out = Vec::new();
    unparse_child(&indirect, &mut out).unwrap();
    assert_eq!(out, b"20 0 R");
}

#[test]
fn unparse_child_recurses_into_a_direct_scalar() {
    let mut out = Vec::new();
    unparse_child(&ObjectHandle::integer(7), &mut out).unwrap();
    assert_eq!(out, b"7");
}

#[test]
fn unparse_object_writes_a_scalar() {
    let mut out = Vec::new();
    ObjectHandle::integer(42).unparse_object(&mut out).unwrap();
    assert_eq!(out, b"42");
}

#[test]
fn unparse_object_writes_a_boolean() {
    let mut out = Vec::new();
    ObjectHandle::boolean(true)
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"true");
    out.clear();
    ObjectHandle::boolean(false)
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"false");
}

#[test]
fn unparse_object_writes_a_real() {
    let mut out = Vec::new();
    ObjectHandle::real(0.5).unparse_object(&mut out).unwrap();
    assert_eq!(out, b"0.5");
}

#[test]
fn unparse_object_writes_a_string() {
    let mut out = Vec::new();
    ObjectHandle::string(b"hi".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"(hi)");
}

#[test]
fn unparse_object_writes_an_operator_verbatim() {
    // ObjectValue::InlineImage shares the identical match arm and byte
    // path, so this one case covers both bindings.
    let mut out = Vec::new();
    ObjectHandle::operator(b"q".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"q");
}

#[test]
fn unparse_object_serializes_large_scalar_payloads_without_deep_snapshotting() {
    let string_payload = vec![b's'; 256 * 1024];
    let mut out = Vec::new();
    ObjectHandle::string(string_payload.clone())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out.len(), string_payload.len() + 2);
    assert_eq!(out.first(), Some(&b'('));
    assert_eq!(out.last(), Some(&b')'));

    let operator_payload = vec![b'o'; 256 * 1024];
    out.clear();
    ObjectHandle::operator(operator_payload.clone())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, operator_payload);

    let inline_image_payload = vec![b'i'; 256 * 1024];
    out.clear();
    ObjectHandle::inline_image(inline_image_payload.clone())
        .unparse_object_qdf(&mut out, 0)
        .unwrap();
    assert_eq!(out, inline_image_payload);
}

#[test]
fn unparse_object_inlines_only_the_dictionary_of_a_direct_stream_value() {
    // A *direct* Stream ObjectValue has no qpdf counterpart (a real
    // QPDFObjectHandle's resolved value is never itself a stream
    // outside an indirect object), so there is no byte-parity oracle
    // here. This pins down the same "inline the dictionary, do not
    // write the `stream`/`endstream` framing" behavior for which
    // `unparse_stream_body` is separately responsible, and stays consistent with
    // that primitive's scope rather than reproducing framing logic here.
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let handle = ObjectHandle::from_value(ObjectValue::Stream(Box::new(StreamValue {
        stream_dict: dict,
        stream_data: Some(Rc::new(b"ab".to_vec())),
        stream_provider: None,
        filter_on_write: true,
        stream_token_filters: Default::default(),
        content_normalization_applied: false,
        stream_length: 0,
    })));
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /Length 2 >>");
}

#[test]
fn unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary() {
    // Unlike the direct-stream case above, this *is* a real, reachable
    // qpdf shape: an indirect object whose resolved value is a stream.
    // `unparse_object`/`unparse_object_walk` dispatch on `self` directly
    // (never through `unparse_child`'s indirect-reference short-circuit,
    // which only applies to *child* positions during recursion), so
    // this reaches the same `ObjectValue::Stream` arm as the direct
    // case and inlines just the dictionary -- not qpdf's real
    // stream-writing output at this position (see
    // `ObjectHandle::unparse_object`'s own doc). Pins today's actual
    // behavior; `unparse_stream_body` is the primitive that implements the
    // real stream-writing path.
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let (indirect, _resolver) =
        resolver_bearing_handle(ObjectValue::Stream(Box::new(StreamValue {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_token_filters: Default::default(),
            content_normalization_applied: false,
            stream_length: 0,
        })));
    let mut out = Vec::new();
    indirect.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /Length 2 >>");
}

#[test]
fn unparse_object_writes_a_name_escaped() {
    let mut out = Vec::new();
    ObjectHandle::name(b"application/pdf".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"/application#2fpdf");
}

#[test]
fn unparse_object_writes_a_real_literal_when_safe() {
    let mut out = Vec::new();
    ObjectHandle::real_literal(0.4, b".4".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b".4");
}

#[test]
fn unparse_object_falls_back_to_canonical_when_literal_is_unsafe() {
    let mut out = Vec::new();
    ObjectHandle::real_literal(0.4, b"nope".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"0.4");
}

#[test]
fn unparse_object_writes_an_array_with_qpdf_spacing() {
    let handle = ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(2)]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"[ 1 2 ]");
}

#[test]
fn unparse_object_writes_an_empty_array() {
    let mut out = Vec::new();
    ObjectHandle::array(vec![])
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"[ ]");
}

#[test]
fn unparse_object_writes_a_dict_and_suppresses_direct_null() {
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"B".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /A 1 >>");
}

#[test]
fn unparse_object_suppresses_an_indirect_entry_resolving_to_null() {
    let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"RefNull".to_vec(), indirect_null),
    ]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /A 1 >>");
}

#[test]
fn unparse_object_writes_an_empty_dict_when_every_entry_is_suppressed() {
    let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< >>");
}

#[test]
fn unparse_object_writes_a_retained_indirect_entry_as_reference_form() {
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), indirect)]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /A 20 0 R >>");
}

#[test]
fn unparse_object_propagates_a_dropped_document_error() {
    let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let mut out = Vec::new();
    assert!(indirect.unparse_object(&mut out).is_err());
}

#[test]
fn unparse_encrypted_string_writer_replaces_direct_strings_only() {
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::String(b"hidden".to_vec()));
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::string(b"plain".to_vec())),
        (b"Indirect".to_vec(), indirect),
        (
            b"Nested".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::string(b"nested".to_vec())]),
        ),
    ]);
    let mut out = Vec::new();
    let mut callback = compact_string_hook;
    handle
        .unparse_object_with_string_writer(&mut out, &mut callback)
        .unwrap();
    assert_eq!(
        out,
        b"<< /A <hook:plain> /Indirect 20 0 R /Nested [ <hook:nested> ] >>"
    );
}

#[test]
fn unparse_encrypted_string_writer_qdf_keeps_signature_contents_cleartext_hex() {
    let handle = ObjectHandle::dictionary(vec![
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (
            b"Contents".to_vec(),
            ObjectHandle::string(b"contents".to_vec()),
        ),
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
    ]);
    let mut out = Vec::new();
    let mut callback = qdf_string_hook;
    handle
        .unparse_object_qdf_with_string_writer(&mut out, 4, &mut callback)
        .unwrap();
    assert_eq!(
        out,
        b"<<\n      /ByteRange [\n      ]\n      /Contents <636f6e74656e7473>\n      /Type /Sig\n    >>"
    );

    let mut compact = Vec::new();
    let mut compact_callback = compact_string_hook;
    handle
        .unparse_object_with_string_writer(&mut compact, &mut compact_callback)
        .unwrap();
    assert_eq!(
        compact,
        b"<< /ByteRange [ ] /Contents <636f6e74656e7473> /Type /Sig >>"
    );

    let mut stream_compact = Vec::new();
    let mut stream_compact_callback = compact_string_hook;
    handle
        .unparse_stream_body_with_string_writer(
            &mut stream_compact,
            false,
            &mut stream_compact_callback,
        )
        .unwrap();
    assert_eq!(
        stream_compact,
        b"<< /ByteRange [ ] /Contents <636f6e74656e7473> /Type /Sig >>"
    );

    let mut stream_qdf = Vec::new();
    let mut stream_qdf_callback = qdf_string_hook;
    handle
        .unparse_stream_body_qdf_with_string_writer(&mut stream_qdf, 0, &mut stream_qdf_callback)
        .unwrap();
    assert_eq!(
        stream_qdf,
        b"<<\n  /ByteRange [\n  ]\n  /Contents <636f6e74656e7473>\n  /Type /Sig\n>>"
    );
}

#[test]
fn unparse_encrypted_string_writer_stream_bodies_keep_qpdf_layout() {
    let dict = ObjectHandle::dictionary(vec![
        (
            b"DecodeParms".to_vec(),
            ObjectHandle::dictionary(vec![(
                b"Name".to_vec(),
                ObjectHandle::string(b"params".to_vec()),
            )]),
        ),
        (
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        ),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut compact = Vec::new();
    let mut compact_callback = compact_string_hook;
    dict.unparse_stream_body_with_string_writer(&mut compact, false, &mut compact_callback)
        .unwrap();
    assert_eq!(
        compact,
        b"<< /DecodeParms << /Name <hook:params> >> /Filter /FlateDecode /Length 3 >>"
    );

    let mut qdf = Vec::new();
    let mut qdf_callback = qdf_string_hook;
    dict.unparse_stream_body_qdf_with_string_writer(&mut qdf, 2, &mut qdf_callback)
        .unwrap();
    assert_eq!(
        qdf,
        b"<<\n    /DecodeParms <<\n      /Name {hook:params}\n    >>\n    /Filter /FlateDecode\n    /Length 3\n  >>"
    );
}

#[test]
fn unparse_string_writer_covers_stream_reserved_refiltered_and_special_children() {
    let stream_dict = ObjectHandle::dictionary(vec![
        (
            b"DecodeParms".to_vec(),
            ObjectHandle::dictionary(vec![(
                b"Name".to_vec(),
                ObjectHandle::string(b"params".to_vec()),
            )]),
        ),
        (
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        ),
        (b"Label".to_vec(), ObjectHandle::string(b"stream".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let stream = ObjectHandle::stream(stream_dict.clone(), Rc::new(b"raw".to_vec()));
    let mut compact = Vec::new();
    let mut compact_callback = compact_string_hook;
    stream
        .unparse_object_with_string_writer(&mut compact, &mut compact_callback)
        .expect("direct stream object callback emission");
    assert!(compact
        .windows(b"<hook:stream>".len())
        .any(|part| part == b"<hook:stream>"));

    let mut qdf = Vec::new();
    let mut qdf_callback = qdf_string_hook;
    stream
        .unparse_object_qdf_with_string_writer(&mut qdf, 2, &mut qdf_callback)
        .expect("direct stream object QDF callback emission");
    assert!(qdf
        .windows(b"{hook:stream}".len())
        .any(|part| part == b"{hook:stream}"));

    let mut stream_body = Vec::new();
    let mut stream_body_callback = compact_string_hook;
    stream
        .unparse_stream_body_with_string_writer(&mut stream_body, false, &mut stream_body_callback)
        .expect("direct stream compact body callback emission");
    assert!(stream_body
        .windows(b"<hook:stream>".len())
        .any(|part| part == b"<hook:stream>"));

    let mut refiltered = Vec::new();
    let mut refiltered_callback = compact_string_hook;
    stream_dict
        .unparse_stream_body_with_string_writer(&mut refiltered, true, &mut refiltered_callback)
        .expect("refiltered stream dictionary callback emission");
    assert!(!refiltered
        .windows(b"DecodeParms".len())
        .any(|part| part == b"DecodeParms"));
    assert!(refiltered
        .windows(b"/Filter /FlateDecode".len())
        .any(|part| part == b"/Filter /FlateDecode"));

    let mut stream_qdf = Vec::new();
    let mut stream_qdf_callback = qdf_string_hook;
    stream
        .unparse_stream_body_qdf_with_string_writer(&mut stream_qdf, 1, &mut stream_qdf_callback)
        .expect("stream value QDF body callback emission");
    assert!(stream_qdf
        .windows(b"{hook:stream}".len())
        .any(|part| part == b"{hook:stream}"));

    let scalar_stream = ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(Vec::new()));
    let mut scalar_stream_body = Vec::new();
    let mut scalar_stream_callback = compact_string_hook;
    scalar_stream
        .unparse_stream_body_with_string_writer(
            &mut scalar_stream_body,
            false,
            &mut scalar_stream_callback,
        )
        .expect("stream with a non-dictionary dictionary handle uses an empty body");
    assert_eq!(scalar_stream_body, b"<< >>");
    let mut scalar_stream_qdf_body = Vec::new();
    let mut scalar_stream_qdf_callback = qdf_string_hook;
    scalar_stream
        .unparse_stream_body_qdf_with_string_writer(
            &mut scalar_stream_qdf_body,
            1,
            &mut scalar_stream_qdf_callback,
        )
        .expect("QDF stream with a non-dictionary dictionary handle uses an empty body");
    assert_eq!(scalar_stream_qdf_body, b"<<\n >>");

    let mut scalar_body = Vec::new();
    let mut scalar_callback = compact_string_hook;
    ObjectHandle::integer(1)
        .unparse_stream_body_with_string_writer(&mut scalar_body, false, &mut scalar_callback)
        .expect("non-dictionary stream body degrades to an empty dictionary");
    assert_eq!(scalar_body, b"<< >>");
    let mut scalar_qdf_body = Vec::new();
    let mut scalar_qdf_callback = qdf_string_hook;
    ObjectHandle::integer(1)
        .unparse_stream_body_qdf_with_string_writer(
            &mut scalar_qdf_body,
            1,
            &mut scalar_qdf_callback,
        )
        .expect("non-dictionary QDF stream body degrades to an empty dictionary");
    assert_eq!(scalar_qdf_body, b"<<\n >>");

    let reserved = ObjectHandle::new_reserved_direct();
    let mut reserved_out = Vec::new();
    let mut reserved_callback = compact_string_hook;
    assert!(reserved
        .unparse_object_with_string_writer(&mut reserved_out, &mut reserved_callback)
        .is_err());
    assert!(reserved
        .unparse_object_qdf_with_string_writer(&mut reserved_out, 0, &mut reserved_callback,)
        .is_err());
    assert!(reserved
        .unparse_stream_body_with_string_writer(&mut reserved_out, false, &mut reserved_callback,)
        .is_err());
    assert!(reserved
        .unparse_stream_body_qdf_with_string_writer(&mut reserved_out, 0, &mut reserved_callback,)
        .is_err());

    let non_string_sig = ObjectHandle::dictionary(vec![
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::integer(7)),
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
    ]);
    let mut non_string_sig_out = Vec::new();
    let mut non_string_sig_callback = compact_string_hook;
    non_string_sig
        .unparse_object_with_string_writer(&mut non_string_sig_out, &mut non_string_sig_callback)
        .expect("non-string signature contents use the ordinary child writer");
    assert!(non_string_sig_out
        .windows(b"/Contents 7".len())
        .any(|part| part == b"/Contents 7"));

    let (indirect_contents, _resolver) =
        resolver_bearing_handle(ObjectValue::String(b"indirect".to_vec()));
    let indirect_sig = ObjectHandle::dictionary(vec![
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), indirect_contents),
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
    ]);
    let mut indirect_sig_out = Vec::new();
    let mut indirect_sig_callback = qdf_string_hook;
    indirect_sig
        .unparse_object_qdf_with_string_writer(&mut indirect_sig_out, 0, &mut indirect_sig_callback)
        .expect("indirect signature contents use the reference writer");
    assert!(indirect_sig_out
        .windows(b"/Contents 20 0 R".len())
        .any(|part| part == b"/Contents 20 0 R"));
}

#[test]
fn unparse_object_qdf_writes_a_scalar_like_plain_unparse() {
    let mut out = Vec::new();
    ObjectHandle::integer(42)
        .unparse_object_qdf(&mut out, 0)
        .unwrap();
    assert_eq!(out, b"42");
}

#[test]
fn unparse_object_qdf_writes_an_array_with_newline_indent() {
    let handle = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"[\n  1\n]");
}

#[test]
fn unparse_object_qdf_writes_a_dict_with_newline_indent_and_suppresses_null() {
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"B".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /A 1\n>>");
}

#[test]
fn unparse_object_qdf_nests_indent_one_level_deeper() {
    let handle = ObjectHandle::dictionary(vec![(
        b"Kids".to_vec(),
        ObjectHandle::array(vec![ObjectHandle::integer(1)]),
    )]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /Kids [\n    1\n  ]\n>>");
}

#[test]
fn unparse_object_qdf_writes_a_retained_indirect_entry_as_reference_form() {
    // QDF-mode sibling of unparse_object_writes_a_retained_indirect_entry_as_reference_form:
    // exercises unparse_child_qdf's indirect arm, which the four
    // plan-specified literals above never reach (every handle in them is
    // direct).
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), indirect)]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /A 20 0 R\n>>");
}

#[test]
fn unparse_object_qdf_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary() {
    // QDF-mode sibling of
    // unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary:
    // exercises unparse_object_value_qdf's Stream arm, unreached by the
    // four plan-specified literals above.
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let (indirect, _resolver) =
        resolver_bearing_handle(ObjectValue::Stream(Box::new(StreamValue {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_token_filters: Default::default(),
            content_normalization_applied: false,
            stream_length: 0,
        })));
    let mut out = Vec::new();
    indirect.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /Length 2\n>>");
}

#[test]
fn unparse_object_qdf_nests_a_dict_inside_a_dict_at_every_indent_slot() {
    // The plan-specified nesting literal only stretches the *array*
    // closing bracket's indent slot (unparse_object_qdf_nests_indent_one_level_deeper).
    // A dict nested in a dict stretches unparse_dict_entries_qdf's own
    // closing `>>` indent slot too, which that test leaves at the
    // top-level `indent = 0` default.
    let handle = ObjectHandle::dictionary(vec![(
        b"D".to_vec(),
        ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]),
    )]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /D <<\n    /A 1\n  >>\n>>");
}

#[test]
fn unparse_object_qdf_propagates_a_dropped_document_error() {
    let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let mut out = Vec::new();
    assert!(indirect.unparse_object_qdf(&mut out, 0).is_err());
}

#[test]
fn unparse_object_qdf_respects_a_nonzero_starting_indent() {
    // Every other QDF test in this module calls the public entry point
    // with `indent = 0`, so the internal `indent + 2` recursion is
    // exercised but the *argument's own arrival* at the public method
    // never is -- a stray `indent = 0` hardcoded inside the function
    // body would still pass every one of them. Start at a nonzero
    // column instead, so the dict's closing `>>` (written at the
    // caller's own `indent`, unincremented) and its one entry (written
    // at `indent + 2`) both prove the argument actually reached them.
    let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 4).unwrap();
    assert_eq!(out, b"<<\n      /A 1\n    >>");
}

#[test]
fn unparse_object_qdf_writes_an_empty_dict_at_a_nonzero_indent() {
    // Sibling of the suppresses-null test above, but with *every* entry
    // gone (here: none to begin with) and at a nonzero starting indent
    // -- untested at any indent before this. Only the closing `>>`
    // carries an indent slot when there are no surviving entries; this
    // pins that it still lands at the caller's own `indent`, not the
    // default `0`.
    let handle = ObjectHandle::dictionary(vec![]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 4).unwrap();
    assert_eq!(out, b"<<\n    >>");
}

#[test]
fn unparse_stream_body_writes_length_last_preserved() {
    // `/DecodeParms` is a non-null (empty-dictionary) value here, not
    // `ObjectHandle::null()`: a null value would already be excluded by
    // `visible_dict_entries`'s own null-suppression pass before the
    // `refiltered` check ever saw the key, which would let this test
    // pass even if `unparse_stream_dict_entries` unconditionally
    // dropped `/DecodeParms` regardless of `refiltered`. With
    // `refiltered == false`, both `/DecodeParms` and `/Filter` must
    // survive at their natural (`BTreeMap`) lexicographic positions
    // (`DecodeParms` < `Filter` < the pulled-out-and-appended
    // `Length`), unlike the refiltered case pinned by
    // `unparse_stream_body_refiltered_drops_filter_and_decodeparms_appends_flate`
    // below, where both are dropped.
    let dict = ObjectHandle::dictionary(vec![
        (
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        ),
        (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(
        out,
        b"<< /DecodeParms << >> /Filter /FlateDecode /Length 3 >>"
    );
}

#[test]
fn unparse_stream_body_refiltered_drops_filter_and_decodeparms_appends_flate() {
    // `/DecodeParms` is a non-null (empty-dictionary) value here, not
    // `ObjectHandle::null()`: a null value would already be excluded by
    // `visible_dict_entries`'s own null-suppression pass before the
    // refiltered check ever saw the key, which would let this test pass
    // even if the `key.as_slice() == b"DecodeParms"` disjunct were
    // dropped from that check entirely.
    let dict = ObjectHandle::dictionary(vec![
        (
            b"Filter".to_vec(),
            ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
        ),
        (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, true).unwrap();
    assert_eq!(out, b"<< /Length 3 /Filter /FlateDecode >>");
}

#[test]
fn unparse_stream_body_refiltered_removes_filter_parameters_before_visibility() {
    // Refiltered output must exclude these entries before visibility
    // checks:
    // `visible_dict_entries` previously ran over every entry --
    // including `/Filter`/`/DecodeParms` -- before the `refiltered`
    // skip in the write loop below ever got a chance to drop them,
    // force-resolving (and potentially failing on) a value guaranteed
    // to be discarded from the refiltered output. Real qpdf removes
    // both keys from a shallow copy of the dict entirely BEFORE its
    // null-suppression loop runs (`QPDFWriter.cc:1454-1455`, ahead of
    // `:1488-1491`) -- it never calls `isNull()` on a key it is about
    // to discard anyway.
    //
    // An indirect `/Filter` whose resolver would error, with
    // `refiltered == true`, must be excluded before suppression inspects
    // it. `/DecodeParms` is a direct empty array here so qpdf's earlier
    // empty-array probe is exercised without changing that error boundary.
    let (filter, _filter_resolver) = error_resolving_handle(ObjectRef::new(40, 0));
    let decode_parms = ObjectHandle::array(Vec::new());
    let dict = ObjectHandle::dictionary(vec![
        (b"Filter".to_vec(), filter),
        (b"DecodeParms".to_vec(), decode_parms),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, true).unwrap();
    assert_eq!(out, b"<< /Length 3 /Filter /FlateDecode >>");
}

#[test]
fn unparse_stream_body_not_refiltered_still_propagates_an_indirect_filter_error() {
    // Contrast with the test above: when `refiltered` is false,
    // `/Filter` is a surviving key that must actually be written, so an
    // indirect value whose resolver errors must still surface that
    // error -- proving the fix scopes "skip resolution" to the
    // refiltered case alone, rather than exempting
    // `/Filter`/`/DecodeParms` from resolution unconditionally (which
    // would silently corrupt a non-refiltered stream's real `/Filter`
    // value).
    let (filter, _resolver) = error_resolving_handle(ObjectRef::new(42, 0));
    let dict = ObjectHandle::dictionary(vec![
        (b"Filter".to_vec(), filter),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    assert!(dict.unparse_stream_body(&mut out, false).is_err());
}

#[test]
fn refiltered_stream_still_probes_an_indirect_decode_parms_before_removal() {
    let (decode_parms, _resolver) = error_resolving_handle(ObjectRef::new(43, 0));
    let dict = ObjectHandle::dictionary(vec![
        (
            b"Filter".to_vec(),
            ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
        ),
        (b"DecodeParms".to_vec(), decode_parms),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    let error = dict
        .unparse_stream_body(&mut out, true)
        .expect_err("qpdf probes DecodeParms before the filtered removal branch");
    assert_eq!(error.to_string(), "resolver failed");
}

#[test]
fn mapped_unparse_writes_stream_children_and_non_dictionary_shapes() {
    let (child, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let dict = ObjectHandle::dictionary(vec![
        (b"Child".to_vec(), child),
        (b"Length".to_vec(), ObjectHandle::integer(2)),
    ]);
    let stream = ObjectHandle::stream(dict, Rc::new(b"ab".to_vec()));
    let map = |object_ref| {
        assert_eq!(object_ref, ObjectRef::new(20, 0));
        Ok(ObjectRef::new(8, 0))
    };

    let mut object = Vec::new();
    stream
        .unparse_object_with_ref_map_and_removed(&mut object, &map, &BTreeSet::new())
        .unwrap();
    assert_eq!(object, b"<< /Child 8 0 R /Length 2 >>");

    let mut body = Vec::new();
    stream
        .unparse_stream_body_with_ref_map_and_removed(&mut body, false, &map, &BTreeSet::new())
        .unwrap();
    assert_eq!(body, b"<< /Child 8 0 R /Length 2 >>");

    let signature = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        ]),
        Rc::new(b"ab".to_vec()),
    );
    let mut signature_body = Vec::new();
    signature
        .unparse_stream_body_with_ref_map_and_removed(
            &mut signature_body,
            false,
            &map,
            &BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        signature_body,
        b"<< /ByteRange [ ] /Contents <6869> /Type /Sig >>"
    );

    let mut nested_non_dictionary = Vec::new();
    ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()))
        .unparse_stream_body_with_ref_map_and_removed(
            &mut nested_non_dictionary,
            false,
            &map,
            &BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(nested_non_dictionary, b"<< >>");

    let mut non_dictionary = Vec::new();
    ObjectHandle::integer(5)
        .unparse_stream_body_with_ref_map_and_removed(
            &mut non_dictionary,
            false,
            &map,
            &BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(non_dictionary, b"<< >>");
}

#[test]
fn mapped_unparse_omits_removed_indirect_dictionary_entries() {
    let removed_ref = ObjectRef::new(20, 0);
    let (removed, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let kept = ObjectHandle::new_indirect_unresolved(ObjectRef::new(21, 0), 0);
    kept.set_resolved(ObjectValue::Integer(8));
    let mut removed_refs = BTreeSet::new();
    removed_refs.insert(removed_ref);
    let map = |object_ref| {
        assert_ne!(object_ref, removed_ref);
        Ok(ObjectRef::new(8, 0))
    };

    let dict = ObjectHandle::dictionary(vec![
        (b"Mapped".to_vec(), kept.clone()),
        (b"Removed".to_vec(), removed.clone()),
    ]);
    let mut object = Vec::new();
    dict.unparse_object_with_ref_map_and_removed(&mut object, &map, &removed_refs)
        .unwrap();
    assert_eq!(object, b"<< /Mapped 8 0 R >>");

    let stream = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"Mapped".to_vec(), kept),
            (b"Length".to_vec(), ObjectHandle::integer(2)),
            (b"Removed".to_vec(), removed),
        ]),
        Rc::new(b"ab".to_vec()),
    );
    let mut body = Vec::new();
    stream
        .unparse_stream_body_with_ref_map_and_removed(&mut body, false, &map, &removed_refs)
        .unwrap();
    assert_eq!(body, b"<< /Mapped 8 0 R /Length 2 >>");
}

#[test]
fn mapped_stream_writers_cover_qdf_length_and_filter_variants() {
    let kept = ObjectHandle::new_indirect_unresolved(ObjectRef::new(30, 0), 0);
    kept.set_resolved(ObjectValue::Integer(7));
    let removed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(31, 0), 0);
    removed.set_resolved(ObjectValue::Integer(8));
    let stream = ObjectHandle::from_value(ObjectValue::Stream(Box::new(StreamValue {
        stream_dict: ObjectHandle::dictionary(vec![
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"sig".to_vec())),
            (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            ),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
            (b"Link".to_vec(), kept),
            (b"Removed".to_vec(), removed),
            (b"Text".to_vec(), ObjectHandle::string(b"plain".to_vec())),
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        ]),
        stream_data: Some(Rc::new(b"abc".to_vec())),
        stream_provider: None,
        filter_on_write: true,
        stream_token_filters: Default::default(),
        content_normalization_applied: false,
        stream_length: 3,
    })));
    let mut removed_refs = BTreeSet::new();
    removed_refs.insert(ObjectRef::new(31, 0));
    let map = |object_ref: ObjectRef| {
        Ok(ObjectRef::new(
            object_ref.number + 100,
            object_ref.generation,
        ))
    };

    let mut compact = Vec::new();
    stream
        .unparse_stream_body_with_ref_map_and_removed_with_string_writer(
            &mut compact,
            false,
            &map,
            &removed_refs,
            &mut compact_string_hook,
        )
        .unwrap();
    let compact_text = String::from_utf8_lossy(&compact);
    assert!(compact_text.contains("/Link 130 0 R"));
    assert!(compact_text.contains("/Length 3"));
    assert!(compact_text.contains("<hook:plain>"));
    assert!(!compact_text.contains("/Removed"));

    let mut refiltered = Vec::new();
    stream
        .unparse_stream_body_with_ref_map_and_removed_with_string_writer(
            &mut refiltered,
            true,
            &map,
            &removed_refs,
            &mut compact_string_hook,
        )
        .unwrap();
    let refiltered_text = String::from_utf8_lossy(&refiltered);
    assert!(!refiltered_text.contains("/Filter /ASCIIHexDecode"));
    assert!(!refiltered_text.contains("/DecodeParms"));
    assert!(refiltered_text.contains("/Filter /FlateDecode"));

    let mut qdf_source_length = Vec::new();
    stream
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
            &mut qdf_source_length,
            2,
            &map,
            &removed_refs,
            None,
        )
        .unwrap();
    let qdf_source_text = String::from_utf8_lossy(&qdf_source_length);
    assert!(qdf_source_text.contains("/Length 3"));
    assert!(qdf_source_text.contains("/Link 130 0 R"));

    let mut qdf_synthetic_length = Vec::new();
    stream
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
            &mut qdf_synthetic_length,
            2,
            &map,
            &removed_refs,
            Some(ObjectRef::new(77, 0)),
        )
        .unwrap();
    assert!(String::from_utf8_lossy(&qdf_synthetic_length).contains("/Length 77 0 R"));

    let mut qdf_encrypted_source_length = Vec::new();
    stream
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
            &mut qdf_encrypted_source_length,
            2,
            &map,
            &removed_refs,
            None,
            &mut qdf_string_hook,
        )
        .unwrap();
    let qdf_encrypted_source_text = String::from_utf8_lossy(&qdf_encrypted_source_length);
    assert!(qdf_encrypted_source_text.contains("{hook:plain}"));
    assert!(qdf_encrypted_source_text.contains("/Length 3"));

    let mut qdf_encrypted_synthetic_length = Vec::new();
    stream
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
            &mut qdf_encrypted_synthetic_length,
            2,
            &map,
            &removed_refs,
            Some(ObjectRef::new(78, 0)),
            &mut qdf_string_hook,
        )
        .unwrap();
    assert!(String::from_utf8_lossy(&qdf_encrypted_synthetic_length).contains("/Length 78 0 R"));

    let scalar = ObjectHandle::integer(5);
    let mut scalar_qdf = Vec::new();
    scalar
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
            &mut scalar_qdf,
            0,
            &map,
            &removed_refs,
            None,
        )
        .unwrap();
    assert_eq!(scalar_qdf, b"<<\n>>");
    let mut scalar_qdf_encrypted = Vec::new();
    scalar
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
            &mut scalar_qdf_encrypted,
            0,
            &map,
            &removed_refs,
            None,
            &mut qdf_string_hook,
        )
        .unwrap();
    assert_eq!(scalar_qdf_encrypted, b"<<\n  /Length null\n>>");
    let mut scalar_compact_encrypted = Vec::new();
    scalar
        .unparse_stream_body_with_ref_map_and_removed_with_string_writer(
            &mut scalar_compact_encrypted,
            false,
            &map,
            &removed_refs,
            &mut compact_string_hook,
        )
        .unwrap();
    assert_eq!(scalar_compact_encrypted, b"<< >>");

    let no_length = ObjectHandle::dictionary(vec![(
        b"Text".to_vec(),
        ObjectHandle::string(b"no-length".to_vec()),
    )]);
    let mut no_length_qdf = Vec::new();
    no_length
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
            &mut no_length_qdf,
            0,
            &map,
            &removed_refs,
            None,
            &mut qdf_string_hook,
        )
        .unwrap();
    assert!(String::from_utf8_lossy(&no_length_qdf).contains("/Length null"));

    let reserved = ObjectHandle::new_reserved_direct();
    let mut reserved_out = Vec::new();
    assert!(reserved
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
            &mut reserved_out,
            0,
            &map,
            &removed_refs,
            None,
        )
        .is_err());
    assert!(reserved
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
            &mut reserved_out,
            0,
            &map,
            &removed_refs,
            None,
            &mut qdf_string_hook,
        )
        .is_err());
    assert!(reserved
        .unparse_stream_body_with_ref_map_and_removed_with_string_writer(
            &mut reserved_out,
            false,
            &map,
            &removed_refs,
            &mut compact_string_hook,
        )
        .is_err());

    let stream_with_non_dictionary_dict =
        ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(Vec::new()));
    let mut non_dictionary_dict_qdf = Vec::new();
    stream_with_non_dictionary_dict
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
            &mut non_dictionary_dict_qdf,
            0,
            &map,
            &removed_refs,
            None,
        )
        .unwrap();
    assert_eq!(non_dictionary_dict_qdf, b"<<\n>>");
    let mut non_dictionary_dict_qdf_string = Vec::new();
    stream_with_non_dictionary_dict
        .unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
            &mut non_dictionary_dict_qdf_string,
            0,
            &map,
            &removed_refs,
            None,
            &mut qdf_string_hook,
        )
        .unwrap();
    assert_eq!(non_dictionary_dict_qdf_string, b"<<\n  /Length null\n>>");
    let mut non_dictionary_dict_compact_string = Vec::new();
    stream_with_non_dictionary_dict
        .unparse_stream_body_with_ref_map_and_removed_with_string_writer(
            &mut non_dictionary_dict_compact_string,
            false,
            &map,
            &removed_refs,
            &mut compact_string_hook,
        )
        .unwrap();
    assert_eq!(non_dictionary_dict_compact_string, b"<< >>");
}

#[test]
fn unparse_stream_body_suppresses_a_null_valued_key() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Length".to_vec(), ObjectHandle::integer(3)),
        (b"Metadata".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< /Length 3 >>");
}

#[test]
fn unparse_stream_body_uses_the_dictionary_of_a_direct_stream_value() {
    // Mirrors unparse_object_inlines_only_the_dictionary_of_a_direct_stream_value
    // above: a *direct* Stream ObjectValue has no qpdf counterpart (a
    // real QPDFObjectHandle's resolved value is never itself a stream
    // outside an indirect object), but `unparse_stream_body` must still
    // use its `stream_dict`'s entries rather than falling into the
    // non-dictionary-self `<< >>` degrade below -- keeping the promise
    // those two `unparse_object`/`unparse_object_qdf` tests made on this
    // primitive's behalf (`unparse_stream_body` is separately responsible
    // for this).
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let handle = ObjectHandle::from_value(ObjectValue::Stream(Box::new(StreamValue {
        stream_dict: dict,
        stream_data: Some(Rc::new(b"ab".to_vec())),
        stream_provider: None,
        filter_on_write: true,
        stream_token_filters: Default::default(),
        content_normalization_applied: false,
        stream_length: 0,
    })));
    let mut out = Vec::new();
    handle.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< /Length 2 >>");
}

#[test]
fn unparse_stream_body_on_an_indirect_handle_resolving_to_a_stream_uses_the_dictionary() {
    // Mirrors unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary
    // above: a real, reachable qpdf shape -- an indirect object whose
    // resolved value is a stream (e.g. a production reader's own
    // resolution of a stream object). The mock-resolver harness resolves
    // `self` to `Stream { stream_dict, .. }` the same way that reader
    // would; `stream_dict`'s own entries must still surface here rather
    // than the non-dictionary-self `<< >>` degrade.
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let (indirect, _resolver) =
        resolver_bearing_handle(ObjectValue::Stream(Box::new(StreamValue {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_token_filters: Default::default(),
            content_normalization_applied: false,
            stream_length: 0,
        })));
    let mut out = Vec::new();
    indirect.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< /Length 2 >>");
}

#[test]
fn unparse_stream_body_resolves_an_unresolved_indirect_stream_dict() {
    // `stream_dict` is itself an `ObjectHandle` that may not yet be
    // resolved (e.g. a production reader's lazily-resolved stream
    // dictionary), not just an already-direct one as the two tests
    // above build. `self` stays a *direct* Stream value here so the
    // only variable under test is `stream_dict`'s own resolution state:
    // without the `stream_dict.try_dereference()?` call this fix added,
    // `with_value` on a not-yet-resolved indirect handle returns `None`
    // and this would degrade to `<< >>` instead of using the resolved
    // dictionary's entries.
    let (inner, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
        [(b"Length".to_vec(), ObjectHandle::integer(2))]
            .into_iter()
            .collect(),
    ));
    let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
    let mut out = Vec::new();
    handle.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< /Length 2 >>");
}

#[test]
fn unparse_stream_body_propagates_a_dropped_document_error_from_stream_dict() {
    // Mirrors unparse_stream_body_propagates_a_dropped_document_error
    // below, but for the new Stream-handling path added by this fix:
    // the dropped document lives behind `stream_dict`, not `self`
    // directly (`self` is a *direct* Stream value; only `stream_dict`
    // is the as-yet-unresolved indirect handle whose resolver is
    // dropped). The new `stream_dict.try_dereference()?` call must
    // surface this error too, not silently degrade to an empty `<< >>`
    // the way an unresolved `with_value` read alone would.
    let (inner, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
    let mut out = Vec::new();
    assert!(handle.unparse_stream_body(&mut out, false).is_err());
}

#[test]
fn unparse_stream_body_writes_empty_dict_when_stream_dict_is_not_a_dictionary() {
    // `stream_dict` is itself typed as an `ObjectHandle`, so nothing at
    // the type level prevents it from resolving to something other than
    // a `Dictionary` -- mirroring the same typed-input assumption
    // `self` itself is held to by
    // unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self
    // below. Exercises the new nested `_ => Vec::new()` arm for
    // `stream_dict`'s own resolved value.
    let handle = ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()));
    let mut out = Vec::new();
    handle.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< >>");
}

#[test]
fn unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self() {
    // Pins the doc comment's typed-input-assumption claim: a
    // non-dictionary `self` (mirroring `write_pdf_stream`'s own
    // assumption that it is only ever called on a stream's dictionary)
    // writes an empty `<< >>` rather than panicking or erroring.
    let mut out = Vec::new();
    ObjectHandle::integer(5)
        .unparse_stream_body(&mut out, false)
        .unwrap();
    assert_eq!(out, b"<< >>");
}

#[test]
fn unparse_stream_body_propagates_a_dropped_document_error() {
    // Mirrors unparse_object_propagates_a_dropped_document_error: an
    // as-yet-unresolved indirect handle whose document has been dropped
    // must surface as an error here too, not silently degrade to an
    // empty `<< >>` the way an unresolved `with_value` read alone would.
    let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let mut out = Vec::new();
    assert!(indirect.unparse_stream_body(&mut out, false).is_err());
}

// QDF-mode sibling suite of the `unparse_stream_body_*` tests above,
// for `unparse_stream_body_qdf`. Every hardcoded expected byte string
// below was cross-checked against a live call to
// `Dictionary::write_pdf_stream_qdf` (`object.rs`) with an equivalent
// dictionary before being pinned here, not hand-derived from reading
// the algorithm alone -- see this primitive's own doc for the full
// qpdf-correspondence and the deliberate absence of a `refiltered`
// parameter.

#[test]
fn unparse_stream_body_qdf_writes_length_last_preserved() {
    // No `refiltered` dimension exists for the QDF shape (see
    // `unparse_stream_body_qdf`'s own doc for why), so unlike its
    // compact sibling this has only one shape to pin: every other key
    // stays at its natural alphabetical position, and `/Length` is
    // pulled out and written last, immediately before the closing
    // `>>`. Deliberately includes `/Width`, which sorts *after*
    // `/Length` alphabetically (`DecodeParms` < `Filter` < `Length` <
    // `Width`): with only `{Filter, DecodeParms, Length}` (no key past
    // `Length`), `/Length`'s natural BTreeMap position already happens
    // to be last, so a broken implementation that forgot the pull-out
    // entirely would still pass -- `/Width` makes the two shapes
    // actually diverge (mutation-tested: deleting the pull-out
    // `key.as_slice() == b"Length"` branch does not fail this suite
    // without `/Width` present). Cross-checked against
    // `Dictionary::write_pdf_stream_qdf` with the equivalent dict:
    // `<<\n  /DecodeParms <<\n  >>\n  /Filter /FlateDecode\n  /Width 100\n  /Length 3\n>>`.
    let dict = ObjectHandle::dictionary(vec![
        (
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        ),
        (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
        (b"Width".to_vec(), ObjectHandle::integer(100)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(
        out,
        b"<<\n  /DecodeParms <<\n  >>\n  /Filter /FlateDecode\n  /Width 100\n  /Length 3\n>>"
    );
}

#[test]
fn unparse_stream_body_qdf_writes_sig_contents_as_a_hex_string() {
    // QDF sibling of `unparse_stream_body_writes_sig_contents_as_a_hex_string`
    // above -- `unparse_stream_dict_entries_qdf` applies the same
    // Sig+ByteRange special case its compact sibling does (see that
    // function's own doc). `/Length` is pulled out and written last, at
    // `indent + 2`, same as the non-Sig QDF stream shape above.
    let dict = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(2)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(
        out,
        b"<<\n  /ByteRange [\n  ]\n  /Contents <6869>\n  /Type /Sig\n  /Length 2\n>>"
    );
}

#[test]
fn unparse_stream_body_qdf_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
    // QDF-stream sibling of
    // `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
    // above -- `unparse_stream_dict_entries_qdf` needed the identical
    // fix at its own call site. See that test's own doc for the full
    // eager-vs-lazy rationale and why a plain success-vs-error
    // assertion cannot discriminate this bug.
    let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
    drop(aaa_resolver);
    let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
    drop(type_resolver);
    let dict = ObjectHandle::dictionary(vec![
        (b"AAA".to_vec(), aaa),
        (b"Type".to_vec(), sig_type),
        (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        (b"Length".to_vec(), ObjectHandle::integer(0)),
    ]);
    let mut out = Vec::new();
    let error = dict.unparse_stream_body_qdf(&mut out, 0).unwrap_err();
    assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
}

#[test]
fn unparse_stream_body_qdf_suppresses_a_null_valued_key() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Length".to_vec(), ObjectHandle::integer(3)),
        (b"Metadata".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
    // Cross-checked against a direct `Dictionary::write_pdf_stream_qdf`
    // call on the equivalent dict *without* the null key removed: that
    // call writes `/Metadata null` verbatim (`write_pdf_stream_qdf`
    // itself applies no null suppression -- that is layered on top by
    // `visible_dict_entries`, exactly like the compact
    // `unparse_stream_dict_entries` does), confirming the suppression
    // observed here is this primitive's own added behavior, not
    // something already built into the legacy function it delegates
    // scalar/container formatting to.
    assert_eq!(out, b"<<\n  /Length 3\n>>");
}

#[test]
fn unparse_stream_body_qdf_writes_an_empty_dict_when_every_entry_is_suppressed() {
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::null())]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
    // No surviving entries and no `/Length`: matches
    // `write_pdf_stream_qdf`'s own empty-input shape `<<\n>>` (no
    // interior spaces at indent 0 -- `push_spaces(indent)` only adds
    // spaces when `indent > 0`).
    assert_eq!(out, b"<<\n>>");
}

#[test]
fn unparse_stream_body_qdf_respects_a_nonzero_indent() {
    // Mirrors unparse_object_qdf_respects_a_nonzero_starting_indent:
    // every other QDF test in this suite pins `indent = 0`, which would
    // still pass with a stray hardcoded `0` inside the function body.
    // Both values here are scalars, so this alone does not prove the
    // `indent + 2` passed to `unparse_child_qdf` for each entry actually
    // carries the caller's `indent` -- a scalar ignores that argument
    // entirely (see `unparse_stream_body_qdf_respects_a_nonzero_indent_for_a_nested_container_value`
    // below for the test that does). Cross-checked against
    // `Dictionary::write_pdf_stream_qdf(&mut out, 4)` on the equivalent
    // dict: `<<\n      /A 1\n      /Length 2\n    >>`.
    let dict = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"Length".to_vec(), ObjectHandle::integer(2)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 4).unwrap();
    assert_eq!(out, b"<<\n      /A 1\n      /Length 2\n    >>");
}

#[test]
fn unparse_stream_body_qdf_respects_a_nonzero_indent_for_a_nested_container_value() {
    // The test above only proves `indent` reaches the closing `>>`'s
    // own `push_spaces` and the entry lines' *leading* `push_spaces(out,
    // indent + 2)` -- both scalar values in it ignore the `indent + 2`
    // this primitive also threads through `unparse_child_qdf(value,
    // indent + 2, out)` for each entry (a scalar's own QDF form does
    // not depend on indent at all). A nested container value does: its
    // own children land at `(indent + 2) + 2`, and a hardcoded `2` in
    // place of that `indent + 2` argument would still pass every other
    // test in this suite (mutation-tested: it survives without this
    // test). Cross-checked against `Dictionary::write_pdf_stream_qdf`
    // with the equivalent dict at indent 4:
    // `<<\n      /DecodeParms <<\n        /Predictor 12\n      >>\n      /Length 3\n    >>`.
    let dict = ObjectHandle::dictionary(vec![
        (
            b"DecodeParms".to_vec(),
            ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), ObjectHandle::integer(12))]),
        ),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 4).unwrap();
    assert_eq!(
        out,
        b"<<\n      /DecodeParms <<\n        /Predictor 12\n      >>\n      /Length 3\n    >>"
    );
}

#[test]
fn unparse_stream_body_qdf_writes_a_retained_indirect_entry_as_reference_form() {
    // QDF-mode sibling of unparse_stream_body_writes_length_last_preserved
    // that exercises unparse_child_qdf's indirect arm instead of a direct
    // scalar -- unreached by the tests above, whose every value is
    // direct.
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let dict = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), indirect),
        (b"Length".to_vec(), ObjectHandle::integer(2)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /A 20 0 R\n  /Length 2\n>>");
}

#[test]
fn unparse_stream_body_qdf_uses_the_dictionary_of_a_direct_stream_value() {
    // Mirrors unparse_stream_body_uses_the_dictionary_of_a_direct_stream_value:
    // a *direct* Stream ObjectValue has no qpdf counterpart, but this
    // primitive must still use its `stream_dict`'s entries rather than
    // falling into the non-dictionary-self `<< >>` degrade below.
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let handle = ObjectHandle::from_value(ObjectValue::Stream(Box::new(StreamValue {
        stream_dict: dict,
        stream_data: Some(Rc::new(b"ab".to_vec())),
        stream_provider: None,
        filter_on_write: true,
        stream_token_filters: Default::default(),
        content_normalization_applied: false,
        stream_length: 0,
    })));
    let mut out = Vec::new();
    handle.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /Length 2\n>>");
}

#[test]
fn unparse_stream_body_qdf_on_an_indirect_handle_resolving_to_a_stream_uses_the_dictionary() {
    // Mirrors unparse_stream_body_on_an_indirect_handle_resolving_to_a_stream_uses_the_dictionary:
    // a real, reachable qpdf shape -- an indirect object whose resolved
    // value is a stream (e.g. a production reader's own resolution of a
    // stream object).
    let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
    let (indirect, _resolver) =
        resolver_bearing_handle(ObjectValue::Stream(Box::new(StreamValue {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_token_filters: Default::default(),
            content_normalization_applied: false,
            stream_length: 0,
        })));
    let mut out = Vec::new();
    indirect.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /Length 2\n>>");
}

#[test]
fn unparse_stream_body_qdf_resolves_an_unresolved_indirect_stream_dict() {
    // Mirrors unparse_stream_body_resolves_an_unresolved_indirect_stream_dict:
    // `stream_dict` is itself an `ObjectHandle` that may not yet be
    // resolved. `self` stays a *direct* Stream value here so the only
    // variable under test is `stream_dict`'s own resolution state:
    // without the `stream_dict.try_dereference()?` call this primitive
    // makes, `with_value` on a not-yet-resolved indirect handle returns
    // `None` and this would degrade to `<< >>` instead of using the
    // resolved dictionary's entries.
    let (inner, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
        [(b"Length".to_vec(), ObjectHandle::integer(2))]
            .into_iter()
            .collect(),
    ));
    let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
    let mut out = Vec::new();
    handle.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /Length 2\n>>");
}

#[test]
fn unparse_stream_body_qdf_propagates_a_dropped_document_error_from_stream_dict() {
    // Mirrors unparse_stream_body_propagates_a_dropped_document_error_from_stream_dict:
    // the dropped document lives behind `stream_dict`, not `self`
    // directly (`self` is a *direct* Stream value; only `stream_dict`
    // is the as-yet-unresolved indirect handle whose resolver is
    // dropped).
    let (inner, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
    let mut out = Vec::new();
    assert!(handle.unparse_stream_body_qdf(&mut out, 0).is_err());
}

#[test]
fn unparse_stream_body_qdf_writes_empty_dict_when_stream_dict_is_not_a_dictionary() {
    // Mirrors unparse_stream_body_writes_empty_dict_when_stream_dict_is_not_a_dictionary:
    // `stream_dict` is itself typed as an `ObjectHandle`, so nothing at
    // the type level prevents it from resolving to something other than
    // a `Dictionary`.
    let handle = ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()));
    let mut out = Vec::new();
    handle.unparse_stream_body_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n>>");
}

#[test]
fn unparse_stream_body_qdf_writes_empty_dict_for_a_non_dictionary_self() {
    // Mirrors unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self:
    // pins the doc comment's typed-input-assumption claim.
    let mut out = Vec::new();
    ObjectHandle::integer(5)
        .unparse_stream_body_qdf(&mut out, 0)
        .unwrap();
    assert_eq!(out, b"<<\n>>");
}

#[test]
fn unparse_stream_body_qdf_propagates_a_dropped_document_error() {
    // Mirrors unparse_stream_body_propagates_a_dropped_document_error:
    // an as-yet-unresolved indirect handle whose document has been
    // dropped must surface as an error here too, not silently degrade
    // to an empty `<< >>` the way an unresolved `with_value` read alone
    // would.
    let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let mut out = Vec::new();
    assert!(indirect.unparse_stream_body_qdf(&mut out, 0).is_err());
}

#[test]
fn unparse_trailer_classic_forces_id_and_encrypt_last() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (b"Root".to_vec(), ObjectHandle::integer(1)), // stand-in reference shape
        (b"Encrypt".to_vec(), ObjectHandle::integer(9)),
        (
            b"ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(vec![0u8; 16]),
                ObjectHandle::string(vec![1u8; 16]),
            ]),
        ),
    ]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("trailer << "));
    assert!(text.ends_with(">>"));
    // /Root and /Size appear before /ID, /ID appears before /Encrypt,
    // regardless of the dict's own (alphabetical) key order.
    let root_pos = text.find("/Root").unwrap();
    let id_pos = text.find("/ID").unwrap();
    let encrypt_pos = text.find("/Encrypt").unwrap();
    assert!(root_pos < id_pos);
    assert!(id_pos < encrypt_pos);
}

#[test]
fn unparse_trailer_xref_stream_does_not_write_its_own_open_brace() {
    // The caller (a future `writeXRefStream`-shaped consumer) has
    // already opened `<<` and hand-emitted the xref-specific keys
    // before calling this method with `xref_stream = true` -- see
    // this method's own doc for why those keys are never part of
    // `entries` here to begin with.
    let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, true, None).unwrap();
    assert!(!String::from_utf8_lossy(&out).contains("<<"));
    assert!(String::from_utf8_lossy(&out).ends_with(">>"));
}

#[test]
fn unparse_trailer_without_id_or_encrypt_omits_both() {
    let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("/ID"));
    assert!(!text.contains("/Encrypt"));
}

#[test]
fn unparse_trailer_does_not_suppress_a_null_valued_key() {
    // writeTrailer's own key loop has no isNull check anywhere in it
    // (QPDFWriter.cc:1174-1192) -- unlike unparseObject's dictionary
    // branch. `/Prev` is chosen only as a convenient never-suppressed
    // example key for this unit-level test; in production `/Prev`
    // itself would already have been stripped by the caller before
    // this primitive ever sees the dict (see this method's own doc on
    // the `getTrimmedTrailer`-equivalent split), so this does not
    // claim `/Prev` null survives end to end -- only that *this*
    // primitive's key loop applies no suppression to whatever keys
    // the caller does hand it.
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (b"Prev".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("/Prev null"));
}

#[test]
fn unparse_trailer_id_writer_substitutes_the_id_value() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (
            b"ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(vec![0u8; 16]),
                ObjectHandle::string(vec![0u8; 16]),
            ]),
        ),
    ]);
    let mut out = Vec::new();
    let mut id_writer = |out: &mut Vec<u8>| out.extend_from_slice(b"<computed>");
    dict.write_trailer(&mut out, false, Some(&mut id_writer))
        .unwrap();
    assert!(String::from_utf8_lossy(&out).contains("/ID <computed>"));
}

#[test]
fn unparse_trailer_id_writer_none_uses_qpdf_compact_hex_shape() {
    // Pins the exact byte shape unparse_trailer_id_writer_substitutes_the_id_value's
    // dict (all-zero bytes) can't distinguish from a generic array
    // serialization -- mirrors write_id_style_value_emits_compact_hex_pair
    // (object.rs) byte-for-byte.
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (
            b"ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(vec![0xabu8, 0xcdu8, 0xefu8]),
                ObjectHandle::string(vec![0x12u8, 0x34u8, 0x56u8]),
            ]),
        ),
    ]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    assert_eq!(out, b"trailer << /Size 9 /ID [<abcdef><123456>] >>");
}

#[test]
fn unparse_trailer_id_writer_none_falls_back_for_unexpected_id_shape() {
    // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
    // (object.rs): wrong arity falls back to the generic array
    // serializer (spaces) rather than silently truncating to two
    // elements.
    let dict = ObjectHandle::dictionary(vec![(
        b"ID".to_vec(),
        ObjectHandle::array(vec![
            ObjectHandle::string(vec![0x00u8]),
            ObjectHandle::string(vec![0x11u8]),
            ObjectHandle::string(vec![0x8fu8]),
        ]),
    )]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    assert_eq!(out, b"trailer << /ID [ <00> <11> <8f> ] >>");
}

#[test]
fn unparse_trailer_id_writer_none_falls_back_for_non_string_element() {
    // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
    // (object.rs): right arity (2 elements) but a non-String element
    // type falls back to the generic array serializer rather than
    // treating the element as a string.
    let dict = ObjectHandle::dictionary(vec![(
        b"ID".to_vec(),
        ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::string(vec![0x8fu8]),
        ]),
    )]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    assert_eq!(out, b"trailer << /ID [ 1 <8f> ] >>");
}

#[test]
fn unparse_trailer_id_writer_none_falls_back_for_scalar_id() {
    // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
    // (object.rs): a non-array /ID value is delegated to unparse_child
    // verbatim rather than being routed through the compact-pair path.
    let dict = ObjectHandle::dictionary(vec![(b"ID".to_vec(), ObjectHandle::integer(7))]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    assert_eq!(out, b"trailer << /ID 7 >>");
}

#[test]
fn unparse_trailer_writes_an_indirect_id_as_reference_form_not_inlined() {
    // `object_ref().is_some()` must be checked before shape inspection
    // in write_id_style_value_handle: an indirect /ID value (not a
    // shape real qpdf itself ever produces, but nothing at the type
    // level rules it out) writes as its own "N G R" form, the same
    // reference-vs-recurse split unparse_child applies everywhere else
    // in this primitive family -- never inlined as compact hex even
    // though it would resolve to a matching Array([String, String])
    // shape.
    let (indirect_id, _resolver) = resolver_bearing_handle(ObjectValue::Array(vec![
        ObjectHandle::string(vec![0u8; 2]),
        ObjectHandle::string(vec![1u8; 2]),
    ]));
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (b"ID".to_vec(), indirect_id),
    ]);
    let mut out = Vec::new();
    dict.write_trailer(&mut out, false, None).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("/ID 20 0 R"));
    assert!(!text.contains("[<"));
}

#[test]
fn unparse_trailer_writes_empty_shell_for_a_non_dictionary_self() {
    // Mirrors unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self:
    // a non-dictionary `self` degrades to an empty trailer shell
    // rather than panicking or erroring, matching write_pdf_trailer's
    // own typed-input assumption.
    let mut out = Vec::new();
    ObjectHandle::integer(5)
        .write_trailer(&mut out, false, None)
        .unwrap();
    assert_eq!(out, b"trailer << >>");
}

#[test]
fn unparse_trailer_resolves_an_unresolved_indirect_trailer_dict() {
    // Without the `self.try_dereference()?` call this method makes
    // before `with_value`, `with_value` on a not-yet-resolved
    // indirect handle returns `None` and this would degrade to an
    // empty `trailer << >>` shell instead of using the resolved
    // dictionary's entries -- mirrors
    // unparse_stream_body_resolves_an_unresolved_indirect_stream_dict's
    // same proof for the same fix pattern.
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
        [(b"Size".to_vec(), ObjectHandle::integer(9))]
            .into_iter()
            .collect(),
    ));
    let mut out = Vec::new();
    indirect.write_trailer(&mut out, false, None).unwrap();
    assert_eq!(out, b"trailer << /Size 9 >>");
}

#[test]
fn unparse_trailer_propagates_a_dropped_document_error() {
    // Mirrors unparse_stream_body_propagates_a_dropped_document_error:
    // an as-yet-unresolved indirect handle whose document has been
    // dropped must surface as an error here too, not silently degrade
    // to an empty `trailer << >>` shell the way an unresolved
    // `with_value` read alone would.
    let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let mut out = Vec::new();
    assert!(indirect.write_trailer(&mut out, false, None).is_err());
}
