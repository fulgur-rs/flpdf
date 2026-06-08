//! Extract the first 5 pages of a document into a new file.
//!
//! Run with: `cargo run --example extract_first_5_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{pages::page_refs, rebuild_page_tree, ObjectRef, PagePlan, Pdf, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An 8-page source document (all pages share one font object).
    let src_path = common::write_temp("first5-src", &common::build_shared_font_pdf(8))?;
    let out_path = common::temp_path("first5-out");

    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Select the first five pages (1-based 1..=5).
    let plan = PagePlan::from_1based_indices(&mut pdf, &[1, 2, 3, 4, 5])?;
    let selected: Vec<ObjectRef> = plan.pages().iter().map(|p| p.page_ref).collect();

    rebuild_page_tree(&mut pdf, &selected)?;

    #[allow(clippy::field_reassign_with_default)]
    let opts = {
        let mut opts = WriteOptions::default();
        opts.full_rewrite = true;
        opts
    };
    let out = BufWriter::new(File::create(&out_path)?);
    flpdf::write_pdf_with_options(&mut pdf, out, &opts)?;

    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let count = page_refs(&mut out_pdf)?.len();
    assert_eq!(count, 5, "expected 5 pages, got {count}");
    println!("extract_first_5_pages: output has {count} pages");

    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
