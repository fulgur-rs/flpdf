//! Pull every embedded attachment out of a document to disk.
//!
//! Run with: `cargo run --example pull_attachments -p flpdf`

#![allow(deprecated)]

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::{BufReader, Read, Seek};

use flpdf::job::QPDFJob;
use flpdf::{extract_attachment, insert_embedded_file, FileSpecBuilder, Pdf, PdfWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a base PDF, then attach two files using the library's own API.
    let base_path = common::write_temp("attach-base", &common::build_shared_font_pdf(1))?;
    let with_files = common::temp_path("attach-src");
    {
        let mut pdf = Pdf::open(BufReader::new(File::open(&base_path)?))?;
        attach(&mut pdf, "notes.txt", b"hello from flpdf")?;
        attach(&mut pdf, "data.csv", b"a,b,c\n1,2,3\n")?;

        // The canonical writer materializes the new name-tree objects.
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_output_file(&with_files)?;
        writer.write()?;
    }

    // Expected payloads keyed by display name, used to verify the round-trip.
    let expected: std::collections::HashMap<&str, &[u8]> = [
        ("notes.txt", b"hello from flpdf".as_slice()),
        ("data.csv", b"a,b,c\n1,2,3\n".as_slice()),
    ]
    .into_iter()
    .collect();

    // Re-open and pull each attachment back out, asserting the round-trip.
    let mut job = QPDFJob::new();
    let mut pdf = job.open(
        BufReader::new(File::open(&with_files)?),
        "attach-src.pdf",
        flpdf::PdfOpenOptions::default(),
    )?;
    let _status = job.list_attachments(&mut pdf, false)?;
    let mut pulled = 0usize;
    for name in expected.keys().copied() {
        let bytes = extract_attachment(&mut pdf, name.as_bytes())?;
        let want = expected
            .get(name)
            .unwrap_or_else(|| panic!("unexpected attachment name {name:?}"));
        assert_eq!(
            bytes, *want,
            "payload mismatch for {name:?}: extracted bytes differ from the original"
        );
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
/// `/Names /EmbeddedFiles` name tree (via `insert_embedded_file`) so that the
/// canonical `QPDFJob::list_attachments` route finds it after a rewrite + re-open.
fn attach<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    name: &str,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let filespec_ref = FileSpecBuilder::new(name.as_bytes(), payload.to_vec()).build(pdf)?;
    insert_embedded_file(pdf, name.as_bytes(), filespec_ref)?;
    Ok(())
}
