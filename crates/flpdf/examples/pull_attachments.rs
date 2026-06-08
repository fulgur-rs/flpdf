//! Pull every embedded attachment out of a document to disk.
//!
//! Run with: `cargo run --example pull_attachments -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek};

use flpdf::{
    extract_attachment, insert_embedded_file, list_attachment_info, FileSpecBuilder, Pdf,
    WriteOptions,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a base PDF, then attach two files using the library's own API.
    let base_path = common::write_temp("attach-base", &common::build_shared_font_pdf(1))?;
    let with_files = common::temp_path("attach-src");
    {
        let mut pdf = Pdf::open(BufReader::new(File::open(&base_path)?))?;
        attach(&mut pdf, "notes.txt", b"hello from flpdf")?;
        attach(&mut pdf, "data.csv", b"a,b,c\n1,2,3\n")?;

        // `WriteOptions` is `#[non_exhaustive]`, so it must be built from its
        // `Default` and then mutated rather than via a struct literal. A full
        // (non-incremental) rewrite materializes the new name-tree objects.
        #[allow(clippy::field_reassign_with_default)]
        let opts = {
            let mut opts = WriteOptions::default();
            opts.full_rewrite = true;
            opts
        };
        let out = BufWriter::new(File::create(&with_files)?);
        flpdf::write_pdf_with_options(&mut pdf, out, &opts)?;
    }

    // Re-open and pull each attachment back out, asserting the round-trip.
    let mut pdf = Pdf::open(BufReader::new(File::open(&with_files)?))?;
    let infos = list_attachment_info(&mut pdf)?;
    let mut pulled = 0usize;
    for info in &infos {
        let bytes = extract_attachment(&mut pdf, &info.key)?;
        let name = info
            .display_name
            .clone()
            .unwrap_or_else(|| "<unnamed>".into());
        println!("  pulled {} ({} bytes)", name, bytes.len());
        pulled += 1;
    }
    assert_eq!(pulled, 2, "expected 2 attachments, got {pulled}");
    println!("pull_attachments: pulled {pulled} attachment(s)");

    // Close the open file handle before deleting: on Windows, removing a file
    // that is still open by the process fails with a permission error.
    drop(pdf);
    let _ = std::fs::remove_file(&base_path);
    let _ = std::fs::remove_file(&with_files);
    Ok(())
}

/// Embed `payload` under `name` and register it in `/Names /EmbeddedFiles`.
///
/// `FileSpecBuilder::build` only creates the `/Filespec` + `/EmbeddedFile`
/// objects; the caller must register the returned ref in the document's
/// `/Names /EmbeddedFiles` name tree (via `insert_embedded_file`) so that
/// `list_attachment_info` finds it after a rewrite + re-open.
fn attach<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    name: &str,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let filespec_ref = FileSpecBuilder::new(name.as_bytes(), payload.to_vec()).build(pdf)?;
    insert_embedded_file(pdf, name.as_bytes(), filespec_ref)?;
    Ok(())
}
