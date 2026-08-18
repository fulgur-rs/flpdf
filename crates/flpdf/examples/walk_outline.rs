//! Walk the document outline (bookmarks), printing an indented tree.
//!
//! Run with: `cargo run --example walk_outline -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::Pdf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("outline-src", &common::build_outline_pdf())?;
    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    let mut helper = pdf.outline();
    let tree = helper.get_tree()?;
    let mut visited = 0;
    for (depth, _id, item) in tree.preorder() {
        let title = item.title(&mut helper)?;
        println!("{}{}", "  ".repeat(depth - 1), title);
        visited += 1;
    }

    assert_eq!(visited, 3, "expected 3 outline items, got {visited}");
    println!("walk_outline: visited {visited} outline item(s)");

    drop(pdf);
    let _ = std::fs::remove_file(&src_path);
    Ok(())
}
