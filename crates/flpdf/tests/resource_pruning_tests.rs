//! Tests for `flpdf::resources::remove_unreferenced_resources` (flpdf-9hc.12.4).
//!
//! Acceptance criteria verified:
//!   (a) Single page: /Font has F1, F2; content uses only F1 → F2 removed, F1 kept.
//!   (b) Form XObject recurse: font referenced only inside a Form is NOT pruned.
//!   (c) Auto mode: two pages sharing the same /Resources → NOT pruned.
//!   (d) Yes mode: same shared situation → pruned (union over sharing pages).
//!   (e) No mode: nothing changes.

use flpdf::content_stream::{ContentStreamParser, ContentToken};
use flpdf::resources::{remove_unreferenced_resources, RemoveUnreferencedResources};
use flpdf::{Dictionary, Object, ObjectRef, Pdf};
use std::io::Cursor;

// ── Minimal PDF builder ───────────────────────────────────────────────────────

/// Write a one-line stream object.
fn stream_obj(num: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!("{num} 0 obj\n<< /Length {} >>\nstream\n", body.len()).as_bytes(),
    );
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out
}

/// Write an indirect object (arbitrary dict / non-stream).
fn obj_bytes(num: u32, body: &str) -> Vec<u8> {
    format!("{num} 0 obj\n{body}\nendobj\n").into_bytes()
}

/// Build and parse a PDF from raw object byte slices.
///
/// `page_dicts` – one entry per page, each is the *body* of the Page dict
///               (placed after the standard `/Type /Page /Parent 2 0 R`).
/// `extra`     – additional objects `(number, bytes)` appended verbatim.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages  (/Kids = all page refs)
///   3..    Pages (one per entry in page_dicts)
///   then extra objects
fn build_pdf(page_dicts: &[&str], extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    // We'll write objects and record byte offsets for the xref table.
    let mut offsets: Vec<(u32, u64)> = Vec::new();

    let off1 = pdf.len() as u64;
    offsets.push((1, off1));
    let pages_ref_list: Vec<String> = (0..page_dicts.len())
        .map(|i| format!("{} 0 R", i + 3))
        .collect();
    let kids = pages_ref_list.join(" ");
    let count = page_dicts.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = pdf.len() as u64;
    offsets.push((2, off2));
    pdf.extend_from_slice(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {count} >>\nendobj\n").as_bytes(),
    );

    for (i, page_body) in page_dicts.iter().enumerate() {
        let num = (i + 3) as u32;
        let off = pdf.len() as u64;
        offsets.push((num, off));
        pdf.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page_body} >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    for (num, body) in extra {
        let off = pdf.len() as u64;
        offsets.push((*num, off));
        pdf.extend_from_slice(body);
    }

    let xref_start = pdf.len() as u64;
    let max_num = offsets.iter().map(|(n, _)| *n).max().unwrap_or(2);
    let total = max_num as usize + 1;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    for i in 1..=max_num {
        if let Some((_, off)) = offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    pdf.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

/// Read back the /Font sub-dictionary for the page at `page_ref` from the
/// already-updated pdf.
fn font_dict_keys(pdf: &mut Pdf<Cursor<Vec<u8>>>, res_ref: ObjectRef) -> Vec<String> {
    let res_obj = pdf.resolve(res_ref).expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("resources not a dict");
    };
    match res.get("Font") {
        Some(Object::Dictionary(d)) => d
            .iter()
            .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
            .collect(),
        _ => vec![],
    }
}

// ── Test (a): single page, F1 used, F2 unused → F2 pruned ───────────────────

#[test]
fn test_a_unused_font_pruned_used_font_kept() {
    // Content: uses only /F1.
    // Resources: /Font << /F1 << >> /F2 << >> >>
    let content_body = b"BT /F1 12 Tf (Hello) Tj ET";
    let extra = vec![
        (4u32, stream_obj(4, content_body)),
        // resources dict as indirect object
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
    ];
    // page: /Contents 4 0 R /Resources 5 0 R
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 must be kept: {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "F2 must be pruned: {keys:?}"
    );

    // Verify re-parsing the content stream still works (rendering-safe check).
    let content =
        flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).expect("content bytes");
    let tokens: Vec<_> = ContentStreamParser::new(&content)
        .collect::<Result<Vec<_>, _>>()
        .expect("parse");
    let has_tf = tokens
        .iter()
        .any(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"Tf"));
    assert!(has_tf, "Tf operator must survive");
}

// ── Test (b): Form XObject — font used inside Form must not be pruned ────────

#[test]
fn test_b_form_xobject_recurse_font_kept() {
    // Page content: `Do` invokes /Fm0 Form XObject.
    // Page /Resources has /XObject << /Fm0 6 0 R >> and /Font << /F1 << >> >>
    // The Form XObject 6 0 R uses /F1 in its own content.
    // (Form's /Resources reference back to the same /Font entry via page scope.)

    let form_content = b"BT /F1 10 Tf (inside form) Tj ET";
    // Form XObject stream: /Subtype /Form, content uses /F1.
    let form_stream_bytes = {
        let header = format!(
            "6 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        );
        let mut b = header.into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    // Page content stream just invokes the form.
    let page_content = b"/Fm0 Do";
    let extra = vec![
        (4u32, stream_obj(4, page_content)),
        (6, form_stream_bytes),
        // resources: /XObject has /Fm0, /Font has /F1
        (
            5,
            obj_bytes(
                5,
                "<< /XObject << /Fm0 6 0 R >> /Font << /F1 << /Type /Font >> >> >>",
            ),
        ),
    ];

    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 referenced via Form must NOT be pruned: {keys:?}"
    );
}

// ── Test (c): Auto — shared /Resources not pruned ────────────────────────────

#[test]
fn test_c_auto_shared_resources_not_pruned() {
    // Two pages share object 5 as /Resources.
    // Page 1 content uses F1; page 2 content uses F2.
    // Both pages would have "unused" fonts if checked individually, but since
    // they share the resources dict, Auto mode must leave it untouched.
    //
    // Note: with 2 pages, build_pdf allocates 3 0 R and 4 0 R as page objects.
    // Content streams must use numbers >= 5.

    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";
    let extra = vec![
        // resources dict
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
        // content streams (7 and 8 avoid collision with page objects 3 and 4)
        (7u32, stream_obj(7, content1)),
        (8, stream_obj(8, content2)),
    ];

    // Both pages reference the SAME resources object 5.
    let page1_body = "/Contents 7 0 R /Resources 5 0 R";
    let page2_body = "/Contents 8 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page1_body, page2_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Auto).expect("prune");

    // Both F1 and F2 must remain — shared resources not touched in Auto mode.
    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "Auto: F1 must remain in shared resources: {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "Auto: F2 must remain in shared resources: {keys:?}"
    );
}

// ── Test (d): Yes — shared /Resources pruned via union ───────────────────────

#[test]
fn test_d_yes_shared_resources_pruned_to_union() {
    // Same two-page setup as test (c), but mode = Yes.
    // Page 1 uses F1 only; Page 2 uses F2 only.
    // Union = {F1, F2}. But we also add F3 which is used by neither.
    // Yes mode must prune F3 and keep F1 + F2.
    //
    // Note: with 2 pages, build_pdf allocates 3 0 R and 4 0 R as page objects.
    // Content streams must use numbers >= 5.

    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";
    let extra = vec![
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> /F3 << /Type /Font >> >> >>",
            ),
        ),
        (7u32, stream_obj(7, content1)),
        (8, stream_obj(8, content2)),
    ];

    let page1_body = "/Contents 7 0 R /Resources 5 0 R";
    let page2_body = "/Contents 8 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page1_body, page2_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "Yes: F1 must remain (page1 uses it): {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "Yes: F2 must remain (page2 uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F3".to_string()),
        "Yes: F3 must be pruned (neither page uses it): {keys:?}"
    );
}

// ── Test (e): No — nothing changes ───────────────────────────────────────────

#[test]
fn test_e_no_mode_no_changes() {
    // Same single-page PDF as test (a) but mode = No.
    let content_body = b"BT /F1 12 Tf (Hello) Tj ET";
    let extra = vec![
        (4u32, stream_obj(4, content_body)),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
    ];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::No).expect("prune");

    // Both F1 and F2 must remain — No mode is a strict no-op.
    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "No: F1 must remain: {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "No: F2 must remain: {keys:?}"
    );
}

// ── Test: inline /Resources (direct dict in page) ────────────────────────────

#[test]
fn test_inline_resources_pruned() {
    // Page has /Resources as an inline dict (not indirect reference).
    // Only F1 is used; F2 should be pruned.
    let content_body = b"BT /F1 12 Tf (Hello) Tj ET";
    let extra = vec![(4u32, stream_obj(4, content_body))];

    // /Resources is embedded directly in the page dict (no indirect ref).
    let page_body =
        "/Contents 4 0 R /Resources << /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    // Re-read the page dict to check the inline resources.
    let page_obj = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve page");
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page not a dict");
    };
    let Object::Dictionary(res) = page_dict.get("Resources").expect("resources") else {
        panic!("resources not a dict");
    };
    let Object::Dictionary(fonts) = res.get("Font").expect("font dict") else {
        panic!("font not a dict");
    };
    let keys: Vec<String> = fonts
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "inline: F1 must remain: {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "inline: F2 must be pruned: {keys:?}"
    );
}

// ── NEW TESTS for roborev #802 ────────────────────────────────────────────────

// Helper: build a PDF from raw bytes with an explicit xref table.
// `objects` is a list of (object_number, raw_bytes).
// Caller must include Catalog (1 0 R), Pages (2 0 R) and any page/other objects.
fn build_pdf_raw(objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let mut offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in objects {
        let off = pdf.len() as u64;
        offsets.push((*num, off));
        pdf.extend_from_slice(body);
    }

    let xref_start = pdf.len() as u64;
    let max_num = offsets.iter().map(|(n, _)| *n).max().unwrap_or(2);
    let total = max_num as usize + 1;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    for i in 1..=max_num {
        if let Some((_, off)) = offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    pdf.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

// ── Test (f): indirect category sub-dict (/Font N 0 R) ───────────────────────
//
// /Resources << /Font 6 0 R >> where 6 0 R = << /F1 << >> /F2 << >> >>
// Content uses only /F1. After pruning:
//   - pdf.resolve(6 0 R) must return a dict containing only F1.
//   - F2 must be absent.

#[test]
fn test_f_indirect_category_subdict_pruned() {
    // Object layout:
    //   1 0 R  Catalog
    //   2 0 R  Pages
    //   3 0 R  Page
    //   4 0 R  content stream (uses /F1)
    //   5 0 R  /Resources dict  << /Font 6 0 R >>
    //   6 0 R  /Font sub-dict   << /F1 << >> /F2 << >> >>

    let content_body = b"BT /F1 12 Tf (Hello) Tj ET";
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, content_body)),
        (5, obj_bytes(5, "<< /Font 6 0 R >>")),
        (
            6,
            obj_bytes(6, "<< /F1 << /Type /Font >> /F2 << /Type /Font >> >>"),
        ),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    // Verify the indirect Font sub-dict (6 0 R) was updated in-place.
    let font_obj = pdf
        .resolve(ObjectRef::new(6, 0))
        .expect("resolve font dict");
    let Object::Dictionary(font_dict) = font_obj else {
        panic!("6 0 R is not a dictionary");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 must be kept: {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "F2 must be pruned from indirect Font sub-dict: {keys:?}"
    );
}

// ── Tests for roborev #803: Form XObject own-/Resources scoping (flpdf-9hc.12.4) ──

// Test (h): Form has own /Resources/Font/F2; page /Resources/Font has F2 unused.
//
// Setup:
//   - Page /Resources/Font = { F1 << >>, F2 << >> }
//   - Page /Resources/XObject = { Fm0 → 7 0 R (Form) }
//   - Form (7 0 R) has /Resources << /Font << /F2 << >> >> >>
//   - Form content: BT /F2 10 Tf (inside) Tj ET  (only F2 inside Form)
//   - Page content: /Fm0 Do  (page itself never uses F1 or F2 directly)
//
// Expected after Yes-mode prune:
//   - Page /Font/F2 is PRUNED (Form's own /F2 must not bleed into page used-set)
//   - Page /Font/F1 is also PRUNED (neither page nor Form references it at page scope)
//   - Page /XObject/Fm0 is KEPT (page content invokes it via Do)
#[test]
fn test_h_form_own_resources_do_not_pollute_page_used() {
    let form_content = b"BT /F2 10 Tf (inside form) Tj ET";
    // Form XObject with its own /Resources containing /Font/F2.
    let form_stream = {
        let header = format!(
            "7 0 obj\n<< /Subtype /Form /Length {} /Resources << /Font << /F2 << /Type /Font >> >> >> >>\nstream\n",
            form_content.len()
        );
        let mut b = header.into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    // Page content only invokes the Form; no direct font usage.
    let page_content = b"/Fm0 Do";
    let extra: Vec<(u32, Vec<u8>)> = vec![
        (4, stream_obj(4, page_content)),
        // Page resources: Font with F1 and F2 (both should be pruned),
        // XObject with Fm0 (should be kept because page uses Do).
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> \
                   /XObject << /Fm0 7 0 R >> >>",
            ),
        ),
        (7, form_stream),
    ];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let font_keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    // The Form's own /F2 must NOT have caused page-level /F2 to be retained.
    assert!(
        !font_keys.contains(&"F2".to_string()),
        "test_h: page /Font/F2 must be pruned (Form's own /F2 must not pollute page used-set): {font_keys:?}"
    );
    // /F1 is also unreferenced at page scope.
    assert!(
        !font_keys.contains(&"F1".to_string()),
        "test_h: page /Font/F1 must be pruned (unused at page scope): {font_keys:?}"
    );

    // /XObject/Fm0 must still exist (the page Do-invokes it).
    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("not a dict")
    };
    let Some(Object::Dictionary(xobj_dict)) = res.get("XObject") else {
        panic!("test_h: /XObject sub-dict must remain: {res:?}");
    };
    assert!(
        xobj_dict.get("Fm0").is_some(),
        "test_h: /XObject/Fm0 must remain: {xobj_dict:?}"
    );
}

// Test (i): Form without /Resources inherits page scope — existing behaviour preserved.
//
// Setup:
//   - Page /Resources/Font = { F1 << >> }
//   - Page /Resources/XObject = { Fm0 → 7 0 R (Form, NO /Resources key) }
//   - Form content: BT /F1 10 Tf (text) Tj ET
//   - Page content: /Fm0 Do
//
// Expected after Yes-mode prune:
//   - Page /Font/F1 is KEPT (Form inherited page scope and used F1)
//   - Page /XObject/Fm0 is KEPT
#[test]
fn test_i_form_no_resources_inherits_page_scope() {
    let form_content = b"BT /F1 10 Tf (via inherited scope) Tj ET";
    // Form XObject with NO /Resources key — inherits page resources.
    let form_stream = {
        let header = format!(
            "7 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        );
        let mut b = header.into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    let page_content = b"/Fm0 Do";
    let extra: Vec<(u32, Vec<u8>)> = vec![
        (4, stream_obj(4, page_content)),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> >> /XObject << /Fm0 7 0 R >> >>",
            ),
        ),
        (7, form_stream),
    ];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let font_keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        font_keys.contains(&"F1".to_string()),
        "test_i: page /Font/F1 must be kept (Form inherits page scope and uses F1): {font_keys:?}"
    );

    let Object::Dictionary(res_dict) = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources")
    else {
        panic!("test_i: resources is not a dict");
    };
    let Some(Object::Dictionary(xobj_dict)) = res_dict.get("XObject") else {
        panic!("test_i: /XObject sub-dict must remain: {res_dict:?}");
    };
    assert!(
        xobj_dict.get("Fm0").is_some(),
        "test_i: /XObject/Fm0 must remain: {xobj_dict:?}"
    );
}

// ── Test (g): single page inheriting ancestor inline /Resources — Yes prunes ──
//
// /Pages node has an inline /Resources << /Font << /F1 << >> /F2 << >> >> >>
// Single page has no /Resources of its own, so it inherits.
// Content uses only /F1. Yes mode must prune /F2 from the /Pages dict.

#[test]
fn test_g_ancestor_inline_resources_single_page_yes_prunes() {
    // Object layout:
    //   1 0 R  Catalog
    //   2 0 R  Pages  (has inline /Resources with F1 + F2)
    //   3 0 R  Page   (no /Resources → inherits from 2 0 R)
    //   4 0 R  content stream (uses only /F1)

    let content_body = b"BT /F1 12 Tf (Hello) Tj ET";
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >> >>",
            ),
        ),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
        ),
        (4, stream_obj(4, content_body)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    // The /Pages dict (2 0 R) should now have only F1 in its inline /Resources.
    let pages_obj = pdf.resolve(ObjectRef::new(2, 0)).expect("resolve Pages");
    let Object::Dictionary(pages_dict) = pages_obj else {
        panic!("2 0 R is not a dictionary");
    };
    let Object::Dictionary(res) = pages_dict.get("Resources").expect("Resources key") else {
        panic!("Resources is not a dict");
    };
    let Object::Dictionary(fonts) = res.get("Font").expect("Font key") else {
        panic!("Font is not a dict");
    };
    let keys: Vec<String> = fonts
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 must remain in ancestor inline: {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "F2 must be pruned from ancestor inline (single-page Yes): {keys:?}"
    );
}

// ── Test (h): two pages inheriting same ancestor inline /Resources ─────────────
//
// /Pages has inline /Resources << /Font << /F1 << >> /F2 << >> /F3 << >> >> >>
// Page A uses /F1; Page B uses /F2; neither uses /F3.
// Auto mode: ancestor inline shared → NOT pruned (both F1, F2, F3 remain).
// Yes mode: union = {F1, F2} → F3 pruned, F1 + F2 kept.

#[test]
fn test_h_ancestor_inline_resources_two_pages_auto_not_pruned() {
    // 1=Catalog, 2=Pages(inline /Resources F1+F2+F3), 3=Page1, 4=Page2,
    // 5=content1(uses F1), 6=content2(uses F2)
    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Resources << /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> /F3 << /Type /Font >> >> >> >>",
            ),
        ),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>",
            ),
        ),
        (
            4,
            obj_bytes(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>",
            ),
        ),
        (5, stream_obj(5, content1)),
        (6, stream_obj(6, content2)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    // Auto mode: shared ancestor inline → nothing pruned.
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes.clone())).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Auto).expect("prune");

    let pages_obj = pdf.resolve(ObjectRef::new(2, 0)).expect("resolve Pages");
    let Object::Dictionary(pages_dict) = pages_obj else {
        panic!("2 0 R is not a dict");
    };
    let Object::Dictionary(res) = pages_dict.get("Resources").expect("Resources") else {
        panic!("Resources not a dict");
    };
    let Object::Dictionary(fonts) = res.get("Font").expect("Font") else {
        panic!("Font not a dict");
    };
    let keys: Vec<String> = fonts
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "Auto: F1 must remain: {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "Auto: F2 must remain: {keys:?}"
    );
    assert!(
        keys.contains(&"F3".to_string()),
        "Auto: F3 must remain (shared ancestor): {keys:?}"
    );
}

#[test]
fn test_h2_ancestor_inline_resources_two_pages_yes_union_prunes() {
    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Resources << /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> /F3 << /Type /Font >> >> >> >>",
            ),
        ),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>",
            ),
        ),
        (
            4,
            obj_bytes(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>",
            ),
        ),
        (5, stream_obj(5, content1)),
        (6, stream_obj(6, content2)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    // Yes mode: union = {F1, F2}, so F3 pruned.
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let pages_obj = pdf.resolve(ObjectRef::new(2, 0)).expect("resolve Pages");
    let Object::Dictionary(pages_dict) = pages_obj else {
        panic!("2 0 R is not a dict");
    };
    let Object::Dictionary(res) = pages_dict.get("Resources").expect("Resources") else {
        panic!("Resources not a dict");
    };
    let Object::Dictionary(fonts) = res.get("Font").expect("Font") else {
        panic!("Font not a dict");
    };
    let keys: Vec<String> = fonts
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "Yes: F1 must remain (p1 uses it): {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "Yes: F2 must remain (p2 uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F3".to_string()),
        "Yes: F3 must be pruned (neither page uses it): {keys:?}"
    );
}

// ── Test: /ExtGState, /Shading, /Pattern, /ColorSpace, /Properties pruning ───

#[test]
fn test_other_categories_pruned() {
    // Content uses:
    //   gs GS1   → ExtGState /GS1 kept, /GS2 pruned
    //   /CS1 cs  → ColorSpace /CS1 kept, /CS2 pruned
    //   /Pat1 scn → Pattern /Pat1 kept, /Pat2 pruned
    //   /Sh1 sh  → Shading /Sh1 kept, /Sh2 pruned
    //   /tag /Prop1 BDC … EMC → Properties /Prop1 kept, /Prop2 pruned
    let content = b"/GS1 gs /CS1 cs /Pat1 scn /Sh1 sh /tag /Prop1 BDC EMC";
    let res_body = "<< \
        /ExtGState << /GS1 << >> /GS2 << >> >> \
        /ColorSpace << /CS1 [ /CalRGB << >> ] /CS2 [ /CalRGB << >> ] >> \
        /Pattern << /Pat1 << /PatternType 1 >> /Pat2 << /PatternType 1 >> >> \
        /Shading << /Sh1 << /ShadingType 2 >> /Sh2 << /ShadingType 2 >> >> \
        /Properties << /Prop1 << >> /Prop2 << >> >> \
    >>";
    let extra = vec![(4u32, stream_obj(4, content)), (5, obj_bytes(5, res_body))];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("resources not a dict");
    };

    fn sub_keys(res: &Dictionary, cat: &str) -> Vec<String> {
        match res.get(cat) {
            Some(Object::Dictionary(d)) => d
                .iter()
                .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
                .collect(),
            _ => vec![],
        }
    }

    let gs_keys = sub_keys(&res, "ExtGState");
    assert!(
        gs_keys.contains(&"GS1".to_string()),
        "GS1 kept: {gs_keys:?}"
    );
    assert!(
        !gs_keys.contains(&"GS2".to_string()),
        "GS2 pruned: {gs_keys:?}"
    );

    let cs_keys = sub_keys(&res, "ColorSpace");
    assert!(
        cs_keys.contains(&"CS1".to_string()),
        "CS1 kept: {cs_keys:?}"
    );
    assert!(
        !cs_keys.contains(&"CS2".to_string()),
        "CS2 pruned: {cs_keys:?}"
    );

    let pat_keys = sub_keys(&res, "Pattern");
    assert!(
        pat_keys.contains(&"Pat1".to_string()),
        "Pat1 kept: {pat_keys:?}"
    );
    assert!(
        !pat_keys.contains(&"Pat2".to_string()),
        "Pat2 pruned: {pat_keys:?}"
    );

    let sh_keys = sub_keys(&res, "Shading");
    assert!(
        sh_keys.contains(&"Sh1".to_string()),
        "Sh1 kept: {sh_keys:?}"
    );
    assert!(
        !sh_keys.contains(&"Sh2".to_string()),
        "Sh2 pruned: {sh_keys:?}"
    );

    let prop_keys = sub_keys(&res, "Properties");
    assert!(
        prop_keys.contains(&"Prop1".to_string()),
        "Prop1 kept: {prop_keys:?}"
    );
    assert!(
        !prop_keys.contains(&"Prop2".to_string()),
        "Prop2 pruned: {prop_keys:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// roborev 指摘1: 間接 category サブ辞書の共有が未考慮
// ═══════════════════════════════════════════════════════════════════════════════
//
// Setup: 2 pages each have a DIFFERENT top-level /Resources object (5 0 R and
// 7 0 R), but BOTH point to the SAME indirect /Font sub-dict (6 0 R).
//
//   5 0 R  << /Font 6 0 R >>    ← page 1's /Resources
//   7 0 R  << /Font 6 0 R >>    ← page 2's /Resources
//   6 0 R  << /F1 << >> /F2 << >> /F3 << >> >>   ← shared Font sub-dict
//
//   Page 1 content: uses /F1
//   Page 2 content: uses /F2
//
// Yes mode: union = {F1, F2}, F3 pruned.  6 0 R must contain F1 and F2.
// Auto mode: 6 0 R is shared across two top-level groups → must NOT be pruned.

#[test]
fn test_roborev1_shared_indirect_font_subdict_yes_union() {
    // Object layout:
    //   1 0 R  Catalog
    //   2 0 R  Pages  /Kids [3 0 R 4 0 R]
    //   3 0 R  Page 1  /Resources 5 0 R  /Contents 8 0 R
    //   4 0 R  Page 2  /Resources 7 0 R  /Contents 9 0 R
    //   5 0 R  Resources for page 1: << /Font 6 0 R >>
    //   6 0 R  Shared Font sub-dict: << /F1 << >> /F2 << >> /F3 << >> >>
    //   7 0 R  Resources for page 2: << /Font 6 0 R >>
    //   8 0 R  Content stream for page 1: uses /F1
    //   9 0 R  Content stream for page 2: uses /F2

    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";

    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            ),
        ),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R /Resources 5 0 R >>",
            ),
        ),
        (
            4,
            obj_bytes(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R /Resources 7 0 R >>",
            ),
        ),
        (5, obj_bytes(5, "<< /Font 6 0 R >>")),
        (
            6,
            obj_bytes(
                6,
                "<< /F1 << /Type /Font >> /F2 << /Type /Font >> /F3 << /Type /Font >> >>",
            ),
        ),
        (7, obj_bytes(7, "<< /Font 6 0 R >>")),
        (8, stream_obj(8, content1)),
        (9, stream_obj(9, content2)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    // The shared Font sub-dict (6 0 R) must contain F1 and F2 (union), and F3
    // must have been pruned.
    let font_obj = pdf
        .resolve(ObjectRef::new(6, 0))
        .expect("resolve shared font dict");
    let Object::Dictionary(font_dict) = font_obj else {
        panic!("6 0 R is not a dictionary");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "Yes: F1 must remain (page1 uses it): {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "Yes: F2 must remain (page2 uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F3".to_string()),
        "Yes: F3 must be pruned (neither page uses it): {keys:?}"
    );
}

#[test]
fn test_roborev1_shared_indirect_font_subdict_auto_protected() {
    // Same layout as above.  Auto mode: the shared cat-ref (6 0 R) appears in
    // two distinct top-level /Resources groups, so it must NOT be pruned even
    // though each top-level group is individually "unshared" at page level.

    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";

    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            ),
        ),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R /Resources 5 0 R >>",
            ),
        ),
        (
            4,
            obj_bytes(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R /Resources 7 0 R >>",
            ),
        ),
        (5, obj_bytes(5, "<< /Font 6 0 R >>")),
        (
            6,
            obj_bytes(
                6,
                "<< /F1 << /Type /Font >> /F2 << /Type /Font >> /F3 << /Type /Font >> >>",
            ),
        ),
        (7, obj_bytes(7, "<< /Font 6 0 R >>")),
        (8, stream_obj(8, content1)),
        (9, stream_obj(9, content2)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Auto).expect("prune");

    // All three fonts must remain — Auto must not prune a shared cat-ref.
    let font_obj = pdf
        .resolve(ObjectRef::new(6, 0))
        .expect("resolve shared font dict");
    let Object::Dictionary(font_dict) = font_obj else {
        panic!("6 0 R is not a dictionary");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "Auto: F1 must remain (shared cat-ref protected): {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "Auto: F2 must remain (shared cat-ref protected): {keys:?}"
    );
    assert!(
        keys.contains(&"F3".to_string()),
        "Auto: F3 must remain (shared cat-ref protected): {keys:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Direct-stream Form XObject → conservatively retain the page (flpdf-u79t)
// ═══════════════════════════════════════════════════════════════════════════════
//
// Setup: the /XObject sub-dict in the page /Resources contains a *direct*
// Object::Stream Form XObject (not an indirect reference).  The Form has no
// /Resources of its own, so it inherits the page's resources.  The Form
// content uses /F1 via Tf.  Page content just invokes the Form via Do.
//
// A direct (inline) stream is malformed per ISO 32000-2 §7.3.8 (streams shall
// be indirect objects) and has no ObjectRef identity, so the ever-seen cycle
// guard cannot dedup a self-reference reached through inherited resources
// (`/Fm0 Do /Fm0 Do` would recurse exponentially — see
// `test_direct_stream_form_self_recursion_no_dos`).  qpdf does not even parse an
// inline stream as a Form (it tokenises `stream` as a stray string and mangles
// the surrounding dictionary), so there is no byte-identical target here.
//
// flpdf therefore refuses to recurse into a direct-stream Form and reports the
// page as incomplete, conservatively retaining ALL of the page's resources.
// That avoids both the DoS and dropping /F1 (which the Form genuinely uses under
// flpdf's more-lenient parse): nothing is pruned, so /F1 and /F2 both survive.

#[test]
fn test_direct_stream_form_xobject_page_conservatively_retained() {
    // Form XObject content: uses /F1 via Tf.
    // The Form has no /Resources — it inherits the page's scope.
    let form_content = b"BT /F1 10 Tf (direct form) Tj ET";

    // We build the PDF bytes manually so that the /XObject sub-dict entry for
    // /Fm0 is a direct stream (not an indirect reference).
    //
    // Object layout:
    //   1 0 R  Catalog
    //   2 0 R  Pages  /Kids [3 0 R]
    //   3 0 R  Page   /Resources 5 0 R  /Contents 4 0 R
    //   4 0 R  Content stream: /Fm0 Do
    //   5 0 R  /Resources dict with direct-stream XObject and /Font sub-dict
    //
    // The /Resources dict (5 0 R) looks like:
    //   << /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >>
    //      /XObject << /Fm0 << /Subtype /Form /Length N >> stream … endstream >> >>
    //
    // Note: the outer object is a Dictionary (not a Stream), so pdf readers
    // parse the inline stream inside the dict value as a direct Object::Stream.

    let form_len = form_content.len();

    // Build the raw resources dict object string.
    // We embed the Form stream directly as a dict value.
    let res_obj_body = format!(
        "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> \
         /XObject << /Fm0 << /Subtype /Form /Length {form_len} >> stream\n{}\nendstream >> >>",
        std::str::from_utf8(form_content).unwrap()
    );

    let page_content = b"/Fm0 Do";
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, page_content)),
        (5, obj_bytes(5, &res_obj_body)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open PDF with direct-stream Form");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    // The page contains a direct-stream Form → flpdf cannot safely analyse its
    // resource usage, so it retains the whole page. Both /F1 and /F2 survive.
    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res_dict) = res_obj else {
        panic!("5 0 R is not a dictionary");
    };
    let font_entry = res_dict.get("Font").expect("/Font key must exist");
    let Object::Dictionary(font_dict) = font_entry else {
        panic!("/Font is not a dict");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "direct-stream Form: /F1 must be kept (page conservatively retained): {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "direct-stream Form: /F2 must also be kept — the page is retained, not pruned: {keys:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// DoS regression: Form XObject traversal must not blow up exponentially
// (flpdf-u79t; ChatGPT Codex Cloud security finding 31f83f8b)
// ═══════════════════════════════════════════════════════════════════════════════

// (A) Direct-stream Form self-recursion via inherited resources. The page maps
// /Fm0 to a direct stream whose content is `/Fm0 Do /Fm0 Do`; with no ObjectRef
// identity the old stack-pop guard could not dedup it, so branching recursion
// blew up to 2^depth. The fix refuses to recurse into direct-stream Forms, so
// pruning must complete promptly (the test harness would otherwise hang).
#[test]
fn test_direct_stream_form_self_recursion_no_dos() {
    let form_content = b"/Fm0 Do /Fm0 Do";
    let res_obj_body = format!(
        "<< /Font << /F1 << /Type /Font >> >> \
         /XObject << /Fm0 << /Subtype /Form /Length {} >> stream\n{}\nendstream >> >>",
        form_content.len(),
        std::str::from_utf8(form_content).unwrap()
    );
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        (5, obj_bytes(5, &res_obj_body)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open recursive direct-stream Form PDF");
    // Must return promptly rather than recursing exponentially.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must complete without exponential recursion");

    // Direct-stream Form ⇒ page conservatively retained ⇒ /F1 kept.
    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font key must exist");
    };
    assert!(
        font_dict.get("F1").is_some(),
        "direct-stream self-recursion: page retained, /F1 kept"
    );
}

// (B) Indirect-reference DAG: Fm(i)'s content is `/Fm(i+1) Do /Fm(i+1) Do`, a
// chain of 40 *distinct* indirect Forms. This is not a cycle, so a stack-pop
// `visited` guard would re-traverse each node 2^level times (≈2^40). The
// ever-seen guard expands each Form once, so pruning completes in O(V+E) and
// matches qpdf: every reachable Form is kept (each resource-less Form's used
// /Fm(i+1) bubbles to the page), while an unused page font is still pruned.
//
// Verified against qpdf 11.9.0:
//   qpdf --remove-unreferenced-resources=yes --split-pages=1 dag.pdf out.pdf
//   → completes in ~0.00s; page /XObject keeps all /Fm0../Fm40; unused /F2 pruned.
#[test]
fn test_indirect_dag_forms_no_exponential_blowup() {
    const N: u32 = 40;
    let mut objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 4 0 R >>",
            ),
        ),
    ];
    // Page /Resources (4): /XObject maps Fm0..FmN to objects 100+i; plus an
    // unused /F2 font that must still be pruned.
    let mut xobj_entries = String::new();
    for i in 0..=N {
        xobj_entries.push_str(&format!("/Fm{i} {} 0 R ", 100 + i));
    }
    objects.push((
        4,
        obj_bytes(
            4,
            &format!("<< /Font << /F2 << /Type /Font >> >> /XObject << {xobj_entries} >> >>"),
        ),
    ));
    // Page content invokes Fm0 once.
    objects.push((5, stream_obj(5, b"/Fm0 Do")));
    // Form objects 100+i: no /Resources (inherit page), content branches twice.
    for i in 0..=N {
        let content = if i < N {
            format!("/Fm{} Do /Fm{} Do", i + 1, i + 1)
        } else {
            String::new()
        };
        let mut v = format!(
            "{} 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            100 + i,
            content.len()
        )
        .into_bytes();
        v.extend_from_slice(content.as_bytes());
        v.extend_from_slice(b"\nendstream\nendobj\n");
        objects.push((100 + i, v));
    }
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open indirect-DAG PDF");
    // Must complete in O(V+E), not 2^N.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must complete without exponential recursion");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve res")
    else {
        panic!("4 0 R is not a dictionary");
    };
    // Every reachable Form is kept (bubbled up through the resource-less chain),
    // matching qpdf.
    let Some(Object::Dictionary(xobj_dict)) = res_dict.get("XObject") else {
        panic!("/XObject key must exist");
    };
    for i in 0..=N {
        assert!(
            xobj_dict.get(format!("Fm{i}").as_bytes()).is_some(),
            "indirect-DAG: /Fm{i} must be kept (reachable Form)"
        );
    }
    // The unused page font is still pruned.
    assert!(
        res_dict.get("Font").is_none()
            || matches!(res_dict.get("Font"), Some(Object::Dictionary(d)) if d.get("F2").is_none()),
        "indirect-DAG: unused /F2 must be pruned"
    );
}

// An inline image inside an own-/Resources Form must not leak its /CS colour
// space to the page's used set (record_direct == false path). The page's own
// /ColorSpace with the same abbreviation is therefore still pruned. (flpdf-u79t)
#[test]
fn test_inline_image_in_own_resources_form_cs_not_bubbled_to_page() {
    // Form Fm0 (6 0 R): has its OWN /Resources; content is a single inline image
    // referencing colour space /Foo. Because the Form owns its resources, that
    // /Foo resolves against the Form's scope and must NOT be attributed to the
    // page.
    let form_content = b"BI /CS /Foo /W 1 /H 1 /BPC 8 ID \x00 EI";
    let form_stream = {
        let res = "/Resources << /ColorSpace << /Foo [ /CalRGB << >> ] >> >>";
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form {} /Length {} >>\nstream\n",
            res,
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // Page /Resources: an own /ColorSpace/Foo the page never references, plus
        // the Form XObject.
        (
            5,
            obj_bytes(
                5,
                "<< /ColorSpace << /Foo [ /CalRGB << >> ] >> /XObject << /Fm0 6 0 R >> >>",
            ),
        ),
        (6, form_stream),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    assert!(
        res_dict.get("ColorSpace").is_none(),
        "page /ColorSpace/Foo must be pruned — the inline image lives in the Form's \
         own resource scope, not the page's: {:?}",
        res_dict.get("ColorSpace")
    );
}

// An indirect /XObject entry whose ref-chain terminal is NOT a stream (e.g. a
// dictionary) is simply not recursed into; the page stays complete and prunes
// normally. (flpdf-u79t: covers the non-stream terminal arm.)
#[test]
fn test_indirect_xobject_entry_non_stream_terminal_handled() {
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // /Fm0 is an indirect reference to a NON-stream object (a plain dict).
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F2 << /Type /Font >> >> /XObject << /Fm0 9 0 R >> >>",
            ),
        ),
        (9, obj_bytes(9, "<< /NotA /Stream >>")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    // /Fm0 is recorded as used by the page's Do → its category survives.
    let Some(Object::Dictionary(xobj_dict)) = res_dict.get("XObject") else {
        panic!("/XObject must survive (Fm0 used via Do)");
    };
    assert!(
        xobj_dict.get("Fm0").is_some(),
        "/XObject/Fm0 must remain (used by page): {xobj_dict:?}"
    );
    // The unused page font is still pruned (page stayed complete).
    assert!(
        res_dict.get("Font").is_none(),
        "unused /Font must be pruned — a non-stream XObject terminal keeps the page complete: {res_dict:?}"
    );
}

// A Form XObject whose content stream fails to DECODE (corrupt filter) makes the
// page incomplete → its resources are conservatively retained. (flpdf-u79t:
// covers the Form decode-failure arm.)
#[test]
fn test_form_decode_failure_retains_page() {
    // Fm0 (6 0 R): declares /Filter /FlateDecode but the body is not valid flate.
    let bad_body = b"this is not valid flate data";
    let form_stream = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Filter /FlateDecode /Length {} >>\nstream\n",
            bad_body.len()
        )
        .into_bytes();
        b.extend_from_slice(bad_body);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // Page has an unused font that would normally be pruned.
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> >> /XObject << /Fm0 6 0 R >> >>",
            ),
        ),
        (6, form_stream),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    // The Form could not be decoded → page conservatively retained → /F1 kept
    // even though the page's own content never references it.
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font must be retained when a Form fails to decode");
    };
    assert!(
        font_dict.get("F1").is_some(),
        "decode failure: page retained, /F1 kept: {font_dict:?}"
    );
}

// A page-level inline image referencing a NON-built-in colour space records that
// name in the page's used set, keeping it while an unused sibling is pruned.
// (flpdf-u79t: covers the inline-image /CS recording under record_direct.)
#[test]
fn test_page_inline_image_nonbuiltin_cs_kept() {
    // /Foo is used by the inline image; /Bar is never referenced.
    let content = b"BI /CS /Foo /W 1 /H 1 /BPC 8 ID \x00 EI";
    let res_body = "<< /ColorSpace << /Foo [ /CalRGB << >> ] /Bar [ /CalRGB << >> ] >> >>";
    let extra = vec![(4u32, stream_obj(4, content)), (5, obj_bytes(5, res_body))];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res") else {
        panic!("5 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(cs)) = res.get("ColorSpace") else {
        panic!("/ColorSpace must survive (/Foo used by inline image)");
    };
    assert!(
        cs.get("Foo").is_some(),
        "/ColorSpace/Foo must be kept (used by inline image): {cs:?}"
    );
    assert!(
        cs.get("Bar").is_none(),
        "/ColorSpace/Bar must be pruned (unused): {cs:?}"
    );
}

// A Form XObject whose OWN /Resources is an indirect reference is resolved via
// that reference; its names stay scoped to the Form and do not leak to the page.
// (flpdf-u79t: covers the indirect own-/Resources resolution arm.)
#[test]
fn test_form_own_resources_indirect_reference_resolved() {
    // Fm0 (6 0 R): own /Resources is `7 0 R`; content uses /FA (its own font).
    let form_content = b"BT /FA 10 Tf (own res) Tj ET";
    let form_stream = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Resources 7 0 R /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // Page /Font/FA shares the Form font's name but the page never uses it →
        // it must be pruned (the Form's /FA is scoped to the Form's own resources).
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /FA << /Type /Font >> >> /XObject << /Fm0 6 0 R >> >>",
            ),
        ),
        (6, form_stream),
        // Form's own /Resources (indirect): its own /FA font.
        (7, obj_bytes(7, "<< /Font << /FA << /Type /Font >> >> >>")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    // The Form owns its resource scope (resolved via 7 0 R), so its /FA does not
    // bubble to the page; the page's own unused /FA font is pruned.
    assert!(
        res_dict.get("Font").is_none(),
        "page /Font must be pruned — the Form's /FA is scoped to its own (indirect) /Resources: {res_dict:?}"
    );
}

// A Form whose OWN /Resources is a MULTI-hop holder chain (ref -> ref -> dict)
// must be followed to its terminal dict; otherwise a resource-less Form nested
// inside it is unreachable and its page-relevant font is wrongly pruned.
// (flpdf-u79t / gemini review of PR #442; supersedes flpdf-73cp now that nested
// Forms under own-resources Forms bubble to the page.)
#[test]
fn test_form_own_resources_multi_hop_chain_nested_form_font_kept() {
    // Fm0 (6): own /Resources is `7 0 R` → `8 0 R` → dict (2-hop). That dict maps
    // /Fx to a resource-less Form (9) which uses the PAGE font /FP.
    let fm0_content = b"/Fx Do";
    let fm0 = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Resources 7 0 R /Length {} >>\nstream\n",
            fm0_content.len()
        )
        .into_bytes();
        b.extend_from_slice(fm0_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let fx_content = b"BT /FP 10 Tf (nested) Tj ET";
    let fx = {
        let mut b = format!(
            "9 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            fx_content.len()
        )
        .into_bytes();
        b.extend_from_slice(fx_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // Page /Font: /FP is used by the nested Form (via bubble-to-page); /FQ is
        // unused and must be pruned.
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /FP << /Type /Font >> /FQ << /Type /Font >> >> /XObject << /Fm0 6 0 R >> >>",
            ),
        ),
        (6, fm0),
        // 2-hop holder chain for Fm0's own /Resources: 7 -> 8 -> dict.
        (7, obj_bytes(7, "8 0 R")),
        (8, obj_bytes(8, "<< /XObject << /Fx 9 0 R >> >>")),
        (9, fx),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font must survive (/FP used by nested Form via 2-hop /Resources)");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"FP".to_string()),
        "multi-hop /Resources: /FP must be kept — the nested resource-less Form uses \
         it, but is only reachable if Fm0's chained /Resources is followed: {keys:?}"
    );
    assert!(
        !keys.contains(&"FQ".to_string()),
        "multi-hop /Resources: unused /FQ must still be pruned: {keys:?}"
    );
}

// Two DISTINCT reference aliases that resolve to the SAME terminal Form are
// deduplicated once, keyed on the terminal ref (matching qpdf's terminal
// `QPDFObjGen`). Pruning completes and yields the same result either way — this
// documents the aliasing scenario and guards against a crash / regression in the
// terminal-ref resolution. (flpdf-u79t / gemini review of PR #442.)
//
// NB: first-hop keying is *not* exponential for shared aliases — a shared
// terminal's children carry fixed refs that dedup after the first walk, so the
// redundant re-entry is O(children). The terminal-ref key still matches qpdf and
// removes that redundancy; it also closes the one path that could re-walk a
// subtree (a resource-less Form reached under divergent inherited scopes).
#[test]
fn test_aliased_refs_to_same_terminal_pruned_consistently() {
    // Page content invokes /Aa and /Ab; both alias the SAME Form (7 0 R) through
    // distinct one-hop objects (5 0 R, 6 0 R). That Form uses page font /FP.
    let form_content = b"BT /FP 10 Tf (shared terminal) Tj ET";
    let form = {
        let mut b = format!(
            "7 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 8 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Aa Do /Ab Do")),
        // /Aa and /Ab are distinct one-hop objects, both aliasing terminal 7 0 R.
        (5, obj_bytes(5, "7 0 R")),
        (6, obj_bytes(6, "7 0 R")),
        (7, form),
        // Page /Resources: the two aliases + /FP (used by the shared Form) and
        // /FQ (unused → pruned).
        (
            8,
            obj_bytes(
                8,
                "<< /Font << /FP << /Type /Font >> /FQ << /Type /Font >> >> \
                   /XObject << /Aa 5 0 R /Ab 6 0 R >> >>",
            ),
        ),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open aliased-terminal PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must complete");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(8, 0)).expect("resolve res")
    else {
        panic!("8 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font must survive (/FP used by the shared terminal Form)");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"FP".to_string()),
        "aliased terminal: /FP must be kept (used by the shared Form): {keys:?}"
    );
    assert!(
        !keys.contains(&"FQ".to_string()),
        "aliased terminal: unused /FQ must be pruned: {keys:?}"
    );
}

// A resource-less Form reached under TWO scopes that resolve its nested `Do`
// name differently must contribute BOTH scopes' names. Keying `visited` on the
// Form alone (ignoring scope) would mark it seen on the first reach and skip the
// second, over-pruning a resource the page renders. (flpdf-u79t / Codex review
// r3516069927 of PR #442.)
//
// Layout: page content `/Fa Do /Fx Do`.
//   - Fa (6) has its own /Resources mapping only /Fx.  Content: `/Fx Do`.
//   - Fx (7) is resource-less.  Content: `/G Do`.
//   - Page /XObject maps /G to Form 8, which uses page font /F1.
// Under Fa's scope, Fx's `/G` does not resolve (Fa lacks /G); only under the
// PAGE scope does `/G` reach Form 8 and its /F1. Verified with qpdf 11.9.0
// (--remove-unreferenced-resources=yes) which keeps /F1.
#[test]
fn test_resource_less_form_two_scopes_diverging_do_keeps_page_font() {
    let form_obj = |num: u32, extra: &str, body: &[u8]| -> Vec<u8> {
        let mut v = format!(
            "{num} 0 obj\n<< /Subtype /Form {extra}/Length {} >>\nstream\n",
            body.len()
        )
        .into_bytes();
        v.extend_from_slice(body);
        v.extend_from_slice(b"\nendstream\nendobj\n");
        v
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fa Do /Fx Do")),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> >> \
                   /XObject << /Fa 6 0 R /Fx 7 0 R /G 8 0 R >> >>",
            ),
        ),
        // Fa: own /Resources maps only /Fx; invokes /Fx.
        (
            6,
            form_obj(6, "/Resources << /XObject << /Fx 7 0 R >> >> ", b"/Fx Do"),
        ),
        // Fx: resource-less; invokes /G.
        (7, form_obj(7, "", b"/G Do")),
        // G: resource-less; uses page font /F1.
        (8, form_obj(8, "", b"BT /F1 10 Tf (g) Tj ET")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open scope-divergent PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font must survive: the page's /Fx Do -> /G Do -> Form 8 uses /F1");
    };
    assert!(
        font_dict.get("F1").is_some(),
        "/F1 must be kept — reachable only via Fx under the PAGE scope, which a \
         Form-only visited key would skip after Fx is seen under Fa: {font_dict:?}"
    );
}

// A Form XObject with `/Resources null` inherits the calling scope (ISO 32000-2
// §7.3.9: a null entry is equivalent to absent), so its inherited names are
// page-relevant and recorded. Verified with qpdf 11.9.0
// (--remove-unreferenced-resources=yes keeps /F1, prunes /F2). Treating the key
// as present (own-resources) would drop /F1 → over-prune. (flpdf-u79t /
// coderabbit review of PR #442.)
#[test]
fn test_form_resources_null_is_inherited_not_own_scope() {
    let form_content = b"BT /F1 10 Tf (null-res form) Tj ET";
    let form = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Resources null /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> /XObject << /Fm0 6 0 R >> >>",
            ),
        ),
        (6, form),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open /Resources null PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font must survive — the null-/Resources Form inherits and uses /F1");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "/Resources null Form inherits page scope → /F1 kept: {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "unused /F2 must still be pruned: {keys:?}"
    );
}

// The `/Resources null` inheritance rule also applies when the null is reached
// through an indirect reference (`/Resources 7 0 R` where 7 0 R resolves to
// null, or a holder chain ending in null): key presence is not enough, the
// reference must be resolved. Verified with qpdf 11.9.0
// (--remove-unreferenced-resources=yes keeps /F1). (flpdf-u79t / Codex review
// r3517182000 of PR #442.)
#[test]
fn test_form_resources_indirect_null_is_inherited() {
    let form_content = b"BT /F1 10 Tf (indirect-null-res form) Tj ET";
    let form = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Resources 7 0 R /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> /XObject << /Fm0 6 0 R >> >>",
            ),
        ),
        (6, form),
        // The Form's /Resources reference resolves to null → inherited.
        (7, obj_bytes(7, "null")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open indirect-null /Resources PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!(
            "/Font must survive — the Form's /Resources resolves to null (inherited) and uses /F1"
        );
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "indirect-null /Resources inherits page scope → /F1 kept: {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "unused /F2 must still be pruned: {keys:?}"
    );
}

// When Form nesting exceeds MAX_FORM_DEPTH the walk is cut off; the page must
// then be conservatively RETAINED (not pruned against the partial name set),
// because a Form beyond the cutoff may reference a page resource we never
// recorded. (flpdf-u79t / coderabbit review of PR #442.)
#[test]
fn test_form_depth_limit_retains_page() {
    // A chain of 70 resource-less Forms (Fm0 → Fm1 → … → Fm70), deeper than the
    // 64-level cap. Fm70 uses page font /F1, which the walk never reaches.
    const N: u32 = 70;
    let mut objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
    ];
    // Page /Resources: all Forms live in the page /XObject (resource-less Forms
    // inherit the page scope), plus a font /F1 used only by the deepest Form.
    let mut xobj = String::new();
    for i in 0..=N {
        xobj.push_str(&format!("/Fm{i} {} 0 R ", 100 + i));
    }
    objects.push((
        5,
        obj_bytes(
            5,
            &format!("<< /Font << /F1 << /Type /Font >> >> /XObject << {xobj} >> >>"),
        ),
    ));
    for i in 0..=N {
        let content = if i < N {
            format!("/Fm{} Do", i + 1)
        } else {
            "BT /F1 10 Tf (deep) Tj ET".to_string()
        };
        let mut v = format!(
            "{} 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            100 + i,
            content.len()
        )
        .into_bytes();
        v.extend_from_slice(content.as_bytes());
        v.extend_from_slice(b"\nendstream\nendobj\n");
        objects.push((100 + i, v));
    }
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open deep-nesting PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    // The walk stops at the depth cap before reaching Fm70's /F1, so the page is
    // retained rather than pruned against the partial set → /F1 survives.
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font must be retained when the Form nesting exceeds the depth cap");
    };
    assert!(
        font_dict.get("F1").is_some(),
        "depth cap: page retained, /F1 (used beyond the cutoff) kept: {font_dict:?}"
    );
}

// A `Do` operator with no name operand (malformed content) is a no-op — it does
// not record an XObject and does not stop the walk, so surrounding operators are
// still processed and pruning proceeds normally. (flpdf-u79t.)
#[test]
fn test_bare_do_operator_without_name_is_noop() {
    // Content: a bare `Do` (no operand) followed by a real font use.
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"Do BT /F1 10 Tf (x) Tj ET")),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open bare-Do PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let font_keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        font_keys.contains(&"F1".to_string()),
        "bare Do is a no-op → the following /F1 Tf is still recorded: {font_keys:?}"
    );
    assert!(
        !font_keys.contains(&"F2".to_string()),
        "unused /F2 must still be pruned: {font_keys:?}"
    );
}

// An indirect XObject reference that resolves to a *non-Form* stream (e.g. an
// image) is recorded as used but not recursed into. The page stays complete and
// prunes normally. (flpdf-u79t.)
#[test]
fn test_indirect_image_xobject_kept_not_recursed() {
    // Object 6 is an image XObject (/Subtype /Image), referenced via `/Im0 Do`.
    let img_body: &[u8] = b"\x00";
    let img = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
             /ColorSpace /DeviceGray /Length {} >>\nstream\n",
            img_body.len()
        )
        .into_bytes();
        b.extend_from_slice(img_body);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Im0 Do")),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> >> /XObject << /Im0 6 0 R >> >>",
            ),
        ),
        (6, img),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open indirect-image PDF");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R is not a dictionary");
    };
    // /Im0 is used by the page → kept; the image is not a Form, so nothing is
    // recursed and the unused /F1 font is pruned (page stayed complete).
    let Some(Object::Dictionary(xobj_dict)) = res_dict.get("XObject") else {
        panic!("/XObject must survive (/Im0 used by page)");
    };
    assert!(
        xobj_dict.get("Im0").is_some(),
        "/XObject/Im0 must remain (used by page): {xobj_dict:?}"
    );
    assert!(
        res_dict.get("Font").is_none(),
        "unused /F1 must be pruned — an image XObject keeps the page complete: {res_dict:?}"
    );
}

#[test]
fn test_roborev3_indirect_xobject_category_form_recurse_font_kept() {
    // The /XObject *resource category* is itself an indirect reference
    // (`/XObject 7 0 R`). A Form invoked via `/Fm0 Do` inherits the page
    // scope and uses /F1. recurse_form_xobject must resolve the indirect
    // category dict, otherwise /F1 is wrongly pruned.
    let form_content = b"BT /F1 10 Tf (form via indirect xobject cat) Tj ET";
    let form_stream = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // /XObject category is an INDIRECT reference (7 0 R), not a direct dict.
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> /XObject 7 0 R >>",
            ),
        ),
        (6, form_stream),
        (7, obj_bytes(7, "<< /Fm0 6 0 R >>")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R not a dict");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font missing");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "roborev3: /F1 must be kept (Form via indirect /XObject category uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "roborev3: /F2 must be pruned (unused): {keys:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// roborev medium 指摘1: is_builtin_color_space が広すぎる
// ═══════════════════════════════════════════════════════════════════════════════

// Test: /ColorSpace に "ICCBased" という名前のエントリ、content で `/ICCBased cs`
// → ICCBased が残り、未使用の別エントリは剪定される。
//
// Before the fix, `ICCBased` was treated as a built-in and NOT recorded in
// the used-set, causing `/ColorSpace/ICCBased` to be incorrectly pruned.
// After the fix only Device*/Pattern are treated as built-ins for cs/CS ops.
#[test]
fn test_medium1_iccbased_named_entry_kept_via_cs_op() {
    // Content uses /ICCBased as a /ColorSpace resource name (not a built-in),
    // and /Unused is a color space resource that is not referenced.
    let content = b"/ICCBased cs";
    // /ColorSpace dict: "ICCBased" -> array CS, "Unused" -> array CS (should be pruned).
    let res_body = "<< /ColorSpace << \
        /ICCBased [ /ICCBased << >> ] \
        /Unused [ /ICCBased << >> ] \
    >> >>";

    let extra = vec![(4u32, stream_obj(4, content)), (5, obj_bytes(5, res_body))];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("resources not a dict");
    };
    let cs_entry = res.get("ColorSpace");
    let Object::Dictionary(cs_dict) = cs_entry.expect("/ColorSpace must remain") else {
        panic!("/ColorSpace is not a dict");
    };
    let cs_keys: Vec<String> = cs_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();

    assert!(
        cs_keys.contains(&"ICCBased".to_string()),
        "medium1: /ColorSpace/ICCBased must be KEPT (referenced via `/ICCBased cs`): {cs_keys:?}"
    );
    assert!(
        !cs_keys.contains(&"Unused".to_string()),
        "medium1: /ColorSpace/Unused must be PRUNED (not referenced): {cs_keys:?}"
    );
}

// Test: DeviceRGB used via cs op must NOT cause a /ColorSpace lookup.
// Content: `/DeviceRGB cs` → DeviceRGB is a built-in for cs/CS, so no /ColorSpace
// entry should be "used". A /ColorSpace entry named "DeviceRGB" is pruned.
#[test]
fn test_medium1_device_rgb_cs_op_is_builtin_not_a_resource_lookup() {
    // /ColorSpace dict has an entry named "DeviceRGB" — but the cs op /DeviceRGB
    // must NOT prevent it from being pruned (it is a built-in).
    let content = b"/DeviceRGB cs";
    let res_body = "<< /ColorSpace << /DeviceRGB [ /CalRGB << >> ] >> >>";

    let extra = vec![(4u32, stream_obj(4, content)), (5, obj_bytes(5, res_body))];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("resources not a dict");
    };
    // The /ColorSpace sub-dict should be entirely gone (the only entry was pruned).
    assert!(
        res.get("ColorSpace").is_none(),
        "medium1: /ColorSpace/DeviceRGB must be PRUNED (DeviceRGB is a built-in for cs op): {:?}",
        res.get("ColorSpace")
    );
}

// Test: inline image BI /CS /RGB EI does NOT cause /ColorSpace/RGB to survive.
// RGB is an inline-image built-in abbreviation; it must NOT be treated as a
// /ColorSpace resource reference. If a /ColorSpace entry named "RGB" exists and
// is not referenced by any cs/CS op, it must be pruned.
#[test]
fn test_medium1_inline_image_rgb_abbrev_does_not_prevent_pruning() {
    // Inline image with /CS /RGB — should not add "RGB" to the ColorSpace used-set.
    // /ColorSpace has an entry "RGB" that is never referenced by a cs/CS op.
    // After pruning, /ColorSpace/RGB must be gone.
    //
    // The inline image body: BI /CS /RGB /W 1 /H 1 /BPC 8 ID \x00 EI
    let content = b"BI /CS /RGB /W 1 /H 1 /BPC 8 ID \x00 EI";
    let res_body = "<< /ColorSpace << /RGB [ /CalRGB << >> ] >> >>";

    let extra = vec![(4u32, stream_obj(4, content)), (5, obj_bytes(5, res_body))];
    let page_body = "/Contents 4 0 R /Resources 5 0 R";
    let pdf_bytes = build_pdf(&[page_body], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("resources not a dict");
    };
    // /ColorSpace/RGB must be pruned: the inline-image abbreviation is built-in.
    assert!(
        res.get("ColorSpace").is_none(),
        "medium1: inline BI /CS /RGB must NOT prevent pruning of /ColorSpace/RGB: {:?}",
        res.get("ColorSpace")
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// roborev medium 指摘2: 非UTF-8リソース名で lookup 失敗
// ═══════════════════════════════════════════════════════════════════════════════

// Test: Form XObject with a non-UTF-8 name (raw bytes \xff\xfeFm encoded as
// `/#ff#feFm` in PDF syntax). The Form inherits the page scope and uses /F1.
// After pruning, /F1 must remain (the Form used it).
//
// Before the fix, from_utf8(b"\xff\xfeFm").unwrap_or("") returned "" which
// failed to look up the XObject → Form was not recursed → /F1 appeared unused.
#[test]
fn test_medium2_non_utf8_xobject_name_form_font_kept() {
    // The XObject name in the resources dict and in the Do operand is the raw
    // byte sequence [0xff, 0xfe, b'F', b'm'].  In PDF syntax this is written
    // as `/#ff#feFm` (the parser decodes #xx hex escapes to raw bytes).

    let form_content = b"BT /F1 10 Tf (non-utf8 name form) Tj ET";
    let form_stream = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    // Page content: invoke the Form via its non-UTF-8 name.
    let page_content = b"/#ff#feFm Do";

    // The /XObject sub-dict key must use the same #xx encoding so the parser
    // stores the same raw bytes as the key.
    let res_body = "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> \
         /XObject << /#ff#feFm 6 0 R >> >>";

    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, page_content)),
        (5, obj_bytes(5, res_body)),
        (6, form_stream),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open PDF with non-UTF-8 XObject name");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let res_obj = pdf
        .resolve(ObjectRef::new(5, 0))
        .expect("resolve resources");
    let Object::Dictionary(res) = res_obj else {
        panic!("resources not a dict");
    };
    let font_entry = res.get("Font").expect("/Font key must exist");
    let Object::Dictionary(font_dict) = font_entry else {
        panic!("/Font is not a dict");
    };
    let font_keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .collect();

    assert!(
        font_keys.contains(&"F1".to_string()),
        "medium2: /F1 must be KEPT (non-UTF-8 Form uses it via inherited scope): {font_keys:?}"
    );
    assert!(
        !font_keys.contains(&"F2".to_string()),
        "medium2: /F2 must be PRUNED (unused): {font_keys:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Shared resource-less Form reached via two paths keeps its page-level font
// (flpdf-u79t: ever-seen traversal + bubble-to-page attribution)
// ═══════════════════════════════════════════════════════════════════════════════
//
// Scenario:
//   - Form Fa (7 0 R) has its own /Resources with /XObject << /Fx 8 0 R >>.
//     Fa's content invokes /Fx via Do.
//   - Form Fx (8 0 R) has NO /Resources — it inherits the calling scope.
//     Fx's content uses BT /F1 10 Tf.
//   - Page /Resources has /XObject << /Fa 7 0 R /Fx 8 0 R >> and /Font << /F1 >>.
//   - Page content: /Fa Do /Fx Do  (Fa is processed first, reaching Fx inside
//     Fa's own-resources scope; then the page reaches Fx directly).
//
// The traversal uses an *ever-seen* `visited` set (each indirect Form expanded
// once, matching qpdf), so Fx is walked only on its FIRST reach (via Fa). The
// key invariant that keeps /F1 is that a resource-less Form attributes its used
// names to the PAGE's real `used` set regardless of which scope reached it
// (mirroring qpdf's path-independent "unresolved" names). So on that single
// visit, Fx's /F1 lands in the page's `used` and /F1 is kept; the later direct
// `/Fx Do` is correctly a no-op (already seen) rather than a re-traversal.
//
// A naive ever-seen set WITHOUT that bubble-to-page rule would attribute Fx's
// /F1 to Fa's throwaway and then skip the page's direct reach → /F1 wrongly
// pruned. A stack-pop set would instead RE-traverse Fx, which reintroduces the
// exponential DoS this fix removes (see test_indirect_dag_forms_no_exponential_blowup).

#[test]
fn test_shared_resource_less_form_font_kept_via_bubble_to_page() {
    // Form Fx (8 0 R): no own /Resources, content uses /F1.
    let fx_content = b"BT /F1 10 Tf (from Fx) Tj ET";
    let fx_stream = {
        let mut b = format!(
            "8 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            fx_content.len()
        )
        .into_bytes();
        b.extend_from_slice(fx_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    // Form Fa (7 0 R): has own /Resources with /XObject << /Fx 8 0 R >>.
    // Fa's content invokes /Fx via Do.
    let fa_content = b"/Fx Do";
    let fa_stream = {
        let res = "/Resources << /XObject << /Fx 8 0 R >> >>";
        let mut b = format!(
            "7 0 obj\n<< /Subtype /Form /Length {} {} >>\nstream\n",
            fa_content.len(),
            res
        )
        .into_bytes();
        b.extend_from_slice(fa_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    // Page content: invoke Fa first, then Fx directly.
    let page_content = b"/Fa Do /Fx Do";

    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, page_content)),
        // Page /Resources: /Font { F1 }, /XObject { Fa, Fx }
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> >> \
                   /XObject << /Fa 7 0 R /Fx 8 0 R >> >>",
            ),
        ),
        (7, fa_stream),
        (8, fx_stream),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let font_keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        font_keys.contains(&"F1".to_string()),
        "/F1 must be KEPT — the resource-less Form Fx bubbles its used /F1 to the \
         page's `used` set on its single (ever-seen) visit, independent of the \
         scope that reached it: {font_keys:?}"
    );
}

// Regression: true cycle (Fx → Fx via own /Resources) must not infinite-loop.
#[test]
fn test_roborev_medium_true_cycle_no_infinite_loop() {
    // Fx (6 0 R) has own /Resources << /XObject << /Fx 6 0 R >> >>.
    // Fx's content: /Fx Do  → self-reference → must be caught by cycle guard.
    let fx_content = b"/Fx Do";
    let fx_stream = {
        let res = "/Resources << /XObject << /Fx 6 0 R >> >>";
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Length {} {} >>\nstream\n",
            fx_content.len(),
            res
        )
        .into_bytes();
        b.extend_from_slice(fx_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };

    let page_content = b"/Fx Do";
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, page_content)),
        (
            5,
            obj_bytes(5, "<< /XObject << /Fx 6 0 R >> >>"),
        ),
        (6, fx_stream),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    // Must complete without hanging or panicking.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must not loop or panic on true cycle");
}

// ═══════════════════════════════════════════════════════════════════════════════
// roborev low 指摘2 (resources.rs:242): synthetic group key 衝突
// ═══════════════════════════════════════════════════════════════════════════════
//
// Bug scenario:
//   - Page 1 (object 3 0 R) has a page-INLINE /Resources containing /Font 6 0 R.
//     Page 1 content uses /F1.
//   - Page 2 (object 4 0 R) has /Resources pointing to an indirect object at
//     ObjectRef(3, 1) (same object number as page 1, different generation).
//     Page 2 content uses /F2.
//   - Font sub-dict 6 0 R: /F1 /F2 /F3 (F3 unused by either page).
//
// With the old bug (synthetic key = ObjectRef::new(page_refs[i].number, 1)):
//   Page 1's PageInline group gets synthetic key ObjectRef(3, 1).
//   Page 2's Indirect group key is ObjectRef(3, 1) (the real ref).
//   → Both groups map to the SAME entry in cat_ref_seen_groups.
//   → 6 0 R's group_count stays at 1 (two groups counted as one).
//   → Yes mode misses F2 from page 2's union → F2 incorrectly pruned.
//
// With the ResGroupKey enum fix:
//   Page 1 → ResGroupKey::PageInline(ObjectRef(3, 0))
//   Page 2 → ResGroupKey::Indirect(ObjectRef(3, 1))
//   → Distinct variants → group_count == 2 → union {F1, F2} → F3 pruned, F1+F2 kept.
//
// Standard PDF xref tables cannot hold two entries for the same object number
// simultaneously, so we reproduce the collision by injecting the gen-1 object
// directly into the Pdf cache via `set_object` after opening a base PDF, and
// by patching page 2's /Resources reference to point at it.

#[test]
fn test_roborev_low_page_inline_group_key_no_collision_with_gen1_indirect() {
    // Base PDF (two pages, page-inline /Resources on page 1):
    //   1 0 R  Catalog
    //   2 0 R  Pages /Kids [3 0 R 4 0 R]
    //   3 0 R  Page 1 — inline /Resources << /Font 6 0 R >>  /Contents 7 0 R
    //   4 0 R  Page 2 — (stub; /Resources will be injected below)   /Contents 8 0 R
    //   6 0 R  Shared Font sub-dict: /F1 /F2 /F3
    //   7 0 R  Content stream page 1: uses /F1
    //   8 0 R  Content stream page 2: uses /F2

    let content1 = b"BT /F1 12 Tf (p1) Tj ET";
    let content2 = b"BT /F2 12 Tf (p2) Tj ET";

    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
        ),
        // Page 1: inline /Resources with /Font 6 0 R.
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                   /Contents 7 0 R /Resources << /Font 6 0 R >> >>",
            ),
        ),
        // Page 2: initially no /Resources (we inject 3 1 R below).
        (
            4,
            obj_bytes(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R >>",
            ),
        ),
        // Font sub-dict: F1, F2, F3.
        (
            6,
            obj_bytes(
                6,
                "<< /F1 << /Type /Font >> /F2 << /Type /Font >> /F3 << /Type /Font >> >>",
            ),
        ),
        (7, stream_obj(7, content1)),
        (8, stream_obj(8, content2)),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");

    // Inject the generation-1 /Resources object for page 2 into the cache.
    // ObjectRef(3, 1) has the same object number as page 1 (3 0 R) but a
    // different generation, reproducing the old key collision.
    let res_31_ref = ObjectRef::new(3, 1);
    {
        let mut res31 = Dictionary::new();
        res31.insert("Font", Object::Reference(ObjectRef::new(6, 0)));
        pdf.set_object(res_31_ref, Object::Dictionary(res31));
    }

    // Patch page 2 (4 0 R) to point its /Resources at the injected (3, 1) object.
    {
        let page2_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve page 2");
        let Object::Dictionary(mut page2) = page2_obj else {
            panic!("page 2 not a dict");
        };
        page2.insert("Resources", Object::Reference(res_31_ref));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(page2));
    }

    // Yes mode: union = {F1, F2}, F3 pruned.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let font_obj = pdf
        .resolve(ObjectRef::new(6, 0))
        .expect("resolve font dict");
    let Object::Dictionary(font_dict) = font_obj else {
        panic!("6 0 R not a dict");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "low2: F1 must remain (page1 inline uses it): {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "low2: F2 must remain (page2 via injected gen-1 indirect uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F3".to_string()),
        "low2: F3 must be pruned (neither page uses it): {keys:?}"
    );
}

// ── Graceful degradation: undecodable page /Contents (flpdf-s9s) ─────────────
//
// A page whose /Contents cannot be decoded (corrupt FlateDecode, etc.) must not
// abort `remove_unreferenced_resources`; its resources are conservatively
// retained — never pruned — matching the Form XObject decode-failure path.

/// Write a stream object carrying a `/Filter`, embedding `body` verbatim so the
/// declared filter does NOT match the bytes (used to fabricate an undecodable
/// content stream).
fn stream_obj_with_filter(num: u32, filter: &str, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "{num} 0 obj\n<< /Length {} /Filter {filter} >>\nstream\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out
}

/// Resolve an indirect /Font sub-dictionary object and return its key names.
fn font_subdict_keys_by_ref(pdf: &mut Pdf<Cursor<Vec<u8>>>, font_ref: ObjectRef) -> Vec<String> {
    match pdf
        .resolve(font_ref)
        .expect("resolve indirect font sub-dict")
    {
        Object::Dictionary(d) => d
            .iter()
            .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
            .collect(),
        _ => vec![],
    }
}

// Exercises the `protected_groups` prune-loop skip: a corrupt page sharing a
// top-level /Resources with a healthy sibling must not have that shared dict
// pruned to the healthy page's (incomplete) used-name union. Yes mode, because
// Auto skips shared-resource collection entirely (so the failure never fires).
#[test]
fn corrupt_page_content_shared_resources_not_pruned_yes_mode() {
    // Page A (obj 3): valid content using only /F1, shared /Resources 7 0 R.
    // Page B (obj 4): corrupt /Contents (FlateDecode garbage), shared 7 0 R.
    // Shared resources 7 0 R holds /F1 and /F2.
    let extra = vec![
        (5u32, stream_obj(5, b"BT /F1 12 Tf (hi) Tj ET")),
        (
            6,
            stream_obj_with_filter(6, "/FlateDecode", b"not-zlib-garbage!!"),
        ),
        (
            7,
            obj_bytes(
                7,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
    ];
    let pdf_bytes = build_pdf(
        &[
            "/Contents 5 0 R /Resources 7 0 R",
            "/Contents 6 0 R /Resources 7 0 R",
        ],
        &extra,
    );

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    // Must NOT abort despite page B's undecodable /Contents.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must degrade gracefully, not abort");

    let keys = font_dict_keys(&mut pdf, ObjectRef::new(7, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 must be kept: {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "F2 must be conservatively retained — page B's content is undecodable so \
         we cannot prove F2 is unused: {keys:?}"
    );
}

// Exercises the `CatRefInfo.protected` flag: a corrupt page and a healthy page
// have DIFFERENT top-level /Resources but share an indirect category sub-dict
// (/Font 9 0 R). The healthy page's pruning must not trim the shared sub-dict to
// its own union, because the corrupt page's usage is unknown. Yes mode.
#[test]
fn corrupt_page_content_shared_cat_subdict_not_pruned_yes_mode() {
    // Page A (obj 3): valid content using /F1, /Resources 7 0 R → /Font 9 0 R.
    // Page B (obj 4): corrupt /Contents,          /Resources 8 0 R → /Font 9 0 R.
    // Shared indirect /Font sub-dict 9 0 R holds /F1 and /F2.
    let extra = vec![
        (5u32, stream_obj(5, b"BT /F1 12 Tf (hi) Tj ET")),
        (
            6,
            stream_obj_with_filter(6, "/FlateDecode", b"not-zlib-garbage!!"),
        ),
        (7, obj_bytes(7, "<< /Font 9 0 R >>")),
        (8, obj_bytes(8, "<< /Font 9 0 R >>")),
        (
            9,
            obj_bytes(9, "<< /F1 << /Type /Font >> /F2 << /Type /Font >> >>"),
        ),
    ];
    let pdf_bytes = build_pdf(
        &[
            "/Contents 5 0 R /Resources 7 0 R",
            "/Contents 6 0 R /Resources 8 0 R",
        ],
        &extra,
    );

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must degrade gracefully, not abort");

    let keys = font_subdict_keys_by_ref(&mut pdf, ObjectRef::new(9, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 must be kept: {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "F2 must be retained — the shared /Font is also referenced by the corrupt \
         page B, whose usage is unknown: {keys:?}"
    );
}

// The acceptance criterion's headline scenario, verbatim: a single page in the
// DEFAULT (Auto) mode whose /Contents cannot be decoded. A single page is never
// "shared", so it bypasses Auto's pre-collection skip and reaches the failing
// decode — which must degrade gracefully (no abort) and retain the page's
// resources rather than pruning them to an empty used-name set.
#[test]
fn corrupt_page_content_single_page_default_mode_does_not_abort() {
    let extra = vec![
        (
            4u32,
            stream_obj_with_filter(4, "/FlateDecode", b"not-zlib-garbage!!"),
        ),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
    ];
    let pdf_bytes = build_pdf(&["/Contents 4 0 R /Resources 5 0 R"], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    // Default mode == Auto, exactly the `rewrite --full-rewrite` default path.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::default())
        .expect("default-mode prune must degrade gracefully, not abort");

    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()) && keys.contains(&"F2".to_string()),
        "both fonts must be conservatively retained — the page content is \
         undecodable so no entry can be proven unused: {keys:?}"
    );
}

// A page whose /Contents DECODES fine (no filter) but is malformed PART-WAY:
// `BT /F1 Tf ... ` collects /F1, then trailing dangling operands make the
// content-stream tokeniser error mid-stream. The pre-fix code `break`-ed and
// treated the partial collection as complete, so /F2 (whose usage cannot be
// proven either way) was pruned. The collection must instead report itself
// incomplete and the page's resources be conservatively retained (flpdf-s9s).
#[test]
fn malformed_page_content_midstream_retains_resources() {
    // `1 2 3` after `Tf` are dangling operands with no operator → at EOF the
    // ContentStreamParser yields Err("content stream ended with dangling
    // operands"). /F1 is recorded before the error; /F2 must NOT be pruned.
    let extra = vec![
        (4u32, stream_obj(4, b"BT /F1 12 Tf (x) Tj ET 1 2 3")),
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>",
            ),
        ),
    ];
    let pdf_bytes = build_pdf(&["/Contents 4 0 R /Resources 5 0 R"], &extra);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    // Yes mode would prune the partial set most aggressively — use it to make the
    // discriminating assertion (F2 retained) meaningful.
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("malformed-midstream content must not abort");

    let keys = font_dict_keys(&mut pdf, ObjectRef::new(5, 0));
    assert!(
        keys.contains(&"F1".to_string()),
        "F1 must be kept: {keys:?}"
    );
    assert!(
        keys.contains(&"F2".to_string()),
        "F2 must be retained — tokenisation stopped mid-stream, so the used-name \
         set is incomplete and nothing can be proven unused: {keys:?}"
    );
}

// Exercises the AncestorInline branch of the `protected_groups` prune-loop skip
// (step 5): a corrupt page inheriting its /Resources from an ancestor /Pages
// node (AncestorInline loc) poisons that group, so the ancestor's inline
// resources must NOT be pruned to the healthy sibling's incomplete used-name
// union. Yes mode, because Auto would skip the shared ancestor group entirely.
#[test]
fn corrupt_page_content_ancestor_inline_resources_not_pruned_yes_mode() {
    // 1=Catalog, 2=Pages(inline /Resources F1+F2), 3=Page A (valid, uses F1),
    // 4=Page B (corrupt /Contents), 5=content A, 6=corrupt content B.
    // Both pages have NO own /Resources → both inherit AncestorInline(2 0 R).
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (
            2,
            obj_bytes(
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Resources << /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >> >>",
            ),
        ),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>",
            ),
        ),
        (
            4,
            obj_bytes(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>",
            ),
        ),
        (5, stream_obj(5, b"BT /F1 12 Tf (a) Tj ET")),
        (
            6,
            stream_obj_with_filter(6, "/FlateDecode", b"not-zlib-garbage!!"),
        ),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must degrade gracefully, not abort");

    // The /Pages (2 0 R) inline /Resources must retain BOTH fonts: the corrupt
    // sibling protects the shared ancestor group from being pruned to {F1}.
    let pages_obj = pdf.resolve(ObjectRef::new(2, 0)).expect("resolve Pages");
    let Object::Dictionary(pages_dict) = pages_obj else {
        panic!("2 0 R is not a dictionary");
    };
    let Object::Dictionary(res) = pages_dict.get("Resources").expect("Resources key") else {
        panic!("Resources is not a dict");
    };
    let Object::Dictionary(fonts) = res.get("Font").expect("Font key") else {
        panic!("Font is not a dict");
    };
    let keys: Vec<String> = fonts
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()) && keys.contains(&"F2".to_string()),
        "ancestor inline /Font must retain both F1 and F2 — the corrupt sibling \
         page protects the shared AncestorInline group from pruning: {keys:?}"
    );
}

// Exercises the `ResourcesLoc::None` arm of `res_group_key` (resources.rs:81):
// a corrupt page that has NO /Resources anywhere in its chain. Collection fails,
// `res_group_key` returns `None` (nothing to protect — there is no group to key
// on), and the operation must still degrade gracefully rather than abort.
#[test]
fn corrupt_page_content_no_resources_anywhere_does_not_abort() {
    // 1=Catalog, 2=Pages (no /Resources), 3=Page (no /Resources), 4=corrupt content.
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
        ),
        (
            4,
            stream_obj_with_filter(4, "/FlateDecode", b"not-zlib-garbage!!"),
        ),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("prune must degrade gracefully on a resource-less corrupt page, not abort");

    // The page never had /Resources; it must remain absent (nothing fabricated).
    let page_obj = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve page");
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("3 0 R is not a dictionary");
    };
    assert!(
        page_dict.get("Resources").is_none(),
        "page without /Resources must stay resource-less after graceful degradation"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// flpdf-3x23: holder-chain resolution in recurse_form_xobject
// ═══════════════════════════════════════════════════════════════════════════════
//
// A PDF value reached through more than one indirect hop (`ref → ref → value`,
// a "holder chain") must be followed to its terminal. Single-hop resolution
// (`resolve_borrowed`) stops at the first reference and type-matches against a
// still-indirect `Object::Reference`, silently dropping the target. These two
// tests construct 2-hop chains and assert the pruning EFFECT distinguishes
// "chain followed" (used resource RETAINED) from "chain dropped" (over-pruned).

// Site 1 — /XObject resource *category* reached through a 2-hop chain.
//
//   5 0 R  /Resources  << ... /XObject 7 0 R >>
//   7 0 R  body = "8 0 R"          ← intermediate holder (bare reference)
//   8 0 R  << /Fm0 6 0 R >>        ← terminal /XObject category dict
//   6 0 R  Form XObject (no own /Resources) using /F1 via Tf
//   page content: /Fm0 Do
//
// Pre-fix: resolve_borrowed(7 0 R) yields Object::Reference(8 0 R), which is not
// an Object::Dictionary → the category arm falls to `_ => None` → the Form is
// never recursed → /F1 appears unused → wrongly pruned. Post-fix: the chain is
// followed to 8 0 R, the Form recurses, /F1 is recorded and kept.
#[test]
fn test_3x23_xobject_category_holder_chain_form_font_kept() {
    let form_content = b"BT /F1 10 Tf (form via 2-hop xobject cat) Tj ET";
    let form_stream = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // /XObject category is a 2-hop holder chain: 7 0 R → 8 0 R → dict.
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> /XObject 7 0 R >>",
            ),
        ),
        (6, form_stream),
        (7, obj_bytes(7, "8 0 R")),
        (8, obj_bytes(8, "<< /Fm0 6 0 R >>")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R not a dict");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font missing");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "3x23 site1: /F1 must be kept (Form via 2-hop /XObject category uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "3x23 site1: /F2 must be pruned (unused): {keys:?}"
    );
}

// Site 2 — XObject *stream* reached through a 2-hop chain.
//
//   5 0 R  /Resources  << ... /XObject << /Fm0 7 0 R >> >>
//   7 0 R  body = "6 0 R"          ← intermediate holder (bare reference)
//   6 0 R  Form XObject (no own /Resources) using /F1 via Tf  ← terminal stream
//   page content: /Fm0 Do
//
// Pre-fix: resolve_borrowed(7 0 R) yields Object::Reference(6 0 R), which is not
// an Object::Stream → the stream arm falls to `_ => return Ok(true)` → the Form
// is never recursed → /F1 appears unused → wrongly pruned. Post-fix: the chain
// is followed to 6 0 R, the Form recurses, /F1 is recorded and kept.
#[test]
fn test_3x23_xobject_stream_holder_chain_form_font_kept() {
    let form_content = b"BT /F1 10 Tf (form via 2-hop xobject stream) Tj ET";
    let form_stream = {
        let mut b = format!(
            "6 0 obj\n<< /Subtype /Form /Length {} >>\nstream\n",
            form_content.len()
        )
        .into_bytes();
        b.extend_from_slice(form_content);
        b.extend_from_slice(b"\nendstream\nendobj\n");
        b
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // /Fm0 points at a 2-hop holder chain to the Form stream: 7 0 R → 6 0 R.
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> /XObject << /Fm0 7 0 R >> >>",
            ),
        ),
        (6, form_stream),
        (7, obj_bytes(7, "6 0 R")),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R not a dict");
    };
    let Some(Object::Dictionary(font_dict)) = res_dict.get("Font") else {
        panic!("/Font missing");
    };
    let keys: Vec<String> = font_dict
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert!(
        keys.contains(&"F1".to_string()),
        "3x23 site2: /F1 must be kept (Form stream via 2-hop chain uses it): {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "3x23 site2: /F2 must be pruned (unused): {keys:?}"
    );
}

// Site 2 (degenerate value) — an /XObject entry whose value is neither a
// reference nor a stream (here a bare Name `/Junk`). `recurse_form_xobject`
// must treat this as "nothing to recurse into" (the stream-resolution match
// yields None) and report the page complete rather than panicking. The Do-named
// entry is recorded as used, so the /XObject category survives; the unused /F2
// font is still pruned.
#[test]
fn test_3x23_xobject_entry_non_stream_value_handled() {
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, obj_bytes(1, "<< /Type /Catalog /Pages 2 0 R >>")),
        (2, obj_bytes(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            obj_bytes(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
            ),
        ),
        (4, stream_obj(4, b"/Fm0 Do")),
        // /Fm0 maps to a bare Name (not a reference, not a stream).
        (
            5,
            obj_bytes(
                5,
                "<< /Font << /F2 << /Type /Font >> >> /XObject << /Fm0 /Junk >> >>",
            ),
        ),
    ];
    let pdf_bytes = build_pdf_raw(&objects);

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");

    let Object::Dictionary(res_dict) = pdf.resolve(ObjectRef::new(5, 0)).expect("resolve res")
    else {
        panic!("5 0 R not a dict");
    };
    // /XObject/Fm0 is recorded as used by the Do operator → category retained.
    let Some(Object::Dictionary(xobj_dict)) = res_dict.get("XObject") else {
        panic!("/XObject category must survive (Fm0 is used via Do)");
    };
    assert!(
        xobj_dict.get("Fm0").is_some(),
        "3x23 site2-degenerate: /XObject/Fm0 must remain: {xobj_dict:?}"
    );
    // /F2 is never referenced → pruned (the whole /Font dict goes away).
    assert!(
        res_dict.get("Font").is_none(),
        "3x23 site2-degenerate: unused /Font must be pruned: {res_dict:?}"
    );
}
