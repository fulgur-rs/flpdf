//! Integration tests for [`flpdf::OutlineDocumentHelper`].

use flpdf::{ObjectRef, Pdf};
use std::collections::BTreeMap;
use std::io::Cursor;

/// Build a minimal cross-reffed PDF from `(objnum, body)` pairs.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=max {
        match offsets.get(&n) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Catalog + pages + a two-level outline:
///   root(4) -> First A(5)
///   A(5)    -> First A1(6); A1 has dest [3 0 R /Fit]
///   A(5)    -> Next  B(7);  B has /Count 2
fn outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 7 0 R /Count 2 >>"),
            (5, "<< /Title (A) /Parent 4 0 R /First 6 0 R /Last 6 0 R /Next 7 0 R /Count 1 >>"),
            (6, "<< /Title (A1) /Parent 5 0 R /Dest [3 0 R /Fit] >>"),
            (7, "<< /Title (B) /Parent 4 0 R /Prev 5 0 R /Count 2 >>"),
        ],
        1,
    )
}

fn no_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// Catalog with an `/Outlines` dict present but with no `/First` child.
fn outline_present_but_empty_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /Count 0 >>"),
        ],
        1,
    )
}

#[test]
fn has_outlines_true_when_present() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    assert!(pdf.outline().has_outlines().unwrap());
}

#[test]
fn has_outlines_false_when_absent() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(!pdf.outline().has_outlines().unwrap());
}

#[test]
fn has_outlines_false_when_outline_dict_has_no_first() {
    let mut pdf = Pdf::open(Cursor::new(outline_present_but_empty_pdf())).unwrap();
    assert!(!pdf.outline().has_outlines().unwrap());
}

#[test]
fn get_root_materializes_tree_with_titles_counts_parents() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();

    // Two top-level nodes: A, B.
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].title, "A");
    assert_eq!(roots[0].depth, 0);
    assert_eq!(roots[0].parent, None); // top-level: /Outlines dict is not an item -> None (qpdf getParent)
    assert_eq!(roots[0].count, 1);
    assert_eq!(roots[1].title, "B");
    assert_eq!(roots[1].count, 2);

    // A has one child A1.
    assert_eq!(roots[0].children.len(), 1);
    let a1 = &roots[0].children[0];
    assert_eq!(a1.title, "A1");
    assert_eq!(a1.depth, 1);
    assert_eq!(a1.parent, Some(ObjectRef::new(5, 0)));
    assert_eq!(a1.count, 0); // /Count absent -> 0 (qpdf)
    assert_eq!(a1.object_ref, ObjectRef::new(6, 0));
}

#[test]
fn get_root_empty_when_no_outline() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(pdf.outline().get_root().unwrap().is_empty());
}
