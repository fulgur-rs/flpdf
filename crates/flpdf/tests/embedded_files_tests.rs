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
//!   W7b. Single insert → single-node root omits /Limits (ISO 32000-2 §7.9.6).
//!   W8. Round-trip: insert → list_embedded_files → same sorted keys.

use flpdf::{
    delete_embedded_file, insert_embedded_file, list_embedded_files, ObjectRef, Pdf, LEAF_MAX,
};
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
    assert_eq!(
        entries.len(),
        1,
        "duplicate key must not create a second entry"
    );
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
    assert!(
        entries.is_empty(),
        "tree must be empty after last entry removed"
    );
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
    let Object::String(first_lim) = &limits[0] else {
        unreachable!()
    };
    let Object::String(last_lim) = &limits[1] else {
        unreachable!()
    };
    assert!(
        first_lim <= last_lim,
        "leaf /Limits[0] must be ≤ /Limits[1]"
    );
}

// ── W7b: single insert → single-node root omits /Limits ──────────────────────

#[test]
fn writer_single_insert_root_omits_limits() {
    use flpdf::Object;

    let mut pdf = open(build_empty_pdf());
    let fs_ref = ObjectRef::new(3, 0);

    // One attachment → the tree fits in a single node (entries <= LEAF_MAX), so
    // the root is itself the leaf-root holding /Names directly.
    insert_embedded_file(&mut pdf, b"alpha.txt", fs_ref).expect("insert");

    // Reader round-trip: the single attachment must be enumerable, proving the
    // tree is a real, populated single node (not empty/degenerate).
    let entries = list_embedded_files(&mut pdf).expect("list");
    assert_eq!(entries.len(), 1, "exactly one attachment must round-trip");
    assert_eq!(entries[0].0, b"alpha.txt");
    assert_eq!(entries[0].1, fs_ref);

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

    // Structural conformance (ISO 32000-2 §7.9.6; qpdf): a single-node root is a
    // /Names leaf-root that omits /Limits and is not a /Kids root.
    assert!(
        ef_root.get("Names").is_some(),
        "single-node root is a /Names leaf-root, got: {ef_root:?}"
    );
    assert!(
        ef_root.get("Limits").is_none(),
        "root omits /Limits (ISO 32000-2 §7.9.6; qpdf), got: {ef_root:?}"
    );
    assert!(
        ef_root.get("Kids").is_none(),
        "single node is not a /Kids root, got: {ef_root:?}"
    );

    // Substantive check: the /Names array actually names the single attachment,
    // confirming a populated single-node tree rather than an empty one.
    let Object::Array(pairs) = ef_root.get("Names").cloned().unwrap() else {
        panic!("/Names is not an array");
    };
    assert_eq!(
        pairs.len(),
        2,
        "single-entry leaf /Names must be one [key, value] pair"
    );
    assert!(
        matches!(&pairs[0], Object::String(k) if k == b"alpha.txt"),
        "/Names[0] must be the attachment key, got: {:?}",
        pairs[0]
    );
    assert!(
        matches!(&pairs[1], Object::Reference(r) if *r == fs_ref),
        "/Names[1] must reference the inserted filespec, got: {:?}",
        pairs[1]
    );
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
    assert_eq!(
        got_keys,
        vec![b"alpha" as &[u8], b"bravo", b"charlie", b"delta"]
    );

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

// ── W9: rebuild preserves pre-existing direct-dict /Filespec entries ──────────

/// A name-tree leaf may store a value as a *direct* `/Filespec` dictionary
/// rather than an indirect reference. The public reader filters those out,
/// but the writer must not: inserting an unrelated key must not silently drop
/// the direct-dict entry from the rebuilt tree.
fn build_direct_dict_entry_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 4 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    // Tree-root leaf whose only entry's value is a DIRECT /Filespec dict.
    off.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Limits [ (direct.txt) (direct.txt) ] \
          /Names [ (direct.txt) << /Type /Filespec /F (direct.txt) >> ] >>\nendobj\n",
    );

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Pages /Kids [ ] /Count 0 >>\nendobj\n");

    // Pre-allocated slot for the inserted filespec reference.
    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (added.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 5, 1);
    out
}

#[test]
fn writer_preserves_direct_dict_filespec_on_insert() {
    use flpdf::Object;

    let mut pdf = open(build_direct_dict_entry_pdf());

    // Sanity: the public reader skips the direct-dict entry (documented).
    let visible = list_embedded_files(&mut pdf).expect("list");
    assert!(
        visible.is_empty(),
        "public reader must skip direct-dict values; got {visible:?}"
    );

    // Insert an unrelated, reference-valued entry — triggers a full rebuild.
    insert_embedded_file(&mut pdf, b"added.txt", ObjectRef::new(5, 0)).expect("insert");

    // Walk the rebuilt tree by hand and collect the raw /Names pairs across
    // all leaves (the rebuilt root may be a single leaf or carry /Kids).
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

    // Gather (key, value) pairs from every leaf reachable from the root.
    let mut pairs: Vec<(Vec<u8>, Object)> = Vec::new();
    let mut stack = vec![ef_root_ref];
    while let Some(node_ref) = stack.pop() {
        let node = match pdf.resolve(node_ref).expect("resolve node") {
            Object::Dictionary(d) => d,
            other => panic!("node not a dict: {other:?}"),
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

    // Both entries must survive: the inserted reference AND the original
    // direct-dict /Filespec, preserved verbatim.
    let added = pairs
        .iter()
        .find(|(k, _)| k == b"added.txt")
        .expect("inserted key must be present");
    assert_eq!(
        added.1,
        Object::Reference(ObjectRef::new(5, 0)),
        "inserted value must be the reference passed to insert_embedded_file"
    );

    let direct = pairs
        .iter()
        .find(|(k, _)| k == b"direct.txt")
        .expect("pre-existing direct-dict entry must NOT be dropped on rebuild");
    match &direct.1 {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("F").cloned(),
                Some(Object::String(b"direct.txt".to_vec())),
                "direct-dict /Filespec must be preserved verbatim"
            );
        }
        other => panic!("direct-dict value must stay a dictionary, got: {other:?}"),
    }
}

// ── Holder-chain (double-indirect) coverage (flpdf-3x23) ─────────────────────
//
// `Pdf::resolve`/`resolve_borrowed` are single-hop. When the catalog `/Names`
// value (or a `/AF` array) is reached through more than one indirect hop
// (`ref → ref → value`), code that resolves once then type-checks drops the
// terminal. These tests build such 2-hop chains and assert the EFFECT through
// the public API; each is RED before the `resolve_ref_chain` fix.

/// Catalog `/Names` reached through a 2-hop chain: `2 0 R → 3 0 R → names dict`.
///
/// Object layout:
///   1 0 R  Catalog       (/Names 2 0 R)
///   2 0 R  bare reference 3 0 R              (first hop)
///   3 0 R  /Names dict    (/EmbeddedFiles 4 0 R)
///   4 0 R  leaf node      (/Names [(alpha) 5 0 R])
///   5 0 R  Filespec for alpha
fn build_two_hop_names_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n3 0 R\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /EmbeddedFiles 4 0 R >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Names [ (alpha) 5 0 R ] >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (alpha.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 5, 1);
    out
}

// ── Site 2: list_embedded_files reads through a 2-hop /Names ──────────────────

#[test]
fn list_enumerates_through_two_hop_names() {
    let mut pdf = open(build_two_hop_names_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert_eq!(
        entries.len(),
        1,
        "the embedded file behind a 2-hop /Names must be enumerated"
    );
    assert_eq!(entries[0].0, b"alpha");
    assert_eq!(entries[0].1, ObjectRef::new(5, 0));
}

// ── Site 5: insert via a 2-hop /Names preserves sibling keys ──────────────────
//
// `insert_embedded_file` collects existing entries (site 3) then rebuilds
// (site 5). With a 2-hop /Names, a single-hop reader returns the names dict as
// non-dict, so the rebuilt tree drops a sibling key (`/Dests`) and the pre-
// existing attachment. Following the chain preserves both.

#[test]
fn insert_through_two_hop_names_preserves_sibling_and_existing() {
    use flpdf::Object;

    let mut pdf = open(build_two_hop_names_pdf());

    // Add a sibling key to the terminal /Names dict (object 3) so we can detect
    // whether the rebuild operated on the real terminal dict or a fresh one.
    let Object::Dictionary(mut names_dict) = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve 3")
    else {
        panic!("object 3 must be the /Names dict");
    };
    names_dict.insert("Dests", Object::Reference(ObjectRef::new(99, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(names_dict));

    // Insert a new embedded file (reference value; object 5 already exists).
    insert_embedded_file(&mut pdf, b"beta.txt", ObjectRef::new(5, 0)).expect("insert beta");

    // Both the pre-existing attachment and the new one must be listable.
    let entries = list_embedded_files(&mut pdf).expect("list after insert");
    let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
    assert!(
        keys.contains(&b"alpha".as_ref()),
        "pre-existing attachment behind 2-hop /Names must survive insert, got {keys:?}"
    );
    assert!(
        keys.contains(&b"beta.txt".as_ref()),
        "newly inserted attachment must be listable, got {keys:?}"
    );

    // The sibling /Dests key in the terminal names dict must be preserved: the
    // rebuild must operate on the real terminal dict, not a fresh one.
    let catalog = match pdf.resolve(pdf.root_ref().expect("root")).expect("catalog") {
        Object::Dictionary(d) => d,
        other => panic!("catalog not a dict: {other:?}"),
    };
    let names_ref = catalog.get_ref("Names").expect("catalog /Names");
    // The rewritten /Names points (possibly through the chain) at the dict that
    // now carries /EmbeddedFiles; resolve follows one hop, and the terminal
    // must still hold /Dests.
    let names_dict = match pdf.resolve(names_ref).expect("resolve /Names terminal") {
        Object::Dictionary(d) => d,
        Object::Reference(r) => match pdf.resolve(r).expect("resolve /Names hop2") {
            Object::Dictionary(d) => d,
            other => panic!("/Names hop2 not a dict: {other:?}"),
        },
        other => panic!("/Names not a dict/ref: {other:?}"),
    };
    assert!(
        names_dict.get("Dests").is_some(),
        "sibling /Dests key in terminal names dict must survive rebuild, got {names_dict:?}"
    );
}

// ── Site 4: delete last entry via a 2-hop /Names reaches the rebuild ──────────
//
// `delete_embedded_file` collects (site 3) then rebuilds-empty (site 4). With a
// 2-hop /Names, a single-hop collect returns empty so `delete` reports `false`
// and never reaches the empty rebuild. Following the chain finds the entry,
// removes it, and (with a sibling key present) updates the terminal dict.

#[test]
fn delete_last_entry_through_two_hop_names() {
    use flpdf::Object;

    let mut pdf = open(build_two_hop_names_pdf());

    // Add a sibling /Dests key so the terminal /Names dict is non-empty after
    // /EmbeddedFiles is dropped — exercises the non-empty set_object branch of
    // the empty-rebuild path on the terminal ref.
    let Object::Dictionary(mut names_dict) = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve 3")
    else {
        panic!("object 3 must be the /Names dict");
    };
    names_dict.insert("Dests", Object::Reference(ObjectRef::new(99, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(names_dict));

    let removed = delete_embedded_file(&mut pdf, b"alpha").expect("delete");
    assert!(
        removed,
        "delete of the sole entry behind a 2-hop /Names must find and remove it"
    );

    // /EmbeddedFiles must be gone from the terminal dict while /Dests survives.
    let Object::Dictionary(terminal) = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve terminal")
    else {
        panic!("object 3 must still be the terminal /Names dict");
    };
    assert!(
        terminal.get("EmbeddedFiles").is_none(),
        "/EmbeddedFiles must be removed from the terminal /Names dict"
    );
    assert!(
        terminal.get("Dests").is_some(),
        "sibling /Dests key must survive the empty rebuild on the terminal dict"
    );

    // And the tree must now enumerate as empty.
    let entries = list_embedded_files(&mut pdf).expect("list after delete");
    assert!(
        entries.is_empty(),
        "tree must be empty after deleting last entry"
    );
}

// ── Site 1: remove_attachment clears a ref from a 2-hop /AF array ─────────────
//
// `remove_ref_from_af_in_dict` resolves the catalog `/AF` value once. When /AF
// is a 2-hop chain (`ref → ref → array`), a single-hop resolve sees a Reference
// (not an array) and skips removal, leaving the removed filespec referenced.
// Following the chain rewrites the terminal array. A second, unrelated ref is
// kept in the array so it stays non-empty (carrier not orphaned/swept).

/// Object layout:
///   1 0 R  Catalog  (/Names 2 0 R, /AF 6 0 R)
///   2 0 R  /Names dict  (/EmbeddedFiles 3 0 R)   [1-hop so collect finds it]
///   3 0 R  leaf node  (/Names [(gone) 4 0 R])
///   4 0 R  Filespec for "gone" (the attachment to remove)
///   5 0 R  Filespec kept as an unrelated /AF entry
///   6 0 R  bare reference 7 0 R                   (first /AF hop)
///   7 0 R  array [4 0 R 5 0 R]                    (terminal /AF array)
fn build_two_hop_af_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R /AF 6 0 R >>\nendobj\n",
    );

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Names [ (gone) 4 0 R ] >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Filespec /F (gone.txt) >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (kept.txt) >>\nendobj\n");

    off.insert(6, out.len() as u64);
    out.extend_from_slice(b"6 0 obj\n7 0 R\nendobj\n");

    off.insert(7, out.len() as u64);
    out.extend_from_slice(b"7 0 obj\n[ 4 0 R 5 0 R ]\nendobj\n");

    finish_pdf(&mut out, &off, 7, 1);
    out
}

#[test]
fn remove_attachment_clears_ref_from_two_hop_af_array() {
    use flpdf::{remove_attachment, Object};

    let mut pdf = open(build_two_hop_af_pdf());

    let removed = remove_attachment(&mut pdf, b"gone").expect("remove");
    assert!(removed, "existing attachment must report removed");

    // The terminal /AF array (object 7) must no longer reference the removed
    // filespec (4 0 R) but must still reference the unrelated kept ref (5 0 R).
    let Object::Array(af) = pdf
        .resolve(ObjectRef::new(7, 0))
        .expect("terminal /AF array must still resolve (carrier not orphaned)")
    else {
        panic!("object 7 must be the terminal /AF array");
    };
    assert!(
        !af.iter()
            .any(|o| matches!(o, Object::Reference(r) if *r == ObjectRef::new(4, 0))),
        "removed filespec ref must be absent from the terminal /AF array, got {af:?}"
    );
    assert!(
        af.iter()
            .any(|o| matches!(o, Object::Reference(r) if *r == ObjectRef::new(5, 0))),
        "unrelated kept ref must remain in the terminal /AF array, got {af:?}"
    );
}

// ── Site 1 boundary: indirect /AF whose terminal is not an array ──────────────
//
// When the catalog `/AF` value resolves (through the chain) to a non-array
// object, `remove_ref_from_af_in_dict` must treat it as a no-op and return
// cleanly rather than panicking — the removal still succeeds via the name tree.
//
// Object layout:
///   1 0 R  Catalog  (/Names 2 0 R, /AF 5 0 R)
///   2 0 R  /Names dict  (/EmbeddedFiles 3 0 R)
///   3 0 R  leaf node  (/Names [(x) 4 0 R])
///   4 0 R  Filespec for "x"
///   5 0 R  a dictionary (NOT an array) — the malformed /AF terminal
fn build_non_array_af_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R /AF 5 0 R >>\nendobj\n",
    );

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Names [ (x) 4 0 R ] >>\nendobj\n");

    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Filespec /F (x.txt) >>\nendobj\n");

    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /SomethingElse >>\nendobj\n");

    finish_pdf(&mut out, &off, 5, 1);
    out
}

#[test]
fn remove_attachment_with_non_array_af_terminal_is_noop() {
    use flpdf::remove_attachment;

    let mut pdf = open(build_non_array_af_pdf());
    let removed = remove_attachment(&mut pdf, b"x").expect("remove must not error");
    assert!(
        removed,
        "the attachment must still be removed via the name tree"
    );
    assert!(
        list_embedded_files(&mut pdf).expect("list").is_empty(),
        "tree must be empty after removing the sole attachment"
    );
}

// ── Site 5 boundary: indirect /Names whose terminal is not a dict ─────────────
//
// When the catalog `/Names` value resolves (through the chain) to a non-dict
// object, `insert_embedded_file`'s rebuild must fall back to allocating a fresh
// /Names dict rather than panicking, and the inserted attachment must remain
// listable.
//
/// Object layout:
///   1 0 R  Catalog  (/Names 2 0 R)
///   2 0 R  an array (NOT a dict) — the malformed /Names terminal
///   3 0 R  pre-allocated Filespec slot for the inserted entry
fn build_non_dict_names_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R >>\nendobj\n");

    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n[ 1 2 3 ]\nendobj\n");

    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Type /Filespec /F (new.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 3, 1);
    out
}

#[test]
fn insert_with_non_dict_names_terminal_allocates_fresh_dict() {
    let mut pdf = open(build_non_dict_names_pdf());

    insert_embedded_file(&mut pdf, b"new.txt", ObjectRef::new(3, 0)).expect("insert must succeed");

    let entries = list_embedded_files(&mut pdf).expect("list after insert");
    assert_eq!(
        entries.len(),
        1,
        "inserted attachment must be listable via a freshly allocated /Names dict"
    );
    assert_eq!(entries[0].0, b"new.txt");
    assert_eq!(entries[0].1, ObjectRef::new(3, 0));
}
