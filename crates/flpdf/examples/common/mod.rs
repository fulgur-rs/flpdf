#![allow(dead_code)]
//! Shared helpers for the page-op examples.
//!
//! Included by each example via `#[path = "common/mod.rs"] mod common;`.
//! Every example uses only a subset of these helpers, so `dead_code` is allowed.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Append a classic `xref` table + `trailer` + `startxref`/`%%EOF` for objects
/// `1..=last`. `offsets` must contain a byte offset for every object `1..=last`.
fn append_xref_trailer(out: &mut Vec<u8>, offsets: &BTreeMap<u32, u64>, last: u32) {
    let xref_start = out.len() as u64;
    let size = last + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=last {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
}

/// Build a minimal multi-page PDF whose pages all share a single `/Font` object.
///
/// Each page carries a distinct MediaBox width (`100 + 1-based page index`) so
/// examples can observe page order.
///
/// Object layout:
///   1: Catalog
///   2: Pages root
///   3: shared Font (referenced by every page's `/Resources`)
///   4 ..= 3 + `page_count`: individual Page objects
pub fn build_shared_font_pdf(page_count: u32) -> Vec<u8> {
    assert!(page_count >= 1, "need at least 1 page");

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let first_page = 4u32;
    let last_page = 3 + page_count;

    // 1: Catalog
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // 2: Pages root
    offsets.insert(2, out.len() as u64);
    let kids: String = (first_page..=last_page)
        .map(|i| format!("{i} 0 R "))
        .collect();
    out.extend_from_slice(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {page_count} >>\nendobj\n")
            .as_bytes(),
    );

    // 3: one shared Font object, referenced by all pages.
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    // 4 ..= 3+page_count: Page objects, each referencing shared font `3 0 R`.
    // Give each page a distinct MediaBox width (`100 + 1-based page index`) so
    // examples can observe page identity/order after a reorder or extract.
    for i in first_page..=last_page {
        offsets.insert(i, out.len() as u64);
        let page_index = i - first_page + 1; // 1-based page position
        let width = 100 + page_index;
        out.extend_from_slice(
            format!(
                "{i} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} 792] \
                 /Resources << /Font << /F1 3 0 R >> >> >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    append_xref_trailer(&mut out, &offsets, last_page);
    out
}

/// Build a minimal single-page PDF carrying an interactive form (`/AcroForm`)
/// with two top-level fields: a text field `FirstName` (value `Alice`) and a
/// checkbox `Agree` (value `/Off`). Each field dictionary is also its widget
/// annotation (the merged field/widget form), referenced from the page `/Annots`.
///
/// Object layout:
///   1: Catalog (`/Pages`, `/AcroForm`)
///   2: Pages root
///   3: AcroForm (`/Fields`, `/DA`)
///   4: Page (`/Annots` -> 5, 6)
///   5: text field/widget `FirstName`
///   6: checkbox field/widget `Agree`
pub fn build_acroform_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let objs: [(u32, &str); 6] = [
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 3 0 R >>"),
        (2, "<< /Type /Pages /Kids [4 0 R] /Count 1 >>"),
        (3, "<< /Fields [5 0 R 6 0 R] /DA (/Helv 0 Tf 0 g) /NeedAppearances true >>"),
        (
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R] >>",
        ),
        (
            5,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (FirstName) /V (Alice) \
             /Rect [100 700 300 720] >>",
        ),
        (
            6,
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (Agree) /V /Off \
             /Rect [100 660 120 680] >>",
        ),
    ];
    for (n, body) in objs {
        offsets.insert(n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    append_xref_trailer(&mut out, &offsets, 6);
    out
}

/// Build a minimal 2-page PDF with a document outline (`/Outlines`) two levels
/// deep: `Chapter 1` (with child `Section 1.1`) and `Chapter 2`. Each item has
/// an explicit `/Dest [page /Fit]`.
///
/// Object layout:
///   1: Catalog (`/Pages`, `/Outlines`)
///   2: Pages root (Kids 4, 5)
///   3: Outlines root (First 6, Last 7, Count 3)
///   4,5: Page objects (share font 8)
///   6: item `Chapter 1`   (Parent 3, First/Last 9, Next 7, Dest -> 4)
///   7: item `Chapter 2`   (Parent 3, Prev 6,        Dest -> 5)
///   8: shared Font
///   9: item `Section 1.1` (Parent 6,                Dest -> 4)
pub fn build_outline_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let page = "/Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                /Resources << /Font << /F1 8 0 R >> >>";
    let objs: [(u32, String); 9] = [
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [4 0 R 5 0 R] /Count 2 >>".into()),
        (3, "<< /Type /Outlines /First 6 0 R /Last 7 0 R /Count 3 >>".into()),
        (4, format!("<< {page} >>")),
        (5, format!("<< {page} >>")),
        (
            6,
            "<< /Title (Chapter 1) /Parent 3 0 R /First 9 0 R /Last 9 0 R \
             /Count 1 /Next 7 0 R /Dest [4 0 R /Fit] >>"
                .into(),
        ),
        (
            7,
            "<< /Title (Chapter 2) /Parent 3 0 R /Prev 6 0 R /Dest [5 0 R /Fit] >>".into(),
        ),
        (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
        (
            9,
            "<< /Title (Section 1.1) /Parent 6 0 R /Dest [4 0 R /Fit] >>".into(),
        ),
    ];
    for (n, body) in objs {
        offsets.insert(n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    append_xref_trailer(&mut out, &offsets, 9);
    out
}

/// Build a unique temp-file path for this example run (no file is created).
///
/// Uses the process id to avoid collisions between concurrent example runs.
pub fn temp_path(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("flpdf-ex-{}-{tag}.pdf", std::process::id()));
    path
}

/// Write `bytes` to a uniquely-named temp file and return its path.
pub fn write_temp(tag: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = temp_path(tag);
    std::fs::write(&path, bytes)?;
    Ok(path)
}
