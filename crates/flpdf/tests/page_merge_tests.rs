//! Integration tests for [`flpdf::merge_documents`].

use flpdf::{
    merge_documents, pages, write_pdf, write_pdf_with_options, MergeInput, Object, Pdf,
    WriteOptions,
};
use std::collections::BTreeMap;

/// Build a PDF from `(number, body)` object definitions plus a `/Root` number.
/// `body` is the literal text between `N 0 obj` and `endobj`.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
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
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Three-page document; pages 3 and 4 SHARE font 7; page 5 has its own font 8.
fn three_page_shared_font_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F2 8 0 R >> >> >>"),
            (6, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
        ],
        1,
    )
}

/// One-page document whose single page uses a single named font with the given
/// `BaseFont`.
fn single_font_pdf(base: &[u8]) -> Vec<u8> {
    let base = std::str::from_utf8(base).expect("BaseFont must be valid UTF-8");
    let font_obj = format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} >>");
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
            ),
            (4, font_obj.as_str()),
            (5, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
        ],
        1,
    )
}

/// Resolve the `/BaseFont` name of the single font on the leaf page at index
/// `page_idx` in `doc`, following its `/Resources /Font` dictionary.
fn leaf_base_font(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, page_idx: usize) -> Vec<u8> {
    let leaf_ref = pages::page_refs(doc).unwrap()[page_idx];
    let leaf = doc.resolve(leaf_ref).unwrap().as_dict().cloned().unwrap();
    let resources = resolve_dict_entry(doc, &leaf, "Resources");
    let fonts = resolve_dict_entry(doc, &resources, "Font");
    // The single font entry (named /F1 in the fixture); resolve its /BaseFont.
    let (_, font_obj) = fonts.iter().next().expect("page has one font");
    let font = match font_obj {
        Object::Reference(r) => doc.resolve(*r).unwrap().as_dict().cloned().unwrap(),
        Object::Dictionary(d) => d.clone(),
        _ => panic!("font entry is not a dict"),
    };
    font.get("BaseFont")
        .and_then(|o| o.as_name())
        .expect("font has /BaseFont")
        .to_vec()
}

/// Resolve `dict[key]` (which may be a reference) into an owned dictionary.
fn resolve_dict_entry(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    dict: &flpdf::Dictionary,
    key: &str,
) -> flpdf::Dictionary {
    match dict.get(key).expect("dict has key") {
        Object::Reference(r) => doc.resolve(*r).unwrap().as_dict().cloned().unwrap(),
        Object::Dictionary(d) => d.clone(),
        _ => panic!("entry {key} is not a dict"),
    }
}

/// Resolve the catalog's /Pages dict from a freshly-merged document.
fn pages_dict(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let catalog_ref = doc.root_ref().unwrap();
    let catalog = doc
        .resolve_borrowed(catalog_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let pages_ref = catalog
        .get("Pages")
        .and_then(|o| match o {
            Object::Reference(r) => Some(*r),
            _ => None,
        })
        .unwrap();
    doc.resolve_borrowed(pages_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap()
}

/// Count objects whose dict is `/Type /Font` with the given `/BaseFont`.
fn count_font_objects(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, base: &[u8]) -> usize {
    let mut n = 0;
    for r in doc.object_refs() {
        if let Ok(obj) = doc.resolve(r) {
            if let Some(d) = obj.as_dict() {
                if d.get("Type").and_then(|o| o.as_name()) == Some(&b"Font"[..])
                    && d.get("BaseFont").and_then(|o| o.as_name()) == Some(base)
                {
                    n += 1;
                }
            }
        }
    }
    n
}

// Single-input merge equals extract_pages: page count + shared-resource dedup.
#[test]
fn merge_single_input_copies_selected_pages_with_shared_dedup() {
    let bytes = three_page_shared_font_pdf(); // pages 0,1 share font 7; page 2 has font 8
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut inputs = [MergeInput {
        source: &mut src,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    let root = pages_dict(&mut doc);
    assert!(matches!(root.get("Count"), Some(Object::Integer(2))));
    assert_eq!(count_font_objects(&mut doc, b"Helvetica"), 1); // shared font copied once
    assert_eq!(count_font_objects(&mut doc, b"Courier"), 0); // unselected page's font absent
}

// The merged document round-trips: writing then re-opening yields a valid PDF
// with the same page count.
#[test]
fn merge_single_input_round_trips() {
    let bytes = three_page_shared_font_pdf();
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut inputs = [MergeInput {
        source: &mut src,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    let mut out: Vec<u8> = Vec::new();
    write_pdf_with_options(&mut doc, &mut out, &options).unwrap();

    let mut reopened = Pdf::open_mem_owned(out).unwrap();
    let refs = pages::page_refs(&mut reopened).unwrap();
    assert_eq!(refs.len(), 2, "round-tripped doc must keep both pages");
}

// Empty input slice is rejected (merge requires at least one input).
#[test]
fn merge_rejects_empty_inputs() {
    let mut inputs: [MergeInput<std::io::Cursor<Vec<u8>>>; 0] = [];
    match merge_documents(&mut inputs) {
        Ok(_) => panic!("empty inputs must error"),
        Err(err) => assert!(matches!(err, flpdf::Error::Unsupported(_)), "got {err:?}"),
    }
}

// Two inputs concatenate in input order: page 1 from input A, page 2 from
// input B, each carrying its own independent font.
#[test]
fn merge_two_inputs_concatenates_in_order() {
    let mut a = Pdf::open_mem_owned(single_font_pdf(b"Helvetica")).unwrap();
    let mut b = Pdf::open_mem_owned(single_font_pdf(b"Courier")).unwrap();
    let mut inputs = [
        MergeInput {
            source: &mut a,
            pages: vec![0],
        },
        MergeInput {
            source: &mut b,
            pages: vec![0],
        },
    ];
    let mut doc = merge_documents(&mut inputs).unwrap();
    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    // Both fonts present independently, exactly once each.
    assert_eq!(count_font_objects(&mut doc, b"Helvetica"), 1);
    assert_eq!(count_font_objects(&mut doc, b"Courier"), 1);
    // Concat ORDER: page 1 is input A's font, page 2 is input B's font.
    assert_eq!(leaf_base_font(&mut doc, 0), b"Helvetica".to_vec());
    assert_eq!(leaf_base_font(&mut doc, 1), b"Courier".to_vec());
    // Round-trip valid.
    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// An out-of-range page index is rejected.
#[test]
fn merge_rejects_out_of_range_index() {
    let bytes = three_page_shared_font_pdf();
    let mut src = Pdf::open_mem_owned(bytes).unwrap();
    let mut inputs = [MergeInput {
        source: &mut src,
        pages: vec![0, 9],
    }];
    match merge_documents(&mut inputs) {
        Ok(_) => panic!("out-of-range index must error"),
        Err(err) => assert!(matches!(err, flpdf::Error::Unsupported(_)), "got {err:?}"),
    }
}
