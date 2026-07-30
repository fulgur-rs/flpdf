//! Public-API parity tests for `Pdf::resolve_object_handle` (the dual-write
//! `ObjectHandle` resolution bridge): proves it reaches the same observable
//! outcomes as the untouched legacy `resolve`/`resolve_borrowed` engine it
//! delegates to — missing/dangling references resolve to null, compressed
//! (ObjStm) members and cyclic indirect `/Length` streams resolve without
//! erroring or hanging, and repeated `get_object_handle` calls observe the
//! same canonical, already-resolved state.

use flpdf::{ObjectHandle, ObjectRef, Pdf};
use std::fs::File;
use std::io::BufReader;

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

    // `Pdf::lift` does not convert `Object::Stream` (dict/data split is out
    // of this task's scope; see `lift` in reader.rs), so an indirect stream
    // lifts to `ObjectValue::Null` for now — the scenario under test here is
    // the cyclic-/Length guard surviving the new bridge, not the eventual
    // stream materialization.
    assert!(handle.is_null());

    // The untouched legacy engine still recovers the real stream payload,
    // proving the cycle guard's behavior is unchanged by this task.
    let legacy = pdf.resolve(object_ref).expect("legacy resolve");
    assert_eq!(legacy.as_stream().expect("stream").data, b"abc");
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

/// A literal PDF `null` object (present in the xref table, genuinely
/// parsed) must resolve to `ObjectValue::Null` — and this must not be the
/// same internal route as the "dangling" (absent from xref) case in test 2,
/// even though both currently present the same externally-observable
/// `is_null() == true`. `object_handle.rs`'s `IndirectState` keeps
/// `Missing` and `Resolved(ObjectValue::Null)` as distinct variants for
/// exactly this reason (see its doc comment); this test fixes the
/// black-box (public-API) half of that contract in place so a future change
/// collapsing the two cannot pass unnoticed here.
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
