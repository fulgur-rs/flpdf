//! Capstone integration tests for the flpdf document-helper public API.
//!
//! Layer 1 (smoke): each helper read API is cross-checked against an
//! independent manual raw-`Object` extraction. Layer 2 (round-trip): each
//! mutating helper produces byte-identical output to the equivalent direct
//! `Object` manipulation, serialised with `full_rewrite + static_id`.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Minimal PDF builder (copied verbatim from page_document_helper_tests.rs)
// ---------------------------------------------------------------------------

/// Build a flat N-page PDF.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages  (/Kids [3 0 R … (2+N) 0 R], /Count N)
///   3 0 R  Page 1
///   …
///   (2+N) 0 R  Page N
fn build_n_page_pdf(n: u32) -> Vec<u8> {
    assert!(n >= 1, "must have at least 1 page");

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

    // Catalog (1 0 R)
    offs.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Build /Kids string: [3 0 R 4 0 R …]
    let kids: String = (3..=2 + n)
        .map(|i| format!("{i} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    // Pages (2 0 R)
    offs.insert(2, out.len() as u64);
    let pages_str = format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n");
    out.extend_from_slice(pages_str.as_bytes());

    // Individual pages (3 0 R … (2+n) 0 R)
    for i in 0..n {
        let obj_num = 3 + i;
        offs.insert(obj_num, out.len() as u64);
        let page_str = format!(
            "{obj_num} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
        );
        out.extend_from_slice(page_str.as_bytes());
    }

    let max_num = 2 + n;
    let total = max_num + 1; // 0 .. max_num inclusive
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for i in 1..=max_num {
        out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
    }
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Serialise a PDF with the canonical full-rewrite + static-id settings so two
/// independently-constructed (but isomorphic) graphs are byte-comparable.
fn write_canonical<R: std::io::Read + std::io::Seek>(pdf: &mut flpdf::Pdf<R>) -> Vec<u8> {
    // `WriteOptions` is `#[non_exhaustive]`, so it cannot be built with a
    // struct literal from outside the crate; mutate a default instead.
    let mut opts = flpdf::WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    let mut buf = Vec::new();
    flpdf::write_pdf_with_options(pdf, &mut buf, &opts).expect("write_canonical");
    buf
}

// ---------------------------------------------------------------------------
// Layer 1 smoke: page helper vs manual raw extraction
// ---------------------------------------------------------------------------

#[test]
fn page_helper_pages_matches_manual_kids() {
    let bytes = build_n_page_pdf(3);
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    let helper_pages = {
        let mut helper = flpdf::PageDocumentHelper::new(&mut pdf);
        helper.pages().unwrap()
    };
    let root = pdf.root_ref().unwrap();
    let cat = pdf.resolve(root).unwrap();
    let pages_ref = cat.as_dict().unwrap().get_ref("Pages").unwrap();
    let pages = pdf.resolve(pages_ref).unwrap();
    let manual: Vec<_> = pages
        .as_dict()
        .unwrap()
        .get("Kids")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_ref_id().unwrap())
        .collect();
    assert_eq!(helper_pages, manual);
}

// ---------------------------------------------------------------------------
// Keystone: full-rewrite renumber converges across object numbers
// ---------------------------------------------------------------------------

/// Insert a new blank page as the second child of the page tree, allocating the
/// new page object at the caller-chosen object number `new_num`. Pure raw
/// `Object` manipulation — no helper involved.
fn insert_page_at(pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>, new_num: u32) {
    use flpdf::{Object, ObjectRef};
    let root = pdf.root_ref().unwrap();
    let pages_ref = pdf
        .resolve(root)
        .unwrap()
        .as_dict()
        .unwrap()
        .get_ref("Pages")
        .unwrap();
    let page_ref = ObjectRef::new(new_num, 0);
    let mut page = flpdf::Dictionary::new();
    page.insert("Type", Object::Name(b"Page".to_vec()));
    page.insert("Parent", Object::Reference(pages_ref));
    page.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    pdf.set_object(page_ref, Object::Dictionary(page));
    let mut pages = pdf.resolve(pages_ref).unwrap().as_dict().unwrap().clone();
    let kids = pages.get("Kids").unwrap().as_array().unwrap().to_vec();
    let mut new_kids = kids.clone();
    new_kids.insert(1, Object::Reference(page_ref));
    pages.insert("Kids", Object::Array(new_kids));
    pages.insert("Count", Object::Integer(kids.len() as i64 + 1));
    pdf.set_object(pages_ref, Object::Dictionary(pages));
}

#[test]
fn full_rewrite_converges_across_object_numbers() {
    let mut a = flpdf::Pdf::open(std::io::Cursor::new(build_n_page_pdf(2))).unwrap();
    let mut b = flpdf::Pdf::open(std::io::Cursor::new(build_n_page_pdf(2))).unwrap();
    insert_page_at(&mut a, 50);
    insert_page_at(&mut b, 80);
    assert_eq!(
        write_canonical(&mut a),
        write_canonical(&mut b),
        "full_rewrite renumber must converge regardless of internal object number"
    );
}
