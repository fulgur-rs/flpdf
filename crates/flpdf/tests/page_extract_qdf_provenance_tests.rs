//! QDF `%% Original object ID` provenance for [`flpdf::extract_pages`].
//!
//! `extract_pages` models qpdf's `emptyPDF()` plus foreign `addPage`, which is
//! what `qpdf --empty --pages src 1-2 -- out.pdf` performs. In that shape the
//! copied objects belong to the destination document, so `QPDFWriter` emits
//! the *destination* ObjGen in the comment — `object.getObjGen().unparse(' ')`
//! at `libqpdf/QPDFWriter.cc:1786` reads the handle as it exists in the QPDF
//! being written. Only qpdf's `--pages .` form, which keeps the primary QPDF
//! itself as the destination (`libqpdf/QPDFJob.cc:2511-2593`), reports source
//! ObjGens.
//!
//! Observed with qpdf 11.9.0 on a source whose page objects are 11 and 12:
//!
//! ```text
//! qpdf --qdf --static-id --empty --pages src.pdf 1-2 -- out.pdf
//!   %% Original object ID: 1 0 / 2 0 / 3 0 / 4 0      (destination)
//! qpdf --qdf --static-id src.pdf --pages . 1-2 -- out.pdf
//!   %% Original object ID: 1 0 / 2 0 / 11 0 / 12 0    (source)
//! ```

use flpdf::{extract_pages, Pdf, PdfWriter};
use std::collections::BTreeMap;

/// A two-page source whose page objects are numbered far above the range a
/// fresh destination would allocate, so destination and source provenance are
/// distinguishable.
fn source_pdf() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [11 0 R 12 0 R] /Count 2 >>"),
        (
            11,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (
            12,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (number, body) in objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len();
    out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for (number, offset) in &offsets {
        out.extend_from_slice(format!("{number} 1\n{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 13 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

fn original_object_ids(pdf_bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(pdf_bytes)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("%% Original object ID: ")
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn extract_pages_reports_destination_original_object_ids() {
    let mut source = Pdf::open_mem_owned(source_pdf()).expect("open source");
    let mut extracted = extract_pages(&mut source, &[0, 1]).expect("extract pages");

    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("extracted.pdf");
    let mut writer = PdfWriter::new(&mut extracted);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_file(&output).expect("set output file");
    writer.write().expect("write extracted document");
    drop(writer);

    let ids = original_object_ids(&std::fs::read(&output).expect("read extracted document"));
    assert_eq!(
        ids,
        vec!["1 0", "2 0", "3 0", "4 0"],
        "extract_pages copies foreign objects into a fresh document, so the QDF \
         provenance comments must carry destination ObjGens, not the source's 11/12"
    );
}
