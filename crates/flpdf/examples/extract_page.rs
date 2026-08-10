//! Extract a single page (0-based) from a PDF into a new minimal PDF.
//!
//! Usage: cargo run --example extract_page -- <input.pdf> <page-index> <output.pdf>

use flpdf::{extract_page, Pdf, PdfWriter};
use std::fs::File;
use std::io::BufReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: extract_page <input.pdf> <page-index> <output.pdf>")?;
    let index: usize = args.next().ok_or("missing <page-index>")?.parse()?;
    let output = args.next().ok_or("missing <output.pdf>")?;

    let mut source = Pdf::open(BufReader::new(File::open(&input)?))?;
    let mut extracted = extract_page(&mut source, index)?;

    let mut writer = PdfWriter::new(&mut extracted);
    writer.set_output_file(&output)?;
    writer.write()?;

    eprintln!("extracted page {index} (0-based) from {input} -> {output}");
    Ok(())
}
