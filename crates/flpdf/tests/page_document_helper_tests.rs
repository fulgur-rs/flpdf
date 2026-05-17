//! Integration tests for [`flpdf::PageDocumentHelper`].
//!
//! All tests build in-memory PDFs without touching the filesystem.  They use
//! `PageDocumentHelper` for all page-list access rather than calling
//! `pages::page_refs` or touching raw [`Object`] values directly.

use flpdf::{write_pdf, Object, ObjectRef, PageDocumentHelper, PageRange, Pdf, RotateMode};
use std::collections::BTreeMap;
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Minimal PDF builder
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

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

// ---------------------------------------------------------------------------
// pages() / iter() / get()
// ---------------------------------------------------------------------------

#[test]
fn pages_returns_correct_count() {
    let mut pdf = open(build_n_page_pdf(3));
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let pages = helper.pages().unwrap();
    assert_eq!(pages.len(), 3);
}

#[test]
fn iter_yields_all_pages_in_order() {
    let mut pdf = open(build_n_page_pdf(3));
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let from_pages = helper.pages().unwrap();
    let from_iter: Vec<ObjectRef> = helper.iter().unwrap().collect();
    assert_eq!(from_pages, from_iter);
}

#[test]
fn get_returns_correct_ref() {
    let mut pdf = open(build_n_page_pdf(3));
    let mut helper = PageDocumentHelper::new(&mut pdf);
    // Page 1 is object 3 0 R, page 2 is 4 0 R, page 3 is 5 0 R.
    assert_eq!(helper.get(0).unwrap(), Some(ObjectRef::new(3, 0)));
    assert_eq!(helper.get(1).unwrap(), Some(ObjectRef::new(4, 0)));
    assert_eq!(helper.get(2).unwrap(), Some(ObjectRef::new(5, 0)));
    assert_eq!(helper.get(3).unwrap(), None); // out of bounds
}

// ---------------------------------------------------------------------------
// rotate()
// ---------------------------------------------------------------------------

#[test]
fn rotate_all_pages_adds_rotate_key() {
    let mut pdf = open(build_n_page_pdf(2));
    let range = PageRange::parse("").unwrap(); // all pages
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        helper.rotate(&range, 90, RotateMode::Add).unwrap();
    }
    // Both leaves should now carry /Rotate 90.
    for obj_num in [3u32, 4u32] {
        let obj = pdf.resolve(ObjectRef::new(obj_num, 0)).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("object {obj_num} is not a dict");
        };
        assert_eq!(
            dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "page {obj_num} should have /Rotate 90"
        );
    }
}

#[test]
fn rotate_partial_range_only_affects_selected_pages() {
    let mut pdf = open(build_n_page_pdf(3));
    let range = PageRange::parse("1").unwrap(); // page 1 only
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        helper.rotate(&range, 180, RotateMode::Add).unwrap();
    }
    // Page 1 (3 0 R) should have /Rotate 180.
    let obj1 = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Dictionary(d1) = obj1 else {
        panic!()
    };
    assert_eq!(d1.get("Rotate"), Some(&Object::Integer(180)));

    // Pages 2 and 3 (4, 5) must not have been touched (no /Rotate key added).
    for obj_num in [4u32, 5u32] {
        let obj = pdf.resolve(ObjectRef::new(obj_num, 0)).unwrap();
        let Object::Dictionary(d) = obj else { panic!() };
        // The helper only writes /Rotate when called; no /Rotate was present
        // originally, and we didn't rotate these pages.
        assert_eq!(
            d.get("Rotate"),
            None,
            "page {obj_num} must not have /Rotate"
        );
    }
}

/// Round-trip: rotate then write→reopen, verify /Rotate is persisted.
#[test]
fn rotate_round_trip_persists_after_write_reopen() {
    let mut pdf = open(build_n_page_pdf(1));
    let range = PageRange::parse("").unwrap(); // all pages
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        helper.rotate(&range, 270, RotateMode::Assign).unwrap();
    }

    let mut serialized: Vec<u8> = Vec::new();
    write_pdf(&mut pdf, &mut serialized).unwrap();

    let mut pdf2 = open(serialized);
    let mut helper2 = PageDocumentHelper::new(&mut pdf2);
    let pages = helper2.pages().unwrap();
    assert_eq!(pages.len(), 1);
    drop(helper2);

    let obj = pdf2.resolve(pages[0]).unwrap();
    let Object::Dictionary(dict) = obj else {
        panic!()
    };
    assert_eq!(
        dict.get("Rotate"),
        Some(&Object::Integer(270)),
        "/Rotate 270 must survive serialization round-trip"
    );
}

// ---------------------------------------------------------------------------
// remove()
// ---------------------------------------------------------------------------

#[test]
fn remove_decreases_page_count() {
    let mut pdf = open(build_n_page_pdf(3));
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        // Remove the second page (0-based index 1).
        helper.remove(1).unwrap();
    }
    let mut helper = PageDocumentHelper::new(&mut pdf);
    assert_eq!(helper.pages().unwrap().len(), 2);
}

#[test]
fn remove_correct_page_removed() {
    let mut pdf = open(build_n_page_pdf(3));
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        helper.remove(0).unwrap(); // remove first page (3 0 R)
    }
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let pages = helper.pages().unwrap();
    assert_eq!(pages.len(), 2);
    // After removing page 1 (obj 3), remaining pages are obj 4 and 5.
    assert_eq!(pages[0], ObjectRef::new(4, 0));
    assert_eq!(pages[1], ObjectRef::new(5, 0));
}

#[test]
fn remove_out_of_bounds_is_error() {
    let mut pdf = open(build_n_page_pdf(2));
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let err = helper.remove(5).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

#[test]
fn remove_only_page_is_error() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let err = helper.remove(0).unwrap_err();
    // We return Missing for the empty-document case.
    assert!(
        matches!(err, flpdf::Error::Missing(_)),
        "expected Missing, got {err:?}"
    );
}

/// Round-trip: remove a page then write→reopen, verify page count decreased.
#[test]
fn remove_round_trip_page_count_decreases() {
    let mut pdf = open(build_n_page_pdf(3));
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        helper.remove(2).unwrap(); // remove last page
    }

    let mut serialized: Vec<u8> = Vec::new();
    write_pdf(&mut pdf, &mut serialized).unwrap();

    let mut pdf2 = open(serialized);
    let mut helper2 = PageDocumentHelper::new(&mut pdf2);
    assert_eq!(helper2.pages().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// insert()
// ---------------------------------------------------------------------------

#[test]
fn insert_increases_page_count() {
    let mut pdf = open(build_n_page_pdf(2));
    let existing_ref = {
        let mut h = PageDocumentHelper::new(&mut pdf);
        h.get(0).unwrap().unwrap()
    };
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        // Append a duplicate of page 1 at the end.
        helper.insert(2, existing_ref).unwrap();
    }
    let mut helper = PageDocumentHelper::new(&mut pdf);
    assert_eq!(helper.pages().unwrap().len(), 3);
}

#[test]
fn insert_at_beginning_prepends() {
    let mut pdf = open(build_n_page_pdf(3));
    // Grab obj ref for page 3 (5 0 R) and insert it at position 0.
    let last_ref = ObjectRef::new(5, 0);
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        helper.insert(0, last_ref).unwrap();
    }
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let pages = helper.pages().unwrap();
    // Now page order should be [5, 3, 4, 5] but since rebuild_page_tree
    // handles the duplicate by cloning, just verify the first page is obj 5
    // and total count is 4.
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[0], last_ref, "inserted page must be at index 0");
}

#[test]
fn insert_out_of_bounds_is_error() {
    let mut pdf = open(build_n_page_pdf(2));
    let some_ref = ObjectRef::new(3, 0);
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let err = helper.insert(10, some_ref).unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

#[test]
fn insert_at_end_appends() {
    let mut pdf = open(build_n_page_pdf(2));
    let first_ref = ObjectRef::new(3, 0);
    {
        let mut helper = PageDocumentHelper::new(&mut pdf);
        // Append at idx == page_count is valid.
        helper.insert(2, first_ref).unwrap();
    }
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let pages = helper.pages().unwrap();
    assert_eq!(pages.len(), 3);
}
