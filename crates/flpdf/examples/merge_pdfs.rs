//! Merge two PDFs, preserving fonts shared between the merged-in pages.
//!
//! `copy_objects` deduplicates objects shared *within a single call*: the two
//! source pages share one font, so after copying them together the merged
//! output still references that font exactly once.
//!
//! Run with: `cargo run --example merge_pdfs -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{
    copy_objects, page_closure::page_object_closure, pages::page_refs, splice_pages, ObjectRef,
    Pdf,
};

/// Resolve a page's `/Resources /Font /F1` indirect reference.
///
/// The synthetic pages keep `/Resources` inline, so a single `resolve` of the
/// page object is enough; the nested dictionaries are read through accessors
/// (`as_dict` / `as_ref_id`) rather than matching `Object` variants by hand.
fn font_ref_of_page<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page: ObjectRef,
) -> Option<ObjectRef> {
    let page_obj = pdf.resolve(page).ok()?;
    let resources = page_obj.as_dict()?.get("Resources")?;
    let fonts = resources.as_dict()?.get("Font")?;
    fonts.as_dict()?.get("F1")?.as_ref_id()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Target A: 2 pages. Source B: 2 pages that share one font object.
    let a_path = common::write_temp("merge-a", &common::build_shared_font_pdf(2))?;
    let b_path = common::write_temp("merge-b", &common::build_shared_font_pdf(2))?;
    let out_path = common::temp_path("merge-out");

    let mut a = Pdf::open(BufReader::new(File::open(&a_path)?))?;
    let mut b = Pdf::open(BufReader::new(File::open(&b_path)?))?;

    // Union the closures of B's pages, then copy them in ONE call so the font
    // they share is copied once (sharing preserved).
    let b_pages = page_refs(&mut b)?;
    let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
    for &pr in &b_pages {
        closure.extend(page_object_closure(&mut b, pr)?);
    }
    let map = copy_objects(&mut b, &mut a, &closure)?;
    let copied: Vec<ObjectRef> = b_pages.iter().map(|r| map[r]).collect();

    // Append B's copied pages at the end of A.
    let a_len = page_refs(&mut a)?.len();
    splice_pages(&mut a, a_len..a_len, &copied)?;

    // A plain write keeps existing objects; we only append pages, so the
    // unreferenced-object pruning of `full_rewrite` (see extract_pages) is unnecessary.
    let out = BufWriter::new(File::create(&out_path)?);
    flpdf::write_pdf(&mut a, out)?;

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

    for p in [&a_path, &b_path, &out_path] {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}
