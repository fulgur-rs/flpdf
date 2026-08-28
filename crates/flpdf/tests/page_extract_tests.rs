//! Integration tests for [`flpdf::extract_page`] / [`flpdf::extract_pages`].

use flpdf::{extract_page, extract_pages, pages, ObjectHandle, ObjectRef, Pdf};
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

/// An indirect null resource entry is visible to the legacy pre-closed copier
/// but qpdf's `copyForeignObject` omits it through `getKeys()` null filtering.
fn page_with_indirect_null_resource_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R /Null 4 0 R >> >> >>"),
            (4, "null"),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    )
}

/// Resolve the catalog's /Pages dict from a freshly-extracted document.
fn resolved_handle(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, object_ref: ObjectRef) -> ObjectHandle {
    let handle = doc.get_object_handle(object_ref);
    doc.resolve(&handle).unwrap();
    handle
}

fn resolved_key(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    object: &ObjectHandle,
    key: &[u8],
) -> ObjectHandle {
    let value = object.get_key(key);
    doc.resolve(&value).unwrap();
    value
}

fn pages_dict(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> ObjectHandle {
    let catalog = resolved_handle(doc, doc.root_ref().unwrap());
    let pages_ref = resolved_key(doc, &catalog, b"/Pages")
        .object_ref()
        .expect("/Pages ref");
    resolved_handle(doc, pages_ref)
}

/// Fetch the single extracted leaf page dict.
fn only_leaf(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> ObjectHandle {
    let refs = pages::page_refs(doc).unwrap();
    assert_eq!(refs.len(), 1);
    resolved_handle(doc, refs[0])
}

fn resolved_value(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, value: ObjectHandle) -> ObjectHandle {
    doc.resolve(&value).unwrap();
    value
}

fn integer_array(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, value: ObjectHandle) -> Vec<i64> {
    resolved_value(doc, value)
        .as_array()
        .expect("array")
        .into_iter()
        .map(|item| {
            let item = resolved_value(doc, item);
            item.as_integer().expect("integer array item")
        })
        .collect()
}

fn integer_array_key(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    object: &ObjectHandle,
    key: &[u8],
) -> Vec<i64> {
    let value = resolved_key(doc, object, key);
    integer_array(doc, value)
}

fn resolved_array_key(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    object: &ObjectHandle,
    key: &[u8],
) -> Vec<ObjectHandle> {
    resolved_key(doc, object, key).as_array().expect("array")
}

fn first_array_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    object: &ObjectHandle,
    key: &[u8],
) -> ObjectRef {
    resolved_array_key(doc, object, key)
        .into_iter()
        .next()
        .and_then(|item| item.object_ref())
        .expect("first array item is an indirect reference")
}

fn first_annotation(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, page: &ObjectHandle) -> ObjectHandle {
    let annotation_ref = first_array_ref(doc, page, b"/Annots");
    resolved_handle(doc, annotation_ref)
}

fn destination_page_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    value: ObjectHandle,
) -> flpdf::ObjectRef {
    resolved_value(doc, value)
        .as_array()
        .expect("destination array")
        .into_iter()
        .next()
        .and_then(|value| value.object_ref())
        .expect("destination page reference")
}

fn assert_destination_page_is_null(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    value: ObjectHandle,
    context: &str,
) {
    let page_ref = destination_page_ref(doc, value);
    assert!(resolved_handle(doc, page_ref).is_null(), "{context}");
}

fn assert_destination_key_is_null(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    object: &ObjectHandle,
    key: &[u8],
    context: &str,
) {
    let value = resolved_key(doc, object, key);
    assert_destination_page_is_null(doc, value, context);
}

fn assert_reference_target_is_null(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    value: &ObjectHandle,
    context: &str,
) {
    let reference = value.object_ref().expect("page reference");
    assert!(resolved_handle(doc, reference).is_null(), "{context}");
}

#[test]
fn extracts_single_page_with_count_one() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();

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
    assert_eq!(
        resolved_key(&mut out, &root, b"/Count").as_integer(),
        Some(1)
    );
    assert_eq!(
        resolved_key(&mut out, &root, b"/Kids")
            .as_array()
            .expect("/Kids array")
            .len(),
        1
    );
}

#[test]
fn extract_omits_indirect_null_dictionary_entries_like_qpdf_copy_foreign_object() {
    let src = page_with_indirect_null_resource_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);
    let resources = resolved_key(&mut out, &leaf, b"/Resources");
    let fonts = resolved_key(&mut out, &resources, b"/Font");

    assert!(fonts.has_key(b"/F1"), "live font entry must be retained");
    assert!(
        !fonts.has_key(b"/Null"),
        "qpdf copyForeignObject drops an indirect-null dictionary entry"
    );
}

/// The extracted document is built the same way qpdf's library-level
/// `QPDF::emptyPDF()` + `QPDFPageDocumentHelper::addPage()` pattern would:
/// neither call touches the document's PDF version, so the result carries
/// `emptyPDF()`'s own header version (1.3) regardless of `source`'s version.
/// (Propagating `source`'s version, as `qpdf --pages` does, is `QPDFJob`
/// CLI-orchestration behavior — a different qpdf class not mirrored here.)
#[test]
fn extracted_document_version_is_the_empty_pdf_baseline_not_the_source_version() {
    let src = two_page_pdf(); // source header is 1.4
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let out = extract_page(&mut source, 0).unwrap();

    assert_eq!(out.version(), "1.3");
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    assert_eq!(
        integer_array_key(&mut out, &leaf, b"/MediaBox"),
        vec![0, 0, 400, 500]
    );
    assert_eq!(
        resolved_key(&mut out, &leaf, b"/Rotate").as_integer(),
        Some(90)
    );

    let res = resolved_key(&mut out, &leaf, b"/Resources");
    let fonts = resolved_key(&mut out, &res, b"/Font");
    let font = resolved_key(&mut out, &fonts, b"/F1");
    let font = resolved_handle(&mut out, font.object_ref().expect("/Font /F1 ref"));
    assert_eq!(
        resolved_key(&mut out, &font, b"/Subtype")
            .as_name()
            .as_deref(),
        Some(&b"Type1"[..])
    );
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // Own /MediaBox preserved.
    assert_eq!(
        integer_array_key(&mut out, &leaf, b"/MediaBox"),
        vec![0, 0, 612, 792]
    );
    // Inherited /CropBox materialized onto the leaf.
    assert_eq!(
        integer_array_key(&mut out, &leaf, b"/CropBox"),
        vec![5, 5, 590, 770]
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // The leaf's own /CropBox wins over the ancestor's inheritable one.
    assert_eq!(
        integer_array_key(&mut out, &leaf, b"/CropBox"),
        vec![1, 1, 400, 500]
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_pages(&mut source, &[0]).unwrap();
    let leaf = only_leaf(&mut out);

    // Inherited /MediaBox materialized onto the leaf.
    assert_eq!(
        integer_array_key(&mut out, &leaf, b"/MediaBox"),
        vec![0, 0, 612, 792]
    );
    // Inherited /CropBox materialized onto the leaf.
    assert_eq!(
        integer_array_key(&mut out, &leaf, b"/CropBox"),
        vec![10, 10, 600, 780]
    );
}

/// Ancestor /Pages stores /MediaBox as an INDIRECT reference (obj 6), the qpdf
/// shared-array pattern. The leaf (obj 3) inherits it. Exercises rewrite_refs'
/// The extracted leaf's inherited /MediaBox must resolve to a
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);

    // /MediaBox must be present and resolve to the live array (not Null, not a
    // dangling source ref).
    let arr = integer_array_key(&mut out, &leaf, b"/MediaBox");
    assert_eq!(
        arr,
        vec![0, 0, 321, 654],
        "indirect inherited /MediaBox must be remapped into the extracted doc, not nulled"
    );
}

#[test]
fn own_mediabox_is_preserved() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut p0 = extract_page(&mut source, 0).unwrap();
    let leaf0 = only_leaf(&mut p0);
    assert_eq!(
        integer_array_key(&mut p0, &leaf0, b"/MediaBox"),
        vec![0, 0, 612, 792]
    );

    let mut p1 = extract_page(&mut source, 1).unwrap();
    let leaf1 = only_leaf(&mut p1);
    assert_eq!(
        integer_array_key(&mut p1, &leaf1, b"/MediaBox"),
        vec![0, 0, 200, 300]
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
        let obj = resolved_handle(doc, r);
        let Some(dict) = obj
            .as_dictionary()
            .map(|_| obj.clone())
            .or_else(|| obj.as_stream_dict())
        else {
            continue;
        };
        if resolved_key(doc, &dict, b"/Subtype").as_name().as_deref() == Some(subtype) {
            n += 1;
        }
    }
    n
}

/// Count how many live objects in `doc` carry the given /Type name.
fn count_type(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, type_name: &[u8]) -> usize {
    let mut n = 0;
    for r in doc.live_object_refs() {
        let obj = resolved_handle(doc, r);
        let Some(dict) = obj
            .as_dictionary()
            .map(|_| obj.clone())
            .or_else(|| obj.as_stream_dict())
        else {
            continue;
        };
        if resolved_key(doc, &dict, b"/Type").as_name().as_deref() == Some(type_name) {
            n += 1;
        }
    }
    n
}

#[test]
fn extracted_doc_has_no_unrelated_objects() {
    let src = shared_resource_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();

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

    // Exactly one /Pages node — the fresh destination root. The canonical
    // foreign copier stops at the source /Pages boundary, so no source
    // ancestor remains in the object table.
    assert_eq!(
        count_type(&mut out, b"Pages"),
        1,
        "only the fresh destination /Pages root must remain"
    );
    assert_eq!(pages::page_refs(&mut out).unwrap().len(), 1);

    // Sanity: the minimal document still writes and reopens to a single page,
    // with no source /Pages node reappearing.
    let mut bytes = Vec::new();
    write_default(&mut out, &mut bytes).unwrap();
    let mut rt = Pdf::open_mem_owned(bytes).unwrap();
    assert_eq!(pages::page_refs(&mut rt).unwrap().len(), 1);
    assert_eq!(
        count_type(&mut rt, b"Pages"),
        1,
        "no source /Pages node after round-trip"
    );
}

mod common;
#[allow(unused_imports)]
use common::{write_default, write_with_settings, WriterTestSettings};

#[test]
fn extracted_contents_match_source_page() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let src_pages = pages::page_refs(&mut source).unwrap();
    let src_leaf = resolved_handle(&mut source, src_pages[0]);
    let src_contents_ref = resolved_key(&mut source, &src_leaf, b"/Contents")
        .object_ref()
        .expect("/Contents ref");
    let src_stream = resolved_handle(&mut source, src_contents_ref);
    let src_data = src_stream
        .get_raw_stream_data()
        .expect("source stream data");

    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf = only_leaf(&mut out);
    let out_contents_ref = resolved_key(&mut out, &leaf, b"/Contents")
        .object_ref()
        .expect("/Contents ref");
    let out_stream = resolved_handle(&mut out, out_contents_ref);
    let out_data = out_stream
        .get_raw_stream_data()
        .expect("output stream data");

    assert_eq!(
        out_data.as_slice(),
        src_data.as_slice(),
        "content stream bytes must be identical"
    );
}

#[test]
fn out_of_range_index_errors() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();
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
fn source_page_membership_and_order_remain_stable_after_extract() {
    let src = two_page_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let before = pages::page_refs(&mut source).unwrap();
    assert_eq!(before.len(), 2);

    let _ = extract_page(&mut source, 0).unwrap();

    // qpdf may materialize inherited attributes, but source page membership
    // and ordering remain unchanged.
    let after = pages::page_refs(&mut source).unwrap();
    assert_eq!(
        after, before,
        "extract_page must preserve the source page tree"
    );
}

/// Page 0 (obj 3) has a Link annotation (obj 5) whose explicit /Dest targets the
/// SIBLING page (obj 4). The canonical foreign copier must retain the carrier
/// while replacing the unselected page at the `/Pages` boundary.
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_page(&mut source, 0).unwrap();

    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    assert_eq!(
        count_type(&mut out, b"Pages"),
        1,
        "only the fresh destination /Pages root must remain"
    );

    // Annotation and /Dest are retained; the referenced page resolves to null.
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(leaf_refs.len(), 1);
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annots = resolved_array_key(&mut out, &leaf, b"/Annots");
    assert_eq!(annots.len(), 1, "annotation must be retained, not dropped");
    let annot_ref = annots[0].object_ref().expect("annot is an indirect ref");
    let annot = resolved_handle(&mut out, annot_ref);
    assert_destination_key_is_null(
        &mut out,
        &annot,
        b"/Dest",
        "cross-page /Dest target must resolve to null",
    );
    assert_eq!(
        resolved_key(&mut out, &annot, b"/Subtype")
            .as_name()
            .as_deref(),
        Some(&b"Link"[..]),
        "annotation subtype preserved"
    );

    // CORE GUARANTEE: extracted leaf content + resources intact.
    let contents_ref = resolved_key(&mut out, &leaf, b"/Contents")
        .object_ref()
        .expect("/Contents ref");
    let stream = resolved_handle(&mut out, contents_ref);
    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("content stream data")
            .as_slice(),
        b"BT /F1 12 Tf ET",
        "leaf content stream intact"
    );
    let res = resolved_key(&mut out, &leaf, b"/Resources");
    let fonts = resolved_key(&mut out, &res, b"/Font");
    assert!(fonts.has_key(b"/F1"), "leaf /Resources /Font /F1 intact");
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    assert!(annot.has_key(b"/Dest"), "self-link /Dest must be preserved");
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "named dest must not leak a sibling"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    assert_eq!(
        resolved_key(&mut out, &annot, b"/Dest")
            .as_name()
            .as_deref(),
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    // The /A action and /D are retained; the referenced page is null.
    let action = resolved_key(&mut out, &annot, b"/A");
    assert!(action.as_dictionary().is_some(), "/A action retained");
    assert_eq!(
        resolved_key(&mut out, &action, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "/A action is still a GoTo"
    );
    assert_destination_key_is_null(
        &mut out,
        &action,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    let aa = resolved_key(&mut out, &annot, b"/AA");
    assert!(aa.as_dictionary().is_some(), "/AA retained");
    let u = resolved_key(&mut out, &aa, b"/U");
    assert!(u.as_dictionary().is_some(), "/AA /U retained");
    assert_eq!(
        resolved_key(&mut out, &u, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "/AA /U is still a GoTo"
    );
    assert_destination_key_is_null(
        &mut out,
        &u,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    assert!(action.as_dictionary().is_some(), "/A retained");
    assert_eq!(
        resolved_key(&mut out, &action, b"/S").as_name().as_deref(),
        Some(&b"URI"[..]),
        "/A is still the URI action"
    );
    assert!(action.has_key(b"/URI"), "/A /URI value must be preserved");
    let next = resolved_key(&mut out, &action, b"/Next");
    assert!(next.as_dictionary().is_some(), "/A /Next retained");
    assert_eq!(
        resolved_key(&mut out, &next, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "/Next action is still a GoTo"
    );
    assert_destination_key_is_null(
        &mut out,
        &next,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    let next = resolved_key(&mut out, &action, b"/Next");
    let elems = next.as_array().expect("/A /Next array");
    assert_eq!(elems.len(), 2, "both /Next actions retained");
    let first = resolved_value(&mut out, elems[0].clone());
    assert!(
        first.as_dictionary().is_some(),
        "first /Next element is a dict"
    );
    assert!(first.has_key(b"/URI"), "first (URI) /Next action untouched");
    let second = resolved_value(&mut out, elems[1].clone());
    assert!(
        second.as_dictionary().is_some(),
        "second /Next element is a dict"
    );
    assert_eq!(
        resolved_key(&mut out, &second, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "second /Next action is still a GoTo"
    );
    assert_destination_key_is_null(
        &mut out,
        &second,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let aa = resolved_key(&mut out, &leaf, b"/AA");
    let o = resolved_key(&mut out, &aa, b"/O");
    assert!(o.as_dictionary().is_some(), "page /AA /O retained");
    assert_eq!(
        resolved_key(&mut out, &o, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "page /AA /O is still a GoTo"
    );
    assert_destination_key_is_null(
        &mut out,
        &o,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    // /A remains an indirect ref to the unchanged action carrier.
    let action_ref = resolved_key(&mut out, &annot, b"/A")
        .object_ref()
        .expect("/A ref");
    let action = resolved_handle(&mut out, action_ref);
    assert_eq!(
        resolved_key(&mut out, &action, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "indirect action is still a GoTo"
    );
    assert_destination_key_is_null(
        &mut out,
        &action,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    assert!(annot.has_key(b"/Dest"), "self-link /Dest must be preserved");
    let action = resolved_key(&mut out, &annot, b"/A");
    assert!(action.as_dictionary().is_some(), "/A action retained");
    assert_destination_key_is_null(
        &mut out,
        &action,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    assert!(annot.has_key(b"/A"), "/A URI must be preserved");
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    assert_destination_key_is_null(
        &mut out,
        &annot,
        b"/Dest",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    // /AA stays an indirect reference.
    let aa_ref = resolved_key(&mut out, &annot, b"/AA")
        .object_ref()
        .expect("/AA must stay indirect");
    // Resolve the indirect /AA and confirm /U kept a /D that targets null.
    let aa = resolved_handle(&mut out, aa_ref);
    assert!(aa.as_dictionary().is_some(), "/AA resolves to a dict");
    let u = resolved_key(&mut out, &aa, b"/U");
    assert_eq!(
        resolved_key(&mut out, &u, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "action kept"
    );
    assert_destination_key_is_null(
        &mut out,
        &u,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    let next = resolved_key(&mut out, &action, b"/Next");
    let goto = resolved_value(
        &mut out,
        next.as_array()
            .expect("indirect /Next array")
            .into_iter()
            .next()
            .expect("/Next GoTo action"),
    );
    assert!(goto.as_dictionary().is_some(), "/Next GoTo action");
    assert_destination_key_is_null(
        &mut out,
        &goto,
        b"/D",
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
    mut value: ObjectHandle,
) -> ObjectHandle {
    for _ in 0..=70 {
        let mut items = resolved_value(doc, value)
            .as_array()
            .expect("singleton action array");
        assert_eq!(items.len(), 1);
        value = items.remove(0);
    }
    let value = resolved_value(doc, value);
    assert!(value.as_dictionary().is_some(), "terminal GoTo action");
    value
}

#[test]
fn long_indirect_next_array_keeps_carrier_and_nulls_removed_page() {
    let bytes = long_indirect_next_array_pdf();
    let mut source = Pdf::open_mem_owned(bytes).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();

    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    let next = resolved_key(&mut out, &action, b"/Next");
    let terminal = action_after_71_array_holders(&mut out, next);
    let removed_page = terminal
        .get_key(b"/D")
        .as_array()
        .and_then(|items| items.into_iter().next())
        .and_then(|item| item.object_ref())
        .unwrap();
    assert!(resolved_handle(&mut out, removed_page).is_null());
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annots = resolved_array_key(&mut out, &leaf, b"/Annots");
    let annot_ref = annots[0]
        .object_ref()
        .expect("first annotation is an indirect reference");
    let annot = resolved_handle(&mut out, annot_ref);
    assert_destination_key_is_null(
        &mut out,
        &annot,
        b"/Dest",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    let aa = resolved_key(&mut out, &annot, b"/AA");
    assert!(aa.as_dictionary().is_some(), "/AA kept");
    let u = resolved_key(&mut out, &aa, b"/U");
    assert!(u.as_dictionary().is_some(), "/AA /U kept");
    assert_eq!(
        resolved_key(&mut out, &u, b"/S").as_name().as_deref(),
        Some(&b"URI"[..]),
        "/URI subaction untouched"
    );
}

#[test]
fn indirect_next_cycle_is_preserved_and_removed_page_is_null() {
    // /A -> action 8 whose /Next is 9, and 9's /Next is 8 (an A<->B indirect
    // cycle). Both are cross-page GoTos. The canonical copier preserves the
    // action cycle and both /D carriers target the same null boundary.
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "copied unselected page must be nulled"
    );
    let leaf = only_leaf(&mut out);
    let annot = first_annotation(&mut out, &leaf);
    let first = resolved_key(&mut out, &annot, b"/A");
    assert!(first.as_dictionary().is_some(), "indirect /A retained");
    assert_destination_key_is_null(
        &mut out,
        &first,
        b"/D",
        "first cyclic action /D target must resolve to null",
    );
    let second = resolved_key(&mut out, &first, b"/Next");
    assert!(
        second.as_dictionary().is_some(),
        "second cyclic action retained"
    );
    assert_destination_key_is_null(
        &mut out,
        &second,
        b"/D",
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf_refs = pages::page_refs(&mut out).unwrap();
    let leaf = resolved_handle(&mut out, leaf_refs[0]);
    let annot = first_annotation(&mut out, &leaf);
    let a = resolved_key(&mut out, &annot, b"/A");
    assert!(a.as_dictionary().is_some(), "/A kept");
    assert!(a.has_key(b"/D"), "self-link /A GoTo /D must be preserved");
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
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
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    assert!(action.as_dictionary().is_some(), "/A action retained");
    assert_eq!(
        resolved_key(&mut out, &action, b"/S").as_name().as_deref(),
        Some(&b"GoTo"[..]),
        "GoTo action retained"
    );
    let structure_destination = resolved_key(&mut out, &action, b"/SD");
    let struct_ref = destination_page_ref(&mut out, structure_destination);
    let struct_elem = resolved_handle(&mut out, struct_ref);
    assert!(struct_elem.as_dictionary().is_some(), "StructElem retained");
    let parent_page = resolved_key(&mut out, &struct_elem, b"/Pg");
    assert_reference_target_is_null(
        &mut out,
        &parent_page,
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
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    assert!(action.as_dictionary().is_some(), "/A action retained");
    assert!(action.has_key(b"/SD"), "self-page /SD must be preserved");
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
    let annot = first_annotation(&mut out, &leaf);
    let action = resolved_key(&mut out, &annot, b"/A");
    assert!(action.as_dictionary().is_some(), "/A action retained");
    assert!(
        action.has_key(b"/SD"),
        "named structure destination /SD must be preserved"
    );
}

#[test]
fn annot_p_is_preserved_and_removed_page_is_null() {
    // A malformed annotation /P points at the SIBLING page (obj 4); the
    // canonical copier reaches the `/Pages` boundary and leaves a null target.
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
    let annot = first_annotation(&mut out, &leaf);
    let page_ref = resolved_key(&mut out, &annot, b"/P");
    assert_reference_target_is_null(
        &mut out,
        &page_ref,
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
    let annot = first_annotation(&mut out, &leaf);
    assert!(annot.has_key(b"/P"), "self-page /P must be preserved");
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
    assert!(leaf.has_key(b"/B"), "page /B must be retained");

    // The kept page's own bead (obj 10) targets the kept page, so its /P must
    // stay live and still resolve to a /Type /Page dictionary.
    let bead_ref = resolved_array_key(&mut out, &leaf, b"/B")[0]
        .object_ref()
        .expect("/B[0] bead ref");
    let bead = resolved_handle(&mut out, bead_ref);
    let p_ref = resolved_key(&mut out, &bead, b"/P")
        .object_ref()
        .expect("kept bead /P must be preserved as a page reference");
    let p_page = resolved_handle(&mut out, p_ref);
    assert_eq!(
        resolved_key(&mut out, &p_page, b"/Type")
            .as_name()
            .as_deref(),
        Some(&b"Page"[..]),
        "preserved bead /P must resolve to a /Type /Page"
    );

    let sibling_bead = resolved_key(&mut out, &bead, b"/N");
    assert!(sibling_bead.as_dictionary().is_some(), "bead /N retained");
    let sibling_page = resolved_key(&mut out, &sibling_bead, b"/P");
    assert_reference_target_is_null(
        &mut out,
        &sibling_page,
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
        let obj = resolved_handle(doc, r);
        if obj.as_dictionary().is_some() {
            let type_name = resolved_key(doc, &obj, b"/Type");
            let base_font = resolved_key(doc, &obj, b"/BaseFont");
            if type_name.as_name().as_deref() == Some(&b"Font"[..])
                && base_font.as_name().as_deref() == Some(base)
            {
                n += 1;
            }
        }
    }
    n
}

/// Resolve a leaf page's inline /Resources -> /Font -> first entry's
/// reference -> /BaseFont name.
fn leaf_font_basefont(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, leaf: flpdf::ObjectRef) -> Vec<u8> {
    let leaf = resolved_handle(doc, leaf);
    let resources = resolved_key(doc, &leaf, b"/Resources");
    let fonts = resolved_key(doc, &resources, b"/Font");
    let font_ref = fonts
        .as_dictionary()
        .and_then(|entries| entries.into_values().next())
        .and_then(|value| value.object_ref())
        .expect("leaf /Resources /Font first entry must be an indirect ref");
    let font = resolved_handle(doc, font_ref);
    resolved_key(doc, &font, b"/BaseFont")
        .as_name()
        .expect("/BaseFont")
}

#[test]
fn extract_pages_copies_shared_resource_once() {
    let src = three_page_shared_font_pdf();
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2, "extracted doc must have two pages");
    let root = pages_dict(&mut out);
    assert_eq!(
        resolved_key(&mut out, &root, b"/Count").as_integer(),
        Some(2)
    );

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
    let mut source = Pdf::open_mem_owned(src).unwrap();

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
    let mut source = Pdf::open_mem_owned(src).unwrap();

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
    let mut source = Pdf::open_mem_owned(src).unwrap();
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
    let mut source = Pdf::open_mem_owned(src).unwrap();
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 0]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2, "duplicate selection yields two kids");
    assert_ne!(
        page_refs[0], page_refs[1],
        "duplicate kids must be distinct page objects"
    );
    let root = pages_dict(&mut out);
    assert_eq!(
        resolved_key(&mut out, &root, b"/Count").as_integer(),
        Some(2)
    );

    // Sub-objects stay SHARED: both kids reference the same /Contents stream.
    let contents_ref = |doc: &mut Pdf<std::io::Cursor<Vec<u8>>>, r: flpdf::ObjectRef| {
        let page = resolved_handle(doc, r);
        resolved_key(doc, &page, b"/Contents")
            .object_ref()
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2, "two selected pages enumerated");
    let second_page_ref = page_refs[1];

    let leaf = resolved_handle(&mut out, page_refs[0]);
    let annot_refs: Vec<flpdf::ObjectRef> = resolved_array_key(&mut out, &leaf, b"/Annots")
        .into_iter()
        .filter_map(|item| item.object_ref())
        .collect();
    assert_eq!(annot_refs.len(), 2, "both annotations retained");

    let mut kept = 0;
    let mut nulled = 0;
    for annot_ref in annot_refs {
        let annot = resolved_handle(&mut out, annot_ref);
        let dest = resolved_key(&mut out, &annot, b"/Dest");
        let target_ref = destination_page_ref(&mut out, dest);
        if target_ref == second_page_ref {
            kept += 1;
        } else {
            assert!(
                resolved_handle(&mut out, target_ref).is_null(),
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
    let mut source = Pdf::open_mem_owned(bytes).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let page_refs = pages::page_refs(&mut out).unwrap();
    assert_eq!(page_refs.len(), 2);

    let leaf0 = resolved_handle(&mut out, page_refs[0]);
    assert_eq!(
        integer_array_key(&mut out, &leaf0, b"/MediaBox"),
        vec![0, 0, 100, 200],
        "leaf 0 inherits /MediaBox from its own parent (obj 3)"
    );
    assert_eq!(
        resolved_key(&mut out, &leaf0, b"/Rotate").as_integer(),
        Some(90),
        "leaf 0 inherits /Rotate 90 from its own parent (obj 3)"
    );

    let leaf1 = resolved_handle(&mut out, page_refs[1]);
    assert_eq!(
        integer_array_key(&mut out, &leaf1, b"/MediaBox"),
        vec![0, 0, 300, 400],
        "leaf 1 inherits /MediaBox from its own parent (obj 4), not leaf 0's"
    );
    // qpdf's foreign-page insertion does not synthesize a /Rotate key when
    // the source page tree has no inherited rotation for that leaf.
    assert!(
        !leaf1.has_key(b"/Rotate"),
        "leaf 1 must not inherit leaf 0's /Rotate 90 or synthesize a default"
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
    let mut source = Pdf::open_mem_owned(src).unwrap();

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
    let mut source = Pdf::open_mem_owned(src).unwrap();

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
    let mut source = Pdf::open_mem_owned(src).unwrap();

    let mut out = extract_pages(&mut source, &[0, 1]).unwrap();

    let mut h = out.page_labels();
    assert!(
        !h.has_page_labels().unwrap(),
        "a source with no /PageLabels must not gain one"
    );
}
