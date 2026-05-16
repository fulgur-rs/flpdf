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
    let xobj_entry = res.get("XObject");
    assert!(
        xobj_entry.is_some(),
        "test_h: /XObject sub-dict must remain: {res:?}"
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
// roborev 指摘2: 直接 Stream の Form XObject で再帰スキップ
// ═══════════════════════════════════════════════════════════════════════════════
//
// Setup: the /XObject sub-dict in the page /Resources contains a *direct*
// Object::Stream Form XObject (not an indirect reference).  The Form has no
// /Resources of its own, so it inherits the page's resources.  The Form
// content uses /F1 via Tf.  Page content just invokes the Form via Do.
//
// Without the fix, get_ref() returns None for a direct Stream value, so the
// Form is never recursed into and /F1 appears unused → incorrectly pruned.
// After the fix, /F1 must be kept because the Form uses it.
//
// Building a raw PDF with a direct Stream inside a dict value requires writing
// the stream inline.  We construct the bytes manually.

#[test]
fn test_roborev2_direct_stream_form_xobject_font_kept() {
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

    // /F1 must remain — the direct-stream Form uses it via inherited scope.
    // /F2 must be pruned — neither the page nor the Form references it.
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
        "roborev2: /F1 must be kept (direct-stream Form uses it via inherited scope): {keys:?}"
    );
    assert!(
        !keys.contains(&"F2".to_string()),
        "roborev2: /F2 must be pruned (unused by page and Form): {keys:?}"
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
