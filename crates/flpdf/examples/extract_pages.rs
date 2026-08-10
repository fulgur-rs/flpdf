//! Extract pages 1, 3 and 5 from a document into a new file.
//!
//! Run with: `cargo run --example extract_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::{pages::page_refs, rebuild_page_tree, ObjectRef, PagePlan, Pdf, PdfWriter};

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

    // The canonical writer emits one fresh PDF and drops unreachable objects.
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(&out_path)?;
    writer.write()?;

    // Re-open the output and verify it has exactly 3 pages.
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let count = page_refs(&mut out_pdf)?.len();
    assert_eq!(count, 3, "expected 3 extracted pages, got {count}");
    println!("extract_pages: extracted pages 1,3,5 -> output has {count} pages");

    // Close the open file handles before deleting: on Windows, removing a file
    // that is still open by the process fails with a permission error.
    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
