//! Integration tests for [`flpdf::PageObjectHelper`].
//!
//! All tests build in-memory PDFs without touching the filesystem. They
//! exercise the per-page accessor methods (content_streams, resources,
//! rotate, get_annotations, and all bounding-box variants) including
//! inheritance resolution and per-page mutation round-trips.

use flpdf::{
    apply_rotate_to_pages, pages, write_pdf, ContentToken, Error, Object, ObjectRef, PageBox,
    PageObjectHelper, Pdf, RotateMode, RotateOp,
};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Minimal PDF builder helpers
// ---------------------------------------------------------------------------

/// Build a single-page PDF.
///
/// Layout:
///   1 0 R  Catalog
///   2 0 R  Pages  (inheritable attrs from `parent_extras`)
///   3 0 R  Page   (leaf attrs from `page_extras`)
///
/// Both `*_extras` are already-serialised PDF-dictionary key-value pairs
/// (e.g. `"/MediaBox [0 0 612 792]"`).
fn build_single_page_pdf(parent_extras: &str, page_extras: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    let pages_str =
        format!("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 {parent_extras} >>\nendobj\n");
    out.extend_from_slice(pages_str.as_bytes());

    let off3 = out.len() as u64;
    let page_str = format!("3 0 obj\n<< /Type /Page /Parent 2 0 R {page_extras} >>\nendobj\n");
    out.extend_from_slice(page_str.as_bytes());

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Like `build_single_page_pdf` but allows attaching extra indirect objects.
/// `extra_objects` is a slice of `(object_number, serialized_bytes)`.
fn build_pdf_with_extras(
    parent_extras: &str,
    page_extras: &str,
    extra_objects: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    let pages_str =
        format!("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 {parent_extras} >>\nendobj\n");
    out.extend_from_slice(pages_str.as_bytes());

    let off3 = out.len() as u64;
    let page_str = format!("3 0 obj\n<< /Type /Page /Parent 2 0 R {page_extras} >>\nendobj\n");
    out.extend_from_slice(page_str.as_bytes());

    let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in extra_objects {
        let off = out.len() as u64;
        extra_offsets.push((*num, off));
        out.extend_from_slice(body);
    }

    let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
    let total = max_num as usize + 1;
    let xref_start = out.len() as u64;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    xref.push_str(&format!("{:010} 00000 n \n", off1));
    xref.push_str(&format!("{:010} 00000 n \n", off2));
    xref.push_str(&format!("{:010} 00000 n \n", off3));
    for i in 4..=max_num {
        if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

fn make_stream_object(num: u32, body: &[u8]) -> (u32, Vec<u8>) {
    let mut obj_bytes = format!("{num} 0 obj\n<< /Length {} >>\nstream\n", body.len()).into_bytes();
    obj_bytes.extend_from_slice(body);
    obj_bytes.extend_from_slice(b"\nendstream\nendobj\n");
    (num, obj_bytes)
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

// ---------------------------------------------------------------------------
// content_streams()
// ---------------------------------------------------------------------------

#[test]
fn content_streams_empty_when_no_contents() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let tokens = helper.content_streams().unwrap();
    assert!(tokens.is_empty(), "expected no tokens on empty page");
}

#[test]
fn content_streams_tokenizes_single_stream() {
    // Single /Contents stream: "q Q"
    let body = b"q Q";
    let (num, extra) = make_stream_object(4, body);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let tokens = helper.content_streams().unwrap();
    assert!(!tokens.is_empty(), "expected tokens from content stream");
    // q is an op with no operands
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"q")),
        "expected 'q' operator"
    );
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"Q")),
        "expected 'Q' operator"
    );
}

#[test]
fn content_streams_concatenates_array_contents() {
    // Two-element /Contents array — tokens from both streams appear.
    let body1 = b"q";
    let body2 = b"Q";
    let extra1 = make_stream_object(4, body1);
    let extra2 = make_stream_object(5, body2);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents [4 0 R 5 0 R]",
        &[extra1, extra2],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let tokens = helper.content_streams().unwrap();
    // Both q and Q operators must be present.
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"q")),
        "expected 'q' from first stream"
    );
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"Q")),
        "expected 'Q' from second stream"
    );
}

// ---------------------------------------------------------------------------
// resources()
// ---------------------------------------------------------------------------

#[test]
fn resources_returns_direct_resources_on_page() {
    // /Resources directly on the leaf page.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/Resources << /Font << >> >>");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let res = helper.resources().unwrap();
    assert!(res.is_some(), "expected /Resources on leaf page");
    assert!(res.unwrap().get("Font").is_some(), "expected /Font key");
}

#[test]
fn resources_inherits_from_parent() {
    // /Resources only on the /Pages node — must be inherited.
    let bytes = build_single_page_pdf(
        "/MediaBox [0 0 612 792] /Resources << /ProcSet [/PDF] >>",
        "",
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let res = helper.resources().unwrap();
    assert!(res.is_some(), "expected inherited /Resources from parent");
    assert!(
        res.unwrap().get("ProcSet").is_some(),
        "expected /ProcSet in inherited Resources"
    );
}

#[test]
fn resources_returns_none_when_absent() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let res = helper.resources().unwrap();
    assert!(
        res.is_none(),
        "expected Ok(None) when no /Resources anywhere"
    );
}

// ---------------------------------------------------------------------------
// rotate() — getter only
// ---------------------------------------------------------------------------

#[test]
fn rotate_returns_direct_rotate_on_page() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/Rotate 90");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(helper.rotate().unwrap(), 90);
}

#[test]
fn rotate_inherits_from_parent() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792] /Rotate 180", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(helper.rotate().unwrap(), 180);
}

#[test]
fn rotate_inherits_indirect_integer_from_parent() {
    let rotate = (4u32, b"4 0 obj\n270\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/Rotate 4 0 R /MediaBox [0 0 612 792]", "", &[rotate]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    assert_eq!(helper.rotate().unwrap(), 270);
}

#[test]
fn rotate_defaults_to_zero_when_absent() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(helper.rotate().unwrap(), 0);
}

/// Round-trip: apply_rotate_to_pages materializes inherited rotation on the
/// leaf; PageObjectHelper::rotate() should then read the materialized value
/// after write + re-open.
#[test]
fn rotate_round_trip_after_mutation() {
    // Parent has /Rotate 90; leaf has none.  Add 90 → leaf should become 180.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792] /Rotate 90", "");
    let mut pdf = open(bytes);
    let page_ref = ObjectRef::new(3, 0);

    let op = RotateOp {
        mode: RotateMode::Add,
        degrees: 90,
    };
    apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

    // Serialize and re-open.
    let mut serialized: Vec<u8> = Vec::new();
    write_pdf(&mut pdf, &mut serialized).unwrap();
    let mut pdf2 = open(serialized);

    let page_refs = pages::page_refs(&mut pdf2).unwrap();
    let mut helper = PageObjectHelper::new(page_refs[0], &mut pdf2);
    assert_eq!(
        helper.rotate().unwrap(),
        180,
        "materialized rotation must be readable after round-trip"
    );
}

// ---------------------------------------------------------------------------
// get_annotations()
// ---------------------------------------------------------------------------

#[test]
fn get_annotations_empty_when_no_annots() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let annots = helper.get_annotations().unwrap();
    assert!(annots.is_empty(), "expected no annotations");
}

#[test]
fn get_annotations_returns_refs() {
    // Build a PDF with an /Annots array containing two indirect refs.
    // We re-use object numbers 4 and 5 for the annotation dicts.
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let annot5 = (
        5u32,
        b"5 0 obj\n<< /Type /Annot /Subtype /Link >>\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots [4 0 R 5 0 R]",
        &[annot4, annot5],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let annots = helper.get_annotations().unwrap();
    assert_eq!(annots.len(), 2, "expected 2 annotation refs");
    assert_eq!(annots[0], ObjectRef::new(4, 0));
    assert_eq!(annots[1], ObjectRef::new(5, 0));
}

#[test]
fn get_annotations_resolves_indirect_array() {
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let annot_array = (5u32, b"5 0 obj\n[4 0 R]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots 5 0 R",
        &[annot4, annot_array],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let annots = helper.get_annotations().unwrap();

    assert_eq!(annots, vec![ObjectRef::new(4, 0)]);
}

#[test]
fn get_annotations_follows_holder_chain() {
    // /Annots is stored behind a two-hop holder chain:
    //   page /Annots -> 5 0 R -> 6 0 R -> [4 0 R]
    // A single resolve hop would stop at the intermediate reference 6 0 R
    // (not an array) and error; the chain must be followed to the terminal.
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let carrier = (5u32, b"5 0 obj\n6 0 R\nendobj\n".to_vec());
    let annot_array = (6u32, b"6 0 obj\n[4 0 R]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots 5 0 R",
        &[annot4, carrier, annot_array],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let annots = helper.get_annotations().unwrap();

    assert_eq!(annots, vec![ObjectRef::new(4, 0)]);
}

#[test]
fn get_annotations_reference_terminal_not_array_errors() {
    // /Annots is an indirect reference whose terminal is NOT an array.
    // The chain is followed to object 5 (a dictionary), and the helper must
    // surface the specific "does not resolve to an array" error rather than a
    // generic failure.
    let non_array = (5u32, b"5 0 obj\n<< >>\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792]", "/Annots 5 0 R", &[non_array]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    match helper.get_annotations() {
        Err(Error::Unsupported(msg)) => {
            assert!(
                msg.contains("does not resolve to an array"),
                "expected 'does not resolve to an array' message, got: {msg}"
            );
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

#[test]
fn get_annotations_chain_terminal_not_array_errors() {
    // /Annots is stored behind a two-hop holder chain whose terminal is NOT an
    // array:
    //   page /Annots -> 5 0 R -> 6 0 R -> << >>
    // The chain must be followed past the intermediate reference to its
    // non-array terminal, then surface the specific error. A single resolve hop
    // would stop at 6 0 R (still a reference) and never reach the dictionary.
    let carrier = (5u32, b"5 0 obj\n6 0 R\nendobj\n".to_vec());
    let non_array = (6u32, b"6 0 obj\n<< >>\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots 5 0 R",
        &[carrier, non_array],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    match helper.get_annotations() {
        Err(Error::Unsupported(msg)) => {
            assert!(
                msg.contains("does not resolve to an array"),
                "expected 'does not resolve to an array' message, got: {msg}"
            );
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// media_box() — inheritable
// ---------------------------------------------------------------------------

#[test]
fn media_box_on_leaf_page() {
    let bytes = build_single_page_pdf("", "/MediaBox [0 0 612 792]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mb = helper.media_box().unwrap();
    let mb = mb.expect("expected /MediaBox on leaf page");
    assert_eq!(mb, PageBox::new(0.0, 0.0, 612.0, 792.0));
}

#[test]
fn media_box_inherited_from_parent() {
    // /MediaBox only on the /Pages node — must be inherited.
    let bytes = build_single_page_pdf("/MediaBox [0 0 595 842]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mb = helper.media_box().unwrap();
    let mb = mb.expect("expected inherited /MediaBox");
    assert_eq!(mb, PageBox::new(0.0, 0.0, 595.0, 842.0));
}

#[test]
fn media_box_inherits_indirect_array_from_parent() {
    let rect = (4u32, b"4 0 obj\n[0 0 400 500]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox 4 0 R", "", &[rect]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let mb = helper.media_box().unwrap().expect("expected /MediaBox");

    assert_eq!(mb, PageBox::new(0.0, 0.0, 400.0, 500.0));
}

#[test]
fn media_box_leaf_overrides_parent() {
    // Parent has A4; leaf has letter — leaf must win.
    let bytes = build_single_page_pdf("/MediaBox [0 0 595 842]", "/MediaBox [0 0 612 792]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mb = helper.media_box().unwrap().expect("expected /MediaBox");
    assert_eq!(mb.urx, 612.0, "leaf MediaBox must override parent");
}

#[test]
fn media_box_absent_returns_none() {
    let bytes = build_single_page_pdf("", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert!(helper.media_box().unwrap().is_none(), "expected None");
}

// ---------------------------------------------------------------------------
// crop_box() — inheritable, defaults to media_box
// ---------------------------------------------------------------------------

#[test]
fn crop_box_explicit_on_leaf() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/CropBox [10 10 600 780]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let cb = helper.crop_box().unwrap().expect("expected /CropBox");
    assert_eq!(cb, PageBox::new(10.0, 10.0, 600.0, 780.0));
}

#[test]
fn crop_box_defaults_to_media_box_when_absent() {
    // No /CropBox anywhere — should fall back to /MediaBox.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let cb = helper
        .crop_box()
        .unwrap()
        .expect("expected fallback to MediaBox");
    assert_eq!(cb, PageBox::new(0.0, 0.0, 612.0, 792.0));
}

// ---------------------------------------------------------------------------
// bleed_box / trim_box / art_box — leaf-only, fall back to crop_box
// ---------------------------------------------------------------------------

#[test]
fn bleed_box_explicit_on_leaf() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/BleedBox [5 5 607 787]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let bb = helper.bleed_box().unwrap().expect("expected /BleedBox");
    assert_eq!(bb.llx, 5.0);
    assert_eq!(bb.lly, 5.0);
}

#[test]
fn bleed_box_resolves_indirect_leaf_array() {
    let rect = (4u32, b"4 0 obj\n[5 6 607 787]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792]", "/BleedBox 4 0 R", &[rect]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let bb = helper.bleed_box().unwrap().expect("expected /BleedBox");

    assert_eq!(bb, PageBox::new(5.0, 6.0, 607.0, 787.0));
}

#[test]
fn bleed_box_falls_back_to_crop_box() {
    // No BleedBox, CropBox [10 10 600 780] → bleed_box() == crop_box().
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/CropBox [10 10 600 780]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let bb = helper
        .bleed_box()
        .unwrap()
        .expect("expected fallback bleed_box");
    assert_eq!(bb, PageBox::new(10.0, 10.0, 600.0, 780.0));
}

#[test]
fn trim_box_falls_back_to_media_box_when_no_crop_box() {
    // No TrimBox, no CropBox → falls back all the way to MediaBox.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let tb = helper
        .trim_box()
        .unwrap()
        .expect("expected fallback trim_box");
    assert_eq!(tb, PageBox::new(0.0, 0.0, 612.0, 792.0));
}

#[test]
fn art_box_falls_back_to_crop_box() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/CropBox [20 20 590 770]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let ab = helper
        .art_box()
        .unwrap()
        .expect("expected fallback art_box");
    assert_eq!(ab, PageBox::new(20.0, 20.0, 590.0, 770.0));
}

/// Box inheritance round-trip: set MediaBox via set_object, write, re-open,
/// read back via PageObjectHelper.
#[test]
fn media_box_round_trip_after_mutation() {
    // Start with /MediaBox only on parent.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);

    // Materialize a different MediaBox directly on the leaf page.
    let page_obj = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Dictionary(mut page_dict) = page_obj else {
        panic!("expected page dict")
    };
    page_dict.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(500),
            Object::Integer(700),
        ]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

    // Serialize and re-open.
    let mut serialized: Vec<u8> = Vec::new();
    write_pdf(&mut pdf, &mut serialized).unwrap();
    let mut pdf2 = open(serialized);

    let page_refs = pages::page_refs(&mut pdf2).unwrap();
    let mut helper = PageObjectHelper::new(page_refs[0], &mut pdf2);
    let mb = helper
        .media_box()
        .unwrap()
        .expect("expected MediaBox after round-trip");
    assert_eq!(mb.urx, 500.0, "updated MediaBox must survive round-trip");
    assert_eq!(mb.ury, 700.0, "updated MediaBox must survive round-trip");
}

// ---------------------------------------------------------------------------
// PageBox type
// ---------------------------------------------------------------------------

#[test]
fn page_box_fields_are_accessible() {
    let b = PageBox::new(1.0, 2.0, 3.0, 4.0);
    assert_eq!(b.llx, 1.0);
    assert_eq!(b.lly, 2.0);
    assert_eq!(b.urx, 3.0);
    assert_eq!(b.ury, 4.0);
}

// ---------------------------------------------------------------------------
// Regression: accessors must reject a non-leaf /Type /Pages node
// ---------------------------------------------------------------------------

#[test]
fn accessors_reject_pages_tree_node() {
    // Object 2 0 R is the `/Type /Pages` node (not a leaf `/Page`).
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(2, 0), &mut pdf);

    assert!(
        helper.media_box().is_err(),
        "media_box() must reject a /Pages node"
    );
    assert!(
        helper.rotate().is_err(),
        "rotate() must reject a /Pages node"
    );
    assert!(
        helper.resources().is_err(),
        "resources() must reject a /Pages node"
    );
    assert!(
        helper.get_annotations().is_err(),
        "get_annotations() must reject a /Pages node"
    );
    assert!(
        helper.content_streams().is_err(),
        "content_streams() must reject a /Pages node"
    );
}
