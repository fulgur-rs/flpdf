//! Walk the document outline (bookmarks), printing an indented tree.
//!
//! Run with: `cargo run --example walk_outline -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::cell::Cell;
use std::fs::File;
use std::io::BufReader;

use flpdf::Pdf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("outline-src", &common::build_outline_pdf())?;
    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    let visited = Cell::new(0usize);
    // `walk` performs a depth-first traversal, handing each node its depth.
    pdf.outline().walk(|node, depth| {
        println!("{}{}", "  ".repeat(depth), node.title);
        visited.set(visited.get() + 1);
    })?;

    assert_eq!(
        visited.get(),
        3,
        "expected 3 outline items, got {}",
        visited.get()
    );
    println!("walk_outline: visited {} outline item(s)", visited.get());

    drop(pdf);
    let _ = std::fs::remove_file(&src_path);
    Ok(())
}
