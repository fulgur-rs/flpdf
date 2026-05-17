//! Integration tests for [`flpdf::embedded_files`] name-tree reader.
//!
//! All tests build minimal in-memory PDFs without touching the filesystem
//! and exercise the four acceptance scenarios:
//!   1. Single-level `/Names` leaf → ordered list.
//!   2. Multi-level `/Kids` tree → depth-first ordered list.
//!   3. `/Limits` present → still works (limits are non-destructive).
//!   4. `/EmbeddedFiles` absent → empty list, no error.
//!   5. `/Names` catalog key absent → empty list, no error.
//!   6. `/Root` absent → empty list, no error.

use flpdf::{list_embedded_files, ObjectRef, Pdf};
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
    out.extend_from_slice(
        b"3 0 obj\n<< /Names [ (alpha) 4 0 R (beta) 5 0 R ] >>\nendobj\n",
    );

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
    out.extend_from_slice(
        b"3 0 obj\n<< /Limits [(aaa) (zzz)] /Kids [ 4 0 R 5 0 R ] >>\nendobj\n",
    );

    off.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Limits [(aaa) (mmm)] /Names [ (aaa) 6 0 R (mmm) 7 0 R ] >>\nendobj\n",
    );

    off.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Limits [(zzz) (zzz)] /Names [ (zzz) 8 0 R ] >>\nendobj\n",
    );

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
