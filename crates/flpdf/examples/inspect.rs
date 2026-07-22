//! End-to-end inspection example.
//!
//! Prints the document's PDF version, page count, and a flattened outline. Demonstrates
//! the read-only API surface most callers reach for first: `Pdf::open`, the `pages` and
//! `outline` modules, and the `Object::write_pdf` rendering helper.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example inspect -- path/to/file.pdf
//! ```

use flpdf::{pages, Pdf};
use std::env;
use std::fs::File;
use std::io::BufReader;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: inspect <input.pdf>");
        return ExitCode::from(2);
    };

    if let Err(error) = run(&path) {
        eprintln!("inspect: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut pdf = Pdf::open(BufReader::new(File::open(path)?))?;

    println!("version: {}", pdf.version());

    let page_refs = pages::page_refs(&mut pdf)?;
    println!("pages: {}", page_refs.len());

    let outline = pdf.outline().get_tree()?;
    if outline.roots().is_empty() {
        println!("outline: <empty>");
    } else {
        println!("outline:");
        for (depth, _id, item) in outline.preorder() {
            println!("{}- {}", "  ".repeat(depth), item.title);
        }
    }
    Ok(())
}
