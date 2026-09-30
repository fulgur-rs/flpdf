//! Extract the first 5 pages of a document into a new file.
//!
//! Run with: `cargo run --example extract_first_5_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::{
    rebuild_page_tree, ObjectRef, PageDocumentHelper, PageObjectHelper, PageRange, Pdf, PdfWriter,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An 8-page source document (all pages share one font object).
    let src_path = common::write_temp("first5-src", &common::build_shared_font_pdf(8))?;
    let out_path = common::temp_path("first5-out");

    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // Resolve the qpdf page-range syntax 1-5 to concrete page ObjectRefs.
    let all_pages = common::checked_page_refs(&mut pdf)?;
    let page_count = u32::try_from(all_pages.len())?;
    let selected_indices = PageRange::parse_numrange("1-5")?.resolve(page_count)?;
    let selected: Vec<ObjectRef> = selected_indices
        .into_iter()
        .map(|index| all_pages[(index - 1) as usize])
        .collect();

    rebuild_page_tree(&mut pdf, &selected)?;

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(&out_path)?;
    writer.write()?;

    // Re-open and verify these are the *first* five pages by their distinct
    // MediaBox widths (the fixture assigns width = 100 + 1-based page index, so
    // pages 1..=5 carry widths 101..=105).
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let pages = PageDocumentHelper::new(&mut out_pdf).get_all_pages()?;
    let mut widths = Vec::with_capacity(pages.len());
    for page in pages {
        let mut helper = PageObjectHelper::from_object_handle(page, &mut out_pdf);
        let mb = helper.media_box()?.ok_or("page has no MediaBox")?;
        widths.push((mb.urx - mb.llx).round() as i64);
    }
    assert_eq!(
        widths,
        vec![101, 102, 103, 104, 105],
        "expected the first five pages (widths 101..=105), got {widths:?}"
    );
    println!(
        "extract_first_5_pages: output has {} pages, widths {:?}",
        widths.len(),
        widths
    );

    drop(pdf);
    drop(out_pdf);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(())
}
