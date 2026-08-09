//! Public-API parity tests for `Pdf::resolve_object_handle` (the dual-write
//! `ObjectHandle` resolution bridge) and, from the materialization bridge
//! task onward, for `Pdf::resolve`/`Pdf::resolve_borrowed`/`Pdf::set_object`/
//! `Pdf::delete_object` themselves once they are thin views over the same
//! `ObjectHandle` graph: proves the public API reaches the same observable
//! outcomes it always has — missing/dangling references resolve to null,
//! compressed (ObjStm) members and cyclic indirect `/Length` streams resolve
//! without erroring or hanging, repeated `get_object_handle` calls observe
//! the same canonical, already-resolved state, and `resolve`/`set_object`
//! round trip structurally.

use flpdf::{Object, ObjectHandle, ObjectRef, Pdf};
use std::fs::File;
use std::io::BufReader;
use std::rc::Rc;

fn minimal_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf")
}

/// A classic (non-xref-stream) PDF built from `bodies`, one indirect object
/// per body, numbered 1..=bodies.len() in order.
fn classic_pdf_with_bodies(bodies: &[&[u8]], root: ObjectRef) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for body in bodies {
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(body);
    }
    let size = bodies.len() + 1;
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root {} {} R >>\nstartxref\n{xref_start}\n%%EOF\n",
            root.number, root.generation
        )
        .as_bytes(),
    );
    pdf
}

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let bytes_u24 = value.to_be_bytes();
    bytes.extend_from_slice(&bytes_u24[1..]);
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}

/// A PDF with an xref *stream* whose object 2 is a compressed (ObjStm)
/// member of object 3, holding the plain value `Integer(42)`.
fn compressed_entry_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(&catalog);

    let obj3_offset = bytes.len();
    let obj_stream_body = b"2 0 42";
    let obj3 = format!(
        "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        obj_stream_body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&obj3);
    bytes.extend_from_slice(obj_stream_body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

/// Resolving an indirect handle yields its parsed value: the document's
/// root/Catalog dictionary resolves, and its indirect `/Pages` entry lifts to
/// an *unresolved* indirect handle carrying the correct object reference
/// (identity-preserving lift, not an inlined copy).
#[test]
fn resolve_object_handle_resolves_the_catalog_dictionary() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let root_ref = pdf.root_ref().expect("minimal fixture has a root");

    let handle = pdf.get_object_handle(root_ref);
    pdf.resolve_object_handle(&handle).expect("resolve catalog");

    let dict = handle
        .as_dictionary()
        .expect("catalog resolves to a dictionary");
    let pages_handle = dict.get(b"Pages".as_slice()).expect("Pages entry present");
    assert!(
        pages_handle.is_indirect(),
        "an indirect /Pages value must lift to an indirect handle, not an inlined copy"
    );
    assert_eq!(pages_handle.object_ref(), Some(ObjectRef::new(2, 0)));
}

/// A dangling indirect handle (a ref absent from the fixture's xref table)
/// resolves to null without erroring, and its parsed offset stays the
/// no-offset sentinel (this task does not populate parsed offsets).
#[test]
fn resolve_object_handle_resolves_a_dangling_reference_to_null() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let dangling_ref = ObjectRef::new(999, 0);

    let handle = pdf.get_object_handle(dangling_ref);
    pdf.resolve_object_handle(&handle)
        .expect("a dangling reference must not error");

    assert!(handle.is_null());
    assert_eq!(handle.get_parsed_offset(), -1);
}

/// A stream whose `/Length` is an indirect reference forming a cycle
/// (object 1's `/Length` points at object 2, object 2's `/Length` points
/// back at object 1 — the same mutual-cycle fixture the legacy engine's own
/// `qpdf_reader_bounds_unusable_indirect_length_recovery` test exercises)
/// resolves without hanging or erroring through the new bridge. The
/// untouched `Reserved`-state guard is what breaks the cycle; this proves it
/// still works when reached via `resolve_object_handle` instead of
/// `resolve`/`resolve_borrowed`.
///
/// Unlike Task 6 (where an indirect stream lifted to `ObjectValue::Null`,
/// since `Pdf::lift` does not convert `Object::Stream`), this task's native
/// parse for the plain uncompressed-file-object case builds a real
/// `ObjectValue::Stream` — so the object under test here resolves to the
/// actual (cycle-recovered) stream payload, not null.
#[test]
fn resolve_object_handle_survives_a_cyclic_indirect_stream_length() {
    let bytes = classic_pdf_with_bodies(
        &[
            b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n",
            b"2 0 obj\n<< /Length 1 0 R >>\nstream\nxyz\nendstream\nendobj\n",
        ],
        ObjectRef::new(1, 0),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open cyclic-length fixture");
    let object_ref = ObjectRef::new(1, 0);

    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&handle)
        .expect("a cyclic indirect /Length must not error");

    // The untouched legacy engine's cycle-recovered payload ("abc") and the
    // new bridge's own materialized stream data must match.
    let legacy = pdf.resolve(object_ref).expect("legacy resolve");
    assert_eq!(legacy.as_stream().expect("stream").data, b"abc");
    assert_eq!(handle.as_stream_data(), Some(Rc::new(b"abc".to_vec())));

    // The stream's own dictionary is a distinct, natively-parsed handle
    // (not folded into the stream value itself), and its /Length entry
    // still preserves the indirect reference's identity rather than being
    // inlined as the recovered integer.
    let dict = handle
        .as_stream_dict()
        .expect("stream value carries its own dictionary handle")
        .as_dictionary()
        .expect("stream dictionary handle resolves to a dictionary");
    let length_handle = dict.get(b"Length".as_slice()).expect("Length entry");
    assert!(length_handle.is_indirect());
    assert_eq!(length_handle.object_ref(), Some(ObjectRef::new(2, 0)));
}

/// A compressed (ObjStm) member resolves correctly through
/// `resolve_object_handle`.
#[test]
fn resolve_object_handle_resolves_a_compressed_object_stream_member() {
    let mut pdf = Pdf::open(std::io::Cursor::new(compressed_entry_pdf())).unwrap();
    let object_ref = ObjectRef::new(2, 0);

    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&handle)
        .expect("resolve compressed member");

    assert_eq!(handle.as_integer(), Some(42));
}

/// Repeated `get_object_handle` calls for the same already-resolved
/// `ObjectRef` return the same canonical handle. The identity check itself
/// (`ObjectHandle::ptr_eq`) is crate-internal and not visible from this
/// integration test, so this proves it the public-API-observable way
/// instead: the *second* call's handle must already carry the value the
/// *first* call resolved, without `resolve_object_handle` being called on it
/// again — a freshly distinct handle would still read as unresolved.
#[test]
fn get_object_handle_repeated_calls_share_already_resolved_state() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let root_ref = pdf.root_ref().expect("root");

    let first = pdf.get_object_handle(root_ref);
    pdf.resolve_object_handle(&first).expect("resolve catalog");

    let second = pdf.get_object_handle(root_ref);
    assert!(
        second.as_dictionary().is_some(),
        "second handle must already carry the value the first handle resolved"
    );
}

/// Both a literal PDF `null` object (present in the xref table, genuinely
/// parsed) and a dangling reference (absent from the xref table) present as
/// `is_null() == true` through the public API. This test only fixes that
/// black-box observation in place; it does NOT prove the two take different
/// internal routes (`IndirectState::Resolved(ObjectValue::Null)` vs.
/// `IndirectState::Missing`) — the real tripwire for that internal
/// distinction is `reader.rs`'s white-box
/// `resolve_object_handle_literal_null_and_dangling_ref_take_different_cache_paths`,
/// which asserts directly on `Pdf::cache`.
#[test]
fn resolve_object_handle_distinguishes_a_literal_null_from_a_dangling_reference() {
    let bytes = classic_pdf_with_bodies(&[b"1 0 obj\nnull\nendobj\n"], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open literal-null fixture");

    let literal_null_ref = ObjectRef::new(1, 0);
    let literal_null_handle = pdf.get_object_handle(literal_null_ref);
    pdf.resolve_object_handle(&literal_null_handle)
        .expect("resolve literal null");

    let dangling_ref = ObjectRef::new(999, 0);
    let dangling_handle = pdf.get_object_handle(dangling_ref);
    pdf.resolve_object_handle(&dangling_handle)
        .expect("resolve dangling ref");

    assert!(literal_null_handle.is_null());
    assert!(dangling_handle.is_null());
}

/// `resolve_object_handle` is a no-op for a direct handle: it already has a
/// value, and there is no reference to resolve.
#[test]
fn resolve_object_handle_is_a_no_op_for_a_direct_handle() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let direct = ObjectHandle::integer(7);

    pdf.resolve_object_handle(&direct)
        .expect("a direct handle is a no-op");

    assert_eq!(direct.as_integer(), Some(7));
}

/// Calling `resolve_object_handle` a second time on an already-resolved
/// indirect handle must not error or re-resolve; it stays a no-op.
#[test]
fn resolve_object_handle_is_idempotent_for_an_already_resolved_handle() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let root_ref = pdf.root_ref().expect("root");
    let handle = pdf.get_object_handle(root_ref);

    pdf.resolve_object_handle(&handle).expect("first resolve");
    pdf.resolve_object_handle(&handle)
        .expect("second resolve is a no-op");

    assert!(handle.as_dictionary().is_some());
}

/// `Pdf::lift` recursively lifts array elements: an indirect reference
/// inside the array becomes an unresolved indirect handle for that
/// reference (identity-preserving), not an inlined copy of its value.
#[test]
fn resolve_object_handle_lifts_array_elements_recursively() {
    let bytes = classic_pdf_with_bodies(
        &[
            b"1 0 obj\n<< /Kids [2 0 R 3 0 R] /Count 2 >>\nendobj\n",
            b"2 0 obj\n<< /Type /Page >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page >>\nendobj\n",
        ],
        ObjectRef::new(1, 0),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open array fixture");
    let object_ref = ObjectRef::new(1, 0);

    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&handle)
        .expect("resolve dict-with-array");

    let dict = handle.as_dictionary().expect("dictionary");
    let kids_handle = dict.get(b"Kids".as_slice()).expect("Kids entry");
    let kids = kids_handle.as_array().expect("Kids is an array");
    assert_eq!(kids.len(), 2);
    assert!(kids[0].is_indirect());
    assert_eq!(kids[0].object_ref(), Some(ObjectRef::new(2, 0)));
    assert!(kids[1].is_indirect());
    assert_eq!(kids[1].object_ref(), Some(ObjectRef::new(3, 0)));
}

/// Every scalar `Object` variant `Pdf::lift` is responsible for must lift
/// without panicking or being silently dropped from the dictionary. This
/// crate does not yet expose `as_boolean`/`as_real`/`as_name`/`as_string`
/// accessors on `ObjectHandle` (a later task), so the boolean/real/name/
/// string entries can only be checked for presence here; `Integer` and
/// `RealLiteral` (which do have accessors already) are checked by value.
#[test]
fn resolve_object_handle_lifts_every_scalar_object_value_variant() {
    let bytes = classic_pdf_with_bodies(
        &[b"1 0 obj\n<< /B true /I 7 /R 1.5 /RL .5 /N /Foo /S (bar) >>\nendobj\n"],
        ObjectRef::new(1, 0),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open scalar fixture");
    let object_ref = ObjectRef::new(1, 0);

    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&handle)
        .expect("resolve scalar dict");

    let dict = handle.as_dictionary().expect("dictionary");
    assert_eq!(
        dict.get(b"I".as_slice()).and_then(ObjectHandle::as_integer),
        Some(7)
    );
    assert_eq!(
        dict.get(b"RL".as_slice())
            .and_then(ObjectHandle::as_real_literal),
        Some((0.5, b".5".to_vec()))
    );
    assert!(dict.contains_key(b"B".as_slice()));
    assert!(dict.contains_key(b"R".as_slice()));
    assert!(dict.contains_key(b"N".as_slice()));
    assert!(dict.contains_key(b"S".as_slice()));
}

// ---------------------------------------------------------------------
// Task 7: parsed offsets for the plain uncompressed-file-object case.
//
// `classic_pdf_with_bodies` always starts with the fixed 9-byte
// `%PDF-1.7\n` header, so a single-body fixture's own bytes start at file
// offset `PDF_HEADER_LEN`. Every expected offset below is computed from the
// fixture's own bytes (via `find_after`), never hardcoded, so a fixture
// edit cannot silently desynchronize the assertion from reality.
// ---------------------------------------------------------------------

const PDF_HEADER_LEN: usize = 9; // b"%PDF-1.7\n".len()

/// The offset of the first occurrence of `pattern` at or after `after`
/// within `haystack`.
fn find_after(haystack: &[u8], pattern: &[u8], after: usize) -> usize {
    haystack[after..]
        .windows(pattern.len())
        .position(|window| window == pattern)
        .expect("pattern not found")
        + after
}

/// The offset right after a body's own "`N G obj`" header line.
fn after_object_header(body: &[u8]) -> usize {
    body.iter().position(|&b| b == b'\n').expect("header line") + 1
}

#[test]
fn scalar_parsed_offset_includes_leading_whitespace_like_qpdf() {
    let body: &[u8] = b"1 0 obj\n   42\nendobj\n";
    let scalar_local = find_after(body, b"42", after_object_header(body));
    let object_header_end = find_after(body, b"obj", 0) + b"obj".len();
    let expected_offset = (PDF_HEADER_LEN + object_header_end) as i64;
    assert_ne!(
        expected_offset,
        (PDF_HEADER_LEN + scalar_local) as i64,
        "the fixture must separate qpdf's pre-tokenization offset from the scalar token"
    );

    let bytes = classic_pdf_with_bodies(&[body], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open scalar-offset fixture");
    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle).expect("resolve scalar");

    assert_eq!(handle.as_integer(), Some(42));
    assert_eq!(handle.get_parsed_offset(), expected_offset);
}

#[test]
fn array_parsed_offset_is_the_bracket_not_the_first_child() {
    let body: &[u8] = b"1 0 obj\n[  1 2 3]\nendobj\n";
    let after_header = after_object_header(body);
    let bracket_local = find_after(body, b"[", after_header);
    let first_child_local = find_after(body, b"1", bracket_local);
    let expected_array_offset = (PDF_HEADER_LEN + bracket_local) as i64;
    let expected_child_offset = (PDF_HEADER_LEN + first_child_local) as i64;
    assert_ne!(
        expected_array_offset, expected_child_offset,
        "the fixture must actually separate the bracket from the first child"
    );

    let bytes = classic_pdf_with_bodies(&[body], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open array-offset fixture");
    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle).expect("resolve array");

    assert_eq!(handle.get_parsed_offset(), expected_array_offset);
    let children = handle.as_array().expect("array");
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].as_integer(), Some(1));
    assert_eq!(children[0].get_parsed_offset(), expected_child_offset);
}

#[test]
fn dictionary_parsed_offset_is_the_double_angle_bracket() {
    let body: &[u8] = b"1 0 obj\n<<  /A 1>>\nendobj\n";
    let dict_open_local = find_after(body, b"<<", after_object_header(body));
    let expected_dict_offset = (PDF_HEADER_LEN + dict_open_local) as i64;

    let bytes = classic_pdf_with_bodies(&[body], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open dictionary-offset fixture");
    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle)
        .expect("resolve dictionary");

    assert_eq!(handle.get_parsed_offset(), expected_dict_offset);
    let dict = handle.as_dictionary().expect("dictionary");
    assert_eq!(
        dict.get(b"A".as_slice()).and_then(ObjectHandle::as_integer),
        Some(1)
    );
}

/// "the parser constructs QPDF_Null without assigning a description or
/// offset" (design, Fixed qpdf Facts) — even though `null` has a real token
/// position (nonzero here, well past the fixture's own header), the
/// handle's parsed offset must stay the sentinel.
#[test]
fn parsed_null_offset_is_always_the_sentinel() {
    let bytes = classic_pdf_with_bodies(&[b"1 0 obj\nnull\nendobj\n"], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open null-offset fixture");
    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle).expect("resolve null");

    assert!(handle.is_null());
    assert_eq!(handle.get_parsed_offset(), -1);
}

#[test]
fn stream_handle_and_its_dictionary_handle_have_distinct_offsets() {
    let body: &[u8] = b"1 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj\n";
    let after_header = after_object_header(body);
    let dict_open_local = find_after(body, b"<<", after_header);
    let data_start_local = find_after(body, b"Hello", dict_open_local);
    let expected_dict_offset = (PDF_HEADER_LEN + dict_open_local) as i64;
    let expected_stream_offset = (PDF_HEADER_LEN + data_start_local) as i64;
    assert_ne!(expected_dict_offset, expected_stream_offset);

    let bytes = classic_pdf_with_bodies(&[body], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream-offset fixture");
    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle).expect("resolve stream");

    assert_eq!(handle.as_stream_data(), Some(Rc::new(b"Hello".to_vec())));
    assert_eq!(handle.get_parsed_offset(), expected_stream_offset);

    let dict_handle = handle.as_stream_dict().expect("stream dictionary handle");
    assert_eq!(dict_handle.get_parsed_offset(), expected_dict_offset);
}

/// A parsed `N G R` points at the canonical indirect handle for that
/// `ObjectRef`, not a fresh value — `ObjectHandle::ptr_eq` is crate-internal
/// and not visible from this integration test (the same limitation
/// `get_object_handle_repeated_calls_share_already_resolved_state` above
/// documents), so identity is proven the public-API-observable way instead:
/// resolving *only* the dictionary's own child handle must also make a
/// handle obtained *before* parsing (via a wholly separate
/// `get_object_handle` call) observe the resolved value — a freshly
/// constructed, independently-identified handle for the same reference
/// would not.
#[test]
fn indirect_reference_child_is_the_canonical_handle_not_a_fresh_value() {
    let bytes = classic_pdf_with_bodies(
        &[
            b"1 0 obj\n<< /Kid 5 0 R >>\nendobj\n".as_slice(),
            b"2 0 obj\nnull\nendobj\n",
            b"3 0 obj\nnull\nendobj\n",
            b"4 0 obj\nnull\nendobj\n",
            b"5 0 obj\n99\nendobj\n",
        ],
        ObjectRef::new(1, 0),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open kid-ref fixture");

    let canonical = pdf.get_object_handle(ObjectRef::new(5, 0));
    assert!(canonical.as_integer().is_none(), "not yet resolved");

    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle).expect("resolve parent");

    let dict = handle.as_dictionary().expect("dictionary");
    let kid_handle = dict.get(b"Kid".as_slice()).expect("Kid entry").clone();
    assert_eq!(kid_handle.object_ref(), Some(ObjectRef::new(5, 0)));

    pdf.resolve_object_handle(&kid_handle).expect("resolve kid");
    assert_eq!(canonical.as_integer(), Some(99));
}

/// An ObjStm-member handle keeps the Task 6 `lift`-based route (offset `-1`
/// always): full ObjStm-relative parsed-offset coordinate correctness is a
/// later layer's scope, not this task's. Asserting the sentinel here (not
/// merely "resolved without error") is the proof it took the `lift` route —
/// the native route never leaves an `Integer` value at the sentinel offset,
/// since every native-parsed scalar gets a real, non-negative offset from
/// construction.
#[test]
fn compressed_object_stream_member_keeps_the_sentinel_offset_via_legacy_lift() {
    let mut pdf = Pdf::open(std::io::Cursor::new(compressed_entry_pdf())).unwrap();
    let object_ref = ObjectRef::new(2, 0);

    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve_object_handle(&handle)
        .expect("resolve compressed member");

    assert_eq!(handle.as_integer(), Some(42));
    assert_eq!(
        handle.get_parsed_offset(),
        -1,
        "an ObjStm member must keep the no-offset sentinel in this task"
    );
}

/// A file-object body containing `.4`, resolved via the native handle path,
/// must preserve the non-canonical source literal — exercising the shared
/// `real_object`/`real_object_handle` literal-preservation decision through
/// actual parsing (Task 4 only ever exercised this via a hand-built direct
/// handle).
#[test]
fn real_literal_round_trips_through_native_parsing() {
    let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n.4\nendobj\n"], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open real-literal fixture");
    let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
    pdf.resolve_object_handle(&handle)
        .expect("resolve real literal");

    assert_eq!(handle.as_real_literal(), Some((0.4, b".4".to_vec())));
}

// ---------------------------------------------------------------------
// Task 7, Step 2b: cross-path parity — the drift tripwire.
//
// These compare `Pdf::resolve` (legacy) against `resolve_object_handle`
// (native, for the Uncompressed case) at the public-API level, per the
// plan's literal Step 2b list. Note that `resolve_object_handle` calls the
// untouched `resolve_to_cache` engine *first* and propagates its error
// via `?` before the native parse ever runs, so for malformed input these
// two public entry points are *guaranteed* to agree here (both surface the
// exact same underlying error) — that is still worth pinning as a
// regression (a future edit could easily break the delegation), but the
// real drift tripwire for the handle-producing container shells
// themselves (`dictionary_handle`/`array_handle`/`object_handle`) is
// `parser.rs`'s own `handle_path_parity_tests` module, which calls
// `Parser::object` and `Parser::object_handle` directly and so actually
// exercises the native path's own error arms.
// ---------------------------------------------------------------------

#[test]
fn cross_path_parity_unterminated_dictionary_matches_legacy_error() {
    let bytes = classic_pdf_with_bodies(
        &[
            b"1 0 obj\n<< /A 1\nendobj\n".as_slice(),
            b"2 0 obj\nnull\nendobj\n",
        ],
        ObjectRef::new(1, 0),
    );
    let object_ref = ObjectRef::new(1, 0);
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open unterminated-dict fixture");

    let legacy_error = pdf
        .resolve(object_ref)
        .expect_err("legacy must reject the unterminated dictionary")
        .to_string();
    assert!(
        legacy_error.contains("expected byte 47"),
        "unexpected legacy error: {legacy_error}"
    );

    let handle = pdf.get_object_handle(object_ref);
    let native_error = pdf
        .resolve_object_handle(&handle)
        .expect_err("the native path must reject it identically")
        .to_string();
    assert_eq!(legacy_error, native_error);
}

#[test]
fn cross_path_parity_unterminated_array_matches_legacy_error() {
    let bytes = classic_pdf_with_bodies(
        &[b"1 0 obj\n[1 2 3".as_slice(), b"2 0 obj\nnull\nendobj\n"],
        ObjectRef::new(1, 0),
    );
    let object_ref = ObjectRef::new(1, 0);
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open unterminated-array fixture");

    let legacy_error = pdf
        .resolve(object_ref)
        .expect_err("legacy must reject the unterminated array")
        .to_string();
    assert!(
        legacy_error.contains("unexpected EOF in array"),
        "unexpected legacy error: {legacy_error}"
    );

    let handle = pdf.get_object_handle(object_ref);
    let native_error = pdf
        .resolve_object_handle(&handle)
        .expect_err("the native path must reject it identically")
        .to_string();
    assert_eq!(legacy_error, native_error);
}

#[test]
fn cross_path_parity_nesting_past_max_parse_depth_matches_legacy_error() {
    // Matches the stack-budget reasoning in `parser.rs`'s own
    // `handle_path_parity_tests` module: constructing `ObjectHandle`s this
    // deep needs more stack than an unoptimized test binary's default
    // thread provides, so this runs on a dedicated, generously-sized one.
    std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let depth = 501; // > MAX_PARSE_DEPTH (500), same bound either path
            let mut body = b"1 0 obj\n".to_vec();
            body.extend(std::iter::repeat_n(b'[', depth));
            body.extend(std::iter::repeat_n(b']', depth));
            body.extend_from_slice(b"\nendobj\n");
            let bytes = classic_pdf_with_bodies(&[&body], ObjectRef::new(1, 0));
            let object_ref = ObjectRef::new(1, 0);
            let mut pdf = Pdf::open_mem_owned(bytes).expect("open deep-nesting fixture");

            let legacy_error = pdf
                .resolve(object_ref)
                .expect_err("legacy must reject nesting past MAX_PARSE_DEPTH")
                .to_string();
            assert!(
                legacy_error.contains("object nesting too deep"),
                "unexpected legacy error: {legacy_error}"
            );

            let handle = pdf.get_object_handle(object_ref);
            let native_error = pdf
                .resolve_object_handle(&handle)
                .expect_err("the native path must reject it identically")
                .to_string();
            assert_eq!(legacy_error, native_error);
        })
        .expect("comparison thread must start")
        .join()
        .expect("comparison must not overflow the stack");
}

// ---------------------------------------------------------------------
// Materialization bridge: `resolve`/`resolve_borrowed`/`set_object`/
// `delete_object` cut onto the `ObjectHandle` graph.
// ---------------------------------------------------------------------

/// The bridge must materialize an indirect array/dict *child* back to
/// `Object::Reference(ObjectRef)` — not recursively resolve it — so every
/// existing consumer match on `Object::Reference(..)` keeps working exactly
/// as today.
#[test]
fn legacy_resolve_borrowed_still_returns_object_reference_for_indirect_children() {
    let bytes = classic_pdf_with_bodies(
        &[
            b"1 0 obj\n<< /Kid 2 0 R >>\nendobj\n".as_slice(),
            b"2 0 obj\n<< /Type /Page >>\nendobj\n",
        ],
        ObjectRef::new(1, 0),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open nested-indirect fixture");
    let object_ref = ObjectRef::new(1, 0);

    let resolved = pdf.resolve_borrowed(object_ref).unwrap();
    let dict = resolved.as_dict().expect("dict");
    let kid = dict.get("Kid").expect("Kid key");
    assert!(matches!(kid, Object::Reference(_)));
    assert_eq!(kid.as_ref_id(), Some(ObjectRef::new(2, 0)));
}

#[test]
fn legacy_resolve_matches_legacy_resolve_borrowed_cloned() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let object_ref = pdf.root_ref().expect("root");

    let owned = pdf.resolve(object_ref).unwrap();
    let borrowed = pdf.resolve_borrowed(object_ref).unwrap();
    assert_eq!(&owned, borrowed);
}

#[test]
fn legacy_resolve_borrowed_on_dangling_ref_is_null_object() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let dangling = ObjectRef::new(999_999, 0);

    assert_eq!(pdf.resolve_borrowed(dangling).unwrap(), &Object::Null);
}

#[test]
fn materialize_then_set_object_round_trips_structurally() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let object_ref = pdf.root_ref().expect("root");

    let resolved = pdf.resolve(object_ref).unwrap();
    pdf.set_object(object_ref, resolved.clone());
    assert_eq!(pdf.resolve(object_ref).unwrap(), resolved);
}

/// `Object::RealLiteral { value, literal }` preserves a non-canonical source
/// spelling (e.g. `.4`) for byte-identical unparse. If `materialize`/`lift`
/// ever dropped `literal` and fell back to `Object::Real`, this would fail.
#[test]
fn real_literal_survives_resolve_set_object_round_trip() {
    let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n.4\nendobj\n"], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open real-literal fixture");
    let object_ref = ObjectRef::new(1, 0);

    let resolved = pdf.resolve(object_ref).unwrap();
    assert!(matches!(&resolved, Object::RealLiteral { literal, .. } if literal == b".4"));

    pdf.set_object(object_ref, resolved.clone());
    assert_eq!(pdf.resolve(object_ref).unwrap(), resolved);
}

/// `Stream` is `{ dict: Dictionary, data: Vec<u8> }` by value; the handle
/// graph keeps the stream dictionary as a *separate handle* with its own
/// `<<`-start parsed offset (design requirement). `materialize` flattens
/// that into a plain `Dictionary`; `set_object`'s write-through must re-split
/// it by *reusing the existing canonical dictionary handle* rather than
/// minting a fresh one with a lost offset — this test is the tripwire for
/// getting that wrong.
#[test]
fn stream_dictionary_parsed_offset_survives_resolve_set_object_round_trip() {
    let body: &[u8] = b"1 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj\n";
    let bytes = classic_pdf_with_bodies(&[body], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
    let stream_ref = ObjectRef::new(1, 0);

    let handle = pdf.get_object_handle(stream_ref);
    pdf.resolve_object_handle(&handle).expect("resolve stream");
    let dict_offset_before = handle
        .as_stream_dict()
        .expect("stream carries its own dictionary handle")
        .get_parsed_offset();
    assert!(
        dict_offset_before >= 0,
        "native parse must record a real dictionary offset"
    );

    let resolved = pdf.resolve(stream_ref).unwrap();
    assert!(matches!(&resolved, Object::Stream(_)));
    pdf.set_object(stream_ref, resolved);

    let dict_offset_after = handle
        .as_stream_dict()
        .expect("stream still carries its own dictionary handle after set_object")
        .get_parsed_offset();
    assert_eq!(dict_offset_before, dict_offset_after);
}

/// `Object::Operator`/`Object::InlineImage` are content-stream-only tokens
/// that no caller sets a resolved object to in practice, but `set_object`'s
/// signature does not forbid it (it accepts any `Object`). `ObjectValue` has
/// no variant to represent either, so `Pdf::lift` returns `Err` for both,
/// routing `set_object` to its own "cannot be represented in the handle
/// graph" fallback — the same route an excessively deep object already
/// takes — which preserves the caller-supplied value as the authoritative
/// materialized result instead of silently losing it to `Null`.
#[test]
fn set_object_with_a_content_stream_only_token_preserves_the_original_value() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let object_ref = pdf.root_ref().expect("root");

    pdf.set_object(object_ref, Object::Operator(b"q".to_vec()));
    assert_eq!(
        pdf.resolve(object_ref).unwrap(),
        Object::Operator(b"q".to_vec())
    );

    pdf.set_object(object_ref, Object::InlineImage(b"data".to_vec()));
    assert_eq!(
        pdf.resolve(object_ref).unwrap(),
        Object::InlineImage(b"data".to_vec())
    );
}

/// Regression for `Pdf::delete_object`'s own handle-graph write-through:
/// resolving an already-resolved, then-deleted ref must observe
/// `Object::Null` afterward, not the stale pre-delete value the handle graph
/// would otherwise still carry.
#[test]
fn resolve_borrowed_returns_null_after_delete_object() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let root_ref = pdf.root_ref().expect("root");
    pdf.resolve(root_ref).unwrap();

    pdf.delete_object(root_ref);

    assert_eq!(pdf.resolve_borrowed(root_ref).unwrap(), &Object::Null);
}

/// `Pdf::lift`'s bounded-depth guard (private to the crate; mirrors every
/// other post-parse structural walker over an `Object` tree here) cannot
/// represent an excessively deep object as an `ObjectHandle` tree.
/// `Pdf::set_object` is infallible, so it must still make
/// `resolve`/`resolve_borrowed` hand back exactly the value the caller set —
/// it is a later structural walker's job (e.g. `optimization.rs`'s own
/// inline-depth guard, exercised end-to-end by its own test suite) to reject
/// the excess depth, not `set_object`'s.
#[test]
fn set_object_with_excessive_depth_still_round_trips_via_the_memo_override() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let object_ref = pdf.root_ref().expect("root");

    let mut deep = Object::Integer(0);
    for _ in 0..300 {
        deep = Object::Array(vec![deep]);
    }

    pdf.set_object(object_ref, deep.clone());
    assert_eq!(pdf.resolve(object_ref).unwrap(), deep);
}
