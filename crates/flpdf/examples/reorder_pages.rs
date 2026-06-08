//! Reorder a document's pages (here: reverse them) and write the result.
//!
//! Run with: `cargo run --example reorder_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{pages::page_refs, rebuild_page_tree, ObjectRef, PageObjectHelper, Pdf, WriteOptions};

/// Read each page's MediaBox width (`urx - llx`, rounded to `i64`) in document
/// order. The shared-font fixture assigns distinct widths so order is observable.
fn page_widths<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    let refs: Vec<ObjectRef> = page_refs(pdf)?;
    let mut widths = Vec::with_capacity(refs.len());
    for page_ref in refs {
        let mut helper = PageObjectHelper::new(page_ref, pdf);
        let mb = helper.media_box()?.ok_or("page has no MediaBox")?;
        widths.push((mb.urx - mb.llx).round() as i64);
    }
    Ok(widths)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("reorder-src", &common::build_shared_font_pdf(3))?;
    let out_path = common::temp_path("reorder-out");

    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Observe the original page order via each page's distinct MediaBox width.
    let original_widths = page_widths(&mut pdf)?; // [101, 102, 103]
    let mut expected_reversed = original_widths.clone();
    expected_reversed.reverse(); // [103, 102, 101]

    // Reverse the page refs and rebuild the page tree in the new order.
    let mut reversed = page_refs(&mut pdf)?;
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

    // Re-open and prove the reversal happened by reading the page widths again.
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let new_widths = page_widths(&mut out_pdf)?;
    assert_eq!(
        new_widths, expected_reversed,
        "expected reversed page order {expected_reversed:?}, got {new_widths:?}"
    );
    println!(
        "reorder_pages: {} pages, order {:?} -> {:?}",
        new_widths.len(),
        original_widths,
        new_widths
    );

    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
