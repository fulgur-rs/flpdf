//! Integration tests for [`flpdf::extract_page`] / [`flpdf::extract_pages`].

use flpdf::{
    extract_page, extract_pages, pages, write_pdf_with_options, Object, Pdf, WriteOptions,
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

/// Fetch the single extracted leaf page dict.
fn only_leaf(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let refs = pages::page_refs(doc).unwrap();
    assert_eq!(refs.len(), 1);
    doc.resolve_borrowed(refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap()
}

fn resolve_indirect_value(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, mut value: Object) -> Object {
    while let Object::Reference(reference) = value {
        value = doc.resolve(reference).unwrap();
    }
    value
}

fn destination_page_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    value: Object,
) -> flpdf::ObjectRef {
    resolve_indirect_value(doc, value)
        .into_array()
        .expect("destination array")
        .into_iter()
        .next()
        .and_then(|value| value.as_ref_id())
        .expect("destination page reference")
}

fn assert_destination_page_is_null(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    value: Object,
    context: &str,
) {
    let page_ref = destination_page_ref(doc, value);
    assert!(
        matches!(doc.resolve(page_ref).unwrap(), Object::Null),
        "{context}"
    );
}

fn assert_reference_target_is_null(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    value: &Object,
    context: &str,
) {
    let reference = value.as_ref_id().expect("page reference");
    assert!(
        matches!(doc.resolve(reference).unwrap(), Object::Null),
        "{context}"
    );
}

#[test]
fn extracts_single_page_with_count_one() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    // Exactly one page in the extracted document.
    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(
        page_refs.len(),
        1,
        "extracted doc must have exactly one page"
    );

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
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(400),
            Object::Integer(500),
        ]))
    );
    assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));

    let res = leaf
        .get("Resources")
        .and_then(|o| o.as_dict())
        .expect("/Resources");
    let font_ref = res
        .get("Font")
        .and_then(|o| o.as_dict())
        .and_then(|f| f.get("F1"))
        .and_then(|o| match o {
            Object::Reference(r) => Some(*r),
            _ => None,
        })
        .expect("/Font /F1 ref");
    let font = out
        .resolve_borrowed(font_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert_eq!(font.get("Subtype"), Some(&Object::Name(b"Type1".to_vec())));
}

/// Parent /Pages carries an inheritable /CropBox; the leaf (obj 3) has its own
/// /MediaBox but inherits the /CropBox. Covers the /CropBox materialization
/// branch (own /MediaBox wins, inherited /CropBox is materialized).
fn inherited_cropbox_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /CropBox [5 5 590 770] >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

#[test]
fn materializes_inherited_cropbox() {
    let src = inherited_cropbox_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // Own /MediaBox preserved.
    assert_eq!(
        leaf.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]))
    );
    // Inherited /CropBox materialized onto the leaf.
    assert_eq!(
        leaf.get("CropBox"),
        Some(&Object::Array(vec![
            Object::Integer(5),
            Object::Integer(5),
            Object::Integer(590),
            Object::Integer(770),
        ]))
    );
}

/// The leaf carries its OWN /CropBox while the ancestor /Pages offers a
/// different inheritable one; the leaf's own value must win (no inherited
/// overwrite).
fn own_cropbox_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /CropBox [5 5 590 770] >>",
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /CropBox [1 1 400 500] >>",
            ),
        ],
        1,
    )
}

#[test]
fn own_cropbox_is_preserved() {
    let src = own_cropbox_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // The leaf's own /CropBox wins over the ancestor's inheritable one.
    assert_eq!(
        leaf.get("CropBox"),
        Some(&Object::Array(vec![
            Object::Integer(1),
            Object::Integer(1),
            Object::Integer(400),
            Object::Integer(500),
        ]))
    );
}

/// Two-level page tree: root /Pages (obj 2) -> intermediate /Pages (obj 5)
/// carrying both /MediaBox and /CropBox -> leaf (obj 3) with neither. Both
/// boxes must be materialized onto the extracted leaf through the
/// intermediate node.
fn intermediate_boxes_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [5 0 R] /Count 1 >>"),
            (
                5,
                "<< /Type /Pages /Parent 2 0 R /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] /CropBox [10 10 600 780] >>",
            ),
            (3, "<< /Type /Page /Parent 5 0 R >>"),
        ],
        1,
    )
}

#[test]
fn materializes_intermediate_mediabox_and_cropbox() {
    let src = intermediate_boxes_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[0]).unwrap();
    let leaf = only_leaf(&mut out);

    // Inherited /MediaBox materialized onto the leaf.
    assert_eq!(
        leaf.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]))
    );
    // Inherited /CropBox materialized onto the leaf.
    assert_eq!(
        leaf.get("CropBox"),
        Some(&Object::Array(vec![
            Object::Integer(10),
            Object::Integer(10),
            Object::Integer(600),
            Object::Integer(780),
        ]))
    );
}

/// Ancestor /Pages stores /MediaBox as an INDIRECT reference (obj 6), the qpdf
/// shared-array pattern. The leaf (obj 3) inherits it. Exercises rewrite_refs'
/// Object::Reference branch: the extracted leaf's /MediaBox must resolve to a
/// live array, not become Null.
fn indirect_inherited_mediabox_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox 6 0 R >>",
            ),
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
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(321),
            Object::Integer(654),
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
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]))
    );

    let mut p1 = extract_page(&mut source, 1).unwrap();
    let leaf1 = only_leaf(&mut p1);
    assert_eq!(
        leaf1.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(300),
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

/// Count how many live objects in `doc` carry the given /Type name.
fn count_type(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, type_name: &[u8]) -> usize {
    let mut n = 0;
    for r in doc.live_object_refs() {
        if let Ok(obj) = doc.resolve(r) {
            let dict = match &obj {
                Object::Dictionary(d) => Some(d.clone()),
                Object::Stream(s) => Some(s.dict.clone()),
                _ => None,
            };
            if let Some(d) = dict {
                if d.get("Type").and_then(|o| o.as_name()) == Some(type_name) {
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

    // Page 1's shared font survives; page 2's exclusive image was never copied.
    assert_eq!(
        count_subtype(&mut out, b"Type1"),
        1,
        "shared font must be present"
    );
    assert_eq!(
        count_subtype(&mut out, b"Image"),
        0,
        "page 2's image must not leak in"
    );

    // Exactly one /Pages node — the fresh root. The copied ancestor /Pages node
    // must have been pruned by the sweep (no orphan left in the object table).
    assert_eq!(
        count_type(&mut out, b"Pages"),
        1,
        "no orphan ancestor /Pages node"
    );
    assert_eq!(pages::page_refs(&mut out).unwrap().len(), 1);

    // Sanity: the pruned document still writes and reopens to a single page,
    // with no orphan /Pages reappearing.
    let mut bytes = Vec::new();
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    write_pdf_with_options(&mut out, &mut bytes, &opts).unwrap();
    let mut rt = Pdf::open_mem_owned(bytes).unwrap();
    assert_eq!(pages::page_refs(&mut rt).unwrap().len(), 1);
    assert_eq!(
        count_type(&mut rt, b"Pages"),
        1,
        "no orphan /Pages after round-trip"
    );
}

#[test]
fn extracted_contents_match_source_page() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let src_pages = pages::page_refs(&mut source).unwrap();
    let src_leaf = source
        .resolve_borrowed(src_pages[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let src_contents_ref = match src_leaf.get("Contents") {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Contents ref, got {other:?}"),
    };
    let src_stream = match source.resolve(src_contents_ref).unwrap() {
        Object::Stream(s) => s,
        other => panic!("expected stream, got {other:?}"),
    };

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);
    let out_contents_ref = match leaf.get("Contents") {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Contents ref, got {other:?}"),
    };
    let out_stream = match out.resolve(out_contents_ref).unwrap() {
        Object::Stream(s) => s,
        other => panic!("expected stream, got {other:?}"),
    };

    assert_eq!(
        out_stream.data, src_stream.data,
        "content stream bytes must be identical"
    );
}

#[test]
fn out_of_range_index_errors() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();
    let err = match extract_page(&mut source, 2) {
        Ok(_) => panic!("index 2 out of range should error, got Ok"),
        Err(e) => e,
    };
    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "index 2 out of range should yield Error::Unsupported, got {err:?}"
    );
}

#[test]
fn source_is_not_modified_by_extract() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();
    let before = pages::page_refs(&mut source).unwrap();
    assert_eq!(before.len(), 2);

    let _ = extract_page(&mut source, 0).unwrap();

    // Source still has both pages, unchanged refs/order.
    let after = pages::page_refs(&mut source).unwrap();
    assert_eq!(
        after, before,
        "extract_page must not mutate the source page tree"
    );
}

/// Page 0 (obj 3) has a Link annotation (obj 5) whose explicit /Dest targets the
/// SIBLING page (obj 4). The sibling and its ancestor /Pages currently leak into
/// the extracted doc.
fn cross_page_link_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 7 0 R /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>"),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (7, "<< /Length 15 >>\nstream\nBT /F1 12 Tf ET\nendstream"),
        ],
        1,
    )
}

#[test]
fn cross_page_link_keeps_dest_and_nulls_removed_page() {
    // qpdf keeps the explicit cross-page /Dest carrier and replaces the copied
    // unselected page object with null.
    let src = cross_page_link_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    assert_eq!(
        count_type(&mut out, b"Pages"),
        1,
        "ancestor /Pages must be pruned once the copied sibling becomes null"
    );

    // Annotation and /Dest are retained; the referenced page resolves to null.
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(leaf_refs.len(), 1);
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annots = match leaf.get("Annots") {
        Some(Object::Array(a)) => a.clone(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    assert_eq!(annots.len(), 1, "annotation must be retained, not dropped");
    let annot_ref = annots[0].as_ref_id().expect("annot is an indirect ref");
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert_destination_page_is_null(
        &mut out,
        annot.get("Dest").cloned().expect("/Dest retained"),
        "cross-page /Dest target must resolve to null",
    );
    assert_eq!(
        annot.get("Subtype").and_then(|o| o.as_name()),
        Some(&b"Link"[..]),
        "annotation subtype preserved"
    );

    // CORE GUARANTEE: extracted leaf content + resources intact.
    let contents_ref = match leaf.get("Contents") {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Contents ref, got {other:?}"),
    };
    let stream = match out.resolve(contents_ref).unwrap() {
        Object::Stream(s) => s,
        other => panic!("expected content stream, got {other:?}"),
    };
    assert_eq!(
        stream.data, b"BT /F1 12 Tf ET",
        "leaf content stream intact"
    );
    let res = leaf
        .get("Resources")
        .and_then(|o| o.as_dict())
        .expect("/Resources present");
    assert!(
        res.get("Font")
            .and_then(|o| o.as_dict())
            .and_then(|f| f.get("F1"))
            .is_some(),
        "leaf /Resources /Font /F1 intact"
    );
}

#[test]
fn self_page_link_is_preserved() {
    // /Dest targets the extracted page itself, so it remains a live page ref.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [3 0 R /Fit] >>",
            ),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert!(
        annot.get("Dest").is_some(),
        "self-link /Dest must be preserved"
    );
}

#[test]
fn named_dest_is_preserved_no_leak() {
    // A named destination (/Dest is a name) carries no in-doc page ref, so it
    // never pulled a sibling in; leave it untouched.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest /SomeNamedDest >>",
            ),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "named dest must not leak a sibling"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert_eq!(
        annot.get("Dest").and_then(|o| o.as_name()),
        Some(&b"SomeNamedDest"[..]),
        "named /Dest preserved",
    );
}

#[test]
fn action_goto_keeps_d_and_nulls_removed_page() {
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D [4 0 R /Fit] >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    // The /A action and /D are retained; the referenced page is null.
    let action = annot
        .get("A")
        .and_then(|o| o.as_dict())
        .expect("/A action retained");
    assert_eq!(
        action.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "/A action is still a GoTo"
    );
    assert_destination_page_is_null(
        &mut out,
        action.get("D").cloned().expect("/D retained"),
        "cross-page /D target must resolve to null",
    );
}

#[test]
fn annot_aa_goto_keeps_d_and_nulls_removed_page() {
    // Annotation /AA additional-actions dict: an /U subaction is a cross-page
    // GoTo. Its /D, /AA, and /U remain while the copied page becomes null.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /AA << /U << /S /GoTo /D [4 0 R /Fit] >> >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let aa = annot
        .get("AA")
        .and_then(|o| o.as_dict())
        .expect("/AA retained");
    let u = aa
        .get("U")
        .and_then(|o| o.as_dict())
        .expect("/AA /U retained");
    assert_eq!(
        u.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "/AA /U is still a GoTo"
    );
    assert_destination_page_is_null(
        &mut out,
        u.get("D").cloned().expect("/AA /U /D retained"),
        "cross-page /AA /U /D target must resolve to null",
    );
}

#[test]
fn action_next_chain_keeps_d_and_nulls_removed_page() {
    // /A is a /URI action whose /Next is a cross-page GoTo. The URI action is
    // untouched; the chained GoTo's /D remains and targets null.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (http://example.com) /Next << /S /GoTo /D [4 0 R /Fit] >> >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let action = annot
        .get("A")
        .and_then(|o| o.as_dict())
        .expect("/A retained");
    assert_eq!(
        action.get("S").and_then(|o| o.as_name()),
        Some(&b"URI"[..]),
        "/A is still the URI action"
    );
    assert!(
        action.get("URI").is_some(),
        "/A /URI value must be preserved"
    );
    let next = action
        .get("Next")
        .and_then(|o| o.as_dict())
        .expect("/A /Next retained");
    assert_eq!(
        next.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "/Next action is still a GoTo"
    );
    assert_destination_page_is_null(
        &mut out,
        next.get("D").cloned().expect("/Next /D retained"),
        "cross-page /Next /D target must resolve to null",
    );
}

#[test]
fn next_array_goto_keeps_d_and_nulls_removed_page() {
    // /Next is an ARRAY of actions: [URI, GoTo]. The GoTo /D targets null.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (x) /Next [ << /S /URI /URI (y) >> << /S /GoTo /D [4 0 R /Fit] >> ] >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let next = annot
        .get("A")
        .and_then(|o| o.as_dict())
        .and_then(|a| a.get("Next"))
        .cloned()
        .expect("/A /Next retained");
    let elems = match next {
        Object::Array(a) => a,
        other => panic!("expected /Next array, got {other:?}"),
    };
    assert_eq!(elems.len(), 2, "both /Next actions retained");
    let first = elems[0].as_dict().expect("first /Next element is a dict");
    assert!(
        first.get("URI").is_some(),
        "first (URI) /Next action untouched"
    );
    let second = elems[1].as_dict().expect("second /Next element is a dict");
    assert_eq!(
        second.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "second /Next action is still a GoTo"
    );
    assert_destination_page_is_null(
        &mut out,
        second.get("D").cloned().expect("array GoTo /D retained"),
        "cross-page array GoTo /D target must resolve to null",
    );
}

#[test]
fn page_level_aa_goto_keeps_d_and_nulls_removed_page() {
    // The extracted page leaf's OWN /AA (open action) is a cross-page GoTo.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /AA << /O << /S /GoTo /D [4 0 R /Fit] >> >> >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let o = leaf
        .get("AA")
        .and_then(|o| o.as_dict())
        .and_then(|aa| aa.get("O"))
        .and_then(|o| o.as_dict())
        .expect("page /AA /O retained");
    assert_eq!(
        o.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "page /AA /O is still a GoTo"
    );
    assert_destination_page_is_null(
        &mut out,
        o.get("D").cloned().expect("page /AA /O /D retained"),
        "cross-page page /AA /O /D target must resolve to null",
    );
}

#[test]
fn indirect_action_goto_keeps_d_and_nulls_removed_page() {
    // /A is an indirect reference to a GoTo action (obj 8). The action and /D
    // remain indirect while the copied unselected page becomes null.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 8 0 R >>",
            ),
            (8, "<< /S /GoTo /D [4 0 R /Fit] >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    // /A remains an indirect ref to the unchanged action carrier.
    let action_ref = annot.get("A").and_then(Object::as_ref_id).expect("/A ref");
    let action = out
        .resolve_borrowed(action_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert_eq!(
        action.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "indirect action is still a GoTo"
    );
    assert_destination_page_is_null(
        &mut out,
        action
            .get("D")
            .cloned()
            .expect("indirect action /D retained"),
        "cross-page indirect action /D target must resolve to null",
    );
}

#[test]
fn selflink_dest_and_crosspage_action_carriers_are_preserved() {
    // Independence: a self-link /Dest (kept) coexists with a cross-page /A GoTo
    // to an unselected page. Both carriers stay; the latter target is null.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [3 0 R /Fit] /A << /S /GoTo /D [4 0 R /Fit] >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert!(
        annot.get("Dest").is_some(),
        "self-link /Dest must be preserved"
    );
    let action = annot
        .get("A")
        .and_then(|o| o.as_dict())
        .expect("/A action retained");
    assert_destination_page_is_null(
        &mut out,
        action.get("D").cloned().expect("cross-page /A /D retained"),
        "cross-page /A GoTo /D target must resolve to null",
    );
}

#[test]
fn action_uri_is_preserved() {
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (http://example.com) >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert!(annot.get("A").is_some(), "/A URI must be preserved");
}

#[test]
fn indirect_dest_is_preserved_and_removed_page_is_null() {
    // /Dest is an indirect ref (8 0 R) to the [sibling /Fit] array.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest 8 0 R >>",
            ),
            (8, "[4 0 R /Fit]"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    assert_destination_page_is_null(
        &mut out,
        annot.get("Dest").cloned().expect("indirect /Dest retained"),
        "indirect /Dest target must resolve to null",
    );
}

#[test]
fn indirect_aa_goto_keeps_d_and_nulls_removed_page() {
    // /AA is an indirect ref (9 0 R) to the additional-actions dict; its /U
    // subaction is a cross-page GoTo. The carrier remains indirect.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /AA 9 0 R >>",
            ),
            (9, "<< /U << /S /GoTo /D [4 0 R /Fit] >> >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    // /AA stays an indirect reference.
    let aa_ref = match annot.get("AA") {
        Some(Object::Reference(r)) => *r,
        other => panic!("/AA must stay indirect, got {other:?}"),
    };
    // Resolve the indirect /AA and confirm /U kept a /D that targets null.
    let aa = out
        .resolve_borrowed(aa_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .expect("/AA resolves to a dict");
    let u = aa
        .get("U")
        .and_then(|o| o.as_dict())
        .expect("/AA /U present");
    assert_eq!(
        u.get("S").and_then(|o| o.as_name()),
        Some(&b"GoTo"[..]),
        "action kept"
    );
    assert_destination_page_is_null(
        &mut out,
        u.get("D").cloned().expect("indirect /AA /U /D retained"),
        "indirect /AA /U /D target must resolve to null",
    );
}

#[test]
fn indirect_next_array_goto_keeps_d_and_nulls_removed_page() {
    // /A /Next is an indirect ref (10 0 R) to an ARRAY of actions; one is a
    // cross-page GoTo. The whole carrier chain remains intact.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (http://x) /Next 10 0 R >> >>"),
            (10, "[ << /S /GoTo /D [4 0 R /Fit] >> ]"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annot_ref = leaf
        .get("Annots")
        .and_then(Object::as_array)
        .and_then(|annots| annots.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").and_then(Object::as_dict).unwrap();
    let next = resolve_indirect_value(
        &mut out,
        action
            .get("Next")
            .cloned()
            .expect("indirect /Next retained"),
    )
    .into_array()
    .unwrap();
    let goto = next[0].as_dict().unwrap();
    assert_destination_page_is_null(
        &mut out,
        goto.get("D").cloned().expect("/Next GoTo /D retained"),
        "indirect /Next array GoTo /D target must resolve to null",
    );
}

fn long_indirect_next_array_pdf() -> Vec<u8> {
    let mut owned: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),
        ),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"
                .into(),
        ),
        (
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>".into(),
        ),
        (
            5,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (https://example.test) /Next 10 0 R >> >>"
                .into(),
        ),
    ];
    for number in 10..80 {
        owned.push((number, format!("[{} 0 R]", number + 1)));
    }
    owned.push((80, "[<< /S /GoTo /D [4 0 R /Fit] >>]".into()));
    let borrowed: Vec<(u32, &str)> = owned
        .iter()
        .map(|(number, body)| (*number, body.as_str()))
        .collect();
    build_pdf(&borrowed, 1)
}

fn action_after_71_array_holders(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    mut value: Object,
) -> flpdf::Dictionary {
    for _ in 0..=70 {
        let concrete = match value {
            Object::Reference(reference) => doc.resolve(reference).unwrap(),
            direct => direct,
        };
        let mut items = concrete.into_array().expect("singleton action array");
        assert_eq!(items.len(), 1);
        value = items.remove(0);
    }
    value.into_dict().expect("terminal GoTo action")
}

#[test]
fn long_indirect_next_array_keeps_carrier_and_nulls_removed_page() {
    let bytes = long_indirect_next_array_pdf();
    let mut source = Pdf::open_mem(&bytes).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();

    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = leaf
        .get("Annots")
        .and_then(Object::as_array)
        .and_then(|items| items.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").and_then(Object::as_dict).unwrap();
    let terminal = action_after_71_array_holders(
        &mut out,
        action.get("Next").cloned().expect("/Next is preserved"),
    );
    let removed_page = terminal
        .get("D")
        .and_then(Object::as_array)
        .and_then(|items| items.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    assert!(matches!(out.resolve(removed_page).unwrap(), Object::Null));
}

// --- Additional coverage for indirect carrier shapes ---

#[test]
fn indirect_annots_array_keeps_dest_and_nulls_removed_page() {
    // /Annots is an indirect ref (9 0 R) to the array. The annotation's
    // cross-page /Dest remains and targets the nulled copied page.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 9 0 R >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>",
            ),
            (9, "[5 0 R]"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annots = resolve_indirect_value(
        &mut out,
        leaf.get("Annots")
            .cloned()
            .expect("indirect /Annots retained"),
    )
    .into_array()
    .unwrap();
    let annot_ref = annots[0].as_ref_id().unwrap();
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    assert_destination_page_is_null(
        &mut out,
        annot.get("Dest").cloned().expect("/Dest retained"),
        "indirect /Annots /Dest target must resolve to null",
    );
}

#[test]
fn aa_with_only_local_subaction_is_unchanged() {
    // /AA carries a single non-GoTo (/URI) subaction and no page reference.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /AA << /U << /S /URI /URI (http://example.com) >> >> >>",
            ),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let aa = annot.get("AA").and_then(|o| o.as_dict()).expect("/AA kept");
    let u = aa.get("U").and_then(|o| o.as_dict()).expect("/AA /U kept");
    assert_eq!(
        u.get("S").and_then(|o| o.as_name()),
        Some(&b"URI"[..]),
        "/URI subaction untouched"
    );
}

#[test]
fn indirect_next_cycle_is_preserved_and_removed_page_is_null() {
    // /A -> action 8 whose /Next is 9, and 9's /Next is 8 (an A<->B indirect
    // cycle). Both are cross-page GoTos. Generic closure traversal terminates
    // via its visited set, and both /D carriers target the same nulled page.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 8 0 R >>",
            ),
            (8, "<< /S /GoTo /D [4 0 R /Fit] /Next 9 0 R >>"),
            (9, "<< /S /GoTo /D [4 0 R /Fit] /Next 8 0 R >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annot_ref = leaf
        .get("Annots")
        .and_then(Object::as_array)
        .and_then(|annots| annots.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let first = resolve_indirect_value(
        &mut out,
        annot.get("A").cloned().expect("indirect /A retained"),
    )
    .into_dict()
    .unwrap();
    assert_destination_page_is_null(
        &mut out,
        first.get("D").cloned().expect("first /D retained"),
        "first cyclic action /D target must resolve to null",
    );
    let second = resolve_indirect_value(
        &mut out,
        first.get("Next").cloned().expect("first /Next retained"),
    )
    .into_dict()
    .unwrap();
    assert_destination_page_is_null(
        &mut out,
        second.get("D").cloned().expect("second /D retained"),
        "second cyclic action /D target must resolve to null",
    );
}

#[test]
fn action_goto_self_link_is_preserved() {
    // An /A /GoTo whose /D targets the extracted page itself: the /D is retained
    // (self-link), exercising the "dest not absent -> re-insert /D" arm.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D [3 0 R /Fit] >> >>",
            ),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = out
        .resolve_borrowed(leaf_refs[0])
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let annot_ref = match leaf.get("Annots") {
        Some(Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("got {other:?}"),
    };
    let annot = out
        .resolve_borrowed(annot_ref)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let a = annot.get("A").and_then(|o| o.as_dict()).expect("/A kept");
    assert!(
        a.get("D").is_some(),
        "self-link /A GoTo /D must be preserved"
    );
}

#[test]
fn deep_inline_next_chain_is_preserved() {
    // A deeply nested inline /Next chain of /URI actions carries no page ref,
    // so extraction preserves it subject only to the generic inline-depth cap.
    let mut a = String::from("<< /S /URI /URI (http://leaf) >>");
    for _ in 0..70 {
        a = format!("<< /S /URI /URI (http://x) /Next {a} >>");
    }
    let annot = format!("<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A {a} >>");
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
            (5, annot.as_str()),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
}

/// GoTo /SD -> StructElem(/Pg sibling) keeps the carrier chain reachable while
/// the copied sibling page itself becomes null. (ISO 32000-2 §12.6.4.3.)
fn cross_page_sd_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /SD [8 0 R /Fit] >> >>"),
            (8, "<< /Type /StructElem /S /P /Pg 4 0 R >>"),
        ],
        1,
    )
}

#[test]
fn action_goto_sd_keeps_carrier_and_nulls_removed_page() {
    let mut src = Pdf::open(std::io::Cursor::new(cross_page_sd_pdf())).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    assert_eq!(
        count_type(&mut out, b"StructElem"),
        1,
        "StructElem carrier reachable through /SD must be retained"
    );
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").unwrap().as_dict().unwrap();
    assert_eq!(
        action.get("S"),
        Some(&flpdf::Object::Name(b"GoTo".to_vec())),
        "GoTo action retained"
    );
    let struct_ref =
        destination_page_ref(&mut out, action.get("SD").cloned().expect("/SD retained"));
    let struct_elem = out.resolve(struct_ref).unwrap().into_dict().unwrap();
    assert_reference_target_is_null(
        &mut out,
        struct_elem.get("Pg").expect("StructElem /Pg retained"),
        "/SD StructElem /Pg target must resolve to null",
    );
}

#[test]
fn action_goto_sd_self_page_is_preserved() {
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /SD [8 0 R /Fit] >> >>"),
            (8, "<< /Type /StructElem /S /P /Pg 3 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").unwrap().as_dict().unwrap();
    assert!(
        action.get("SD").is_some(),
        "self-page /SD must be preserved"
    );
}

#[test]
fn action_goto_sd_named_dest_is_preserved() {
    // A named structure destination (/SD is a name, not an array) carries no
    // in-doc page ref, so it never pulled a sibling in; leave it untouched.
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /SD /SomeStructDest >> >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").unwrap().as_dict().unwrap();
    assert!(
        action.get("SD").is_some(),
        "named structure destination /SD must be preserved"
    );
}

#[test]
fn annot_p_is_preserved_and_removed_page_is_null() {
    // A malformed annotation /P points at the SIBLING page (obj 4); the closure
    // copies the sibling, whose copied object is then replaced with null.
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                5,
                "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 4 0 R >>",
            ),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    assert_reference_target_is_null(
        &mut out,
        annot.get("P").expect("annotation /P retained"),
        "annotation /P target must resolve to null",
    );
}

#[test]
fn annot_p_self_page_is_preserved() {
    // /P points at the extracted page itself: kept (remapped to the new ref).
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (
                5,
                "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 3 0 R >>",
            ),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    assert!(annot.get("P").is_some(), "self-page /P must be preserved");
}

#[test]
fn bead_p_carrier_is_preserved_and_removed_page_is_null() {
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [10 0 R] >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [11 0 R] >>",
            ),
            // Bead ring: 10 (on kept page) <-> 11 (on sibling page).
            (
                10,
                "<< /T 12 0 R /N 11 0 R /V 11 0 R /P 3 0 R /R [0 0 10 10] >>",
            ),
            (
                11,
                "<< /T 12 0 R /N 10 0 R /V 10 0 R /P 4 0 R /R [0 0 10 10] >>",
            ),
            (12, "<< /T (Article) /F 10 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    // The kept page's /B is retained (qpdf keeps the ring).
    let leaf = only_leaf(&mut out);
    assert!(leaf.get("B").is_some(), "page /B must be retained");

    // The kept page's own bead (obj 10) targets the kept page, so its /P must
    // stay live and still resolve to a /Type /Page dictionary.
    let bead_ref = match leaf.get("B") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /B array, got {other:?}"),
    };
    let bead = out.resolve(bead_ref).unwrap().into_dict().unwrap();
    let p_ref = bead
        .get("P")
        .and_then(flpdf::Object::as_ref_id)
        .expect("kept bead /P must be preserved as a page reference");
    let p_page = out.resolve(p_ref).unwrap().into_dict().unwrap();
    assert_eq!(
        p_page.get("Type"),
        Some(&flpdf::Object::Name(b"Page".to_vec())),
        "preserved bead /P must resolve to a /Type /Page"
    );

    let sibling_bead =
        resolve_indirect_value(&mut out, bead.get("N").cloned().expect("bead /N retained"))
            .into_dict()
            .unwrap();
    assert_reference_target_is_null(
        &mut out,
        sibling_bead.get("P").expect("sibling bead /P retained"),
        "sibling bead /P target must resolve to null",
    );
}

// --- extract_pages: multi-page extraction (dedup, ordering, duplicates) ---

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

/// Resolve a leaf page's inline /Resources -> /Font -> first entry's
/// reference -> /BaseFont name.
fn leaf_font_basefont(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, leaf: flpdf::ObjectRef) -> Vec<u8> {
    let leaf = doc
        .resolve_borrowed(leaf)
        .unwrap()
        .as_dict()
        .cloned()
        .unwrap();
    let font_ref = leaf
        .get("Resources")
        .and_then(|o| o.as_dict())
        .and_then(|r| r.get("Font"))
        .and_then(|o| o.as_dict())
        .and_then(|f| f.iter().next().map(|(_, v)| v.clone()))
        .and_then(|v| v.as_ref_id())
        .expect("leaf /Resources /Font first entry must be an indirect ref");
    let font = doc.resolve(font_ref).unwrap().into_dict().unwrap();
    font.get("BaseFont")
        .and_then(|o| o.as_name())
        .expect("/BaseFont")
        .to_vec()
}

#[test]
fn extract_pages_copies_shared_resource_once() {
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2, "extracted doc must have two pages");
    let root = pages_dict(&mut out);
    assert_eq!(root.get("Count"), Some(&Object::Integer(2)));

    assert_eq!(
        count_font_objects(&mut out, b"Helvetica"),
        1,
        "the shared font must be copied exactly once"
    );
    assert_eq!(
        count_font_objects(&mut out, b"Courier"),
        0,
        "page 3's exclusive font must not leak in"
    );
}

#[test]
fn extract_pages_object_count_sublinear_vs_per_page_extracts() {
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let combined = extract_pages(&mut source, &[0, 1])
        .unwrap()
        .object_refs()
        .len();
    let separate = extract_page(&mut source, 0).unwrap().object_refs().len()
        + extract_page(&mut source, 1).unwrap().object_refs().len();
    assert!(
        combined < separate,
        "single-map extract must dedup shared objects: {combined} >= {separate}"
    );
}

#[test]
fn extract_pages_preserves_selection_order() {
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[2, 0]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2);
    assert_eq!(
        leaf_font_basefont(&mut out, page_refs[0]),
        b"Courier".to_vec(),
        "first output page must be source page 2 (Courier font)"
    );
    assert_eq!(
        leaf_font_basefont(&mut out, page_refs[1]),
        b"Helvetica".to_vec(),
        "second output page must be source page 0 (Helvetica font)"
    );
}

#[test]
fn extract_pages_empty_selection_errors() {
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();
    let err = match extract_pages(&mut source, &[]) {
        Ok(_) => panic!("empty selection should error, got Ok"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, flpdf::Error::Unsupported(msg) if msg == "empty page selection"),
        "empty selection should yield Error::Unsupported(\"empty page selection\"), got {err:?}"
    );
}

#[test]
fn extract_pages_out_of_range_index_errors() {
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();
    let err = match extract_pages(&mut source, &[0, 3]) {
        Ok(_) => panic!("index 3 out of range should error, got Ok"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, flpdf::Error::Unsupported(msg)
            if msg == "page index 3 out of range (document has 3 pages)"),
        "got {err:?}"
    );
}

#[test]
fn extract_pages_duplicate_index_shallow_clones_page() {
    // qpdf-compatible duplicate selection: the second occurrence becomes a
    // fresh page object whose sub-objects (/Contents, /Resources) stay shared
    // with the first copy.
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 0]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2, "duplicate selection yields two kids");
    assert_ne!(
        page_refs[0], page_refs[1],
        "duplicate kids must be distinct page objects"
    );
    let root = pages_dict(&mut out);
    assert_eq!(root.get("Count"), Some(&Object::Integer(2)));

    // Sub-objects stay SHARED: both kids reference the same /Contents stream.
    let contents_ref = |doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, r: flpdf::ObjectRef| {
        doc.resolve_borrowed(r)
            .unwrap()
            .as_dict()
            .and_then(|d| d.get("Contents"))
            .and_then(Object::as_ref_id)
            .expect("/Contents ref")
    };
    assert_eq!(
        contents_ref(&mut out, page_refs[0]),
        contents_ref(&mut out, page_refs[1]),
        "duplicate pages must share the same /Contents object"
    );
    assert_eq!(
        count_font_objects(&mut out, b"Helvetica"),
        1,
        "the shared font is still copied exactly once"
    );
}

/// Page 3 carries two link annotations: one to page 4 (/Dest [4 0 R /Fit]),
/// one to page 5 (/Dest [5 0 R /Fit]).
fn three_page_linked_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R 7 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                6,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>",
            ),
            (
                7,
                "<< /Type /Annot /Subtype /Link /Rect [20 0 30 10] /Dest [5 0 R /Fit] >>",
            ),
        ],
        1,
    )
}

#[test]
fn extract_pages_keeps_dest_between_selected_pages() {
    // A /Dest from one selected page to ANOTHER selected page is remapped and
    // kept (the target is present in the output); a /Dest to a NON-selected
    // page is also kept, but its copied page target is null.
    let src = three_page_linked_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2, "two selected pages enumerated");
    let second_page_ref = page_refs[1];

    let leaf = out.resolve(page_refs[0]).unwrap().into_dict().unwrap();
    let annot_refs: Vec<flpdf::ObjectRef> = match leaf.get("Annots") {
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_ref_id).collect(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    assert_eq!(annot_refs.len(), 2, "both annotations retained");

    let mut kept = 0;
    let mut nulled = 0;
    for annot_ref in annot_refs {
        let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
        let target_ref = destination_page_ref(
            &mut out,
            annot.get("Dest").cloned().expect("/Dest retained"),
        );
        if target_ref == second_page_ref {
            kept += 1;
        } else {
            assert!(
                matches!(out.resolve(target_ref).unwrap(), Object::Null),
                "non-selected /Dest target must resolve to null"
            );
            nulled += 1;
        }
    }
    assert_eq!(kept, 1, "the link to selected page 4 must survive");
    assert_eq!(
        nulled, 1,
        "the link to non-selected page 5 must target null"
    );

    // Page 5's copied object remains reachable but is null: exactly the two
    // selected live /Page dictionaries remain.
    assert_eq!(
        count_type(&mut out, b"Page"),
        2,
        "non-selected copied page must be null"
    );
}

#[test]
fn extract_pages_materializes_inherited_attrs_per_parent() {
    // Two leaves under DIFFERENT intermediate /Pages parents: each leaf must
    // materialize the attributes inherited from ITS OWN parent chain, not the
    // other leaf's.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /MediaBox [0 0 100 200] /Rotate 90 >>"),
            (4, "<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 /MediaBox [0 0 300 400] >>"),
            (5, "<< /Type /Page /Parent 3 0 R >>"),
            (6, "<< /Type /Page /Parent 4 0 R >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem(&bytes).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2);

    let leaf0 = out.resolve(page_refs[0]).unwrap().into_dict().unwrap();
    assert_eq!(
        leaf0.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(200),
        ])),
        "leaf 0 inherits /MediaBox from its own parent (obj 3)"
    );
    assert_eq!(
        leaf0.get("Rotate"),
        Some(&Object::Integer(90)),
        "leaf 0 inherits /Rotate 90 from its own parent (obj 3)"
    );

    let leaf1 = out.resolve(page_refs[1]).unwrap().into_dict().unwrap();
    assert_eq!(
        leaf1.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(300),
            Object::Integer(400),
        ])),
        "leaf 1 inherits /MediaBox from its own parent (obj 4), not leaf 0's"
    );
    // flpdf materializes /Rotate explicitly on every extracted leaf; with no
    // /Rotate anywhere in leaf 1's parent chain, the default 0 is written out.
    assert_eq!(
        leaf1.get("Rotate"),
        Some(&Object::Integer(0)),
        "leaf 1 must NOT inherit leaf 0's /Rotate 90; the default 0 is materialized"
    );
}

#[test]
fn bead_p_via_indirect_chain_is_preserved_and_removed_page_is_null() {
    // The sibling bead (obj 11) is reached from the on-page bead's /N through an
    // indirect-reference chain (obj 13 is itself `11 0 R`). Generic closure
    // traversal follows the chain; bead 11's /P 4 0 R remains and resolves to
    // the nulled copied page.
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [10 0 R] >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [11 0 R] >>",
            ),
            // On-page bead 10 links to the sibling bead through obj 13, which is
            // an indirect reference to bead 11 (a reference-to-reference chain).
            (
                10,
                "<< /T 12 0 R /N 13 0 R /V 13 0 R /P 3 0 R /R [0 0 10 10] >>",
            ),
            (13, "11 0 R"),
            (
                11,
                "<< /T 12 0 R /N 10 0 R /V 10 0 R /P 4 0 R /R [0 0 10 10] >>",
            ),
            (12, "<< /T (Article) /F 10 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    assert!(leaf.get("B").is_some(), "page /B must be retained");
    let bead_ref = leaf
        .get("B")
        .and_then(Object::as_array)
        .and_then(|beads| beads.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    let bead = out.resolve(bead_ref).unwrap().into_dict().unwrap();
    let sibling_bead = resolve_indirect_value(
        &mut out,
        bead.get("N").cloned().expect("indirect bead /N retained"),
    )
    .into_dict()
    .unwrap();
    assert_reference_target_is_null(
        &mut out,
        sibling_bead.get("P").expect("sibling bead /P retained"),
        "indirect sibling bead /P target must resolve to null",
    );
}

// ---------------------------------------------------------------------------
// /PageLabels reconstruction
// ---------------------------------------------------------------------------

/// Four-page document with `/PageLabels`: roman lowercase for pages 0-1,
/// decimal (restart at 1) for pages 2-3.
fn four_page_pdf_with_labels() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels \
                 << /Nums [0 << /S /r >> 2 << /S /D /St 1 >>] >> >>",
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

#[test]
fn extract_pages_reconstructs_labels_in_selection_order_with_duplicates() {
    // Selection order 2,0,2 (0-based): src page2 (decimal "1"), src page0
    // (roman "i"), src page2 again (duplicate -> decimal "1" again).
    // Verified byte-for-byte against qpdf 11.9.0 (`--empty --pages src.pdf
    // 3,1,3 -- out.pdf`), which reconstructs the identical 3-entry /Nums.
    let src = four_page_pdf_with_labels();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[2, 0, 2]).unwrap();

    let mut h = out.page_labels();
    assert_eq!(h.label_string_for_page(0).unwrap(), "1");
    assert_eq!(h.label_string_for_page(1).unwrap(), "i");
    assert_eq!(h.label_string_for_page(2).unwrap(), "1");
    let ranges = h.ranges().unwrap();
    assert_eq!(ranges.len(), 3, "no fold: styles alternate, got {ranges:?}");
}

#[test]
fn extract_pages_folds_redundant_sequential_labels() {
    // Identity selection: labels continue exactly as in the source (roman i,
    // ii, then decimal 1, 2), so the reconstructed tree folds down to the 2
    // real range starts (0 and 2) rather than one entry per page. Verified
    // against qpdf 11.9.0 (`--empty --pages src.pdf 1,2,3,4 -- out.pdf`).
    let src = four_page_pdf_with_labels();
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1, 2, 3]).unwrap();

    let mut h = out.page_labels();
    let ranges = h.ranges().unwrap();
    assert_eq!(
        ranges.len(),
        2,
        "sequential/continuous entries fold to the 2 real range starts, got {ranges:?}"
    );
    assert_eq!(h.label_string_for_page(0).unwrap(), "i");
    assert_eq!(h.label_string_for_page(1).unwrap(), "ii");
    assert_eq!(h.label_string_for_page(2).unwrap(), "1");
    assert_eq!(h.label_string_for_page(3).unwrap(), "2");
}

#[test]
fn extract_pages_without_source_labels_has_none() {
    let src = three_page_shared_font_pdf(); // no /PageLabels
    let mut source = Pdf::open_mem(&src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let mut h = out.page_labels();
    assert!(
        !h.has_page_labels().unwrap(),
        "a source with no /PageLabels must not gain one"
    );
}
