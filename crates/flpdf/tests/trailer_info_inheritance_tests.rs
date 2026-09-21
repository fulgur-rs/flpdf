//! Trailer `/Info` inheritance across the page-assembly primitives.
//!
//! qpdf takes document-level information from the *primary* input only.
//! `qpdf in.pdf --pages . N -- out.pdf` keeps `in.pdf`'s `/Info`, while
//! `qpdf --empty --pages in.pdf N -- out.pdf` produces a trailer with no
//! `/Info` at all, because `QPDF::emptyPDF` has none and
//! `QPDFPageDocumentHelper::addPage` copies pages rather than document-level
//! data. [`flpdf::merge_documents`] is the primary-input form and
//! [`flpdf::extract_pages`] is the `--empty` form, so these tests pin both
//! sides of that split, the primary-only rule that a later input's `/Info`
//! never wins, and the public-handle route a caller uses to install an
//! `/Info` of its own.

use flpdf::{extract_pages, merge_documents, MergeInput, Pdf, PdfWriter};
use std::collections::BTreeMap;
use std::io::Cursor;

/// Build a PDF from `(number, body)` object definitions plus a literal
/// trailer tail appended after `/Size` and `/Root`.
fn build_pdf(objects: &[(u32, &str)], root: u32, trailer_tail: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=max {
        match offsets.get(&n) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root {root} 0 R{trailer_tail} >>\nstartxref\n{xref_start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

const PAGE_OBJECTS: &[(u32, &str)] = &[
    (1, "<< /Type /Catalog /Pages 2 0 R >>"),
    (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
    (
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> >>",
    ),
    (
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << >> >>",
    ),
];

/// Two pages plus an indirect `/Info` dictionary referenced from the trailer.
fn two_page_pdf_with_indirect_info() -> Vec<u8> {
    let mut objects = PAGE_OBJECTS.to_vec();
    objects.push((5, "<< /Title (Indirect Title) /Producer (probe) >>"));
    build_pdf(&objects, 1, " /Info 5 0 R")
}

/// Two pages plus a direct (inline) `/Info` dictionary in the trailer.
fn two_page_pdf_with_direct_info() -> Vec<u8> {
    build_pdf(
        PAGE_OBJECTS,
        1,
        " /Info << /Title (Direct Title) /Producer (probe) >>",
    )
}

/// Two pages and no `/Info` at all.
fn two_page_pdf_without_info() -> Vec<u8> {
    build_pdf(PAGE_OBJECTS, 1, "")
}

/// A second document whose `/Info` carries a distinguishable `/Title`, used
/// as a non-primary merge input.
fn two_page_donor_pdf_with_info() -> Vec<u8> {
    let mut objects = PAGE_OBJECTS.to_vec();
    objects.push((5, "<< /Title (Donor Title) /Producer (donor) >>"));
    build_pdf(&objects, 1, " /Info 5 0 R")
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("open source document")
}

/// Serialize `pdf` through the ordinary writer and reopen the result.
fn round_trip<R: std::io::Read + std::io::Seek + 'static>(
    pdf: &mut Pdf<R>,
) -> Pdf<Cursor<Vec<u8>>> {
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_memory().expect("memory output");
    writer.set_static_id(true);
    writer.write().expect("write assembled document");
    open(writer.get_buffer().expect("take written bytes"))
}

/// `/Title` of the written trailer's `/Info`, or `None` when there is no
/// `/Info` key. The trailer value and each entry may be an indirect
/// reference; `try_is_null`, `try_get_key`, and `try_get_string_value` each
/// resolve their receiver first, so the indirect and direct shapes read the
/// same way.
fn written_info_title<R: std::io::Read + std::io::Seek + 'static>(
    pdf: &mut Pdf<R>,
) -> Option<Vec<u8>> {
    let info = pdf.trailer_key_handle(b"Info");
    if info.try_is_null().expect("probe /Info") {
        return None;
    }
    let title = info.try_get_key(b"/Title").expect("read /Info /Title");
    Some(title.try_get_string_value().expect("/Title is a string"))
}

#[test]
fn merge_documents_carries_the_primary_indirect_info_dictionary() {
    let mut source = open(two_page_pdf_with_indirect_info());
    let mut inputs = [MergeInput {
        source: &mut source,
        pages: vec![1],
    }];
    let mut merged = merge_documents(&mut inputs).expect("merge single input");
    let mut written = round_trip(&mut merged);

    assert_eq!(
        written_info_title(&mut written).as_deref(),
        Some(b"Indirect Title".as_slice()),
        "merge_documents must inherit the primary input's /Info, matching \
         qpdf `in.pdf --pages . N --`"
    );
}

#[test]
fn merge_documents_carries_a_direct_primary_info_dictionary() {
    let mut source = open(two_page_pdf_with_direct_info());
    let mut inputs = [MergeInput {
        source: &mut source,
        pages: vec![0],
    }];
    let mut merged = merge_documents(&mut inputs).expect("merge single input");
    let mut written = round_trip(&mut merged);

    assert_eq!(
        written_info_title(&mut written).as_deref(),
        Some(b"Direct Title".as_slice()),
        "a direct trailer /Info dictionary must survive the merge just like \
         an indirect one"
    );
}

#[test]
fn merge_documents_ignores_a_later_inputs_info_dictionary() {
    let mut primary = open(two_page_pdf_with_indirect_info());
    let mut donor = open(two_page_donor_pdf_with_info());
    let mut inputs = [
        MergeInput {
            source: &mut primary,
            pages: vec![0],
        },
        MergeInput {
            source: &mut donor,
            pages: vec![0],
        },
    ];
    let mut merged = merge_documents(&mut inputs).expect("merge two inputs");
    let mut written = round_trip(&mut merged);

    assert_eq!(
        written_info_title(&mut written).as_deref(),
        Some(b"Indirect Title".as_slice()),
        "document-level data is taken from inputs[0] only, so a later input's \
         /Info must not replace the primary's"
    );
}

#[test]
fn merge_documents_adds_no_info_when_the_primary_has_none() {
    let mut primary = open(two_page_pdf_without_info());
    let mut donor = open(two_page_donor_pdf_with_info());
    let mut inputs = [
        MergeInput {
            source: &mut primary,
            pages: vec![0],
        },
        MergeInput {
            source: &mut donor,
            pages: vec![0],
        },
    ];
    let mut merged = merge_documents(&mut inputs).expect("merge two inputs");
    let mut written = round_trip(&mut merged);

    assert_eq!(
        written_info_title(&mut written),
        None,
        "a primary without /Info must not gain one from a later input"
    );
}

#[test]
fn extract_pages_omits_the_source_info_dictionary() {
    let mut source = open(two_page_pdf_with_indirect_info());
    let mut extracted = extract_pages(&mut source, &[0]).expect("extract page");
    let mut written = round_trip(&mut extracted);

    assert_eq!(
        written_info_title(&mut written),
        None,
        "extract_pages builds on QPDF::emptyPDF, whose addPage copies pages \
         and not document-level data, matching qpdf `--empty --pages`"
    );
}

#[test]
fn an_extracted_document_can_adopt_the_source_info_through_the_public_handle_api() {
    let mut source = open(two_page_pdf_with_indirect_info());
    let mut extracted = extract_pages(&mut source, &[0]).expect("extract page");

    // The source trailer value may be an indirect reference; copy it across
    // documents through the canonical foreign copier, then install it on the
    // extracted document's live trailer handle.
    let source_info = source.trailer_key_handle(b"Info");
    assert!(!source_info.is_null(), "probe source carries /Info");
    let copied = extracted
        .copy_foreign_object(&source_info)
        .expect("copy /Info across documents");
    extracted
        .trailer()
        .replace_key(b"/Info", copied)
        .expect("install /Info on the extracted trailer");

    let mut written = round_trip(&mut extracted);
    assert_eq!(
        written_info_title(&mut written).as_deref(),
        Some(b"Indirect Title".as_slice()),
        "the live trailer handle is writable and the writer serializes the \
         installed /Info"
    );
}
