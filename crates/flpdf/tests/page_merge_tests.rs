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
