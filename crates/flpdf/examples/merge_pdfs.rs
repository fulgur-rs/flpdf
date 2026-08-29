//! Merge two PDFs, preserving fonts shared between the merged-in pages.
//!
//! `Pdf::copy_foreign_object` retains one source-to-target identity map: the
//! two source pages share one font, so after copying them into the target the
//! merged output still references that font exactly once.
//!
//! Run with: `cargo run --example merge_pdfs -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::{pages::page_refs, splice_pages, ObjectHandle, ObjectRef, Pdf, PdfWriter};

/// Resolve a page's `/Resources /Font /F1` indirect reference.
///
/// The synthetic pages keep `/Resources` inline, so resolving the page and
/// then each child handle is enough; the nested dictionaries remain live
/// ObjectHandle values throughout the inspection.
fn font_ref_of_page<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page: ObjectRef,
) -> Option<ObjectRef> {
    let page_obj: ObjectHandle = pdf.get_object_handle(page);
    pdf.resolve(&page_obj).ok()?;
    let resources = page_obj.get_key(b"/Resources");
    pdf.resolve(&resources).ok()?;
    resources.as_dictionary()?;
    let fonts = resources.get_key(b"/Font");
    pdf.resolve(&fonts).ok()?;
    fonts.as_dictionary()?;
    fonts.get_key(b"/F1").object_ref()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Target A: 2 pages. Source B: 2 pages that share one font object.
    let a_path = common::write_temp("merge-a", &common::build_shared_font_pdf(2))?;
    let b_path = common::write_temp("merge-b", &common::build_shared_font_pdf(2))?;
    let out_path = common::temp_path("merge-out");

    let mut a = Pdf::open(BufReader::new(File::open(&a_path)?))?;
    let mut b = Pdf::open(BufReader::new(File::open(&b_path)?))?;

    // Copy each page through the same target-side foreign-object map so the
    // font they share is copied once (sharing preserved).
    let b_pages = page_refs(&mut b)?;
    let mut copied = Vec::with_capacity(b_pages.len());
    for &page_ref in &b_pages {
        let source_page = b.get_object_handle(page_ref);
        let copied_page = a
            .copy_foreign_object(&source_page)?
            .object_ref()
            .ok_or("copyForeignObject did not return an indirect page")?;
        copied.push(copied_page);
    }

    // Append B's copied pages at the end of A.
    let a_len = page_refs(&mut a)?.len();
    splice_pages(&mut a, a_len..a_len, &copied)?;

    // The canonical writer emits one fresh qpdf-style document.
    let mut writer = PdfWriter::new(&mut a);
    writer.set_output_file(&out_path)?;
    writer.write()?;

    // Verify on the output: merged doc has 4 pages, and the two merged-in pages
    // reference a single shared font object.
    let mut out_pdf = Pdf::open(BufReader::new(File::open(&out_path)?))?;
    let out_pages = page_refs(&mut out_pdf)?;
    assert_eq!(
        out_pages.len(),
        4,
        "expected 4 pages after merge, got {}",
        out_pages.len()
    );

    let f_last = font_ref_of_page(&mut out_pdf, out_pages[2]).expect("page 3 font");
    let f_last2 = font_ref_of_page(&mut out_pdf, out_pages[3]).expect("page 4 font");
    assert_eq!(
        f_last, f_last2,
        "merged-in pages must share one font object (got {f_last:?} vs {f_last2:?})"
    );
    println!(
        "merge_pdfs: merged 2+2 -> {} pages; shared font preserved (both ref {:?})",
        out_pages.len(),
        f_last
    );

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
