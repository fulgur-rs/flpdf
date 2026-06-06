//! Extract pages 1, 3 and 5 from a document into a new file.
//!
//! Run with: `cargo run --example extract_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{pages::page_refs, rebuild_page_tree, ObjectRef, PagePlan, Pdf, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A 5-page source document (all pages share one font object).
    let src_path = common::write_temp("extract-src", &common::build_shared_font_pdf(5))?;
    let out_path = common::temp_path("extract-out");

    // Open the source for reading.
    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Plan the 1-based selection 1, 3, 5 (resolves to concrete page ObjectRefs).
    let plan = PagePlan::from_1based_indices(&mut pdf, &[1, 3, 5])?;
    let selected: Vec<ObjectRef> = plan.pages().iter().map(|p| p.page_ref).collect();

    // Rebuild the page tree so only the selected pages remain (flattened /Pages).
    rebuild_page_tree(&mut pdf, &selected)?;

    // Write a full (non-incremental) rewrite so unreferenced objects drop out.
    // `WriteOptions` is `#[non_exhaustive]`, so it must be built from its
    // `Default` and then mutated rather than via a struct literal.
    #[allow(clippy::field_reassign_with_default)]
    let opts = {
        let mut opts = WriteOptions::default();
        opts.full_rewrite = true;
        opts
    };
    write_to(&mut pdf, &out_path, &opts)?;

    // Re-open the output and verify it has exactly 3 pages.
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let count = page_refs(&mut out_pdf)?.len();
    assert_eq!(count, 3, "expected 3 extracted pages, got {count}");
    println!("extract_pages: extracted pages 1,3,5 -> output has {count} pages");

    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}

fn write_to(
    pdf: &mut Pdf<BufReader<File>>,
    path: &std::path::Path,
    opts: &WriteOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let out = BufWriter::new(File::create(path)?);
    flpdf::write_pdf_with_options(pdf, out, opts)?;
    Ok(())
}
