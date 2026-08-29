//! Splice pages from source A into target B at a given index.
//!
//! Run with: `cargo run --example splice_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::{pages::page_refs, splice_pages, Pdf, PdfWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Source A: 3 pages. Target B: 2 pages. Insert A's pages into B at index 1.
    let a_path = common::write_temp("splice-a", &common::build_shared_font_pdf(3))?;
    let b_path = common::write_temp("splice-b", &common::build_shared_font_pdf(2))?;
    let out_path = common::temp_path("splice-out");

    let mut a = Pdf::open(BufReader::new(File::open(&a_path)?))?;
    let mut b = Pdf::open(BufReader::new(File::open(&b_path)?))?;

    // Copy A's pages through the destination's persistent foreign-object map.
    let a_pages = page_refs(&mut a)?;
    let mut copied = Vec::with_capacity(a_pages.len());
    for &page_ref in &a_pages {
        let source_page = a.get_object_handle(page_ref);
        let copied_page = b
            .copy_foreign_object(&source_page)?
            .object_ref()
            .ok_or("copyForeignObject did not return an indirect page")?;
        copied.push(copied_page);
    }

    // Insert the copied pages at index 1 (remove nothing).
    let n = 1usize;
    splice_pages(&mut b, n..n, &copied)?;

    // The canonical writer emits one fresh qpdf-style document.
    let mut writer = PdfWriter::new(&mut b);
    writer.set_output_file(&out_path)?;
    writer.write()?;

    // Verify: B grew from 2 to 5 pages (2 original + 3 inserted).
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let count = page_refs(&mut out_pdf)?.len();
    assert_eq!(count, 5, "expected 5 pages after splice, got {count}");
    println!("splice_pages: inserted 3 pages at index {n} -> output has {count} pages");

    // Close the open file handles before deleting: on Windows, removing a file
    // that is still open by the process fails with a permission error.
    drop(a);
    drop(b);
    drop(out_pdf);
    for p in [&a_path, &b_path, &out_path] {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}
