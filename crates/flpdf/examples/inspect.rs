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

use flpdf::{outline, pages, Pdf};
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

    let outline = outline::outline_items(&mut pdf)?;
    if outline.is_empty() {
        println!("outline: <empty>");
    } else {
        println!("outline:");
        for item in outline {
            println!("{}- {}", "  ".repeat(item.depth + 1), item.title);
        }
    }
    Ok(())
}
