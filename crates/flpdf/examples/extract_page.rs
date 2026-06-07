//! Extract a single page (0-based) from a PDF into a new minimal PDF.
//!
//! Usage: cargo run --example extract_page -- <input.pdf> <page-index> <output.pdf>

use flpdf::{extract_page, write_pdf_with_options, Pdf, WriteOptions};
use std::fs::File;
use std::io::{BufReader, BufWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: extract_page <input.pdf> <page-index> <output.pdf>")?;
    let index: usize = args.next().ok_or("missing <page-index>")?.parse()?;
    let output = args.next().ok_or("missing <output.pdf>")?;

    let mut source = Pdf::open(BufReader::new(File::open(&input)?))?;
    let mut extracted = extract_page(&mut source, index)?;

    // `full_rewrite` is recommended for compaction (not required for
    // correctness; `extract_page` already prunes orphans). `WriteOptions` is
    // `#[non_exhaustive]`, so it must be built from its `Default` and then
    // mutated rather than via a struct literal.
    #[allow(clippy::field_reassign_with_default)]
    let opts = {
        let mut opts = WriteOptions::default();
        opts.full_rewrite = true;
        opts
    };
    let out = BufWriter::new(File::create(&output)?);
    write_pdf_with_options(&mut extracted, out, &opts)?;

    eprintln!("extracted page {index} (0-based) from {input} -> {output}");
    Ok(())
}
