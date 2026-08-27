//! Public-API regression tests for the `Pdf::set_object`/`Pdf::delete_object`
//! materialization bridge: the boundary where a caller-supplied raw `Object`
//! value is written through onto the canonical `ObjectHandle` graph.
//!
//! Unlike `Pdf::resolve_object`/`Pdf::resolve_borrowed` (marked
//! `qpdf-cutover-delete` in `reader.rs` and scheduled for removal once their
//! callers migrate to canonical handle accessors), `Pdf::set_object` and
//! `Pdf::delete_object` carry no such marker and remain part of the
//! supported public API. This file is their dedicated home so their
//! regression coverage does not depend on `object_handle_parity_tests.rs`,
//! whose own route-contract test (`object_handle_parity_route_tests.rs`)
//! intentionally keeps that file limited to the canonical-only resolution
//! path.

use flpdf::{Object, ObjectRef, Pdf};
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

#[test]
fn materialize_then_set_object_round_trips_structurally() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let object_ref = pdf.root_ref().expect("root");

    let resolved = pdf.resolve_object(object_ref).unwrap();
    pdf.set_object(object_ref, resolved.clone());
    assert_eq!(pdf.resolve_object(object_ref).unwrap(), resolved);
}

/// `Object::RealLiteral { value, literal }` preserves a non-canonical source
/// spelling (e.g. `.4`) for byte-identical unparse. If `materialize`/`lift`
/// ever dropped `literal` and fell back to `Object::Real`, this would fail.
#[test]
fn real_literal_survives_resolve_set_object_round_trip() {
    let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n.4\nendobj\n"], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open real-literal fixture");
    let object_ref = ObjectRef::new(1, 0);

    let resolved = pdf.resolve_object(object_ref).unwrap();
    assert!(matches!(&resolved, Object::RealLiteral { literal, .. } if literal == b".4"));

    pdf.set_object(object_ref, resolved.clone());
    assert_eq!(pdf.resolve_object(object_ref).unwrap(), resolved);
}

/// `Stream` is `{ dict: Dictionary, data: Vec<u8> }` by value; the handle
/// graph keeps the stream dictionary as a *separate handle* with its own
/// `<<`-start parsed offset (design requirement). `materialize` flattens
/// that into a plain `Dictionary`; `set_object`'s write-through must re-split
/// it by *reusing the existing canonical dictionary handle* rather than
/// minting a fresh one with a lost offset — this test is the tripwire for
/// getting that wrong (see the cross-reference in `reader.rs`'s
/// `lift_for_set_object` doc comment).
#[test]
fn stream_dictionary_parsed_offset_survives_resolve_set_object_round_trip() {
    let body: &[u8] = b"1 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj\n";
    let bytes = classic_pdf_with_bodies(&[body], ObjectRef::new(1, 0));
    let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
    let stream_ref = ObjectRef::new(1, 0);

    let handle = pdf.get_object_handle(stream_ref);
    pdf.resolve(&handle).expect("resolve stream");
    let dict_offset_before = handle
        .as_stream_dict()
        .expect("stream carries its own dictionary handle")
        .get_parsed_offset();
    assert!(
        dict_offset_before >= 0,
        "native parse must record a real dictionary offset"
    );

    let resolved = pdf.resolve_object(stream_ref).unwrap();
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
        pdf.resolve_object(object_ref).unwrap(),
        Object::Operator(b"q".to_vec())
    );

    pdf.set_object(object_ref, Object::InlineImage(b"data".to_vec()));
    assert_eq!(
        pdf.resolve_object(object_ref).unwrap(),
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
    pdf.resolve_object(root_ref).unwrap();

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
    assert_eq!(pdf.resolve_object(object_ref).unwrap(), deep);
}
