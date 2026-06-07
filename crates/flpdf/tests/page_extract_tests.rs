//! Integration tests for [`flpdf::extract_page`].

use flpdf::{extract_page, pages, write_pdf_with_options, Object, Pdf, WriteOptions};
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

/// Two-page document; each page carries its own /MediaBox and /Resources.
fn two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 5 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    )
}

/// Resolve the catalog's /Pages dict from a freshly-extracted document.
fn pages_dict(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let catalog_ref = doc.root_ref().unwrap();
    let catalog = doc.resolve_borrowed(catalog_ref).unwrap().as_dict().cloned().unwrap();
    let pages_ref = catalog.get("Pages").and_then(|o| match o {
        Object::Reference(r) => Some(*r),
        _ => None,
    }).unwrap();
    doc.resolve_borrowed(pages_ref).unwrap().as_dict().cloned().unwrap()
}

/// Fetch the single extracted leaf page dict.
fn only_leaf(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let refs = pages::page_refs(doc).unwrap();
    assert_eq!(refs.len(), 1);
    doc.resolve_borrowed(refs[0]).unwrap().as_dict().cloned().unwrap()
}

#[test]
fn extracts_single_page_with_count_one() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    // Exactly one page in the extracted document.
    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 1, "extracted doc must have exactly one page");

    // /Pages root: /Count 1, /Kids has one element.
    let root = pages_dict(&mut out);
    assert_eq!(root.get("Count"), Some(&Object::Integer(1)));
    match root.get("Kids") {
        Some(Object::Array(kids)) => assert_eq!(kids.len(), 1),
        other => panic!("expected /Kids array, got {other:?}"),
    }
}

/// Parent /Pages carries /MediaBox, /Resources (font), and /Rotate; the leaf
/// page (obj 3) inherits all three.
fn inherited_attrs_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 400 500] /Rotate 90 /Resources << /Font << /F1 5 0 R >> >> >>"),
            (3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>"),
            (4, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    )
}

#[test]
fn materializes_inherited_attributes() {
    let src = inherited_attrs_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    assert_eq!(
        leaf.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(400), Object::Integer(500),
        ]))
    );
    assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));

    let res = leaf.get("Resources").and_then(|o| o.as_dict()).expect("/Resources");
    let font_ref = res
        .get("Font").and_then(|o| o.as_dict())
        .and_then(|f| f.get("F1"))
        .and_then(|o| match o { Object::Reference(r) => Some(*r), _ => None })
        .expect("/Font /F1 ref");
    let font = out.resolve_borrowed(font_ref).unwrap().as_dict().cloned().unwrap();
    assert_eq!(font.get("Subtype"), Some(&Object::Name(b"Type1".to_vec())));
}

/// Ancestor /Pages stores /MediaBox as an INDIRECT reference (obj 6), the qpdf
/// shared-array pattern. The leaf (obj 3) inherits it. Exercises remap_refs'
/// Object::Reference branch: the extracted leaf's /MediaBox must resolve to a
/// live array, not become Null.
fn indirect_inherited_mediabox_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox 6 0 R >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (6, "[0 0 321 654]"),
        ],
        1,
    )
}

#[test]
fn remaps_indirect_inherited_mediabox() {
    let src = indirect_inherited_mediabox_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // /MediaBox must be present and resolve to the live array (not Null, not a
    // dangling source ref).
    let mb = leaf.get("MediaBox").expect("/MediaBox present");
    let arr = match mb {
        Object::Reference(r) => out.resolve(*r).unwrap(),
        other => other.clone(),
    };
    assert_eq!(
        arr,
        Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(321), Object::Integer(654),
        ]),
        "indirect inherited /MediaBox must be remapped into the extracted doc, not nulled"
    );
}

#[test]
fn own_mediabox_is_preserved() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut p0 = extract_page(&mut source, 0).unwrap();
    let leaf0 = only_leaf(&mut p0);
    assert_eq!(
        leaf0.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(612), Object::Integer(792),
        ]))
    );

    let mut p1 = extract_page(&mut source, 1).unwrap();
    let leaf1 = only_leaf(&mut p1);
    assert_eq!(
        leaf1.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(200), Object::Integer(300),
        ]))
    );
}

/// obj 6 = shared font (both pages); obj 7 = image used ONLY by page 2.
fn shared_resource_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 5 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 7 0 R >> >> >>"),
            (5, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (7, "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /Length 3 >>\nstream\n\x00\x00\x00\nendstream"),
        ],
        1,
    )
}

/// Count how many live objects in `doc` carry the given /Subtype name.
fn count_subtype(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, subtype: &[u8]) -> usize {
    let mut n = 0;
    for r in doc.live_object_refs() {
        if let Ok(obj) = doc.resolve(r) {
            let dict = match &obj {
                Object::Dictionary(d) => Some(d.clone()),
                Object::Stream(s) => Some(s.dict.clone()),
                _ => None,
            };
            if let Some(d) = dict {
                if d.get("Subtype").and_then(|o| o.as_name()) == Some(subtype) {
                    n += 1;
                }
            }
        }
    }
    n
}

#[test]
fn extracted_doc_has_no_unrelated_objects() {
    let src = shared_resource_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    let mut bytes = Vec::new();
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    write_pdf_with_options(&mut out, &mut bytes, &opts).unwrap();
    let mut rt = Pdf::open_mem_owned(bytes).unwrap();

    assert_eq!(count_subtype(&mut rt, b"Type1"), 1, "shared font must be present");
    assert_eq!(count_subtype(&mut rt, b"Image"), 0, "page 2's image must not leak in");
    assert_eq!(pages::page_refs(&mut rt).unwrap().len(), 1);
}
