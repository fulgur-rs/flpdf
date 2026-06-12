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

/// Three-page document whose page 0 carries inter-page destinations reaching
/// both a surviving page (page 1) and a removed page (page 2) through every
/// destination carrier: an annotation `/Dest`, an annotation `/A /GoTo /D`, an
/// annotation-level `/AA /E /GoTo /D`, and a page-level `/AA /O /GoTo /D`.
///
/// - obj 3 = page0, `/Annots [6 7 8 9]`, page-level `/AA` GoTo page2.
/// - obj 4 = page1 (surviving), obj 5 = page2 (removed when only [0,1] chosen).
/// - obj 6 = link annot `/Dest [4 0 R /Fit]`   → surviving page1.
/// - obj 7 = link annot `/Dest [5 0 R /Fit]`   → removed page2.
/// - obj 8 = link annot `/A << /S /GoTo /D [5 0 R /Fit] >>` → removed page2.
/// - obj 9 = annot `/AA << /E << /S /GoTo /D [5 0 R /Fit] >> >>` → removed page2.
fn three_page_multi_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R 7 0 R 8 0 R 9 0 R] \
                 /AA << /O << /S /GoTo /D [5 0 R /Fit] >> >> >>",
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
            (
                8,
                "<< /Type /Annot /Subtype /Link /Rect [40 0 50 10] /A << /S /GoTo /D [5 0 R /Fit] >> >>",
            ),
            (
                9,
                "<< /Type /Annot /Subtype /Widget /Rect [60 0 70 10] /AA << /E << /S /GoTo /D [5 0 R /Fit] >> >> >>",
            ),
        ],
        1,
    )
}

/// Resolve the page-ref destination carried by an annotation-level
/// `/AA /E /GoTo /D` action array, returning `(dest_ref, resolved_is_null)`.
fn annot_aa_dest_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    annot_ref: flpdf::ObjectRef,
) -> (flpdf::ObjectRef, bool) {
    let annot = doc.resolve(annot_ref).unwrap().into_dict().unwrap();
    let aa = match annot.get("AA") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /AA dict, got {other:?}"),
    };
    let enter = match aa.get("E") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /AA /E action, got {other:?}"),
    };
    let d = match enter.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /AA /E /D array, got {other:?}"),
    };
    let page_dest = match d.first() {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /AA /E /D[0] to be an indirect reference, got {other:?}"),
    };
    let is_null = matches!(doc.resolve(page_dest).unwrap(), Object::Null);
    (page_dest, is_null)
}

/// Resolve the page-ref destination carried by an annotation's `/Dest` array
/// (`[<pageRef> /Fit]`), returning `(dest_ref, resolved_is_null)`.
fn annot_dest_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    annot_ref: flpdf::ObjectRef,
) -> (flpdf::ObjectRef, bool) {
    let annot = doc.resolve(annot_ref).unwrap().into_dict().unwrap();
    let dest = match annot.get("Dest") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /Dest array, got {other:?}"),
    };
    let page_ref = match dest.first() {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /Dest[0] to be an indirect reference, got {other:?}"),
    };
    let is_null = matches!(doc.resolve(page_ref).unwrap(), Object::Null);
    (page_ref, is_null)
}

/// Resolve the page-ref destination carried by an annotation's `/A /GoTo /D`
/// action array, returning `(dest_ref, resolved_is_null)`.
fn annot_action_dest_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    annot_ref: flpdf::ObjectRef,
) -> (flpdf::ObjectRef, bool) {
    let annot = doc.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = match annot.get("A") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /A action, got {other:?}"),
    };
    let d = match action.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /A /D array, got {other:?}"),
    };
    let page_ref = match d.first() {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /A /D[0] to be an indirect reference, got {other:?}"),
    };
    let is_null = matches!(doc.resolve(page_ref).unwrap(), Object::Null);
    (page_ref, is_null)
}

/// Resolve the page-ref destination carried by a page's `/AA /O /GoTo /D`,
/// returning `(dest_ref, resolved_is_null)`.
fn page_aa_dest_ref(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    page_ref: flpdf::ObjectRef,
) -> (flpdf::ObjectRef, bool) {
    let page = doc.resolve(page_ref).unwrap().into_dict().unwrap();
    let aa = match page.get("AA") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /AA dict, got {other:?}"),
    };
    let open = match aa.get("O") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /AA /O action, got {other:?}"),
    };
    let d = match open.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /AA /O /D array, got {other:?}"),
    };
    let page_dest = match d.first() {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected /AA /O /D[0] to be an indirect reference, got {other:?}"),
    };
    let is_null = matches!(doc.resolve(page_dest).unwrap(), Object::Null);
    (page_dest, is_null)
}

/// Collect the indirect annotation references on a leaf page's `/Annots` array.
fn annot_refs(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    page_ref: flpdf::ObjectRef,
) -> Vec<flpdf::ObjectRef> {
    let page = doc.resolve(page_ref).unwrap().into_dict().unwrap();
    let annots = match page.get("Annots") {
        Some(Object::Array(arr)) => arr.clone(),
        Some(Object::Reference(r)) => match doc.resolve(*r).unwrap() {
            Object::Array(arr) => arr,
            other => panic!("expected indirect /Annots to be an array, got {other:?}"),
        },
        other => panic!("expected /Annots array, got {other:?}"),
    };
    annots.iter().filter_map(Object::as_ref_id).collect()
}

// Inter-page destinations follow qpdf `--pages` null-out parity (NOT the drop
// behaviour of extract_pages): a destination targeting a SURVIVING page is
// remapped to that page's new ref; a destination targeting a REMOVED page keeps
// its destination array verbatim, with the array's first element pointing at a
// fresh object that is replaced with `null`. This is verified across all three
// destination carriers (annot `/Dest`, annot `/A /GoTo /D`, page `/AA`).
#[test]
fn merge_inter_page_dest_remapped_and_removed_nulled() {
    let mut a = Pdf::open_mem_owned(three_page_multi_dest_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "removed page2 is absent from /Kids");
    let page0_ref = refs[0];
    let page1_ref = refs[1];

    let annots = annot_refs(&mut doc, page0_ref);
    assert_eq!(annots.len(), 4, "all four annotations retained");

    // Annot 0 (/Dest → surviving page1): remapped to page1's new ref, NOT null.
    let (surviving_dest, surviving_is_null) = annot_dest_ref(&mut doc, annots[0]);
    assert_eq!(
        surviving_dest, page1_ref,
        "surviving /Dest must remap to the second output page"
    );
    assert!(
        !surviving_is_null,
        "surviving destination target must not be nulled"
    );

    // Annot 1 (/Dest → removed page2): kept verbatim; /Dest[0] is an indirect
    // reference resolving to a NULL object (qpdf null-out, NOT dropped).
    let (removed_dest, removed_is_null) = annot_dest_ref(&mut doc, annots[1]);
    assert!(
        removed_is_null,
        "removed-target /Dest must keep its array with the first element nulled"
    );

    // Annot 2 (/A /GoTo /D → removed page2): same null-out via the action chain.
    let (action_dest, action_is_null) = annot_action_dest_ref(&mut doc, annots[2]);
    assert!(
        action_is_null,
        "removed-target /A /GoTo /D must keep its array with the first element nulled"
    );

    // Annot 3 (/AA /E /GoTo /D → removed page2): annotation-level additional
    // actions are scanned too; same null-out.
    let (annot_aa_dest, annot_aa_is_null) = annot_aa_dest_ref(&mut doc, annots[3]);
    assert!(
        annot_aa_is_null,
        "removed-target annot /AA /E /GoTo /D must keep its array with the first element nulled"
    );

    // Page-level /AA /O /GoTo /D → removed page2: same null-out.
    let (aa_dest, aa_is_null) = page_aa_dest_ref(&mut doc, page0_ref);
    assert!(
        aa_is_null,
        "removed-target /AA /O /GoTo /D must keep its array with the first element nulled"
    );

    // All removed-target destinations point at the SAME placeholder (the
    // removed page is copied once), and it is distinct from any kept page.
    assert_eq!(removed_dest, action_dest, "shared removed-page placeholder");
    assert_eq!(
        removed_dest, annot_aa_dest,
        "shared removed-page placeholder"
    );
    assert_eq!(removed_dest, aa_dest, "shared removed-page placeholder");
    assert_ne!(removed_dest, page0_ref);
    assert_ne!(removed_dest, page1_ref);

    // The merged document round-trips.
    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

/// Three-page document whose page 0 reaches its annotations through an
/// *indirect* `/Annots` array object (obj 9), exercising the indirect-array
/// path of the removed-target scan. The annotations also include destinations
/// that carry no in-document page reference:
///
/// - obj 6 = `/Dest [5 0 R /Fit]` → removed page2 (explicit page ref).
/// - obj 7 = `/Dest /SomeNamedDest`  → named destination (no page ref).
/// - obj 8 = `/A << /S /URI /URI (http://example.com) >>` → URI action (not GoTo).
fn three_page_indirect_annots_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 9 0 R >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                6,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [5 0 R /Fit] >>",
            ),
            (
                7,
                "<< /Type /Annot /Subtype /Link /Rect [20 0 30 10] /Dest /SomeNamedDest >>",
            ),
            (
                8,
                "<< /Type /Annot /Subtype /Link /Rect [40 0 50 10] /A << /S /URI /URI (http://example.com) >> >>",
            ),
            (9, "[6 0 R 7 0 R 8 0 R]"),
        ],
        1,
    )
}

// An indirect `/Annots` array is followed, and destinations without an
// in-document page reference (named `/Dest`, non-GoTo `/A`) contribute no
// removed target while the explicit-page `/Dest` to the removed page is still
// nulled. Guards the fidelity bug where a removed page reached only through an
// indirect `/Annots` would be copied (by the closure) but left un-nulled.
#[test]
fn merge_indirect_annots_nulls_removed_dest_and_ignores_pageless_dests() {
    let mut a = Pdf::open_mem_owned(three_page_indirect_annots_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "removed page2 is absent from /Kids");
    let page0_ref = refs[0];

    let annots = annot_refs(&mut doc, page0_ref);
    assert_eq!(annots.len(), 3, "all three annotations retained");

    // The explicit-page /Dest to the removed page is kept and nulled even
    // though /Annots was reached through an indirect array object.
    let (_removed_dest, removed_is_null) = annot_dest_ref(&mut doc, annots[0]);
    assert!(
        removed_is_null,
        "removed-target /Dest behind an indirect /Annots must be nulled"
    );

    // The named /Dest carries no page ref: it is retained verbatim as a name.
    let named = doc.resolve(annots[1]).unwrap().into_dict().unwrap();
    assert_eq!(
        named.get("Dest").and_then(|o| o.as_name()),
        Some(&b"SomeNamedDest"[..]),
        "named destination must be retained as-is"
    );

    // The URI action is not a GoTo: it contributes no removed target and is
    // retained verbatim.
    let uri = doc.resolve(annots[2]).unwrap().into_dict().unwrap();
    let action = match uri.get("A") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /A action dict, got {other:?}"),
    };
    assert_eq!(
        action.get("S").and_then(|o| o.as_name()),
        Some(&b"URI"[..]),
        "URI action must be retained"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

/// Five-page document whose page 0 reaches three distinct removed pages (2, 3,
/// 4) through carriers that the one-level collector did NOT follow: a `/Next`
/// action chain, an action array, and a `/SD` structure destination. Pages 0
/// and 1 survive; 2/3/4 are removed when only `[0,1]` is selected.
///
/// - obj 6 = annot `/A << /S /SetOCGState /Next << /S /GoTo /D [7 0 R] >> >>`
///   — the GoTo lives only on the `/Next` continuation; the head action is a
///   non-GoTo. Target is page2 (obj 7).
/// - obj 8 = annot `/A [ << /S /SetOCGState >> << /S /GoTo /D [9 0 R] >> ]`
///   — the action value is an ARRAY; the GoTo is the second element. Target is
///   page3 (obj 9).
/// - obj 10 = annot `/A << /S /GoTo /SD [11 0 R /Fit] >>` where obj 11 is a
///   StructElem whose `/Pg` is page4 (obj 12). Target is page4 via the `/SD`
///   StructElem -> `/Pg` hop.
fn five_page_next_array_sd_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 13 0 R 7 0 R 9 0 R 12 0 R] /Count 5 >>",
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R 8 0 R 10 0 R] >>",
            ),
            // page1 (surviving), second in /Kids so the selected pages are 0,1.
            (
                13,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            // Annot reaching page2 ONLY via /A /Next (head is a non-GoTo action).
            (
                6,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
                 /A << /S /SetOCGState /Next << /S /GoTo /D [7 0 R /Fit] >> >> >>",
            ),
            (7, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // removed page2
            // Annot reaching page3 ONLY via an ACTION ARRAY (GoTo is element 1).
            (
                8,
                "<< /Type /Annot /Subtype /Link /Rect [20 0 30 10] \
                 /A [ << /S /SetOCGState >> << /S /GoTo /D [9 0 R /Fit] >> ] >>",
            ),
            (9, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // removed page3
            // Annot reaching page4 ONLY via /A /GoTo /SD (StructElem -> /Pg).
            (
                10,
                "<< /Type /Annot /Subtype /Link /Rect [40 0 50 10] \
                 /A << /S /GoTo /SD [11 0 R /Fit] >> >>",
            ),
            (11, "<< /Type /StructElem /S /Sect /Pg 12 0 R >>"),
            (12, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // removed page4
        ],
        1,
    )
}

/// `true` when `page_ref` is absent from the catalog's single-level `/Kids`.
fn page_absent_from_kids(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    page_ref: flpdf::ObjectRef,
) -> bool {
    let kids = match pages_dict(doc).get("Kids") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /Kids array, got {other:?}"),
    };
    !kids
        .iter()
        .filter_map(Object::as_ref_id)
        .any(|r| r == page_ref)
}

// A removed page reached ONLY through a /Next action chain, an action array, or
// a /SD structure destination is nulled (qpdf --pages null-out parity), NOT left
// as a live orphan. The merge collector must cover the SAME carriers extract's
// neutralize family covers; before that fix these targets were pulled into the
// copy closure but skipped by the one-level collector, surviving as full
// un-nulled orphan pages reachable via a remapped reference. Each target uses a
// distinct carrier so one assertion pins one carrier.
#[test]
fn merge_nulls_removed_page_via_next_array_and_sd() {
    let mut a = Pdf::open_mem_owned(five_page_next_array_sd_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "only the two selected pages are in /Kids");
    let page0_ref = refs[0];

    let annots = annot_refs(&mut doc, page0_ref);
    assert_eq!(annots.len(), 3, "all three annotations retained");

    // /A /Next chain → removed page2: the GoTo on the /Next continuation must
    // have its target nulled.
    let annot_next = doc.resolve(annots[0]).unwrap().into_dict().unwrap();
    let head = match annot_next.get("A") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /A action dict, got {other:?}"),
    };
    let next = match head.get("Next") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /A /Next action dict, got {other:?}"),
    };
    let next_target = match next.get("D") {
        Some(Object::Array(arr)) => arr.first().and_then(Object::as_ref_id).unwrap(),
        other => panic!("expected /Next /D array, got {other:?}"),
    };
    assert!(
        matches!(doc.resolve(next_target).unwrap(), Object::Null),
        "removed page reached via /A /Next must resolve to null"
    );
    assert!(
        page_absent_from_kids(&mut doc, next_target),
        "/Next-reached orphan must be absent from /Kids"
    );

    // /A action array → removed page3: the GoTo array element's target nulled.
    let annot_arr = doc.resolve(annots[1]).unwrap().into_dict().unwrap();
    let arr_target = match annot_arr.get("A") {
        Some(Object::Array(elems)) => {
            // The GoTo is the second element; find it and read its /D[0].
            let goto = elems
                .iter()
                .filter_map(|e| e.as_dict())
                .find(|d| d.get("S").and_then(Object::as_name) == Some(&b"GoTo"[..]))
                .expect("array carries a GoTo action");
            match goto.get("D") {
                Some(Object::Array(arr)) => arr.first().and_then(Object::as_ref_id).unwrap(),
                other => panic!("expected GoTo /D array, got {other:?}"),
            }
        }
        other => panic!("expected /A action array, got {other:?}"),
    };
    assert!(
        matches!(doc.resolve(arr_target).unwrap(), Object::Null),
        "removed page reached via an action array must resolve to null"
    );
    assert!(
        page_absent_from_kids(&mut doc, arr_target),
        "action-array-reached orphan must be absent from /Kids"
    );

    // /A /GoTo /SD → removed page4: the StructElem's /Pg target nulled.
    let annot_sd = doc.resolve(annots[2]).unwrap().into_dict().unwrap();
    let sd_action = match annot_sd.get("A") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /A action dict, got {other:?}"),
    };
    let struct_elem_ref = match sd_action.get("SD") {
        Some(Object::Array(arr)) => arr.first().and_then(Object::as_ref_id).unwrap(),
        other => panic!("expected /SD array, got {other:?}"),
    };
    // The StructElem itself is copied; its /Pg points at the nulled page4.
    let struct_elem = doc.resolve(struct_elem_ref).unwrap().into_dict().unwrap();
    let pg_target = struct_elem
        .get("Pg")
        .and_then(Object::as_ref_id)
        .expect("StructElem has /Pg ref");
    assert!(
        matches!(doc.resolve(pg_target).unwrap(), Object::Null),
        "removed page reached via /SD StructElem /Pg must resolve to null"
    );
    assert!(
        page_absent_from_kids(&mut doc, pg_target),
        "/SD-reached orphan must be absent from /Kids"
    );

    // The merged document round-trips.
    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

/// Article-thread fixture: page 0's `/B` bead ring chains to a bead whose `/P`
/// points at a removed page. Pages 0 and 1 survive; page 2 (the bead's `/P`) is
/// removed when only `[0,1]` is selected.
///
/// - obj 6 = bead0 on the kept page0, `/N 7 0 R` linking to bead1.
/// - obj 7 = bead1 whose `/P 5 0 R` targets the removed page2 (obj 5).
fn three_page_bead_ring_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [6 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"), // removed page2
            (6, "<< /T 8 0 R /P 3 0 R /N 7 0 R >>"),                        // bead0 on kept page0
            (7, "<< /T 8 0 R /P 5 0 R /N 6 0 R >>"),                        // bead1 → removed page2
            (8, "<< /Type /Thread >>"),
        ],
        1,
    )
}

// KNOWN LIMITATION (documents current behaviour). A page reached only through
// an article-thread bead's `/P` from a selected page belongs to a
// drop-and-garbage-collect family, NOT the destination null-out family. qpdf
// `--pages` GCs such a page entirely rather than leaving a null placeholder, so
// merge deliberately does not null it. Until merge implements the drop, the
// page is pulled into the copy by the page closure (which follows the surviving
// bead `/P`) and stays a LIVE object — outside the output page tree, but still
// reachable through the bead ring. This test pins that current behaviour: the
// bead `/P` target resolves to a live `/Type /Page` dict (NOT null) and is
// absent from `/Kids`. A future drop-fix should flip the first assertion.
#[test]
fn merge_bead_p_removed_page_currently_orphaned() {
    let mut a = Pdf::open_mem_owned(three_page_bead_ring_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "only the two selected pages are in /Kids");
    let page0_ref = refs[0];

    // Walk the copied bead ring from page0's /B to bead1, whose /P targets the
    // removed page.
    let page0 = doc.resolve(page0_ref).unwrap().into_dict().unwrap();
    let b = match page0.get("B") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /B array, got {other:?}"),
    };
    let bead0_ref = b.first().and_then(Object::as_ref_id).unwrap();
    let bead0 = doc.resolve(bead0_ref).unwrap().into_dict().unwrap();
    let bead1_ref = bead0.get("N").and_then(Object::as_ref_id).unwrap();
    let bead1 = doc.resolve(bead1_ref).unwrap().into_dict().unwrap();
    let p_target = bead1.get("P").and_then(Object::as_ref_id).unwrap();

    // Current behaviour: the bead /P target is NOT nulled — it remains a live
    // `/Type /Page` object, distinct from the destination null-out family.
    let resolved = doc.resolve(p_target).unwrap().into_dict();
    assert!(
        resolved
            .as_ref()
            .and_then(|d| d.get("Type"))
            .and_then(Object::as_name)
            == Some(&b"Page"[..]),
        "bead-/P-reached removed page is currently a live Page object (drop deferred)"
    );
    // ...but it is outside the output page tree (never appears in /Kids).
    assert!(
        page_absent_from_kids(&mut doc, p_target),
        "bead-/P-reached orphan must be absent from /Kids"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

/// Three-page document whose pages carry deliberately malformed or structurally
/// varied destination carriers, exercising the collector's tolerant and
/// chain-shape arms without any well-formed *removed* target. All three pages
/// are selected, so nothing is nulled; the test only proves the collector walks
/// these shapes without error.
///
/// page0 (obj 3) — malformed carriers:
/// - page-level `/AA 9 0 R` resolves to a NON-dict (an array) → `/AA` arm.
/// - obj 6 = annot whose `/A 11 0 R` resolves to a NON-dict (an integer) and
///   whose `/AA << /E ... >>` head is a `/GoTo` WITHOUT `/D` → action arms.
/// - obj 7 = a NON-dict annotation (an integer) reached through the indirect
///   `/Annots 8 0 R` array → annotation-not-a-dict arm.
///
/// page1 (obj 4) — `/Annots 14 0 R` resolves to a NON-array (a dict) → the
///   indirect-`/Annots`-not-an-array arm.
///
/// page2 (obj 5) — action-chain shapes and `/SD` carriers targeting *kept*
/// pages (so they are collected-but-not-removed, exercising the "target is
/// selected" branch) and `/SD` shapes carrying no resolvable page ref:
/// - obj 12 = annot whose `/A` head's `/Next` is an ARRAY of actions → the
///   `/Next`-array arm.
/// - obj 13 = annot whose `/A 15 0 R` is an action whose `/Next 15 0 R` points
///   back at itself → the indirect-cycle guard (the same action object is not
///   re-entered).
/// - obj 17 = annot `/A /GoTo /SD` → StructElem (obj 18) whose `/Pg 4 0 R` is a
///   SELECTED page (the `/SD` target is kept, not removed).
/// - objs 21–24 = `/A /GoTo /SD` shapes that each yield no page ref (empty
///   `/SD`, non-dict StructElem, StructElem without `/Pg`, `/Pg` → non-page),
///   exercising the early-return arms of `sd_target_page_ref`.
fn malformed_carriers_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Annots 8 0 R /AA 9 0 R >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 14 0 R >>",
            ),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Annots [12 0 R 13 0 R 17 0 R 21 0 R 22 0 R 23 0 R 24 0 R] >>",
            ),
            // page0 annots.
            (
                6,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 11 0 R \
                 /AA << /E << /S /GoTo >> >> >>",
            ),
            (7, "42"),            // /Annots element resolves to a non-dict
            (8, "[6 0 R 7 0 R]"), // indirect /Annots array
            (9, "[1 2 3]"),       // /AA resolves to a non-dict (array)
            (11, "99"),           // /A resolves to a non-dict (integer)
            // page2: /A head whose /Next is an ARRAY of actions (both non-GoTo,
            // so no target is collected; this exercises the /Next-array walk).
            (
                12,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
                 /A << /S /SetOCGState /Next [ << /S /SetOCGState >> << /S /SetOCGState >> ] >> >>",
            ),
            // page2: /A is an indirect action whose /Next points back at itself,
            // exercising the indirect-cycle guard (visited set).
            (
                13,
                "<< /Type /Annot /Subtype /Link /Rect [20 0 30 10] /A 15 0 R >>",
            ),
            (14, "<< /NotAnArray true >>"), // indirect /Annots resolves to a non-array
            (15, "<< /S /SetOCGState /Next 15 0 R >>"), // self-referential /Next
            // page2: a /A /GoTo /SD whose target is a KEPT (selected) page —
            // collected but not recorded as removed.
            (
                17,
                "<< /Type /Annot /Subtype /Link /Rect [40 0 50 10] \
                 /A << /S /GoTo /SD [18 0 R /Fit] >> >>",
            ),
            (18, "<< /Type /StructElem /S /Sect /Pg 4 0 R >>"),
            // page2: /SD shapes that carry no resolvable page ref (each returns
            // None from sd_target_page_ref, exercising its early-return arms).
            (
                21,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 5 5] /A << /S /GoTo /SD [] >> >>",
            ), // empty /SD array
            (
                22,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 5 5] /A << /S /GoTo /SD [26 0 R /Fit] >> >>",
            ), // /SD[0] resolves to a non-dict StructElem
            (
                23,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 5 5] /A << /S /GoTo /SD [27 0 R /Fit] >> >>",
            ), // StructElem without /Pg
            (
                24,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 5 5] /A << /S /GoTo /SD [28 0 R /Fit] >> >>",
            ), // StructElem whose /Pg resolves to a non-page
            (26, "99"),                      // non-dict StructElem
            (27, "<< /Type /StructElem >>"), // StructElem with no /Pg
            (28, "<< /Type /StructElem /Pg 29 0 R >>"), // /Pg → non-page
            (29, "<< /Type /Annot >>"),      // a non-page object (used as /Pg target)
        ],
        1,
    )
}

// Malformed author-controlled carriers (a non-array indirect /Annots element, a
// non-dict /AA, a non-dict /A action, a GoTo without /D, a non-array indirect
// /Annots), structural action shapes (a /Next array, a self-referential
// indirect /Next), a /A /GoTo /SD whose target is a KEPT page, and /SD shapes
// carrying no resolvable page ref are all tolerated: the merge succeeds and all
// selected pages survive. These shapes pass page_refs (which does not validate
// these sub-objects), so the collector's tolerant arms are genuinely reachable
// from an openable PDF.
#[test]
fn merge_tolerates_malformed_carrier_shapes() {
    let mut a = Pdf::open_mem_owned(malformed_carriers_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1, 2],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 3, "all selected pages survive the merge");

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// ---------------------------------------------------------------------------
// Task 4: document-level inheritance from the PRIMARY input only.
// ---------------------------------------------------------------------------

/// Three-page document carrying a full set of document-level destination
/// carriers, all using INDIRECT page references inside their dest arrays so a
/// removed target's `Dest[0]` is an `Object::Reference` resolving to `null`:
///
/// - `/Outlines` (obj 10) tree with three items:
///   - obj 20 "P1" → `/Dest [3 0 R /XYZ 0 792 0]` (array dest, surviving page0).
///   - obj 21 "P2" → `/A << /S /GoTo /D [4 0 R /Fit] >>` (action dest, page1).
///   - obj 22 "P3" → `/Dest [5 0 R /Fit]` (removed page2 when [0,1] selected).
/// - `/Names` (obj 11) → `/Dests` name-tree leaf (obj 30):
///   d_p1 → page0, d_p3 → removed page2.
/// - legacy `/Catalog /Dests` (obj 12): legacy_p3 → removed page2.
/// - `/OpenAction` (obj 13): `<< /S /GoTo /D [5 0 R /Fit] >>` → removed page2.
fn doc_level_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Names 11 0 R \
                 /Dests 12 0 R /OpenAction 13 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                10,
                "<< /Type /Outlines /First 20 0 R /Last 22 0 R /Count 3 >>",
            ),
            (11, "<< /Dests 30 0 R >>"),
            (12, "<< /legacy_p3 [5 0 R /Fit] >>"),
            (13, "<< /S /GoTo /D [5 0 R /Fit] >>"),
            (
                20,
                "<< /Title (P1) /Parent 10 0 R /Next 21 0 R /Dest [3 0 R /XYZ 0 792 0] >>",
            ),
            (
                21,
                "<< /Title (P2) /Parent 10 0 R /Prev 20 0 R /Next 22 0 R \
                 /A << /S /GoTo /D [4 0 R /Fit] >> >>",
            ),
            (
                22,
                "<< /Title (P3) /Parent 10 0 R /Prev 21 0 R /Dest [5 0 R /Fit] >>",
            ),
            (
                30,
                "<< /Limits [(d_p1) (d_p3)] \
                 /Names [(d_p1) [3 0 R /XYZ 0 792 0] (d_p3) [5 0 R /Fit]] >>",
            ),
        ],
        1,
    )
}

/// Resolve the catalog dict of a merged document.
fn catalog_dict(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Dictionary {
    let catalog_ref = doc.root_ref().unwrap();
    doc.resolve(catalog_ref).unwrap().into_dict().unwrap()
}

/// Walk an `/Outlines` tree (`/First` → `/Next`, descending `/First`) and return
/// every item's `ObjectRef` in document order. Cycle-guarded.
fn outline_item_refs(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    outlines_ref: flpdf::ObjectRef,
) -> Vec<flpdf::ObjectRef> {
    let mut out = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let first = doc
        .resolve(outlines_ref)
        .unwrap()
        .into_dict()
        .unwrap()
        .get_ref("First");
    if let Some(first) = first {
        walk_outline_refs(doc, first, &mut out, &mut visited);
    }
    out
}

fn walk_outline_refs(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    start: flpdf::ObjectRef,
    out: &mut Vec<flpdf::ObjectRef>,
    visited: &mut std::collections::BTreeSet<flpdf::ObjectRef>,
) {
    let mut current = Some(start);
    while let Some(r) = current {
        if !visited.insert(r) {
            break;
        }
        // A /First/Next may point at a non-dict (malformed); stop that chain.
        let Some(item) = doc.resolve(r).unwrap().into_dict() else {
            break;
        };
        out.push(r);
        if let Some(child) = item.get_ref("First") {
            walk_outline_refs(doc, child, out, visited);
        }
        current = item.get_ref("Next");
    }
}

/// Read the first element of a `/Dest`-style array on the item/dict at `r`,
/// returning `(dest_ref, resolves_to_null)`. Panics if the dest isn't an array
/// of `[<ref> ...]` shape.
fn dest_array_first(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    arr: &[Object],
) -> (flpdf::ObjectRef, bool) {
    let page_ref = match arr.first() {
        Some(Object::Reference(r)) => *r,
        other => panic!("expected dest[0] to be a reference, got {other:?}"),
    };
    let is_null = matches!(doc.resolve(page_ref).unwrap(), Object::Null);
    (page_ref, is_null)
}

// The merge inherits the PRIMARY input's /Outlines, /Names /Dests, and
// /OpenAction; the SECONDARY input contributes pages only — its document-level
// structures are NOT merged. Surviving-page dests are remapped to the new page
// refs (folded into the primary closure so copy_objects's single rewrite pass
// remaps them); the secondary's outline items never appear in the output.
#[test]
fn merge_inherits_primary_outline_only() {
    let mut a = Pdf::open_mem_owned(doc_level_dest_pdf()).unwrap(); // primary
    let mut b = Pdf::open_mem_owned(doc_level_dest_pdf()).unwrap(); // secondary
    let mut inputs = [
        MergeInput {
            source: &mut a,
            pages: vec![0, 1],
        },
        MergeInput {
            source: &mut b,
            pages: vec![0],
        },
    ];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 3, "two primary pages + one secondary page");
    let primary_page0 = refs[0];
    let primary_page1 = refs[1];

    // Output catalog wires /Outlines, /Names, /OpenAction.
    let cat = catalog_dict(&mut doc);
    let outlines_ref = cat
        .get_ref("Outlines")
        .expect("output catalog must have /Outlines");
    assert!(
        cat.get("Names").is_some(),
        "output catalog must have /Names"
    );
    assert!(
        cat.get("OpenAction").is_some(),
        "output catalog must have /OpenAction"
    );

    // Exactly the primary's three outline items (secondary's are NOT merged).
    let items = outline_item_refs(&mut doc, outlines_ref);
    assert_eq!(
        items.len(),
        3,
        "only the primary's three outline items are inherited"
    );

    // Item 0 /Dest → surviving primary page0: remapped to its new ref, NOT null.
    let item0 = doc.resolve(items[0]).unwrap().into_dict().unwrap();
    let dest0 = match item0.get("Dest") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected item0 /Dest array, got {other:?}"),
    };
    let (d0_ref, d0_null) = dest_array_first(&mut doc, &dest0);
    assert_eq!(d0_ref, primary_page0, "item0 dest remaps to new page0 ref");
    assert!(!d0_null, "surviving outline dest must not be nulled");

    // Item 1 /A /GoTo /D → surviving primary page1: remapped, NOT null.
    let item1 = doc.resolve(items[1]).unwrap().into_dict().unwrap();
    let action = match item1.get("A") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected item1 /A dict, got {other:?}"),
    };
    let d1 = match action.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected item1 /A /D array, got {other:?}"),
    };
    let (d1_ref, d1_null) = dest_array_first(&mut doc, &d1);
    assert_eq!(d1_ref, primary_page1, "item1 action dest remaps to page1");
    assert!(!d1_null, "surviving action dest must not be nulled");

    // /Names /Dests carries the primary's named dests; d_p1 → surviving page0.
    let names = match cat.get("Names") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /Names, got {other:?}"),
    };
    let dests_leaf_ref = names.get_ref("Dests").expect("/Names /Dests ref");
    let leaf = doc.resolve(dests_leaf_ref).unwrap().into_dict().unwrap();
    let pairs = match leaf.get("Names") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected name-tree /Names array, got {other:?}"),
    };
    // d_p1 is the first pair; its dest array's first element resolves to page0.
    let d_p1_dest = match &pairs[1] {
        Object::Array(arr) => arr.clone(),
        other => panic!("expected d_p1 dest array, got {other:?}"),
    };
    let (np1_ref, np1_null) = dest_array_first(&mut doc, &d_p1_dest);
    assert_eq!(np1_ref, primary_page0, "named dest d_p1 remaps to page0");
    assert!(!np1_null, "surviving named dest must not be nulled");

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// A primary outline / named / legacy / OpenAction destination targeting a page
// NOT selected from the primary keeps its destination reference, and that
// reference resolves to a NULL object (qpdf --pages null-out parity) — the dest
// is NOT dropped and NOT replaced with an inline [null]; the array survives with
// its first element pointing at a nulled placeholder page object.
#[test]
fn merge_primary_outline_dest_to_removed_page_is_nulled() {
    let mut a = Pdf::open_mem_owned(doc_level_dest_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let cat = catalog_dict(&mut doc);
    let outlines_ref = cat.get_ref("Outlines").unwrap();
    let items = outline_item_refs(&mut doc, outlines_ref);
    assert_eq!(items.len(), 3, "all primary outline items kept");

    // Outline item 2 /Dest → removed page2: array kept, first element is a
    // reference resolving to null.
    let item2 = doc.resolve(items[2]).unwrap().into_dict().unwrap();
    let dest2 = match item2.get("Dest") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected item2 /Dest array, got {other:?}"),
    };
    let (od_ref, od_null) = dest_array_first(&mut doc, &dest2);
    assert!(od_null, "removed outline dest target must resolve to null");

    // Named dest d_p3 → removed page2: same null-out.
    let names = match cat.get("Names") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /Names, got {other:?}"),
    };
    let leaf = doc
        .resolve(names.get_ref("Dests").unwrap())
        .unwrap()
        .into_dict()
        .unwrap();
    let pairs = match leaf.get("Names") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected name-tree /Names array, got {other:?}"),
    };
    let d_p3_dest = match &pairs[3] {
        Object::Array(arr) => arr.clone(),
        other => panic!("expected d_p3 dest array, got {other:?}"),
    };
    let (nd_ref, nd_null) = dest_array_first(&mut doc, &d_p3_dest);
    assert!(nd_null, "removed named dest target must resolve to null");

    // Legacy /Catalog /Dests legacy_p3 → removed page2: same null-out.
    let legacy = match cat.get("Dests") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected legacy /Dests, got {other:?}"),
    };
    let legacy_dest = match legacy.get("legacy_p3") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected legacy_p3 dest array, got {other:?}"),
    };
    let (ld_ref, ld_null) = dest_array_first(&mut doc, &legacy_dest);
    assert!(ld_null, "removed legacy dest target must resolve to null");

    // /OpenAction /GoTo /D → removed page2: same null-out.
    let oa = match cat.get("OpenAction") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /OpenAction, got {other:?}"),
    };
    let oa_d = match oa.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected /OpenAction /D array, got {other:?}"),
    };
    let (oad_ref, oad_null) = dest_array_first(&mut doc, &oa_d);
    assert!(
        oad_null,
        "removed /OpenAction dest target must resolve to null"
    );

    // All four removed-target dests point at the SAME placeholder (page2 copied
    // once), distinct from the surviving pages.
    assert_eq!(
        od_ref, nd_ref,
        "shared removed-page placeholder (outline=named)"
    );
    assert_eq!(
        od_ref, ld_ref,
        "shared removed-page placeholder (outline=legacy)"
    );
    assert_eq!(
        od_ref, oad_ref,
        "shared removed-page placeholder (outline=openaction)"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// An inline-on-catalog /OpenAction (a bare dest array, not an indirect action
// object) targeting a removed page is nulled too, and a surviving target is
// remapped. This exercises the inline-on-catalog wiring path (the value lives on
// the primary catalog, which copy_objects never copies, so merge constructs the
// target value from the renumber map).
#[test]
fn merge_inline_openaction_dest_array_remapped_and_nulled() {
    // Build a variant whose /OpenAction is an inline dest array to the removed
    // page2, and a second doc whose /OpenAction targets the surviving page0.
    let removed = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /OpenAction [5 0 R /Fit] >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(removed).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();
    let cat = catalog_dict(&mut doc);
    let oa = match cat.get("OpenAction") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected inline /OpenAction array, got {other:?}"),
    };
    let (_oa_ref, oa_null) = dest_array_first(&mut doc, &oa);
    assert!(oa_null, "inline /OpenAction to removed page must be nulled");

    // Surviving variant: /OpenAction → page0, kept and remapped.
    let surviving = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /OpenAction [3 0 R /Fit] >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut s = Pdf::open_mem_owned(surviving).unwrap();
    let mut inputs2 = [MergeInput {
        source: &mut s,
        pages: vec![0, 1],
    }];
    let mut doc2 = merge_documents(&mut inputs2).unwrap();
    let refs2 = pages::page_refs(&mut doc2).unwrap();
    let cat2 = catalog_dict(&mut doc2);
    let oa2 = match cat2.get("OpenAction") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected inline /OpenAction array, got {other:?}"),
    };
    let (oa2_ref, oa2_null) = dest_array_first(&mut doc2, &oa2);
    assert!(!oa2_null, "surviving inline /OpenAction must not be nulled");
    assert_eq!(oa2_ref, refs2[0], "inline /OpenAction remaps to new page0");
}

/// Three-page primary whose document-level carriers are held INLINE on the
/// catalog (not as indirect objects), exercising the inline-on-catalog wiring
/// and reconstruction paths:
///
/// - inline `/Names << /Dests 30 0 R >>` (the name-tree leaf 30 is still an
///   indirect object, per the spec).
/// - inline legacy `/Dests` dict with a surviving array dest (legacy_p0 → page0),
///   a removed array dest (legacy_p2 → removed page2), a name-form dest
///   (legacy_named → /SomeName, no page ref), and a no-leading-ref array dest
///   (legacy_noref → [/Fit]).
/// - inline `/OpenAction << /S /GoTo /D [5 0 R /Fit] >>` → removed page2.
fn inline_doc_level_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 30 0 R >> \
                 /Dests << /legacy_p0 [3 0 R /XYZ 0 792 0] /legacy_p2 [5 0 R /Fit] \
                 /legacy_named /SomeName /legacy_noref [/Fit] >> \
                 /OpenAction << /S /GoTo /D [5 0 R /Fit] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                30,
                "<< /Limits [(d_p0) (d_p2)] \
                 /Names [(d_p0) [3 0 R /Fit] (d_p2) [5 0 R /Fit]] >>",
            ),
        ],
        1,
    )
}

// Inline-on-catalog document-level carriers: an inline /Names dict, an inline
// legacy /Dests dict (array/named/no-ref entries), and an inline /OpenAction
// GoTo action are all inherited. Surviving dests remap to new page refs; removed
// dests keep their reference resolving to null; page-less dests pass through.
#[test]
fn merge_inherits_inline_doc_level_carriers() {
    let mut a = Pdf::open_mem_owned(inline_doc_level_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    let page0 = refs[0];
    let cat = catalog_dict(&mut doc);

    // Inline /Names was inherited; its /Dests leaf carries the named dests.
    let names = match cat.get("Names") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /Names, got {other:?}"),
    };
    let leaf = doc
        .resolve(names.get_ref("Dests").unwrap())
        .unwrap()
        .into_dict()
        .unwrap();
    let pairs = match leaf.get("Names") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected name-tree /Names array, got {other:?}"),
    };
    // d_p0 → surviving page0 (remapped); d_p2 → removed page2 (nulled).
    let (np0_ref, np0_null) = dest_array_first(
        &mut doc,
        match &pairs[1] {
            Object::Array(a) => a,
            o => panic!("d_p0 dest: {o:?}"),
        },
    );
    assert_eq!(np0_ref, page0);
    assert!(!np0_null);
    let (_np2_ref, np2_null) = dest_array_first(
        &mut doc,
        match &pairs[3] {
            Object::Array(a) => a,
            o => panic!("d_p2 dest: {o:?}"),
        },
    );
    assert!(np2_null, "removed named dest nulled");

    // Inline legacy /Dests reconstructed: surviving remapped, removed nulled,
    // name-form and no-ref-array entries passed through verbatim.
    let legacy = match cat.get("Dests") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected inline legacy /Dests, got {other:?}"),
    };
    let (lp0_ref, lp0_null) = dest_array_first(
        &mut doc,
        match legacy.get("legacy_p0") {
            Some(Object::Array(a)) => a,
            o => panic!("legacy_p0: {o:?}"),
        },
    );
    assert_eq!(lp0_ref, page0, "legacy_p0 remaps to page0");
    assert!(!lp0_null);
    let (_lp2_ref, lp2_null) = dest_array_first(
        &mut doc,
        match legacy.get("legacy_p2") {
            Some(Object::Array(a)) => a,
            o => panic!("legacy_p2: {o:?}"),
        },
    );
    assert!(lp2_null, "legacy_p2 (removed) nulled");
    // Name-form dest: kept verbatim (remap_inline_dest non-array arm).
    assert_eq!(
        legacy.get("legacy_named").and_then(|o| o.as_name()),
        Some(&b"SomeName"[..]),
        "name-form legacy dest passed through unchanged"
    );
    // No-leading-ref array dest: first element is a name, left unchanged.
    let noref = match legacy.get("legacy_noref") {
        Some(Object::Array(a)) => a.clone(),
        o => panic!("legacy_noref: {o:?}"),
    };
    assert_eq!(noref.first(), Some(&Object::Name(b"Fit".to_vec())));

    // Inline /OpenAction GoTo dict → removed page2: /D[0] resolves to null.
    let oa = match cat.get("OpenAction") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected inline /OpenAction dict, got {other:?}"),
    };
    let oa_d = match oa.get("D") {
        Some(Object::Array(a)) => a.clone(),
        other => panic!("expected /OpenAction /D array, got {other:?}"),
    };
    let (_oad_ref, oad_null) = dest_array_first(&mut doc, &oa_d);
    assert!(oad_null, "inline /OpenAction GoTo to removed page nulled");

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// A non-GoTo inline /OpenAction action dict, and an /OpenAction that is neither
// an array nor a dict (a bare name), are both passed through verbatim — their
// /D, if any, is not a local page destination, so no remap is attempted. This
// exercises remap_inline_action's non-GoTo dict and "other" arms.
#[test]
fn merge_inline_non_goto_open_action_passed_through() {
    let non_goto = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /OpenAction << /S /URI /URI (http://example.com) >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(non_goto).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();
    let cat = catalog_dict(&mut doc);
    let oa = match cat.get("OpenAction") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected inline /OpenAction dict, got {other:?}"),
    };
    assert_eq!(
        oa.get("S").and_then(|o| o.as_name()),
        Some(&b"URI"[..]),
        "non-GoTo /OpenAction kept verbatim"
    );

    // /OpenAction as a bare name (neither array nor dict): passed through.
    let name_oa = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /OpenAction /SomeName >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut b = Pdf::open_mem_owned(name_oa).unwrap();
    let mut inputs2 = [MergeInput {
        source: &mut b,
        pages: vec![0],
    }];
    let mut doc2 = merge_documents(&mut inputs2).unwrap();
    let cat2 = catalog_dict(&mut doc2);
    assert_eq!(
        cat2.get("OpenAction").and_then(|o| o.as_name()),
        Some(&b"SomeName"[..]),
        "name-form /OpenAction kept verbatim"
    );
}

/// Primary whose outline tree has a NESTED child item targeting a removed page,
/// a cyclic `/Next` back-edge, and a `/First` pointing at a non-dict object,
/// exercising the outline collector's child-descent, cycle-guard, and
/// malformed-item arms.
///
/// - obj 20 "P0" → `/Dest [3 0 R /Fit]` (surviving page0), child 22, `/Next 21`.
/// - obj 21 "P1" → `/Next 20` (cyclic back-edge to the already-visited 20).
/// - obj 22 "sub" → `/Dest [5 0 R /Fit]` (removed page2), `/First 99` (non-dict).
/// - obj 99 = a non-dict object used as 22's `/First` (malformed child head).
fn nested_cyclic_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                10,
                "<< /Type /Outlines /First 20 0 R /Last 21 0 R /Count 2 >>",
            ),
            (
                20,
                "<< /Title (P0) /Parent 10 0 R /Next 21 0 R /First 22 0 R /Last 22 0 R \
                 /Dest [3 0 R /Fit] >>",
            ),
            (
                21,
                "<< /Title (P1) /Parent 10 0 R /Prev 20 0 R /Next 20 0 R >>",
            ),
            (
                22,
                "<< /Title (sub) /Parent 20 0 R /First 99 0 R /Dest [5 0 R /Fit] >>",
            ),
            (99, "42"),
        ],
        1,
    )
}

// A nested outline child targeting a removed page is nulled (child-descent), a
// cyclic /Next back-edge terminates the walk (cycle guard), and a /First
// pointing at a non-dict object is tolerated (malformed-item arm).
#[test]
fn merge_outline_nested_child_cyclic_and_malformed() {
    let mut a = Pdf::open_mem_owned(nested_cyclic_outline_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2);
    let cat = catalog_dict(&mut doc);
    let outlines_ref = cat.get_ref("Outlines").unwrap();
    let items = outline_item_refs(&mut doc, outlines_ref);
    // Walk order: 20, its child 22 (whose malformed /First 99 stops descent),
    // then 21; 21's /Next 20 is a visited back-edge so the walk terminates.
    assert_eq!(items.len(), 3, "items 20, 22, 21 all visited once");

    // Exactly one outline item's /Dest targets the removed page (the nested
    // child item 22); it must be nulled. The parent item 20's /Dest targets the
    // surviving page0 and must NOT be nulled.
    let mut null_dest_count = 0;
    let mut surviving_dest_count = 0;
    for &r in &items {
        let item = doc.resolve(r).unwrap().into_dict().unwrap();
        if let Some(Object::Array(arr)) = item.get("Dest") {
            let arr = arr.clone();
            let (_ref, is_null) = dest_array_first(&mut doc, &arr);
            if is_null {
                null_dest_count += 1;
            } else {
                surviving_dest_count += 1;
            }
        }
    }
    assert_eq!(
        null_dest_count, 1,
        "nested-child dest to removed page nulled"
    );
    assert_eq!(
        surviving_dest_count, 1,
        "parent dest to surviving page kept"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

/// Two-page primary whose `/Names /Dests` name-tree root is a DIRECT dictionary
/// (inline single leaf), not an indirect object. ISO 32000 permits this — the
/// root is referenced only from `/Names /Dests`. The leaf carries a named dest
/// to a surviving page (d_a → page0) and one to a removed page (d_b → page1,
/// dropped when only [0] is selected).
fn inline_dests_root_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /Names << /Dests << /Limits [(d_a) (d_b)] \
                 /Names [(d_a) [3 0 R /Fit] (d_b) [4 0 R /Fit]] >> >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

// An inline (direct-dict) /Names /Dests name-tree root is inherited like an
// indirect one: its named dest to a surviving page is remapped (not null), and
// its named dest to a removed page keeps its reference resolving to null. This
// is the case that previously dropped the primary's named dests silently because
// the root was extracted with get_ref (which returns None for a direct dict).
#[test]
fn merge_inherits_inline_dests_root() {
    let mut a = Pdf::open_mem_owned(inline_dests_root_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 1);
    let page0 = refs[0];
    let cat = catalog_dict(&mut doc);

    // The inline /Dests root was inherited: catalog has /Names → /Dests leaf.
    let names = match cat.get("Names") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /Names, got {other:?}"),
    };
    let leaf = match names.get("Dests") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected /Dests leaf, got {other:?}"),
    };
    let pairs = match leaf.get("Names") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("expected name-tree /Names array, got {other:?}"),
    };
    assert_eq!(pairs.len(), 4, "both named dests inherited (d_a, d_b)");

    // d_a → surviving page0: remapped to its new ref, NOT null.
    let (da_ref, da_null) = dest_array_first(
        &mut doc,
        match &pairs[1] {
            Object::Array(a) => a,
            o => panic!("d_a dest: {o:?}"),
        },
    );
    assert_eq!(da_ref, page0, "inline-root named dest d_a remaps to page0");
    assert!(!da_null, "surviving named dest must not be nulled");

    // d_b → removed page1: reference kept, resolves to null.
    let (_db_ref, db_null) = dest_array_first(
        &mut doc,
        match &pairs[3] {
            Object::Array(a) => a,
            o => panic!("d_b dest: {o:?}"),
        },
    );
    assert!(db_null, "removed named dest target must resolve to null");

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// A primary whose /Outlines root is empty (/Type /Outlines /Count 0, no /First)
// is a legitimate, reachable PDF shape: merge must tolerate it (no crash, no
// dropped pages) and still inherit the (empty) /Outlines root. Exercises the
// missing-/First early return in collect_outline_doc_dests.
#[test]
fn merge_tolerates_empty_outline_root() {
    let empty_outline = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (10, "<< /Type /Outlines /Count 0 >>"),
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(empty_outline).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0, 1],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    // Pages are not dropped.
    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 2, "both pages survive an empty outline root");

    // The empty /Outlines root is inherited and has no /First.
    let cat = catalog_dict(&mut doc);
    let outlines_ref = cat
        .get_ref("Outlines")
        .expect("empty /Outlines root inherited onto output catalog");
    let outlines = doc.resolve(outlines_ref).unwrap().into_dict().unwrap();
    assert!(
        outlines.get("First").is_none(),
        "empty outline root has no /First"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// ---------------------------------------------------------------------------
// Task 5: AcroForm form-field merge with qpdf `+N` name-collision renaming.
// ---------------------------------------------------------------------------

/// One-page form whose single page carries a widget that IS the field (flat
/// form): `/Subtype /Widget /FT /Tx /T (<field_name>)`. The catalog has an
/// `/AcroForm` with that widget in `/Fields`, a `/DR /Font /Helv`, and a `/DA`.
fn form_pdf(field_name: &[u8]) -> Vec<u8> {
    let widget = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T ({}) /Rect [0 0 100 20] /P 3 0 R >>",
        std::str::from_utf8(field_name).expect("field name is valid UTF-8")
    );
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
            ),
            (4, widget.as_str()),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (
                6,
                "<< /Fields [4 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA (/Helv 0 Tf 0 g) >>",
            ),
        ],
        1,
    )
}

/// Two-page form: page 0 carries field `f1` (obj 4), page 1 carries field `f2`
/// (obj 7). `/AcroForm /Fields` lists both. Selecting page 0 only must keep f1
/// and drop the orphan f2.
fn two_page_form_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
            ),
            (
                4,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (f1) /Rect [0 0 100 20] /P 3 0 R >>",
            ),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (
                6,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [7 0 R] >>",
            ),
            (
                7,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (f2) /Rect [0 0 100 20] /P 6 0 R >>",
            ),
            (
                8,
                "<< /Fields [4 0 R 7 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA (/Helv 0 Tf 0 g) >>",
            ),
        ],
        1,
    )
}

/// Resolve the output `/AcroForm /Fields`, returning each field's `/T` partial
/// name (resolving an indirect `/T`). A field without `/T` yields an empty Vec.
fn acroform_field_names(doc: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> Vec<Vec<u8>> {
    let cat = catalog_dict(doc);
    let acroform = match cat.get("AcroForm") {
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /AcroForm, got {other:?}"),
    };
    let fields = match acroform.get("Fields") {
        Some(Object::Array(arr)) => arr.clone(),
        Some(Object::Reference(r)) => match doc.resolve(*r).unwrap() {
            Object::Array(arr) => arr,
            other => panic!("expected indirect /Fields array, got {other:?}"),
        },
        other => panic!("expected /Fields array, got {other:?}"),
    };
    let mut names = Vec::new();
    for item in fields {
        let field_ref = match item {
            Object::Reference(r) => r,
            other => panic!("expected field ref, got {other:?}"),
        };
        let field = doc.resolve(field_ref).unwrap().into_dict().unwrap();
        let name = match field.get("T") {
            Some(Object::String(s)) => s.clone(),
            Some(Object::Reference(r)) => doc
                .resolve(*r)
                .unwrap()
                .as_string()
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        names.push(name);
    }
    names
}

// Two documents whose only field is named `name` → output keeps `name` (the
// primary) and renames the second to `name+1` (qpdf 11.9.0 observed rule).
#[test]
fn merge_renames_colliding_form_fields() {
    let mut a = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
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
    assert_eq!(
        acroform_field_names(&mut doc),
        vec![b"name".to_vec(), b"name+1".to_vec()]
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// A three-way collision renames the second and third occurrences `name+1`,
// `name+2`.
#[test]
fn merge_renames_three_way_collision() {
    let mut a = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut c = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut inputs = [
        MergeInput {
            source: &mut a,
            pages: vec![0],
        },
        MergeInput {
            source: &mut b,
            pages: vec![0],
        },
        MergeInput {
            source: &mut c,
            pages: vec![0],
        },
    ];
    let mut doc = merge_documents(&mut inputs).unwrap();
    assert_eq!(
        acroform_field_names(&mut doc),
        vec![b"name".to_vec(), b"name+1".to_vec(), b"name+2".to_vec()]
    );
}

// A later input whose field is named `name+1` re-resolves to `name+1+1` when the
// `name+1` candidate is already taken by an earlier rename: `name` + `name`
// (primary, second) + a `name+1` field → `name`, `name+1`, `name+1+1`.
#[test]
fn merge_rename_skips_taken_candidate() {
    let mut a = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut c = Pdf::open_mem_owned(form_pdf(b"name+1")).unwrap();
    let mut inputs = [
        MergeInput {
            source: &mut a,
            pages: vec![0],
        },
        MergeInput {
            source: &mut b,
            pages: vec![0],
        },
        MergeInput {
            source: &mut c,
            pages: vec![0],
        },
    ];
    let mut doc = merge_documents(&mut inputs).unwrap();
    assert_eq!(
        acroform_field_names(&mut doc),
        vec![b"name".to_vec(), b"name+1".to_vec(), b"name+1+1".to_vec()]
    );
}

// Distinct field names pass through unchanged.
#[test]
fn merge_keeps_distinct_field_names() {
    let mut a = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"email")).unwrap();
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
    assert_eq!(
        acroform_field_names(&mut doc),
        vec![b"name".to_vec(), b"email".to_vec()]
    );
}

// A form field whose widget is on an UNSELECTED page is dropped from the output
// `/Fields` (qpdf form subset): selecting page 0 of a two-field, two-page form
// keeps only f1.
#[test]
fn merge_drops_orphan_field_of_unselected_page() {
    let mut a = Pdf::open_mem_owned(two_page_form_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();
    assert_eq!(acroform_field_names(&mut doc), vec![b"f1".to_vec()]);

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// The primary's `/AcroForm /DR` / `/DA` are the merged form's base: the output
// `/AcroForm` carries `/DA` and a `/DR /Font /Helv` pointing at the copied
// (remapped) Helvetica font object.
#[test]
fn merge_inherits_primary_acroform_dr_and_da() {
    let mut a = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
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

    let cat = catalog_dict(&mut doc);
    let acroform = match cat.get("AcroForm") {
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected indirect /AcroForm, got {other:?}"),
    };
    // /DA inherited verbatim from the primary.
    assert_eq!(
        acroform.get("DA").and_then(Object::as_string),
        Some(&b"/Helv 0 Tf 0 g"[..]),
        "primary /DA must be inherited"
    );
    // /DR /Font /Helv points at a copied Helvetica font object (remapped).
    let dr = resolve_dict_entry(&mut doc, &acroform, "DR");
    let font = resolve_dict_entry(&mut doc, &dr, "Font");
    let helv_ref = font.get_ref("Helv").expect("/DR /Font /Helv ref");
    let helv = doc.resolve(helv_ref).unwrap().into_dict().unwrap();
    assert_eq!(
        helv.get("BaseFont").and_then(Object::as_name),
        Some(&b"Helvetica"[..]),
        "/DR font must resolve to the copied Helvetica"
    );
}

// The primary's `/AcroForm /DA` stored as an INDIRECT reference must be remapped
// to the copied object, not copied verbatim (which would leave a source object
// number dangling). The output `/DA` must resolve to the original string.
#[test]
fn merge_remaps_indirect_primary_da() {
    let mut a = Pdf::open_mem_owned(form_pdf_indirect_da(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"other")).unwrap();
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

    let cat = catalog_dict(&mut doc);
    let acroform = match cat.get("AcroForm") {
        Some(Object::Reference(r)) => doc.resolve(*r).unwrap().into_dict().unwrap(),
        other => panic!("expected indirect /AcroForm, got {other:?}"),
    };
    // The indirect /DA must point at a copied object that resolves to the
    // original string — never a dangling source ref nor /Null.
    let da_ref = acroform
        .get_ref("DA")
        .expect("indirect /DA must survive as a (remapped) reference");
    assert_eq!(
        doc.resolve(da_ref).unwrap().as_string(),
        Some(&b"/Helv 0 Tf 0 g"[..]),
        "remapped indirect /DA must resolve to the primary's appearance string"
    );
}

// A merge of form-free inputs gains no `/AcroForm` (the merged catalog stays
// form-free rather than growing an empty `/AcroForm`).
#[test]
fn merge_form_free_inputs_have_no_acroform() {
    let mut a = Pdf::open_mem_owned(single_font_pdf(b"Helvetica")).unwrap();
    let mut inputs = [MergeInput {
        source: &mut a,
        pages: vec![0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();
    let cat = catalog_dict(&mut doc);
    assert!(
        cat.get("AcroForm").is_none(),
        "form-free merge must not add an /AcroForm"
    );
}

/// One-page form whose field's `/T` is an INDIRECT reference (obj 7) rather than
/// a direct string, exercising the indirect-`/T` resolve path (review rule 2).
fn form_pdf_indirect_t(field_name: &[u8]) -> Vec<u8> {
    let name = std::str::from_utf8(field_name).expect("field name is valid UTF-8");
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
            ),
            (
                4,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T 7 0 R /Rect [0 0 100 20] /P 3 0 R >>",
            ),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (
                6,
                "<< /Fields [4 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA (/Helv 0 Tf 0 g) >>",
            ),
            (7, &format!("({name})")),
        ],
        1,
    )
}

/// One-page form whose `/AcroForm /DA` is an INDIRECT reference (obj 20) rather
/// than a direct string, exercising the indirect-`/DA` remap path (review rule
/// 2). Obj 20 holds the default-appearance string. The source number is high and
/// sparse on purpose: the fresh target compacts object numbers, so a verbatim
/// (un-remapped) `/DA 20 0 R` would dangle, while a correctly remapped ref still
/// resolves — letting the regression test discriminate the two paths.
fn form_pdf_indirect_da(field_name: &[u8]) -> Vec<u8> {
    let widget = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T ({}) /Rect [0 0 100 20] /P 3 0 R >>",
        std::str::from_utf8(field_name).expect("field name is valid UTF-8")
    );
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
            ),
            (4, widget.as_str()),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (
                6,
                "<< /Fields [4 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA 20 0 R >>",
            ),
            (20, "(/Helv 0 Tf 0 g)"),
        ],
        1,
    )
}

/// One-page form whose top-level field carries NO `/T` (an unnamed widget on a
/// selected page). The field is still copied and appended, but contributes no
/// name to the collision set.
fn form_pdf_no_t() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>",
            ),
            (
                4,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /Rect [0 0 100 20] /P 3 0 R >>",
            ),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (
                6,
                "<< /Fields [4 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA (/Helv 0 Tf 0 g) >>",
            ),
        ],
        1,
    )
}

// A field name stored as an INDIRECT `/T` reference is resolved and used for
// collision detection: `name` (indirect, primary) + `name` (direct, secondary)
// renames the second to `name+1`.
#[test]
fn merge_resolves_indirect_field_name() {
    let mut a = Pdf::open_mem_owned(form_pdf_indirect_t(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
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
    assert_eq!(
        acroform_field_names(&mut doc),
        vec![b"name".to_vec(), b"name+1".to_vec()]
    );
}

// A secondary input's unnamed top-level field (no `/T`) is appended to the
// output `/Fields` without a name and without disturbing the named field's
// collision resolution.
#[test]
fn merge_appends_unnamed_secondary_field() {
    let mut a = Pdf::open_mem_owned(form_pdf(b"name")).unwrap();
    let mut b = Pdf::open_mem_owned(form_pdf_no_t()).unwrap();
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
    // The primary's named field, then the unnamed secondary field (empty name).
    assert_eq!(
        acroform_field_names(&mut doc),
        vec![b"name".to_vec(), Vec::new()]
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}

// ---------------------------------------------------------------------------
// Regression guard: a single indirect /GoTo action shared by an outline item
// AND a page annotation, under a non-identity page remap, must remain ONE object
// whose destination resolves to a single correct page (no double remap).
// ---------------------------------------------------------------------------

/// Primary document with page A (obj 3) and page B (obj 4). One INDIRECT `/GoTo`
/// action object (obj 7, `<< /S /GoTo /D [4 0 R /Fit] >>`) is shared by BOTH the
/// outline item's `/A` (obj 21) AND page A's link annotation's `/A` (obj 6):
/// both carriers literally reference `7 0 R`. The action's `/D` targets page B
/// (obj 4) via an indirect page reference. Because the action is one indirect
/// object referenced from two carriers, the merge closure contains it exactly
/// once and copy_objects rewrites its `/D` page ref a single time.
fn shared_goto_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 20 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R] >>",
            ),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                6,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 7 0 R >>",
            ),
            (7, "<< /S /GoTo /D [4 0 R /Fit] >>"),
            (
                20,
                "<< /Type /Outlines /First 21 0 R /Last 21 0 R /Count 1 >>",
            ),
            (21, "<< /Title (to B) /Parent 20 0 R /A 7 0 R >>"),
        ],
        1,
    )
}

// flpdf-ygoj regression guard. A shared indirect /GoTo action (referenced by both
// an outline item and a page annotation) must, under a non-identity remap caused
// by duplicate page selection, be copied as a SINGLE object whose /D destination
// resolves to a SINGLE correct page (page B). The discriminating property is the
// shared-object identity: both carriers must point at the SAME copied action ref
// (proving copy dedup held — a reintroduced per-carrier remap pass would deep-copy
// the action twice and split them), and that action's /D[0] must remap to page B's
// new ref exactly once (a double remap would misdirect it to a wrong/extra ref).
#[test]
fn merge_shared_goto_action_resolves_to_single_correct_page() {
    let mut p = Pdf::open_mem_owned(shared_goto_pdf()).unwrap();
    // Duplicate page selection (0,1,0) forces a non-identity remap: page A is
    // duplicated, so its 2nd+ occurrence is shallow-cloned at a fresh number.
    let mut inputs = [MergeInput {
        source: &mut p,
        pages: vec![0, 1, 0],
    }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 3, "A, B, A(clone)");
    let page_a = refs[0];
    let page_b = refs[1];

    // Reach the shared action via the OUTLINE item's /A.
    let cat = catalog_dict(&mut doc);
    let outlines_ref = cat.get_ref("Outlines").expect("output has /Outlines");
    let items = outline_item_refs(&mut doc, outlines_ref);
    assert_eq!(items.len(), 1, "single outline item inherited");
    let item = doc.resolve(items[0]).unwrap().into_dict().unwrap();
    let g_from_outline = match item.get("A") {
        Some(Object::Reference(r)) => *r,
        other => panic!("outline item /A must be an indirect reference, got {other:?}"),
    };

    // Reach the SAME action via page A's annotation /A (clean first-occurrence
    // page A, not the shallow clone).
    let annots = annot_refs(&mut doc, page_a);
    assert_eq!(annots.len(), 1, "page A carries one annotation");
    let annot = doc.resolve(annots[0]).unwrap().into_dict().unwrap();
    let g_from_annot = match annot.get("A") {
        Some(Object::Reference(r)) => *r,
        other => panic!("annotation /A must be an indirect reference, got {other:?}"),
    };

    // Strongest guard: both carriers point at the SAME copied action object. A
    // reintroduced separate remap pass that deep-copies per carrier would split
    // these into two distinct refs and fail here.
    assert_eq!(
        g_from_outline, g_from_annot,
        "shared /GoTo action must remain a single object (copy dedup held)"
    );

    // The shared action's /D[0] must resolve to page B's new ref — a SINGLE
    // correct page, remapped exactly once (no double remap, not null).
    let action = doc.resolve(g_from_outline).unwrap().into_dict().unwrap();
    let d = match action.get("D") {
        Some(Object::Array(arr)) => arr.clone(),
        other => panic!("shared action /D must be an array, got {other:?}"),
    };
    let dest_ref = match d.first() {
        Some(Object::Reference(r)) => *r,
        other => panic!("shared action /D[0] must be an indirect reference, got {other:?}"),
    };
    assert_eq!(
        dest_ref, page_b,
        "shared /GoTo dest must resolve to page B's single new ref (no double remap)"
    );
    assert!(
        !matches!(doc.resolve(dest_ref).unwrap(), Object::Null),
        "page B is selected, so its dest target must not be nulled"
    );

    let mut out = Vec::new();
    write_pdf(&mut doc, &mut out).unwrap();
    assert!(Pdf::open_mem_owned(out).is_ok());
}
