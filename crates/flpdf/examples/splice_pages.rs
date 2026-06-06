//! Splice pages from source A into target B at a given index.
//!
//! Run with: `cargo run --example splice_pages -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, BufWriter};

use flpdf::{
    copy_objects, page_closure::page_object_closure, pages::page_refs, splice_pages, ObjectRef, Pdf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Source A: 3 pages. Target B: 2 pages. Insert A's pages into B at index 1.
    let a_path = common::write_temp("splice-a", &common::build_shared_font_pdf(3))?;
    let b_path = common::write_temp("splice-b", &common::build_shared_font_pdf(2))?;
    let out_path = common::temp_path("splice-out");

    let mut a = Pdf::open(BufReader::new(File::open(&a_path)?))?;
    let mut b = Pdf::open(BufReader::new(File::open(&b_path)?))?;

    // Collect A's page refs and the full object closure they need.
    let a_pages = page_refs(&mut a)?;
    let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
    for &pr in &a_pages {
        closure.extend(page_object_closure(&mut a, pr)?);
    }

    // Copy A's pages (and their dependencies) into B in ONE call; record
    // renumbered refs. A single call dedups the shared font once.
    let map = copy_objects(&mut a, &mut b, &closure)?;
    let copied: Vec<ObjectRef> = a_pages.iter().map(|r| map[r]).collect();

    // Insert the copied pages at index 1 (remove nothing).
    let n = 1usize;
    splice_pages(&mut b, n..n, &copied)?;

    // A plain write keeps existing objects; we only append pages, so the
    // unreferenced-object pruning of `full_rewrite` (see extract_pages) is unnecessary.
    let out = BufWriter::new(File::create(&out_path)?);
    flpdf::write_pdf(&mut b, out)?;

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
