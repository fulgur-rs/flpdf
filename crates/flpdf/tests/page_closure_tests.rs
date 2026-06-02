//! Integration tests for [`flpdf::page_closure::page_object_closure`].

use flpdf::{page_closure, pages, ObjectRef, Pdf};

// ---------------------------------------------------------------------------
// Minimal PDF builder helpers (copied pattern from page_object_helper_tests)
// ---------------------------------------------------------------------------

/// Build a minimal single-page PDF with no resources.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page
fn build_minimal_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn closure_contains_page_ref_itself() {
    let data = build_minimal_pdf();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let page_ref = page_refs[0];

    let closure = page_closure::page_object_closure(&mut pdf, page_ref).unwrap();

    assert!(
        closure.contains(&page_ref),
        "closure must contain the page ref itself"
    );
}

// ---------------------------------------------------------------------------
// Task 4: Font resource
// ---------------------------------------------------------------------------

/// Build a single-page PDF where the page references a Font resource (object 4).
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page  (has /Resources with font ref)
///   4 0 R  Font dictionary
fn build_pdf_with_font() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn closure_includes_font_resource() {
    let data = build_pdf_with_font();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let page_ref = page_refs[0];
    let font_ref = ObjectRef::new(4, 0);

    let closure = page_closure::page_object_closure(&mut pdf, page_ref).unwrap();

    assert!(
        closure.contains(&font_ref),
        "closure must include font object 4 0 R"
    );
}

// ---------------------------------------------------------------------------
// Task 5: Shared object
// ---------------------------------------------------------------------------

/// Build a two-page PDF where both pages share the same font (object 5).
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page 1 (references font 5 0 R)
///   4 0 R  Page 2 (references font 5 0 R)
///   5 0 R  Font (shared)
fn build_two_page_pdf_shared_font() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );

    let off5 = out.len() as u64;
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn shared_object_appears_in_both_page_closures() {
    let data = build_two_page_pdf_shared_font();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let font_ref = ObjectRef::new(5, 0);

    let closure_p1 = page_closure::page_object_closure(&mut pdf, page_refs[0]).unwrap();
    let closure_p2 = page_closure::page_object_closure(&mut pdf, page_refs[1]).unwrap();

    assert!(
        closure_p1.contains(&font_ref),
        "page 1 closure must contain shared font"
    );
    assert!(
        closure_p2.contains(&font_ref),
        "page 2 closure must contain shared font"
    );
    // Page 1 must not contain page 2's ref and vice-versa.
    assert!(
        !closure_p1.contains(&page_refs[1]),
        "page 1 closure must not contain page 2 ref"
    );
    assert!(
        !closure_p2.contains(&page_refs[0]),
        "page 2 closure must not contain page 1 ref"
    );
}

// ---------------------------------------------------------------------------
// Task 6: Cycle
// ---------------------------------------------------------------------------

/// Build a PDF with a synthetic reference cycle: object 4 references object 5,
/// object 5 references object 4.  The page (object 3) references object 4.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page (has /Resources /XObject << /Im0 4 0 R >>)
///   4 0 R  dictionary containing 5 0 R
///   5 0 R  dictionary containing 4 0 R  ← cycle
fn build_pdf_with_cycle() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /XObject << /Im0 4 0 R >> >> >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(b"4 0 obj\n<< /Next 5 0 R >>\nendobj\n");

    let off5 = out.len() as u64;
    out.extend_from_slice(b"5 0 obj\n<< /Next 4 0 R >>\nendobj\n");

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn cycle_does_not_loop_forever() {
    let data = build_pdf_with_cycle();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let page_ref = page_refs[0];

    // This must terminate; if it loops forever, the test hangs.
    let closure = page_closure::page_object_closure(&mut pdf, page_ref).unwrap();

    assert!(closure.contains(&ObjectRef::new(4, 0)));
    assert!(closure.contains(&ObjectRef::new(5, 0)));
}

// ---------------------------------------------------------------------------
// Inherited resources
// ---------------------------------------------------------------------------

/// Build a two-page PDF where /Resources is on the parent /Pages node (inherited),
/// not on the individual page dicts.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages  (has /Resources with font 5 0 R — inherited by both pages)
///   3 0 R  Page 1 (no /Resources of its own)
///   4 0 R  Page 2 (no /Resources of its own)
///   5 0 R  Font (inherited)
fn build_pdf_with_inherited_resources() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let off5 = out.len() as u64;
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn closure_includes_inherited_resources() {
    let data = build_pdf_with_inherited_resources();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let font_ref = ObjectRef::new(5, 0);
    let pages_ref = ObjectRef::new(2, 0);

    let closure_p1 = page_closure::page_object_closure(&mut pdf, page_refs[0]).unwrap();
    let closure_p2 = page_closure::page_object_closure(&mut pdf, page_refs[1]).unwrap();

    // The /Pages node (which carries /Resources) must be in both closures.
    assert!(
        closure_p1.contains(&pages_ref),
        "page 1 closure must contain the /Pages node (inherited resources live there)"
    );
    assert!(
        closure_p2.contains(&pages_ref),
        "page 2 closure must contain the /Pages node"
    );
    // The font itself (transitively reachable via /Pages → /Resources) must be included.
    assert!(
        closure_p1.contains(&font_ref),
        "page 1 closure must contain inherited font 5 0 R"
    );
    assert!(
        closure_p2.contains(&font_ref),
        "page 2 closure must contain inherited font 5 0 R"
    );
    // Sibling pages must not appear in each other's closures.
    assert!(
        !closure_p1.contains(&page_refs[1]),
        "page 1 closure must not contain page 2 ref"
    );
    assert!(
        !closure_p2.contains(&page_refs[0]),
        "page 2 closure must not contain page 1 ref"
    );
}
