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
    delete_embedded_file, insert_embedded_file, list_embedded_files, EmbeddedFileDocumentHelper,
    EmbeddedFileStream, Error, FileSpec, ObjectHandle, ObjectRef, Pdf, Pipeline, Result,
    StreamDataProvider, LEAF_MAX,
};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::rc::Rc;

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

fn handle_array(items: Vec<ObjectHandle>) -> ObjectHandle {
    ObjectHandle::array(items)
}

fn handle_dictionary(entries: Vec<(&[u8], ObjectHandle)>) -> ObjectHandle {
    ObjectHandle::dictionary(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_vec(), value))
            .collect(),
    )
}

fn resolved_handle(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef) -> ObjectHandle {
    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve(&handle).expect("resolve canonical test object");
    handle
}

fn catalog_handle(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> ObjectHandle {
    let catalog_ref = pdf.root_ref().expect("root");
    resolved_handle(pdf, catalog_ref)
}

fn replace_catalog_key(pdf: &mut Pdf<Cursor<Vec<u8>>>, key: &[u8], value: ObjectHandle) {
    let catalog = catalog_handle(pdf);
    catalog
        .replace_key(key, value)
        .expect("replace catalog key");
    pdf.mark_object_handle_dirty(&catalog)
        .expect("mark catalog dirty");
}

fn make_indirect(pdf: &Pdf<Cursor<Vec<u8>>>, value: ObjectHandle) -> ObjectHandle {
    pdf.make_indirect_from_object_handle(value)
        .expect("make canonical indirect object")
}

fn make_filespec(pdf: &mut Pdf<Cursor<Vec<u8>>>, filename: &[u8]) -> ObjectHandle {
    let embedded_file = EmbeddedFileStream::create_ef_stream(pdf, b"payload").expect("stream");
    FileSpec::create_file_spec(pdf, filename, embedded_file).expect("filespec")
}

fn embedded_names_handle(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> ObjectHandle {
    let catalog = catalog_handle(pdf);
    let mut names = catalog.get_key(b"/Names");
    for _ in 0..8 {
        pdf.resolve(&names).expect("resolve /Names");
        if names.as_dictionary().is_some() {
            return names;
        }
        let Some(next_ref) = names.object_ref() else {
            panic!("missing /Names dictionary");
        };
        names = pdf.get_object_handle(next_ref);
    }
    panic!("/Names holder chain exceeded test bound");
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

fn build_pdfdocencoded_key_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Names 2 0 R >>\nendobj\n");
    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");
    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Names [ (\\200) 4 0 R ] >>\nendobj\n");
    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Filespec /F (bullet.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 4, 1);
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

#[test]
fn list_normalizes_pdfdocencoded_name_keys_to_utf8() {
    let mut pdf = open(build_pdfdocencoded_key_pdf());

    assert_eq!(
        list_embedded_files(&mut pdf).expect("list"),
        vec![("•".as_bytes().to_vec(), ObjectRef::new(4, 0))]
    );
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

/// Build a direct embedded-files root whose repaired direct kid reaches a
/// malformed indirect `/Names` array only when the tree is traversed.
fn build_direct_kid_with_broken_names_reference_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Names 2 0 R >>\nendobj\n");
    off.insert(2, out.len() as u64);
    out.extend_from_slice(
        b"2 0 obj\n<< /EmbeddedFiles << /Kids [ << /Names 4 0 R >> ] >> >>\nendobj\n",
    );
    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\nnull\nendobj\n");
    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Broken [ >>\nendobj\n");

    finish_pdf(&mut out, &off, 4, 1);
    out
}

/// Build an indirect embedded-files root whose repaired direct kid reaches a
/// malformed indirect `/Names` array only when the tree is traversed.
fn build_indirect_root_with_broken_names_reference_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Names 2 0 R >>\nendobj\n");
    off.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n");
    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\n<< /Kids [ << /Names 4 0 R >> ] >>\nendobj\n");
    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Broken [ >>\nendobj\n");

    finish_pdf(&mut out, &off, 4, 1);
    out
}

/// Build a direct embedded-files root whose second direct kid fails only after
/// the cursor has enumerated a valid first kid.
fn build_direct_kid_with_broken_names_reference_after_first_entry_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Names 2 0 R >>\nendobj\n");
    off.insert(2, out.len() as u64);
    out.extend_from_slice(
        b"2 0 obj\n<< /EmbeddedFiles << /Kids [ << /Names [ (valid) 5 0 R ] >> << /Names 4 0 R >> ] >> >>\nendobj\n",
    );
    off.insert(3, out.len() as u64);
    out.extend_from_slice(b"3 0 obj\nnull\nendobj\n");
    off.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Broken [ >>\nendobj\n");
    off.insert(5, out.len() as u64);
    out.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (valid.txt) >>\nendobj\n");

    finish_pdf(&mut out, &off, 5, 1);
    out
}

#[test]
fn no_names_key_returns_empty() {
    let mut pdf = open(build_no_names_pdf());
    let entries = list_embedded_files(&mut pdf).expect("list_embedded_files");
    assert!(entries.is_empty(), "expected empty list when /Names absent");
}

fn build_no_root_pdf() -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let object_offset = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \n\
             trailer\n<< /Size 2 >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

fn build_non_dict_root_pdf() -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n[1 2 3]\nendobj\n");
    finish_pdf(&mut out, &offsets, 1, 1);
    out
}

#[test]
fn writer_handles_missing_and_malformed_catalog_paths() {
    let mut no_root = open(build_no_root_pdf());
    insert_embedded_file(&mut no_root, b"x", ObjectRef::new(1, 0)).expect("insert no root");
    assert!(!delete_embedded_file(&mut no_root, b"x").expect("delete no root"));

    let mut non_dict_root = open(build_non_dict_root_pdf());
    insert_embedded_file(&mut non_dict_root, b"x", ObjectRef::new(1, 0))
        .expect("insert non-dict root");
    assert!(!delete_embedded_file(&mut non_dict_root, b"x").expect("delete non-dict root"));

    let mut non_dict_names = open(build_non_dict_names_pdf());
    assert!(!delete_embedded_file(&mut non_dict_names, b"x").expect("non-dict Names"));

    let mut no_names = open(build_no_names_pdf());
    assert!(!delete_embedded_file(&mut no_names, b"x").expect("no Names"));

    let mut no_embedded_files = open(build_no_embedded_files_pdf());
    assert!(!delete_embedded_file(&mut no_embedded_files, b"x").expect("no EmbeddedFiles"));
}

#[test]
fn insert_does_not_allocate_for_direct_names_dictionary() {
    let mut pdf = open(build_single_level_pdf());
    let catalog_ref = pdf.root_ref().expect("catalog");
    let catalog: ObjectHandle = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog).expect("resolve catalog");
    let names_ref = catalog.get_key(b"/Names").object_ref().expect("Names ref");
    let names = pdf.get_object_handle(names_ref);
    pdf.resolve(&names).expect("resolve Names");
    catalog
        .replace_key(b"/Names", names.shallow_copy().expect("copy Names"))
        .expect("make Names direct");
    pdf.mark_object_handle_dirty(&catalog)
        .expect("mark catalog dirty");
    // Register the highest possible identity so any accidental allocation
    // still fails, without using the legacy Object cache setter.
    let _max_handle = pdf.get_object_handle(ObjectRef::new(u32::MAX, 0));

    assert!(insert_embedded_file(&mut pdf, b"alpha", ObjectRef::new(4, 0)).is_ok());
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

#[test]
fn inserting_into_direct_embedded_files_root_preserves_it() {
    let mut pdf = open(build_inline_ef_pdf());

    insert_embedded_file(&mut pdf, b"other", ObjectRef::new(2, 0)).expect("insert");

    let catalog_ref = pdf.root_ref().expect("catalog");
    let catalog: ObjectHandle = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog).expect("resolve catalog");
    let names = catalog.get_key(b"/Names");
    assert!(names.is_direct(), "direct Names must remain direct");
    let embedded_files = names.get_key(b"/EmbeddedFiles");
    assert!(
        embedded_files.is_direct(),
        "direct EmbeddedFiles root must remain direct"
    );
    assert!(
        embedded_files.as_dictionary().is_some(),
        "direct EmbeddedFiles root must remain a dictionary"
    );
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

#[test]
fn writer_second_insert_mutates_existing_tree_root() {
    let mut pdf = open(build_empty_pdf());
    insert_embedded_file(&mut pdf, b"alpha.txt", ObjectRef::new(3, 0)).expect("insert alpha");

    let first_root = {
        embedded_names_handle(&mut pdf)
            .get_key(b"/EmbeddedFiles")
            .object_ref()
            .expect("tree root")
    };

    insert_embedded_file(&mut pdf, b"beta.txt", ObjectRef::new(4, 0)).expect("insert beta");

    let names = embedded_names_handle(&mut pdf);
    assert_eq!(
        names.get_key(b"/EmbeddedFiles").object_ref(),
        Some(first_root),
        "qpdf helper insertion mutates the existing root instead of rebuilding it"
    );
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

#[test]
fn writer_delete_mutates_existing_nonempty_tree_root() {
    let mut pdf = open(build_empty_pdf());
    insert_embedded_file(&mut pdf, b"keep.txt", ObjectRef::new(3, 0)).expect("insert keep");
    insert_embedded_file(&mut pdf, b"remove.txt", ObjectRef::new(4, 0)).expect("insert remove");

    let root_before = {
        embedded_names_handle(&mut pdf)
            .get_key(b"/EmbeddedFiles")
            .object_ref()
            .expect("tree root")
    };

    assert!(delete_embedded_file(&mut pdf, b"remove.txt").expect("delete"));

    let names = embedded_names_handle(&mut pdf);
    assert_eq!(
        names.get_key(b"/EmbeddedFiles").object_ref(),
        Some(root_before)
    );
    assert_eq!(
        list_embedded_files(&mut pdf).expect("list"),
        vec![(b"keep.txt".to_vec(), ObjectRef::new(3, 0))]
    );
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
    // `/Names` is direct when qpdf initializes it; `/EmbeddedFiles` is the
    // indirect root created by `QPDFNameTreeObjectHelper::newEmpty`.
    let names = embedded_names_handle(&mut pdf);
    let ef_root_ref = names
        .get_key(b"/EmbeddedFiles")
        .object_ref()
        .expect("/EmbeddedFiles");
    let ef_root = resolved_handle(&mut pdf, ef_root_ref);

    // The root must have /Kids (not a flat /Names leaf) because count > LEAF_MAX.
    assert!(
        ef_root.get_key(b"/Kids").as_array().is_some(),
        "tree root with {count} entries must have /Kids"
    );
    assert!(
        ef_root.get_key(b"/Names").is_null(),
        "tree root with /Kids must not also have /Names"
    );

    // Verify /Limits on a leaf child.
    let kids = ef_root.get_key(b"/Kids").as_array().expect("/Kids array");
    let first_leaf_ref = kids[0].object_ref().expect("first kid reference");
    let first_leaf = resolved_handle(&mut pdf, first_leaf_ref);
    assert!(
        first_leaf.get_key(b"/Limits").as_array().is_some(),
        "leaf node must have /Limits"
    );
    // /Limits must be a two-element array of strings.
    let limits = first_leaf
        .get_key(b"/Limits")
        .as_array()
        .expect("/Limits array");
    assert_eq!(limits.len(), 2, "/Limits must have exactly 2 elements");
    assert!(
        limits[0].as_string().is_some(),
        "/Limits[0] must be a string"
    );
    assert!(
        limits[1].as_string().is_some(),
        "/Limits[1] must be a string"
    );
    // First limit ≤ last limit within the leaf.
    let first_lim = limits[0].as_string().expect("first limit string");
    let last_lim = limits[1].as_string().expect("last limit string");
    assert!(
        first_lim <= last_lim,
        "leaf /Limits[0] must be ≤ /Limits[1]"
    );
}

// ── W7b: single insert → single-node root omits /Limits ──────────────────────

#[test]
fn writer_single_insert_root_omits_limits() {
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

    let names = embedded_names_handle(&mut pdf);
    let ef_root_ref = names
        .get_key(b"/EmbeddedFiles")
        .object_ref()
        .expect("/EmbeddedFiles");
    let ef_root = resolved_handle(&mut pdf, ef_root_ref);

    // Structural conformance (ISO 32000-2 §7.9.6; qpdf): a single-node root is a
    // /Names leaf-root that omits /Limits and is not a /Kids root.
    assert!(
        ef_root.get_key(b"/Names").as_array().is_some(),
        "single-node root is a /Names leaf-root"
    );
    assert!(
        ef_root.get_key(b"/Limits").is_null(),
        "root omits /Limits (ISO 32000-2 §7.9.6; qpdf)"
    );
    assert!(
        ef_root.get_key(b"/Kids").is_null(),
        "single node is not a /Kids root"
    );

    // Substantive check: the /Names array actually names the single attachment,
    // confirming a populated single-node tree rather than an empty one.
    let pairs = ef_root.get_key(b"/Names").as_array().expect("/Names array");
    assert_eq!(
        pairs.len(),
        2,
        "single-entry leaf /Names must be one [key, value] pair"
    );
    assert!(
        pairs[0].as_string().as_deref() == Some(b"alpha.txt"),
        "/Names[0] must be the attachment key, got: {:?}",
        pairs[0].unparse_resolved()
    );
    assert!(
        pairs[1].object_ref() == Some(fs_ref),
        "/Names[1] must reference the inserted filespec"
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
    let catalog = resolved_handle(&mut pdf, catalog_ref);
    let names_ref = catalog
        .get_key(b"/Names")
        .object_ref()
        .expect("catalog /Names");
    let names_dict = resolved_handle(&mut pdf, names_ref);
    let ef_root_ref = names_dict
        .get_key(b"/EmbeddedFiles")
        .object_ref()
        .expect("/EmbeddedFiles");

    // Gather (key, value) pairs from every leaf reachable from the root.
    let mut pairs: Vec<(Vec<u8>, ObjectHandle)> = Vec::new();
    let mut stack = vec![ef_root_ref];
    while let Some(node_ref) = stack.pop() {
        let node = resolved_handle(&mut pdf, node_ref);
        if let Some(arr) = node.get_key(b"/Names").as_array() {
            let mut it = arr.into_iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                if let Some(key) = k.as_string() {
                    pairs.push((key, v));
                }
            }
        }
        if let Some(kids) = node.get_key(b"/Kids").as_array() {
            for kid in kids {
                if let Some(r) = kid.object_ref() {
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
        added.1.object_ref(),
        Some(ObjectRef::new(5, 0)),
        "inserted value must be the reference passed to insert_embedded_file"
    );

    let direct = pairs
        .iter()
        .find(|(k, _)| k == b"direct.txt")
        .expect("pre-existing direct-dict entry must NOT be dropped on rebuild");
    let direct_dict = direct
        .1
        .as_dictionary()
        .expect("direct-dict value must stay a dictionary");
    assert_eq!(
        direct_dict
            .get(b"/F".as_slice())
            .and_then(ObjectHandle::as_string),
        Some(b"direct.txt".to_vec()),
        "direct-dict /Filespec must be preserved verbatim"
    );
}

// ── Site 1: remove_attachment retains an indirect /AF array ──────────────────
//
// qpdf follows the name-tree removal with Filespec null replacement only; it
// does not rewrite the indirect /AF array or its elements.

/// Object layout:
///   1 0 R  Catalog  (/Names 2 0 R, /AF 7 0 R)
///   2 0 R  /Names dict  (/EmbeddedFiles 3 0 R)   [1-hop so collect finds it]
///   3 0 R  leaf node  (/Names [(gone) 4 0 R])
///   4 0 R  Filespec for "gone" (the attachment to remove)
///   5 0 R  Filespec kept as an unrelated /AF entry
///   6 0 R  unrelated null object (xref filler)
///   7 0 R  array [4 0 R 5 0 R]                    (indirect /AF array)
fn build_indirect_af_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut off: BTreeMap<u32, u64> = BTreeMap::new();

    off.insert(1, out.len() as u64);
    out.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 99 0 R /Names 2 0 R /AF 7 0 R >>\nendobj\n",
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
    out.extend_from_slice(b"6 0 obj\nnull\nendobj\n");

    off.insert(7, out.len() as u64);
    out.extend_from_slice(b"7 0 obj\n[ 4 0 R 5 0 R ]\nendobj\n");

    finish_pdf(&mut out, &off, 7, 1);
    out
}

#[test]
fn remove_attachment_preserves_indirect_af_array_and_nulls_filespec() {
    use flpdf::remove_attachment;

    let mut pdf = open(build_indirect_af_pdf());

    let removed = remove_attachment(&mut pdf, b"gone").expect("remove");
    assert!(removed, "existing attachment must report removed");

    // The terminal /AF array (object 7) retains both object references, while
    // the removed Filespec object is replaced with null.
    let af = resolved_handle(&mut pdf, ObjectRef::new(7, 0))
        .as_array()
        .expect("indirect /AF array must still resolve");
    assert!(af
        .iter()
        .any(|value| value.object_ref() == Some(ObjectRef::new(4, 0))));
    assert!(
        af.iter()
            .any(|value| value.object_ref() == Some(ObjectRef::new(5, 0))),
        "unrelated kept ref must remain in the terminal /AF array"
    );
    assert!(
        resolved_handle(&mut pdf, ObjectRef::new(4, 0)).is_null(),
        "qpdf replaces the removed Filespec with null"
    );
}

// ── Site 1 boundary: indirect /AF whose terminal is not an array ──────────────
//
// When the catalog `/AF` value resolves (through the chain) to a non-array
// object, the canonical helper must treat it as a no-op and return
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

#[test]
fn helper_reads_named_filespecs_as_handles() {
    let mut pdf = open(build_single_level_pdf());
    let files = pdf
        .embedded_files()
        .get_embedded_files()
        .expect("list handles");

    assert_eq!(
        files.keys().cloned().collect::<Vec<_>>(),
        vec![b"alpha".to_vec(), b"beta".to_vec()]
    );
    let alpha = files.get(b"alpha".as_slice()).expect("alpha").clone();
    drop(files);

    assert_eq!(
        FileSpec::new(alpha, &mut pdf)
            .expect("filespec helper")
            .get_filename()
            .expect("filename"),
        b"alpha.txt"
    );
}

#[test]
fn helper_absent_tree_has_no_entries_or_lookup() {
    let mut pdf = open(build_no_embedded_files_pdf());
    let mut helper = EmbeddedFileDocumentHelper::new(&mut pdf);

    assert!(!helper.has_embedded_files().expect("has"));
    assert!(helper.get_embedded_files().expect("list").is_empty());
    assert!(helper
        .get_embedded_file(b"missing")
        .expect("lookup")
        .is_none());
}

#[test]
fn helper_treats_missing_or_malformed_catalog_paths_as_absent() {
    let mut no_root = open(build_no_root_pdf());
    assert!(!no_root
        .embedded_files()
        .has_embedded_files()
        .expect("missing root"));

    let mut non_dict_catalog = open(build_non_dict_root_pdf());
    let filespec = make_filespec(&mut non_dict_catalog, b"ignored.txt");
    non_dict_catalog
        .embedded_files()
        .replace_embedded_file(b"ignored", filespec)
        .expect("non-dictionary catalog is a qpdf no-op");
    assert!(!non_dict_catalog
        .embedded_files()
        .has_embedded_files()
        .expect("non-dict catalog"));

    let mut non_dict_names = open(build_non_dict_names_pdf());
    assert!(!non_dict_names
        .embedded_files()
        .has_embedded_files()
        .expect("non-dict names"));

    let mut non_dict_embedded_files = open(build_no_embedded_files_pdf());
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", ObjectHandle::integer(7))]);
    replace_catalog_key(&mut non_dict_embedded_files, b"/Names", names);
    assert!(!non_dict_embedded_files
        .embedded_files()
        .has_embedded_files()
        .expect("non-dict embedded files"));
}

#[test]
fn helper_replace_rebuilds_non_dictionary_names_and_embedded_files_paths() {
    let mut non_dict_names = open(build_non_dict_names_pdf());
    let names_filespec = make_filespec(&mut non_dict_names, b"names.txt");
    non_dict_names
        .embedded_files()
        .replace_embedded_file(b"names", names_filespec.clone())
        .expect("non-dictionary /Names terminal is rebuilt");
    assert!(non_dict_names
        .embedded_files()
        .get_embedded_file(b"names")
        .expect("rebuilt /Names lookup")
        .expect("rebuilt /Names entry")
        .is_same_object_as(&names_filespec));

    let mut non_dict_embedded_files = open(build_no_embedded_files_pdf());
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", ObjectHandle::integer(7))]);
    replace_catalog_key(&mut non_dict_embedded_files, b"/Names", names);

    let embedded_filespec = make_filespec(&mut non_dict_embedded_files, b"embedded.txt");
    non_dict_embedded_files
        .embedded_files()
        .replace_embedded_file(b"embedded", embedded_filespec.clone())
        .expect("non-dictionary /EmbeddedFiles terminal is rebuilt");
    assert!(non_dict_embedded_files
        .embedded_files()
        .get_embedded_file(b"embedded")
        .expect("rebuilt /EmbeddedFiles lookup")
        .expect("rebuilt /EmbeddedFiles entry")
        .is_same_object_as(&embedded_filespec));
}

#[test]
fn helper_replace_creates_and_replaces_name_tree_entry() {
    let mut pdf = open(build_no_names_pdf());
    let first = make_filespec(&mut pdf, b"first.txt");
    let second = make_filespec(&mut pdf, b"second.txt");

    let mut helper = pdf.embedded_files();
    helper
        .replace_embedded_file(b"entry", first)
        .expect("insert");
    helper
        .replace_embedded_file(b"entry", second.clone())
        .expect("replace");

    assert!(helper.has_embedded_files().expect("has"));
    assert!(helper
        .get_embedded_file(b"entry")
        .expect("lookup")
        .expect("entry")
        .is_same_object_as(&second));
}

#[test]
fn helper_lookup_returns_none_for_missing_key_in_existing_tree() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"present.txt");
    pdf.embedded_files()
        .replace_embedded_file(b"present", filespec)
        .expect("insert");

    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"missing")
        .expect("lookup")
        .is_none());
}

#[test]
fn helper_listing_rejects_a_first_non_string_name_tree_key() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"valid.txt");
    pdf.resolve(&filespec).expect("resolve filespec");
    let tree = handle_dictionary(vec![(
        b"/Names",
        handle_array(vec![
            ObjectHandle::name(b"not-a-string".to_vec()),
            filespec.shallow_copy().expect("copy filespec"),
            ObjectHandle::string(b"valid".to_vec()),
            filespec.shallow_copy().expect("copy filespec"),
        ]),
    )]);
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", tree)]);
    replace_catalog_key(&mut pdf, b"/Names", names);

    assert!(matches!(
        pdf.embedded_files().get_embedded_files(),
        Err(Error::Internal(message))
            if message == "attempt made to dereference an invalid name/number tree iterator"
    ));
}

#[test]
fn helper_listing_skips_a_later_non_string_name_tree_key() {
    let mut pdf = open(build_no_names_pdf());
    let first = make_filespec(&mut pdf, b"first.txt");
    let last = make_filespec(&mut pdf, b"last.txt");
    pdf.resolve(&first).expect("resolve first filespec");
    pdf.resolve(&last).expect("resolve last filespec");
    let tree = handle_dictionary(vec![(
        b"/Names",
        handle_array(vec![
            ObjectHandle::string(b"a".to_vec()),
            first.shallow_copy().expect("copy first filespec"),
            ObjectHandle::name(b"not-a-string".to_vec()),
            ObjectHandle::null(),
            ObjectHandle::string(b"c".to_vec()),
            last.shallow_copy().expect("copy last filespec"),
        ]),
    )]);
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", tree)]);
    replace_catalog_key(&mut pdf, b"/Names", names);

    let entries = pdf
        .embedded_files()
        .get_embedded_files()
        .expect("qpdf skips invalid keys after the initial item");
    assert_eq!(entries.keys().cloned().collect::<Vec<_>>(), [b"a", b"c"]);
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|entry| entry.message.contains("item 2 has the wrong type")));
}

#[test]
fn helper_replace_keeps_direct_names_dictionary_direct() {
    let mut pdf = open(build_no_embedded_files_pdf());
    let filespec = make_filespec(&mut pdf, b"direct-names.txt");
    let catalog_ref = pdf.root_ref().expect("root");
    let catalog: ObjectHandle = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog).expect("resolve catalog");
    let names = ObjectHandle::dictionary(vec![(
        b"/Dests".to_vec(),
        ObjectHandle::dictionary(Vec::new()),
    )]);
    catalog
        .replace_key(b"/Names", names)
        .expect("install direct Names");
    pdf.mark_object_handle_dirty(&catalog)
        .expect("mark catalog dirty");

    pdf.embedded_files()
        .replace_embedded_file(b"direct", filespec)
        .expect("replace");

    let catalog: ObjectHandle = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog).expect("resolve catalog");
    let names = catalog.get_key(b"/Names");
    assert!(names.is_direct(), "direct Names must remain direct");
    assert!(
        names.as_dictionary().is_some(),
        "direct Names must remain a dictionary"
    );
}

#[test]
fn helper_replace_rejects_foreign_indirect_filespec_without_mutation() {
    let mut pdf = open(build_no_names_pdf());
    let mut foreign_pdf = open(build_no_names_pdf());
    let foreign = make_filespec(&mut foreign_pdf, b"foreign.txt");

    assert!(pdf
        .embedded_files()
        .replace_embedded_file(b"foreign", foreign)
        .is_err());
    assert!(list_embedded_files(&mut pdf).expect("list").is_empty());
}

#[test]
fn helper_replace_rejects_foreign_direct_filespec_without_mutation() {
    let mut source = open(build_no_names_pdf());
    let owner = make_indirect(
        &source,
        handle_dictionary(vec![(
            b"/FS",
            handle_dictionary(vec![(b"/F", ObjectHandle::string(b"foreign.txt".to_vec()))]),
        )]),
    );
    source.resolve(&owner).expect("resolve owner");
    let foreign = owner.get_key(b"/FS");

    let mut destination = open(build_no_names_pdf());
    assert!(destination
        .embedded_files()
        .replace_embedded_file(b"foreign", foreign)
        .is_err());
    assert!(destination
        .embedded_files()
        .get_embedded_files()
        .expect("list")
        .is_empty());
}

#[test]
fn helper_returns_a_live_direct_filespec_handle() {
    let mut pdf = open(build_no_names_pdf());
    let direct_filespec =
        handle_dictionary(vec![(b"/F", ObjectHandle::string(b"direct.txt".to_vec()))]);
    let tree = handle_dictionary(vec![(
        b"/Names",
        handle_array(vec![
            ObjectHandle::string(b"direct".to_vec()),
            direct_filespec,
        ]),
    )]);
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", tree)]);
    replace_catalog_key(&mut pdf, b"/Names", names);

    let handle = pdf
        .embedded_files()
        .get_embedded_file(b"direct")
        .expect("get")
        .expect("direct filespec");
    FileSpec::new(handle, &mut pdf)
        .expect("filespec")
        .set_description(b"live")
        .expect("set description");
    let handle = pdf
        .embedded_files()
        .get_embedded_file(b"direct")
        .expect("get again")
        .expect("direct filespec");
    assert_eq!(
        FileSpec::new(handle, &mut pdf)
            .expect("filespec")
            .get_description()
            .expect("description"),
        b"live"
    );
}

#[test]
fn helper_reads_a_direct_filespec_below_indirect_kids() {
    let mut pdf = open(build_no_names_pdf());
    let indirect_filespec = make_indirect(&pdf, handle_dictionary(Vec::new()));
    let direct_filespec =
        handle_dictionary(vec![(b"/F", ObjectHandle::string(b"nested.txt".to_vec()))]);
    let leaf = make_indirect(
        &pdf,
        handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![
                ObjectHandle::string(b"earlier".to_vec()),
                handle_dictionary(Vec::new()),
                ObjectHandle::string(b"nested".to_vec()),
                direct_filespec,
            ]),
        )]),
    );
    let first_leaf = make_indirect(
        &pdf,
        handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![
                ObjectHandle::string(b"indirect".to_vec()),
                indirect_filespec,
            ]),
        )]),
    );
    let root = make_indirect(
        &pdf,
        handle_dictionary(vec![(b"/Kids", handle_array(vec![first_leaf, leaf]))]),
    );
    let names = make_indirect(&pdf, handle_dictionary(vec![(b"/EmbeddedFiles", root)]));
    replace_catalog_key(&mut pdf, b"/Names", names);

    let files = pdf
        .embedded_files()
        .get_embedded_files()
        .expect("read direct filespec");
    let nested = files
        .get(b"nested".as_slice())
        .expect("nested filespec")
        .clone();
    drop(files);
    assert_eq!(
        FileSpec::new(nested, &mut pdf)
            .expect("filespec")
            .get_filename()
            .expect("filename"),
        b"nested.txt"
    );
}

#[test]
fn helper_remove_nulls_indirect_filespec_without_attachment_gc() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"remove.txt");
    let filespec_ref = filespec.object_ref().expect("indirect filespec");

    pdf.embedded_files()
        .replace_embedded_file(b"remove", filespec)
        .expect("insert");
    assert!(pdf
        .embedded_files()
        .remove_embedded_file(b"remove")
        .expect("remove"));

    assert!(resolved_handle(&mut pdf, filespec_ref).is_null());
}

#[test]
fn helper_remove_keeps_empty_embedded_files_tree() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"last.txt");
    pdf.embedded_files()
        .replace_embedded_file(b"last", filespec)
        .expect("insert");

    assert!(pdf
        .embedded_files()
        .remove_embedded_file(b"last")
        .expect("remove"));

    let names = embedded_names_handle(&mut pdf);
    assert!(
        names.get_key(b"/EmbeddedFiles").is_indirect(),
        "qpdf retains an indirect /EmbeddedFiles root after final remove"
    );
}

#[test]
fn helper_reads_repairs_direct_kid() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"kid.txt");
    let filespec_ref = filespec.object_ref().expect("indirect filespec");
    let leaf = handle_dictionary(vec![
        (
            b"/Names",
            handle_array(vec![
                ObjectHandle::string(b"kid".to_vec()),
                pdf.get_object_handle(filespec_ref),
            ]),
        ),
        (
            b"/Limits",
            handle_array(vec![
                ObjectHandle::string(b"deep".to_vec()),
                ObjectHandle::string(b"deep".to_vec()),
            ]),
        ),
    ]);
    let root = handle_dictionary(vec![(b"/Kids", handle_array(vec![leaf]))]);
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", root)]);
    replace_catalog_key(&mut pdf, b"/Names", names);

    assert_eq!(
        pdf.embedded_files()
            .get_embedded_files()
            .expect("read")
            .len(),
        1
    );

    let names = embedded_names_handle(&mut pdf);
    let root = names
        .get_key(b"/EmbeddedFiles")
        .as_dictionary()
        .expect("direct root");
    let kids = root
        .get(b"/Kids".as_slice())
        .and_then(ObjectHandle::as_array)
        .expect("kids");
    assert!(kids.first().is_some_and(ObjectHandle::is_indirect));
}

#[test]
fn helper_persists_direct_kid_repair_before_empty_result() {
    let mut pdf = open(build_direct_kid_with_broken_names_reference_pdf());

    assert!(pdf
        .embedded_files()
        .get_embedded_files()
        .expect("qpdf resolves the malformed child to a null tree node")
        .is_empty());

    let names = embedded_names_handle(&mut pdf);
    let root = names.get_key(b"/EmbeddedFiles");
    let kids = root.get_key(b"/Kids").as_array().expect("kids");
    assert!(kids.first().is_some_and(ObjectHandle::is_indirect));
}

#[test]
fn helper_persists_direct_kid_repair_before_valid_result_after_first_entry() {
    let mut pdf = open(build_direct_kid_with_broken_names_reference_after_first_entry_pdf());

    assert!(pdf
        .embedded_files()
        .get_embedded_files()
        .expect("qpdf skips the malformed second child")
        .contains_key(b"valid".as_slice()));

    let names = embedded_names_handle(&mut pdf);
    let root = names.get_key(b"/EmbeddedFiles");
    let kids = root.get_key(b"/Kids").as_array().expect("kids");
    assert!(kids.first().is_some_and(ObjectHandle::is_indirect));
    assert!(kids.get(1).is_some_and(ObjectHandle::is_direct));
}

#[test]
fn helper_lookup_persists_direct_kid_repair_before_find_none() {
    let mut pdf = open(build_direct_kid_with_broken_names_reference_pdf());

    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"missing")
        .expect("qpdf resolves the malformed child to a null tree node")
        .is_none());

    let names = embedded_names_handle(&mut pdf);
    let root = names.get_key(b"/EmbeddedFiles");
    let kids = root.get_key(b"/Kids").as_array().expect("kids");
    assert!(kids.first().is_some_and(ObjectHandle::is_indirect));
}

#[test]
fn helper_remove_persists_direct_kid_repair_before_find_false() {
    let mut pdf = open(build_direct_kid_with_broken_names_reference_pdf());

    assert!(!pdf
        .embedded_files()
        .remove_embedded_file(b"missing")
        .expect("qpdf resolves the malformed child to a null tree node"));

    let names = embedded_names_handle(&mut pdf);
    let root = names.get_key(b"/EmbeddedFiles");
    let kids = root.get_key(b"/Kids").as_array().expect("kids");
    assert!(kids.first().is_some_and(ObjectHandle::is_indirect));
}

#[test]
fn helper_remove_persists_indirect_root_repair_before_find_false() {
    let mut pdf = open(build_indirect_root_with_broken_names_reference_pdf());

    assert!(!pdf
        .embedded_files()
        .remove_embedded_file(b"missing")
        .expect("qpdf resolves the malformed child to a null tree node"));

    let root = resolved_handle(&mut pdf, ObjectRef::new(3, 0));
    let kids = root.get_key(b"/Kids").as_array().expect("kids");
    assert!(kids.first().is_some_and(ObjectHandle::is_indirect));
}

#[test]
fn helper_remove_missing_persists_direct_kid_repair() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"kid.txt");
    let filespec_ref = filespec.object_ref().expect("indirect filespec");
    let leaf = handle_dictionary(vec![
        (
            b"/Names",
            handle_array(vec![
                ObjectHandle::string(b"kid".to_vec()),
                pdf.get_object_handle(filespec_ref),
            ]),
        ),
        (
            b"/Limits",
            handle_array(vec![
                ObjectHandle::string(b"kid".to_vec()),
                ObjectHandle::string(b"kid".to_vec()),
            ]),
        ),
    ]);
    let root = handle_dictionary(vec![(b"/Kids", handle_array(vec![leaf]))]);
    let names = handle_dictionary(vec![(b"/EmbeddedFiles", root)]);
    replace_catalog_key(&mut pdf, b"/Names", names);

    assert!(!pdf
        .embedded_files()
        .remove_embedded_file(b"missing")
        .expect("absent remove"));

    let names = embedded_names_handle(&mut pdf);
    let root = names.get_key(b"/EmbeddedFiles");
    assert!(root
        .get_key(b"/Kids")
        .as_array()
        .and_then(|kids| kids.first().cloned())
        .is_some_and(|kid| kid.is_indirect()));
}

#[test]
fn helper_remove_accepts_a_tree_deeper_than_legacy_limit() {
    let mut pdf = open(build_no_names_pdf());
    let filespec = make_filespec(&mut pdf, b"deep.txt");
    let filespec_ref = filespec.object_ref().expect("indirect filespec");
    let filespec_handle = pdf.get_object_handle(filespec_ref);
    let leaf = make_indirect(
        &pdf,
        handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![
                ObjectHandle::string(b"deep".to_vec()),
                filespec_handle,
            ]),
        )]),
    );

    let mut child = leaf;
    for _ in 0..101 {
        child = make_indirect(
            &pdf,
            handle_dictionary(vec![
                (b"/Kids", handle_array(vec![child])),
                (
                    b"/Limits",
                    handle_array(vec![
                        ObjectHandle::string(b"deep".to_vec()),
                        ObjectHandle::string(b"deep".to_vec()),
                    ]),
                ),
            ]),
        );
    }

    let names = make_indirect(&pdf, handle_dictionary(vec![(b"/EmbeddedFiles", child)]));
    replace_catalog_key(&mut pdf, b"/Names", names);

    assert!(pdf
        .embedded_files()
        .remove_embedded_file(b"deep")
        .expect("qpdf has no fixed depth limit"));
}

#[test]
fn helper_remove_returns_false_for_absent_tree_and_key() {
    let mut pdf = open(build_no_names_pdf());
    assert!(!pdf
        .embedded_files()
        .remove_embedded_file(b"missing")
        .expect("absent tree"));

    let filespec = make_filespec(&mut pdf, b"present.txt");
    pdf.embedded_files()
        .replace_embedded_file(b"present", filespec)
        .expect("insert");
    assert!(!pdf
        .embedded_files()
        .remove_embedded_file(b"missing")
        .expect("absent key"));
}

#[test]
fn helper_remove_direct_filespec_does_not_null_an_indirect_object() {
    let mut pdf = open(build_no_names_pdf());
    let sentinel_handle = make_indirect(&pdf, ObjectHandle::integer(7));
    let sentinel = sentinel_handle.object_ref().expect("sentinel ref");
    let direct = ObjectHandle::dictionary(vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Filespec".to_vec())),
        (b"/F".to_vec(), ObjectHandle::string(b"direct.txt".to_vec())),
    ]);

    pdf.embedded_files()
        .replace_embedded_file(b"direct", direct)
        .expect("insert direct");
    assert!(pdf
        .embedded_files()
        .remove_embedded_file(b"direct")
        .expect("remove direct"));

    assert_eq!(resolved_handle(&mut pdf, sentinel).as_integer(), Some(7));
    assert!(pdf
        .embedded_files()
        .get_embedded_files()
        .expect("list")
        .is_empty());
}

#[test]
fn helper_replace_keeps_direct_filespec_handle_live() {
    let mut pdf = open(build_no_names_pdf());
    let direct = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Filespec".to_vec())),
        (b"F".to_vec(), ObjectHandle::string(b"direct.txt".to_vec())),
    ]);
    let retained = direct.clone();

    pdf.embedded_files()
        .replace_embedded_file(b"direct", direct)
        .expect("insert direct filespec");
    FileSpec::new(retained, &mut pdf)
        .expect("filespec")
        .set_description(b"live")
        .expect("set description");

    let inserted = pdf
        .embedded_files()
        .get_embedded_file(b"direct")
        .expect("lookup")
        .expect("inserted filespec");
    assert_eq!(
        FileSpec::new(inserted, &mut pdf)
            .expect("filespec")
            .get_description()
            .expect("description"),
        b"live"
    );
}

struct ConstantProvider(&'static [u8]);

impl StreamDataProvider for ConstantProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        pipeline.write(self.0).map_err(Error::from)?;
        pipeline.finish().map_err(Error::from)
    }
}

/// `EmbeddedFileStream::create_ef_stream_from_provider` -- qpdf's provider
/// overload of `createEFStream` (`QPDFEFStreamObjectHelper.cc:102-107`) --
/// had no caller anywhere in the crate or its tests. Exercise it directly
/// and confirm the deferred provider's bytes reach the same finalization
/// (`/Params /Size`, decoded payload) as the eager `create_ef_stream`.
#[test]
fn create_ef_stream_from_provider_finalizes_the_deferred_payload() {
    let mut pdf = open(build_no_names_pdf());

    let stream = EmbeddedFileStream::create_ef_stream_from_provider(
        &mut pdf,
        Rc::new(ConstantProvider(b"deferred payload")),
    )
    .expect("create_ef_stream_from_provider");

    let wrapper = EmbeddedFileStream::new(stream, &mut pdf).expect("wrap stream");
    assert_eq!(
        wrapper.payload().expect("decode payload"),
        b"deferred payload"
    );
    assert_eq!(wrapper.get_size().expect("computed /Params /Size"), 16);
}
