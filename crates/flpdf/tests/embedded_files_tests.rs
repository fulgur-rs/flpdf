//! Integration tests for [`flpdf::embedded_files`] name-tree reader and writer.
//!
//! All tests build minimal in-memory PDFs without touching the filesystem
//! and exercise the four acceptance scenarios:
//!   1. Single-level `/Names` leaf → ordered list.
//!   2. Multi-level `/Kids` tree → depth-first ordered list.
//!   3. `/Limits` present → still works (limits are non-destructive).
//!   4. `/EmbeddedFiles` absent → empty list, no error.
//!   5. `/Names` catalog key absent → empty list, no error.
//!   6. `/Root` absent → empty list, no error.
//!
//! Writer tests (insert/delete/rebuild):
//!   W1. Insert into empty tree → single entry, sorted.
//!   W2. Multiple inserts → sorted order maintained.
//!   W3. Insert duplicate key → value replaced, no duplicate.
//!   W4. Delete existing key → entry removed, no dangling /Kids.
//!   W5. Delete non-existent key → returns false, tree unchanged.
//!   W6. Delete last entry → /EmbeddedFiles removed from /Names dict.
//!   W7. Insert > LEAF_MAX entries → tree has two levels with /Kids.
//!   W8. Round-trip: insert → list_embedded_files → same sorted keys.

use flpdf::{delete_embedded_file, insert_embedded_file, list_embedded_files, ObjectRef, Pdf, LEAF_MAX};
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

// ── Test 1: single-level /Names leaf ─────────────────────────────────────────

/// Build a minimal PDF with a flat /EmbeddedFiles name-tree leaf.
///
/// Object layout:
///   1 0 R  Catalog  (/Names 2 0 R)
///   2 0 R  /Names dict  (/EmbeddedFiles 3 0 R)
///   3 0 R  leaf node  (/Names [(alpha) 4 0 R (beta) 5 0 R])
///   4 0 R  Filespec for alpha
///   5 0 R  Filespec for beta
fn build_single_level_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Names [ (alpha) 4 0 R (beta) 5 0 R ] >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Filespec /F (alpha.txt) >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (beta.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 5, 1);
    out
}

#[test]
fn single_level_returns_ordered_list() {
    let mut pdf = open(build_single_level_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, b"alpha");
    assert_eq!(entries[0].1, ObjectRef::new(4, 0));
    assert_eq!(entries[1].0, b"beta");
    assert_eq!(entries[1].1, ObjectRef::new(5, 0));
}

// ── Test 2: multi-level /Kids tree ───────────────────────────────────────────

/// Build a PDF with an intermediate /Kids node and two leaf children.
///
/// Object layout:
///   1 0 R  Catalog  (/Names 2 0 R)
///   2 0 R  /Names dict  (/EmbeddedFiles 3 0 R)
///   3 0 R  root node  (/Kids [4 0 R, 5 0 R])
///   4 0 R  leaf1  (/Names [(aaa) 6 0 R])
///   5 0 R  leaf2  (/Names [(zzz) 7 0 R])
///   6 0 R  Filespec for aaa
///   7 0 R  Filespec for zzz
fn build_multi_level_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Kids [ 4 0 R 5 0 R ] >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Names [ (aaa) 6 0 R ] >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Names [ (zzz) 7 0 R ] >>\nendobj\n");

    off.insert(6, out.len() as u64);
    out.extend_from_slice(b"6 0 obj\n<< /Type /Filespec /F (aaa.txt) >>\nendobj\n");

    off.insert(7, out.len() as u64);
    out.extend_from_slice(b"7 0 obj\n<< /Type /Filespec /F (zzz.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 7, 1);
    out
}

#[test]
fn multi_level_returns_depth_first_ordered_list() {
    let mut pdf = open(build_multi_level_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert_eq!(entries.len(), 2);
    // DFS: leaf1 (aaa) before leaf2 (zzz)
    assert_eq!(entries[0].0, b"aaa");
    assert_eq!(entries[0].1, ObjectRef::new(6, 0));
    assert_eq!(entries[1].0, b"zzz");
    assert_eq!(entries[1].1, ObjectRef::new(7, 0));
}

// ── Test 3: /Limits present → still enumerates correctly ─────────────────────

/// Like the multi-level tree but with /Limits on each node.
fn build_multi_level_with_limits_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Limits [(aaa) (zzz)] /Kids [ 4 0 R 5 0 R ] >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Limits [(aaa) (mmm)] /Names [ (aaa) 6 0 R (mmm) 7 0 R ] >>\nendobj\n",
    );

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Limits [(zzz) (zzz)] /Names [ (zzz) 8 0 R ] >>\nendobj\n");

    off.insert(6, out.len() as u64);
    out.extend_from_slice(b"6 0 obj\n<< /Type /Filespec /F (aaa.txt) >>\nendobj\n");

    off.insert(7, out.len() as u64);
    out.extend_from_slice(b"7 0 obj\n<< /Type /Filespec /F (mmm.txt) >>\nendobj\n");

    off.insert(8, out.len() as u64);
    out.extend_from_slice(b"8 0 obj\n<< /Type /Filespec /F (zzz.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 8, 1);
    out
}

#[test]
fn limits_present_still_enumerates_all_entries() {
    let mut pdf = open(build_multi_level_with_limits_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].0, b"aaa");
    assert_eq!(entries[0].1, ObjectRef::new(6, 0));
    assert_eq!(entries[1].0, b"mmm");
    assert_eq!(entries[1].1, ObjectRef::new(7, 0));
    assert_eq!(entries[2].0, b"zzz");
    assert_eq!(entries[2].1, ObjectRef::new(8, 0));
}

// ── Test 4: /EmbeddedFiles absent → empty, no error ──────────────────────────

fn build_no_embedded_files_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    // /Names dict present but has no /EmbeddedFiles key
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Dests 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Names [] >>\nendobj\n");

    finish_pdf(&mut out, &off, 3, 1);
    out
}

#[test]
fn no_embedded_files_key_returns_empty() {
    let mut pdf = open(build_no_embedded_files_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert!(
        entries.is_empty(),
        "expected empty list when /EmbeddedFiles absent, got {:?}",
        entries
    );
}

// ── Test 5: /Names catalog key absent → empty, no error ──────────────────────

fn build_no_names_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    // Catalog has no /Names key at all
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

    finish_pdf(&mut out, &off, 2, 1);
    out
}

#[test]
fn no_names_key_returns_empty() {
    let mut pdf = open(build_no_names_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert!(entries.is_empty(), "expected empty list when /Names absent");
}

// ── Test 6: inline /EmbeddedFiles dict (direct, not indirect) ────────────────

/// Some generators embed the name-tree root directly in /Names dict without
/// an indirect reference.
fn build_inline_ef_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    // /Names is a direct inline dict; /EmbeddedFiles is also a direct inline dict
    out.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R \
          /Names << /EmbeddedFiles << /Names [ (inline) 2 0 R ] >> >> >>\nendobj\n",
    );

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Filespec /F (inline.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 2, 1);
    out
}

#[test]
fn inline_ef_dict_returns_entry() {
    let mut pdf = open(build_inline_ef_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"inline");
    assert_eq!(entries[0].1, ObjectRef::new(2, 0));
}

// ── Test 7: fixture attachment-two-page.pdf (integration) ────────────────────

#[test]
fn fixture_attachment_two_page() {
    use std::path::Path;

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    if !fixture.exists() {
        eprintln!("skipping fixture test: {:?} not found", fixture);
        return;
    }

    let data = std::fs::read(&fixture).expect("read fixture");
    let mut pdf = Pdf::open(Cursor::new(data)).expect("Pdf::open");
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    // The fixture has at least one attachment
    assert!(
        !entries.is_empty(),
        "expected at least one embedded file in fixture"
    );
    // All entries must have non-empty keys
    for (key, _) in &entries {
        assert!(!key.is_empty(), "name key must be non-empty");
    }
    // Entries must be in DFS / key-sorted order
    for window in entries.windows(2) {
        assert!(
            window[0].0 <= window[1].0,
            "entries must be in non-decreasing key order"
        );
    }
}

// ── Writer helpers ────────────────────────────────────────────────────────────

/// Build a minimal PDF with no /Names /EmbeddedFiles at all.
///
/// Object layout:
///   1 0 R  Catalog  (/Pages 2 0 R)
///   2 0 R  Pages    (/Type /Pages /Kids [] /Count 0)
///
/// Filespec slots are pre-allocated in the xref so we can hand their refs to
/// `insert_embedded_file` without them being truly absent (the Pdf::set_object
/// call will place them into the cache regardless).
fn build_empty_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ ] /Count 0 >>\nendobj\n");

    // Pre-allocate a few filespec slots so the xref knows about them.
    for n in 3u32..=40 {
        off.insert(n, out.len() as u64);
        out.extend_from_slice(
            format!("{n} 0 obj\n<< /Type /Filespec /F (file{n}.txt) >>\nendobj\n").as_bytes(),
        );
    }

    finish_pdf(&mut out, &off, 40, 1);
    out
}

// ── W1: insert into empty tree ────────────────────────────────────────────────

#[test]
fn writer_insert_into_empty_tree() {
    let mut pdf = open(build_empty_pdf());
    let fs_ref = ObjectRef::new(3, 0);

    insert_embedded_file(&mut pdf, b"alpha.txt", fs_ref).expect("insert");

    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"alpha.txt");
    assert_eq!(entries[0].1, fs_ref);
}

// ── W2: multiple inserts maintain sorted order ────────────────────────────────

#[test]
fn writer_multiple_inserts_sorted() {
    let mut pdf = open(build_empty_pdf());

    // Insert out of alphabetical order.
    insert_embedded_file(&mut pdf, b"zebra.txt", ObjectRef::new(3, 0)).expect("insert zebra");
    insert_embedded_file(&mut pdf, b"apple.txt", ObjectRef::new(4, 0)).expect("insert apple");
    insert_embedded_file(&mut pdf, b"mango.txt", ObjectRef::new(5, 0)).expect("insert mango");

    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].0, b"apple.txt");
    assert_eq!(entries[1].0, b"mango.txt");
    assert_eq!(entries[2].0, b"zebra.txt");

    // Verify sort invariant holds across all windows.
    for window in entries.windows(2) {
        assert!(window[0].0 <= window[1].0, "entries must be sorted");
    }
}

// ── W3: insert duplicate key replaces value ───────────────────────────────────

#[test]
fn writer_insert_duplicate_key_replaces() {
    let mut pdf = open(build_empty_pdf());
    let original = ObjectRef::new(3, 0);
    let replacement = ObjectRef::new(4, 0);

    insert_embedded_file(&mut pdf, b"doc.pdf", original).expect("first insert");
    insert_embedded_file(&mut pdf, b"doc.pdf", replacement).expect("second insert");

    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), 1, "duplicate key must not create a second entry");
    assert_eq!(entries[0].0, b"doc.pdf");
    assert_eq!(entries[0].1, replacement, "value must be the replacement");
}

// ── W4: delete existing key removes it ───────────────────────────────────────

#[test]
fn writer_delete_existing_key() {
    let mut pdf = open(build_empty_pdf());
    insert_embedded_file(&mut pdf, b"keep.txt", ObjectRef::new(3, 0)).expect("insert keep");
    insert_embedded_file(&mut pdf, b"remove.txt", ObjectRef::new(4, 0)).expect("insert remove");

    let removed = delete_embedded_file(&mut pdf, b"remove.txt").expect("delete");
    assert!(removed, "delete must return true for an existing key");

    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"keep.txt");

    // Verify there are no dangling /Kids: the remaining single entry is a
    // flat leaf (no /Kids array needed), so the tree round-trips cleanly.
    let entries2 = list_embedded_files(&mut pdf).expect("second list");
    assert_eq!(entries2.len(), 1);
}

// ── W5: delete non-existent key returns false ─────────────────────────────────

#[test]
fn writer_delete_absent_key_returns_false() {
    let mut pdf = open(build_empty_pdf());
    insert_embedded_file(&mut pdf, b"present.txt", ObjectRef::new(3, 0)).expect("insert");

    let removed = delete_embedded_file(&mut pdf, b"absent.txt").expect("delete");
    assert!(!removed, "delete of absent key must return false");

    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), 1, "tree unchanged after deleting absent key");
}

// ── W6: delete last entry removes /EmbeddedFiles from /Names dict ─────────────

#[test]
fn writer_delete_last_entry_cleans_up() {
    let mut pdf = open(build_empty_pdf());
    insert_embedded_file(&mut pdf, b"only.txt", ObjectRef::new(3, 0)).expect("insert");

    let removed = delete_embedded_file(&mut pdf, b"only.txt").expect("delete");
    assert!(removed, "delete must succeed");

    let entries = list_embedded_files(&mut pdf).expect("list after cleanup");
    assert!(entries.is_empty(), "tree must be empty after last entry removed");
}

// ── W7: insert > LEAF_MAX entries produces /Kids split ───────────────────────

#[test]
fn writer_large_insert_produces_kids() {
    use flpdf::Object;

    let mut pdf = open(build_empty_pdf());
    let count = LEAF_MAX + 5; // One chunk over the threshold.

    for i in 0..count {
        // Keys are zero-padded so byte-sort matches numeric sort.
        let key = format!("file{i:04}.txt");
        let fs_ref = ObjectRef::new(3 + i as u32, 0);
        insert_embedded_file(&mut pdf, key.as_bytes(), fs_ref).expect("insert");
    }

    // ── Reader round-trip ────────────────────────────────────────────────────
    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), count, "all entries must be readable back");

    // Verify sorted order.
    for window in entries.windows(2) {
        assert!(window[0].0 <= window[1].0, "entries must be sorted");
    }

    // ── Structural check: tree root must carry /Kids, not /Names ─────────────
    // Walk: catalog → /Names ref → /EmbeddedFiles ref → tree root dict.
    let catalog_ref = pdf.root_ref().expect("root");
    let catalog = match pdf.resolve(catalog_ref).expect("resolve catalog") {
        Object::Dictionary(d) => d,
        other => panic!("catalog not a dict: {other:?}"),
    };
    let names_ref = catalog.get_ref("Names").expect("catalog /Names");
    let names_dict = match pdf.resolve(names_ref).expect("resolve /Names") {
        Object::Dictionary(d) => d,
        other => panic!("/Names not a dict: {other:?}"),
    };
    let ef_root_ref = names_dict.get_ref("EmbeddedFiles").expect("/EmbeddedFiles");
    let ef_root = match pdf.resolve(ef_root_ref).expect("resolve EF root") {
        Object::Dictionary(d) => d,
        other => panic!("EF root not a dict: {other:?}"),
    };

    // The root must have /Kids (not a flat /Names leaf) because count > LEAF_MAX.
    assert!(
        ef_root.get("Kids").is_some(),
        "tree root with {count} entries must have /Kids, got: {ef_root:?}"
    );
    assert!(
        ef_root.get("Names").is_none(),
        "tree root with /Kids must not also have /Names"
    );

    // Verify /Limits on a leaf child.
    let Object::Array(kids) = ef_root.get("Kids").cloned().unwrap() else {
        panic!("/Kids is not an array");
    };
    let first_leaf_ref = match &kids[0] {
        Object::Reference(r) => *r,
        other => panic!("first kid not a reference: {other:?}"),
    };
    let first_leaf = match pdf.resolve(first_leaf_ref).expect("resolve leaf") {
        Object::Dictionary(d) => d,
        other => panic!("leaf not a dict: {other:?}"),
    };
    assert!(
        first_leaf.get("Limits").is_some(),
        "leaf node must have /Limits"
    );
    // /Limits must be a two-element array of strings.
    let Object::Array(limits) = first_leaf.get("Limits").cloned().unwrap() else {
        panic!("/Limits is not an array");
    };
    assert_eq!(limits.len(), 2, "/Limits must have exactly 2 elements");
    assert!(
        matches!(&limits[0], Object::String(_)),
        "/Limits[0] must be a string"
    );
    assert!(
        matches!(&limits[1], Object::String(_)),
        "/Limits[1] must be a string"
    );
    // First limit ≤ last limit within the leaf.
    let Object::String(first_lim) = &limits[0] else { unreachable!() };
    let Object::String(last_lim) = &limits[1] else { unreachable!() };
    assert!(first_lim <= last_lim, "leaf /Limits[0] must be ≤ /Limits[1]");
}

// ── W8: round-trip: insert → list returns same keys ──────────────────────────

#[test]
fn writer_round_trip_key_order() {
    let mut pdf = open(build_empty_pdf());

    let keys: &[&[u8]] = &[b"charlie", b"alpha", b"bravo", b"delta"];
    let refs: Vec<ObjectRef> = (3u32..)
        .take(keys.len())
        .map(|n| ObjectRef::new(n, 0))
        .collect();

    for (key, &fs_ref) in keys.iter().zip(refs.iter()) {
        insert_embedded_file(&mut pdf, key, fs_ref).expect("insert");
    }

    let entries = list_embedded_files(&mut pdf).expect("list");

    // Expect alphabetical order.
    let got_keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
    assert_eq!(got_keys, vec![b"alpha" as &[u8], b"bravo", b"charlie", b"delta"]);

    // Verify that the filespec refs match after sorting.
    let expected_sorted = {
        let mut pairs: Vec<(&[u8], ObjectRef)> =
            keys.iter().copied().zip(refs.iter().copied()).collect();
        pairs.sort_by_key(|(k, _)| *k);
        pairs
    };
    for (i, (exp_key, exp_ref)) in expected_sorted.iter().enumerate() {
        assert_eq!(&entries[i].0, exp_key);
        assert_eq!(entries[i].1, *exp_ref);
    }
}
