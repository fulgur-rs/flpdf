//! Integration tests for [`flpdf::name_tree_dests`] (the `/Names /Dests`
//! name-tree writer). Reader-side resolved-destination behavior is covered
//! separately in `outline_document_helper_tests.rs`
//! ([`flpdf::OutlineDocumentHelper::name_tree_dests`]).
//!
//! Mirrors `embedded_files_tests.rs`'s writer scenarios (insert/delete/
//! rebuild), minus the `/AF` bookkeeping tests, which have no `/Dests`
//! analogue:
//!   W1. Insert into empty tree → single entry, sorted.
//!   W2. Multiple inserts → sorted order maintained.
//!   W3. Insert duplicate key → value replaced, no duplicate.
//!   W4. Delete existing key → entry removed.
//!   W5. Delete non-existent key → returns false, tree unchanged.
//!   W6. Delete last entry → /Dests removed from /Names dict.
//!   W7. Insert > LEAF_MAX entries → tree has two levels with /Kids.
//!   W7b. Single insert → single-node root omits /Limits.
//!   W8. Round-trip: insert → raw collector returns same sorted keys.
//!   W9. Insert preserves a pre-existing sibling `/Names` key
//!       (`/EmbeddedFiles`) untouched.
//!   Holder-chain (2-hop) coverage for `/Names`, mirroring
//!   `embedded_files_tests.rs`'s sites 2/4/5.

use flpdf::{delete_name_tree_dest, insert_name_tree_dest, Object, ObjectRef, Pdf, LEAF_MAX};
use std::collections::BTreeMap;
use std::io::Cursor;

// ── PDF byte builder helpers ──────────────────────────────────────────────────

/// Build the xref table and trailer for `n` objects (object numbers 1..n inclusive).
fn finish_pdf(out: &mut Vec<u8>, offsets: &BTreeMap<u32, u64>, n: u32, root_obj: u32) {
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", n + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..=n {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    let trailer = format!(
        "trailer\n<< /Size {} /Root {} 0 R >>\nstartxref\n{}\n%%EOF\n",
        n + 1,
        root_obj,
        xref_start
    );
    out.extend_from_slice(trailer.as_bytes());
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("Pdf::open")
}

/// Walk the raw tree by hand (catalog → /Names ref → /Dests ref → nodes) and
/// collect every `(key, value)` pair across all leaves. Used to verify writer
/// output structurally without depending on the resolved-`Dest` reader
/// (which lives in `OutlineDocumentHelper`, a different module under test
/// elsewhere).
fn collect_raw_dests_tree(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<(Vec<u8>, Object)> {
    let catalog_ref = pdf.root_ref().expect("root");
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).expect("resolve catalog") else {
        panic!("catalog not a dict");
    };
    let Some(names_ref) = catalog.get_ref("Names") else {
        return Vec::new();
    };
    let Object::Dictionary(names_dict) = pdf.resolve(names_ref).expect("resolve /Names") else {
        panic!("/Names not a dict");
    };
    let Some(dests_root_ref) = names_dict.get_ref("Dests") else {
        return Vec::new();
    };

    let mut pairs = Vec::new();
    let mut stack = vec![dests_root_ref];
    while let Some(node_ref) = stack.pop() {
        let Object::Dictionary(node) = pdf.resolve(node_ref).expect("resolve node") else {
            panic!("node not a dict");
        };
        if let Some(Object::Array(arr)) = node.get("Names").cloned() {
            let mut it = arr.into_iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                if let Object::String(key) = k {
                    pairs.push((key, v));
                }
            }
        }
        if let Some(Object::Array(kids)) = node.get("Kids").cloned() {
            for kid in kids {
                if let Object::Reference(r) = kid {
                    stack.push(r);
                }
            }
        }
    }
    pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
    pairs
}

// ── Writer helpers ────────────────────────────────────────────────────────────

/// Build a minimal PDF with no `/Names /Dests` at all.
fn build_empty_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    finish_pdf(&mut out, &off, 3, 1);
    out
}

fn dest_array(page: ObjectRef) -> Object {
    Object::Array(vec![Object::Reference(page), Object::Name(b"Fit".to_vec())])
}

// ── W1: insert into empty tree ────────────────────────────────────────────────

#[test]
fn writer_insert_into_empty_tree() {
    let mut pdf = open(build_empty_pdf());

    insert_name_tree_dest(&mut pdf, b"alpha", dest_array(ObjectRef::new(3, 0))).expect("insert");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"alpha");
    assert_eq!(entries[0].1, dest_array(ObjectRef::new(3, 0)));
}

// ── W2: multiple inserts maintain sorted order ────────────────────────────────

#[test]
fn writer_multiple_inserts_sorted() {
    let mut pdf = open(build_empty_pdf());

    insert_name_tree_dest(&mut pdf, b"zebra", dest_array(ObjectRef::new(3, 0)))
        .expect("insert zebra");
    insert_name_tree_dest(&mut pdf, b"apple", dest_array(ObjectRef::new(3, 0)))
        .expect("insert apple");
    insert_name_tree_dest(&mut pdf, b"mango", dest_array(ObjectRef::new(3, 0)))
        .expect("insert mango");

    let entries = collect_raw_dests_tree(&mut pdf);
    let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
    assert_eq!(keys, vec![b"apple" as &[u8], b"mango", b"zebra"]);
}

// ── W3: insert duplicate key replaces value ───────────────────────────────────

#[test]
fn writer_insert_duplicate_key_replaces() {
    let mut pdf = open(build_empty_pdf());
    let original = dest_array(ObjectRef::new(3, 0));
    let replacement = Object::Array(vec![
        Object::Reference(ObjectRef::new(3, 0)),
        Object::Name(b"XYZ".to_vec()),
        Object::Integer(0),
        Object::Integer(792),
        Object::Integer(0),
    ]);

    insert_name_tree_dest(&mut pdf, b"doc", original).expect("first insert");
    insert_name_tree_dest(&mut pdf, b"doc", replacement.clone()).expect("second insert");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(
        entries.len(),
        1,
        "duplicate key must not create a second entry"
    );
    assert_eq!(entries[0].0, b"doc");
    assert_eq!(entries[0].1, replacement, "value must be the replacement");
}

// ── W4: delete existing key removes it ───────────────────────────────────────

#[test]
fn writer_delete_existing_key() {
    let mut pdf = open(build_empty_pdf());
    insert_name_tree_dest(&mut pdf, b"keep", dest_array(ObjectRef::new(3, 0)))
        .expect("insert keep");
    insert_name_tree_dest(&mut pdf, b"remove", dest_array(ObjectRef::new(3, 0)))
        .expect("insert remove");

    let removed = delete_name_tree_dest(&mut pdf, b"remove").expect("delete");
    assert!(removed, "delete must return true for an existing key");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"keep");
}

// ── W5: delete non-existent key returns false ─────────────────────────────────

#[test]
fn writer_delete_absent_key_returns_false() {
    let mut pdf = open(build_empty_pdf());
    insert_name_tree_dest(&mut pdf, b"present", dest_array(ObjectRef::new(3, 0))).expect("insert");

    let removed = delete_name_tree_dest(&mut pdf, b"absent").expect("delete");
    assert!(!removed, "delete of absent key must return false");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(entries.len(), 1, "tree unchanged after deleting absent key");
}

// ── W6: delete last entry removes /Dests from /Names dict ────────────────────

#[test]
fn writer_delete_last_entry_cleans_up() {
    let mut pdf = open(build_empty_pdf());
    insert_name_tree_dest(&mut pdf, b"only", dest_array(ObjectRef::new(3, 0))).expect("insert");

    let removed = delete_name_tree_dest(&mut pdf, b"only").expect("delete");
    assert!(removed, "delete must succeed");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert!(
        entries.is_empty(),
        "tree must be empty after last entry removed"
    );

    // /Names must be fully cleaned up (no leftover, now-pointless dict).
    let catalog_ref = pdf.root_ref().expect("root");
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).expect("resolve catalog") else {
        panic!("catalog not a dict");
    };
    assert!(
        catalog.get("Names").is_none(),
        "/Names must be removed once its only key (/Dests) is emptied"
    );
}

// ── W7: insert > LEAF_MAX entries produces /Kids split ───────────────────────

#[test]
fn writer_large_insert_produces_kids() {
    let mut pdf = open(build_empty_pdf());
    let count = LEAF_MAX + 5; // One chunk over the threshold.

    for i in 0..count {
        // Keys are zero-padded so byte-sort matches numeric sort.
        let key = format!("dest{i:04}");
        insert_name_tree_dest(&mut pdf, key.as_bytes(), dest_array(ObjectRef::new(3, 0)))
            .expect("insert");
    }

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(entries.len(), count, "all entries must round-trip");
    for window in entries.windows(2) {
        assert!(window[0].0 <= window[1].0, "entries must be sorted");
    }

    // ── Structural check: tree root must carry /Kids, not /Names ─────────────
    let catalog_ref = pdf.root_ref().expect("root");
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).expect("resolve catalog") else {
        panic!("catalog not a dict");
    };
    let names_ref = catalog.get_ref("Names").expect("catalog /Names");
    let Object::Dictionary(names_dict) = pdf.resolve(names_ref).expect("resolve /Names") else {
        panic!("/Names not a dict");
    };
    let dests_root_ref = names_dict.get_ref("Dests").expect("/Dests");
    let Object::Dictionary(dests_root) = pdf.resolve(dests_root_ref).expect("resolve root") else {
        panic!("root not a dict");
    };
    assert!(
        dests_root.get("Kids").is_some(),
        "tree root with {count} entries must have /Kids, got: {dests_root:?}"
    );
    assert!(
        dests_root.get("Names").is_none(),
        "tree root with /Kids must not also have /Names"
    );
}

// ── W7b: single insert → single-node root omits /Limits ──────────────────────

#[test]
fn writer_single_insert_root_omits_limits() {
    let mut pdf = open(build_empty_pdf());

    insert_name_tree_dest(&mut pdf, b"alpha", dest_array(ObjectRef::new(3, 0))).expect("insert");

    let catalog_ref = pdf.root_ref().expect("root");
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).expect("resolve catalog") else {
        panic!("catalog not a dict");
    };
    let names_ref = catalog.get_ref("Names").expect("catalog /Names");
    let Object::Dictionary(names_dict) = pdf.resolve(names_ref).expect("resolve /Names") else {
        panic!("/Names not a dict");
    };
    let dests_root_ref = names_dict.get_ref("Dests").expect("/Dests");
    let Object::Dictionary(dests_root) = pdf.resolve(dests_root_ref).expect("resolve root") else {
        panic!("root not a dict");
    };

    assert!(
        dests_root.get("Names").is_some(),
        "single-node root is a /Names leaf-root, got: {dests_root:?}"
    );
    assert!(
        dests_root.get("Limits").is_none(),
        "root omits /Limits (ISO 32000-2 §7.9.6; qpdf), got: {dests_root:?}"
    );
    assert!(
        dests_root.get("Kids").is_none(),
        "single node is not a /Kids root, got: {dests_root:?}"
    );
}

// ── W8: round-trip: insert → raw collector returns same sorted keys ──────────

#[test]
fn writer_round_trip_key_order() {
    let mut pdf = open(build_empty_pdf());

    let keys: &[&[u8]] = &[b"charlie", b"alpha", b"bravo", b"delta"];
    for key in keys {
        insert_name_tree_dest(&mut pdf, key, dest_array(ObjectRef::new(3, 0))).expect("insert");
    }

    let entries = collect_raw_dests_tree(&mut pdf);
    let got_keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
    assert_eq!(
        got_keys,
        vec![b"alpha" as &[u8], b"bravo", b"charlie", b"delta"]
    );
}

// ── W9: insert preserves a pre-existing sibling /Names key ───────────────────

/// A catalog with a pre-existing `/Names /EmbeddedFiles` entry. Inserting a
/// `/Names /Dests` entry must not disturb the sibling key.
fn build_pdf_with_sibling_embedded_files() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 4 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Names [ (att.txt) 5 0 R ] >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Pages /Kids [ ] /Count 0 >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (att.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 5, 1);
    out
}

#[test]
fn insert_preserves_sibling_embedded_files_key() {
    let mut pdf = open(build_pdf_with_sibling_embedded_files());

    insert_name_tree_dest(&mut pdf, b"home", dest_array(ObjectRef::new(4, 0))).expect("insert");

    // /Dests must now be present, alongside the untouched /EmbeddedFiles key.
    let catalog_ref = pdf.root_ref().expect("root");
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).expect("resolve catalog") else {
        panic!("catalog not a dict");
    };
    let names_ref = catalog.get_ref("Names").expect("catalog /Names");
    let Object::Dictionary(names_dict) = pdf.resolve(names_ref).expect("resolve /Names") else {
        panic!("/Names not a dict");
    };
    assert!(
        names_dict.get("Dests").is_some(),
        "/Dests must be present after insert"
    );
    let ef_ref = names_dict
        .get_ref("EmbeddedFiles")
        .expect("/EmbeddedFiles sibling key must survive the /Dests insert");
    let Object::Dictionary(ef_root) = pdf.resolve(ef_ref).expect("resolve EF root") else {
        panic!("EF root not a dict");
    };
    assert_eq!(
        ef_root.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"att.txt".to_vec()),
            Object::Reference(ObjectRef::new(5, 0)),
        ])),
        "pre-existing /EmbeddedFiles entries must be untouched"
    );

    let dests_entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(dests_entries.len(), 1);
    assert_eq!(dests_entries[0].0, b"home");
}

// ── Holder-chain (double-indirect) coverage, mirroring embedded_files_tests ──
//
// `Pdf::resolve`/`resolve_borrowed` are single-hop. When the catalog /Names
// value is reached through more than one indirect hop (`ref → ref → dict`),
// code that resolves once then type-checks drops the terminal. These tests
// build such 2-hop chains and assert the effect through the public API.

/// Catalog `/Names` reached through a 2-hop chain: `2 0 R → 3 0 R → names dict`.
fn build_two_hop_names_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 6 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n3 0 R\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Dests 4 0 R >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Names [ (alpha) [5 0 R /Fit] ] >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Page /Parent 6 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    off.insert(6, out.len() as u64);
    out.extend_from_slice(b"6 0 obj\n<< /Type /Pages /Kids [ 5 0 R ] /Count 1 >>\nendobj\n");

    finish_pdf(&mut out, &off, 6, 1);
    out
}

#[test]
fn insert_through_two_hop_names_preserves_sibling_and_existing() {
    let mut pdf = open(build_two_hop_names_pdf());

    // Add a sibling key to the terminal /Names dict (object 3) so we can
    // detect whether the rebuild operated on the real terminal dict.
    let Object::Dictionary(mut names_dict) = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve 3")
    else {
        panic!("object 3 must be the /Names dict");
    };
    names_dict.insert("EmbeddedFiles", Object::Reference(ObjectRef::new(99, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(names_dict));

    insert_name_tree_dest(&mut pdf, b"beta", dest_array(ObjectRef::new(5, 0)))
        .expect("insert beta");

    let entries = collect_raw_dests_tree(&mut pdf);
    let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
    assert!(
        keys.contains(&b"alpha".as_ref()),
        "pre-existing dest behind 2-hop /Names must survive insert, got {keys:?}"
    );
    assert!(
        keys.contains(&b"beta".as_ref()),
        "newly inserted dest must be listable, got {keys:?}"
    );

    // The sibling /EmbeddedFiles key in the terminal /Names dict must
    // survive: the rebuild must operate on the real terminal dict.
    let catalog = match pdf.resolve(pdf.root_ref().expect("root")).expect("catalog") {
        Object::Dictionary(d) => d,
        other => panic!("catalog not a dict: {other:?}"),
    };
    let names_ref = catalog.get_ref("Names").expect("catalog /Names");
    let names_dict = match pdf.resolve(names_ref).expect("resolve /Names terminal") {
        Object::Dictionary(d) => d,
        Object::Reference(r) => match pdf.resolve(r).expect("resolve /Names hop2") {
            Object::Dictionary(d) => d,
            other => panic!("/Names hop2 not a dict: {other:?}"),
        },
        other => panic!("/Names not a dict/ref: {other:?}"),
    };
    assert!(
        names_dict.get("EmbeddedFiles").is_some(),
        "sibling /EmbeddedFiles key in terminal names dict must survive rebuild, got {names_dict:?}"
    );
}

#[test]
fn delete_last_entry_through_two_hop_names() {
    let mut pdf = open(build_two_hop_names_pdf());

    // Add a sibling /EmbeddedFiles key so the terminal /Names dict is
    // non-empty after /Dests is dropped — exercises the non-empty
    // set_object branch of the empty-rebuild path on the terminal ref.
    let Object::Dictionary(mut names_dict) = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve 3")
    else {
        panic!("object 3 must be the /Names dict");
    };
    names_dict.insert("EmbeddedFiles", Object::Reference(ObjectRef::new(99, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(names_dict));

    let removed = delete_name_tree_dest(&mut pdf, b"alpha").expect("delete");
    assert!(
        removed,
        "delete of the sole entry behind a 2-hop /Names must find and remove it"
    );

    let Object::Dictionary(terminal) = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve terminal")
    else {
        panic!("object 3 must still be the terminal /Names dict");
    };
    assert!(
        terminal.get("Dests").is_none(),
        "/Dests must be removed from the terminal /Names dict"
    );
    assert!(
        terminal.get("EmbeddedFiles").is_some(),
        "sibling /EmbeddedFiles key must survive the empty rebuild on the terminal dict"
    );

    let entries = collect_raw_dests_tree(&mut pdf);
    assert!(
        entries.is_empty(),
        "tree must be empty after deleting last entry"
    );
}

// ── Boundary: indirect /Names whose terminal is not a dict ───────────────────
//
// When the catalog `/Names` value resolves (through the chain) to a
// non-dict object, the rebuild must fall back to allocating a fresh /Names
// dict rather than panicking, and the inserted destination must remain
// listable.
fn build_non_dict_names_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n[ 1 2 3 ]\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [ ] /Count 0 >>\nendobj\n");

    finish_pdf(&mut out, &off, 3, 1);
    out
}

#[test]
fn insert_with_non_dict_names_terminal_allocates_fresh_dict() {
    let mut pdf = open(build_non_dict_names_pdf());

    insert_name_tree_dest(&mut pdf, b"new", dest_array(ObjectRef::new(3, 0)))
        .expect("insert must succeed");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(
        entries.len(),
        1,
        "inserted dest must be listable via a freshly allocated /Names dict"
    );
    assert_eq!(entries[0].0, b"new");
}

// ── Reader-side raw preservation: non-array/non-ref values survive insert ────
//
// Some producers store a `<< /D array >>` dictionary as the leaf value
// instead of a bare array. The writer must preserve such a pre-existing
// entry verbatim when rebuilding for an unrelated insert.
fn build_dict_valued_dest_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 4 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Dests 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Limits [ (existing) (existing) ] \
          /Names [ (existing) << /D [4 0 R /Fit] >> ] >>\nendobj\n",
    );

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Pages /Kids [ ] /Count 0 >>\nendobj\n");

    finish_pdf(&mut out, &off, 4, 1);
    out
}

#[test]
fn writer_preserves_dict_valued_dest_on_insert() {
    let mut pdf = open(build_dict_valued_dest_pdf());

    insert_name_tree_dest(&mut pdf, b"added", dest_array(ObjectRef::new(4, 0))).expect("insert");

    let entries = collect_raw_dests_tree(&mut pdf);
    assert_eq!(entries.len(), 2);

    let existing = entries
        .iter()
        .find(|(k, _)| k == b"existing")
        .expect("pre-existing dict-valued entry must survive rebuild");
    match &existing.1 {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("D"),
                Some(&Object::Array(vec![
                    Object::Reference(ObjectRef::new(4, 0)),
                    Object::Name(b"Fit".to_vec()),
                ]))
            );
        }
        other => panic!("existing value must stay a dictionary, got: {other:?}"),
    }

    let added = entries
        .iter()
        .find(|(k, _)| k == b"added")
        .expect("inserted key must be present");
    assert_eq!(added.1, dest_array(ObjectRef::new(4, 0)));
}
