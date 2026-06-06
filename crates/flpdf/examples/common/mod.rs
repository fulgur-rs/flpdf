#![allow(dead_code)]
//! Shared helpers for the page-op examples.
//!
//! Included by each example via `#[path = "common/mod.rs"] mod common;`.
//! Every example uses only a subset of these helpers, so `dead_code` is allowed.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Build a minimal multi-page PDF whose pages all share a single `/Font` object.
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
    for i in first_page..=last_page {
        offsets.insert(i, out.len() as u64);
        out.extend_from_slice(
            format!(
                "{i} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R >> >> >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    // xref
    let xref_start = out.len() as u64;
    let size = last_page + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n"); // object 0
    for n in 1..=last_page {
        let off = offsets[&n];
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
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
