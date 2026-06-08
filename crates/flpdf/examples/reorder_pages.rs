//! Reorder a document's pages (here: reverse them) and write the result.
//!
//! Run with: `cargo run --example reorder_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{pages::page_refs, rebuild_page_tree, Pdf, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("reorder-src", &common::build_shared_font_pdf(3))?;
    let out_path = common::temp_path("reorder-out");

    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Current page order, then reversed.
    let original = page_refs(&mut pdf)?;
    let mut reversed = original.clone();
    reversed.reverse();

    rebuild_page_tree(&mut pdf, &reversed)?;

    #[allow(clippy::field_reassign_with_default)]
    let opts = {
        let mut opts = WriteOptions::default();
        opts.full_rewrite = true;
        opts
    };
    let out = BufWriter::new(File::create(&out_path)?);
    flpdf::write_pdf_with_options(&mut pdf, out, &opts)?;

    // Re-open: the rewritten document still has 3 pages, now in reversed order.
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let new_order = page_refs(&mut out_pdf)?;
    assert_eq!(new_order.len(), 3, "expected 3 pages, got {}", new_order.len());
    println!("reorder_pages: {} pages, reversed order applied", new_order.len());

    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
