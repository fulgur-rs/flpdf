//! Integration tests for [`flpdf::PageDocumentHelper`].
//!
//! All tests build in-memory PDFs without touching the filesystem.  They use
//! `PageDocumentHelper` for all page-list access rather than calling
//! `pages::page_refs` or touching raw [`Object`] values directly.

use flpdf::{
    Dictionary, Object, ObjectHandle, ObjectRef, PageDocumentHelper, PageInput, Pdf, Stream,
};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::process::Command;

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

fn build_pdf(page_extra: &str, extra_objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();

    for (number, body) in [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
    ] {
        offsets.insert(number, out.len() as u64);
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page_extra} >>\nendobj\n"
        )
        .as_bytes(),
    );
    for &(number, ref body) in extra_objects {
        offsets.insert(number, out.len() as u64);
        out.extend_from_slice(body);
    }

    let max_number = offsets.keys().copied().max().unwrap_or(3);
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_number + 1).as_bytes());
    for number in 1..=max_number {
        match offsets.get(&number) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            max_number + 1
        )
        .as_bytes(),
    );
    out
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

/// Embed the existing page-tree root directly in the catalog, as qpdf permits.
fn make_catalog_pages_root_direct(pdf: &mut Pdf<Cursor<Vec<u8>>>) {
    let Object::Dictionary(pages) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Dictionary(pages));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));
}

fn assert_direct_catalog_pages_root(pdf: &mut Pdf<Cursor<Vec<u8>>>, expected_count: i64) {
    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    let Some(Object::Dictionary(pages)) = catalog.get("Pages") else {
        panic!("catalog /Pages must remain direct");
    };
    assert_eq!(pages.get("Count"), Some(&Object::Integer(expected_count)));
    let Some(Object::Array(kids)) = pages.get("Kids") else {
        panic!("direct pages root must retain /Kids");
    };
    assert_eq!(kids.len(), expected_count as usize);
    let expected_parent = Object::Dictionary(pages.clone());

    for page in PageDocumentHelper::new(pdf).get_all_pages().unwrap() {
        let Object::Dictionary(page) = pdf.resolve(page).unwrap() else {
            panic!("page must remain a dictionary");
        };
        assert_eq!(
            page.get("Parent"),
            Some(&expected_parent),
            "qpdf keeps the final direct root as each flattened leaf's /Parent"
        );
    }
}

/// A one-page PDF whose catalog incorrectly points `/Pages` at the leaf.
/// qpdf walks `/Parent` to repair the catalog before `getAllPages()` returns.
fn pdf_with_catalog_pages_pointing_to_leaf() -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (number, body) in [
        (1, "<< /Type /Catalog /Pages 3 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ] {
        offsets.insert(number, out.len() as u64);
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for number in 1..=3 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

fn pdf_with_need_appearances_and_unread_default_resources() -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (number, body) in [
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
        ),
        (4, "<< /NeedAppearances true /DR 6 0 R >>"),
        (5, "<< /Type /Annot /Subtype /Widget >>"),
        (6, "not-a-pdf-object"),
    ] {
        offsets.insert(number, out.len() as u64);
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for number in 1..=6 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

/// A direct intermediate /Pages node whose /Kids array is an indirect holder.
/// qpdf dereferences that holder through the document before classifying the
/// child as a page.
fn pdf_with_direct_pages_node_kids_holder() -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();
    let objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
        (
            2,
            b"<< /Type /Pages /Kids [<< /Type /NotPages /Kids 6 0 R /Count 1 >>] /Count 1 >>"
                .as_slice(),
        ),
        (6, b"[7 0 R]".as_slice()),
        (
            7,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".as_slice(),
        ),
    ];
    for (number, body) in objects {
        offsets.insert(number, out.len() as u64);
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
    for number in 1..=7 {
        match offsets.get(&number) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

// ---------------------------------------------------------------------------
// getAllPages() / pushInheritedAttributesToPages()
// ---------------------------------------------------------------------------

#[test]
fn add_page_indirects_a_direct_page_input() {
    let mut pdf = open(build_n_page_pdf(1));
    let direct_page = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (
            b"MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(612),
                ObjectHandle::integer(792),
            ]),
        ),
    ]);

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::direct(direct_page), false)
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 2);
    assert_ne!(pages[1], ObjectRef::new(3, 0));
    let Object::Dictionary(page) = pdf.resolve(pages[1]).unwrap() else {
        panic!("direct input must become an indirect page dictionary");
    };
    assert_eq!(
        page.get("Type").and_then(Object::as_name),
        Some(b"Page".as_slice())
    );
    assert_eq!(page.get_ref("Parent"), Some(ObjectRef::new(2, 0)));
}

#[test]
fn add_page_duplicate_does_not_overwrite_a_handle_only_object() {
    let mut pdf = open(build_n_page_pdf(1));
    let reserved = pdf
        .make_indirect_object_handle(ObjectHandle::integer(42))
        .expect("reserve a handle-only object");
    let reserved_ref = reserved.object_ref().expect("reserved handle is indirect");

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::existing(ObjectRef::new(3, 0)), false)
        .expect("duplicate the existing page");

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_ne!(pages[1], reserved_ref);
    assert_eq!(
        pdf.resolve(reserved_ref).unwrap(),
        Object::Integer(42),
        "duplicating a page must not reuse a handle-registry object number"
    );
}

#[test]
fn add_page_copies_a_foreign_page_after_materializing_source_inheritance() {
    let mut source = open(build_n_page_pdf(1));
    let Object::Dictionary(mut source_root) = source.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("source /Pages must be a dictionary");
    };
    let Object::Dictionary(mut source_page) = source.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("source page must be a dictionary");
    };
    let media_box = source_page.remove("MediaBox").unwrap();
    source_root.insert("MediaBox", media_box);
    source.set_object(ObjectRef::new(2, 0), Object::Dictionary(source_root));
    source.set_object(ObjectRef::new(3, 0), Object::Dictionary(source_page));

    let mut target = open(build_n_page_pdf(1));
    PageDocumentHelper::new(&mut target)
        .add_page(PageInput::foreign(&mut source, ObjectRef::new(3, 0)), false)
        .unwrap();

    let Object::Dictionary(materialized_source_page) =
        source.resolve(ObjectRef::new(3, 0)).unwrap()
    else {
        panic!("source page must remain a dictionary");
    };
    assert!(
        materialized_source_page.get("MediaBox").is_some(),
        "qpdf materializes inherited attributes on the source before foreign copy"
    );

    let target_pages = PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .unwrap();
    assert_eq!(target_pages.len(), 2);
    let Object::Dictionary(copied_page) = target.resolve(target_pages[1]).unwrap() else {
        panic!("foreign input must produce a target page dictionary");
    };
    assert!(
        copied_page.get("MediaBox").is_some(),
        "copied page must retain the materialized source MediaBox"
    );
    assert_eq!(copied_page.get_ref("Parent"), Some(ObjectRef::new(2, 0)));
}

#[test]
fn add_page_uses_qpdf_copy_foreign_object_null_key_filtering() {
    let mut source = open(build_n_page_pdf(1));
    let indirect_null = ObjectRef::new(10, 0);
    source.set_object(indirect_null, Object::Null);
    let Object::Dictionary(mut page) = source.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("source page must be a dictionary");
    };
    page.insert("IndirectNull", Object::Reference(indirect_null));
    source.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    let mut target = open(build_n_page_pdf(1));
    PageDocumentHelper::new(&mut target)
        .add_page(PageInput::foreign(&mut source, ObjectRef::new(3, 0)), false)
        .expect("foreign page insertion should succeed");

    let pages = PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .expect("target pages");
    let Object::Dictionary(copied_page) = target.resolve(pages[1]).unwrap() else {
        panic!("foreign input must produce a target page dictionary");
    };
    assert!(
        copied_page.get("IndirectNull").is_none(),
        "qpdf getKeys omits a source dictionary key whose indirect value resolves to null"
    );
}

#[test]
fn add_page_recopies_a_page_left_as_a_nested_boundary_placeholder() {
    // Regression test for qpdf's nested `/Pages`-boundary behavior:
    // `copyForeignObject` reserves an indirect-null placeholder when a source
    // page is first encountered below another copied root, but a later
    // top-level page copy must recopy that page instead of treating the
    // placeholder as a completed object. This matches
    // `reserveObjects` (`libqpdf/QPDF.cc:2118-2132`) and ensures page-tree
    // insertion receives the actual page content.
    let mut source = open(build_n_page_pdf(1));
    let mut target = open(build_n_page_pdf(1));

    // Seed the shared map: copy some unrelated array holder that nests the
    // source's page as a non-top-level reference, exactly as a caller could
    // do before ever calling `add_page` for the same page.
    let page_handle = source.get_object_handle(ObjectRef::new(3, 0));
    let holder = source
        .make_indirect_object_handle(ObjectHandle::array(vec![page_handle]))
        .expect("holder array referencing the source page");
    target
        .copy_foreign_object(&holder)
        .expect("seed a nested-page null placeholder in the shared foreign object map");

    PageDocumentHelper::new(&mut target)
        .add_page(PageInput::foreign(&mut source, ObjectRef::new(3, 0)), false)
        .expect("copy the same page as a real page-tree insertion");

    let pages = PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .unwrap();
    assert_eq!(pages.len(), 2);
    let Object::Dictionary(copied_page) = target.resolve(pages[1]).unwrap() else {
        panic!(
            "foreign input must produce a real target page dictionary, not the \
             leftover null placeholder left by the canonical port's nested-page \
             boundary reservation"
        );
    };
    assert_eq!(
        copied_page.get("Type").and_then(Object::as_name),
        Some(b"Page".as_slice()),
        "the rebuilt page tree must hold the actual copied page content"
    );
}

#[test]
fn add_page_reuses_foreign_resources_from_the_same_source() {
    let mut source = open(build_n_page_pdf(2));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(Dictionary::new()));
    source.set_object(ObjectRef::new(5, 0), Object::Dictionary(resources));
    for page_ref in [ObjectRef::new(3, 0), ObjectRef::new(4, 0)] {
        let Object::Dictionary(mut page) = source.resolve(page_ref).unwrap() else {
            panic!("source page must be a dictionary");
        };
        page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
        source.set_object(page_ref, Object::Dictionary(page));
    }

    let mut target = open(build_n_page_pdf(1));
    for source_page in [ObjectRef::new(3, 0), ObjectRef::new(4, 0)] {
        PageDocumentHelper::new(&mut target)
            .add_page(PageInput::foreign(&mut source, source_page), false)
            .unwrap();
    }

    let pages = PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .unwrap();
    let Object::Dictionary(first) = target.resolve(pages[1]).unwrap() else {
        panic!("first imported page must be a dictionary");
    };
    let Object::Dictionary(second) = target.resolve(pages[2]).unwrap() else {
        panic!("second imported page must be a dictionary");
    };
    assert_eq!(
        first.get_ref("Resources"),
        second.get_ref("Resources"),
        "qpdf reuses one local resource object for repeated foreign copies"
    );
}

#[test]
fn add_page_does_not_copy_a_second_page_referenced_by_a_foreign_page() {
    let mut source = open(build_n_page_pdf(2));
    let Object::Dictionary(mut source_page) = source.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("source page must be a dictionary");
    };
    source_page.insert("Peer", Object::Reference(ObjectRef::new(4, 0)));
    source.set_object(ObjectRef::new(3, 0), Object::Dictionary(source_page));

    let mut target = open(build_n_page_pdf(1));
    PageDocumentHelper::new(&mut target)
        .add_page(PageInput::foreign(&mut source, ObjectRef::new(3, 0)), false)
        .unwrap();

    let pages = PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .unwrap();
    let Object::Dictionary(copied_page) = target.resolve(pages[1]).unwrap() else {
        panic!("foreign input must produce a target page dictionary");
    };
    let peer = copied_page
        .get_ref("Peer")
        .expect("qpdf reserves a target identity for the non-top-level Page");
    assert_eq!(
        target.resolve(peer).unwrap(),
        Object::Null,
        "qpdf copyForeignObject leaves the non-top-level Page reservation null"
    );
}

#[test]
fn get_all_pages_repairs_catalog_pages_pointer() {
    let mut pdf = open(pdf_with_catalog_pages_pointing_to_leaf());

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();

    assert_eq!(pages, vec![ObjectRef::new(3, 0)]);
    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0)))
    );
}

/// qpdf follows `/Parent` even when the catalog embeds the first `/Page`
/// dictionary directly in `/Pages` (QPDF_pages.cc:47-67).
#[test]
fn get_all_pages_follows_parent_from_direct_catalog_page_value() {
    let mut pdf = open(build_n_page_pdf(2));
    let direct_page = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", direct_page);
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages, vec![ObjectRef::new(3, 0), ObjectRef::new(4, 0)]);

    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0)))
    );
}

#[test]
fn get_all_pages_traverses_a_direct_catalog_pages_root() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(pages) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Dictionary(pages));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    assert_eq!(
        PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap(),
        vec![ObjectRef::new(3, 0)],
        "qpdf traverses a direct catalog /Pages dictionary without materializing it"
    );

    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert!(
        matches!(catalog.get("Pages"), Some(Object::Dictionary(_))),
        "qpdf keeps the catalog /Pages root direct"
    );
}

#[test]
fn get_all_pages_marks_qpdf_json_observation() {
    let mut pdf = open(build_n_page_pdf(1));
    assert!(!pdf.ever_called_get_all_pages());

    PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();

    assert!(
        pdf.ever_called_get_all_pages(),
        "qpdf marks everCalledGetAllPages whenever getAllPages initializes the page list"
    );
}

#[test]
fn push_inherited_attributes_marks_qpdf_json_observation() {
    let mut pdf = open(build_n_page_pdf(1));
    assert!(!pdf.ever_called_get_all_pages());

    PageDocumentHelper::new(&mut pdf)
        .push_inherited_attributes_to_pages()
        .unwrap();

    assert!(
        pdf.ever_called_get_all_pages(),
        "qpdf pushInheritedAttributesToPage initializes pages through getAllPages"
    );
}

#[test]
fn get_all_pages_returns_empty_when_catalog_has_no_pages() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.remove("Pages");
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    assert!(
        PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .unwrap()
            .is_empty(),
        "qpdf getAllPages returns an empty list when the catalog has no /Pages"
    );
}

#[test]
fn get_all_pages_returns_empty_when_catalog_pages_is_not_a_dictionary() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Integer(42));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    assert!(PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap()
        .is_empty());
}

#[test]
fn helper_flatten_annotations_repairs_page_tree_before_enumerating() {
    let mut pdf = open(build_n_page_pdf(2));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Reference(ObjectRef::new(3, 0)));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0))),
        "qpdf flattenAnnotations obtains the repaired all-pages list before processing pages"
    );
}

#[test]
fn helper_flatten_annotations_uses_qpdf_flag_contract_and_removes_acroform() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
    );

    let mut appearance = Dictionary::new();
    appearance.insert("Type", Object::Name(b"XObject".to_vec()));
    appearance.insert("Subtype", Object::Name(b"Form".to_vec()));
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    for (object_ref, flags) in [(ObjectRef::new(6, 0), 4), (ObjectRef::new(7, 0), 0)] {
        let mut annot = Dictionary::new();
        annot.insert("Type", Object::Name(b"Annot".to_vec()));
        annot.insert("Subtype", Object::Name(b"Widget".to_vec()));
        let flags = if object_ref == ObjectRef::new(6, 0) {
            Object::Reference(ObjectRef::new(10, 0))
        } else {
            Object::Integer(flags)
        };
        annot.insert("F", flags);
        annot.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        let mut ap = Dictionary::new();
        ap.insert("N", Object::Reference(ObjectRef::new(8, 0)));
        annot.insert("AP", Object::Dictionary(ap));
        pdf.set_object(object_ref, Object::Dictionary(annot));
    }
    pdf.set_object(
        ObjectRef::new(10, 0),
        Object::Reference(ObjectRef::new(11, 0)),
    );
    pdf.set_object(ObjectRef::new(11, 0), Object::Integer(4));

    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert(
        "Annots",
        Object::Array(vec![
            Object::Reference(ObjectRef::new(6, 0)),
            Object::Reference(ObjectRef::new(7, 0)),
        ]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    let mut acroform = Dictionary::new();
    acroform.insert(
        "Fields",
        Object::Array(vec![
            Object::Reference(ObjectRef::new(6, 0)),
            Object::Reference(ObjectRef::new(7, 0)),
        ]),
    );
    pdf.set_object(ObjectRef::new(9, 0), Object::Dictionary(acroform));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("AcroForm", Object::Reference(ObjectRef::new(9, 0)));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    if Command::new("qpdf").arg("--version").output().is_ok() {
        let mut input = Vec::new();
        let options = WriterTestSettings {
            static_id: true,
            ..WriterTestSettings::default()
        };
        write_with_settings(&mut pdf, &mut input, &options).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("flags-input.pdf");
        let output_path = dir.path().join("flags-qpdf.pdf");
        fs::write(&input_path, input).unwrap();
        let output = Command::new("qpdf")
            .arg("--flatten-annotations=print")
            .arg(&input_path)
            .arg(&output_path)
            .output()
            .unwrap();
        // qpdf writes a usable output then returns 3 when it repaired this
        // deliberately minimal probe fixture. Treat that warning exit as a
        // successful oracle observation on every CI platform.
        assert!(
            matches!(output.status.code(), Some(0 | 3)),
            "qpdf did not produce an oracle output: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut qpdf_output = Pdf::open(Cursor::new(fs::read(&output_path).unwrap())).unwrap();
        let qpdf_pages = PageDocumentHelper::new(&mut qpdf_output)
            .get_all_pages()
            .unwrap();
        let qpdf_annots =
            flpdf::enumerate_page_annotations(&mut qpdf_output, qpdf_pages[0]).unwrap();
        assert!(qpdf_annots.is_empty());
        let Object::Dictionary(qpdf_catalog) = qpdf_output.resolve(ObjectRef::new(1, 0)).unwrap()
        else {
            panic!("qpdf output catalog must be a dictionary");
        };
        assert!(qpdf_catalog.get("AcroForm").is_none());
    }

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0x4, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(page.get("Annots").is_none());
    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    assert!(catalog.get("AcroForm").is_none());
    let contents = flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
    assert!(String::from_utf8_lossy(&contents).contains("/Fxo1 Do"));
}

#[test]
fn helper_flatten_annotations_keeps_widgets_when_need_appearances_is_true() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance = Dictionary::new();
    appearance.insert("Type", Object::Name(b"XObject".to_vec()));
    appearance.insert("Subtype", Object::Name(b"Form".to_vec()));
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Reference(ObjectRef::new(5, 0)));
    let mut widget = Dictionary::new();
    widget.insert("Subtype", Object::Name(b"Widget".to_vec()));
    widget.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    widget.insert("AP", Object::Dictionary(ap));
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(widget));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));
    let mut acroform = Dictionary::new();
    pdf.set_object(ObjectRef::new(7, 0), Object::Boolean(true));
    acroform.insert("NeedAppearances", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    pdf.set_object(ObjectRef::new(9, 0), Object::Dictionary(acroform));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("AcroForm", Object::Reference(ObjectRef::new(6, 0)));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(page.get("Annots").is_some());
    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    assert!(catalog.get("AcroForm").is_some());
}

#[test]
fn helper_flatten_annotations_merges_acroform_dr_into_widget_appearance() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance_resources = Dictionary::new();
    appearance_resources.insert("Font", Object::Reference(ObjectRef::new(7, 0)));
    let mut appearance = Dictionary::new();
    appearance.insert("Type", Object::Name(b"XObject".to_vec()));
    appearance.insert("Subtype", Object::Name(b"Form".to_vec()));
    appearance.insert("Resources", Object::Reference(ObjectRef::new(12, 0)));
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    let mut existing_fonts = Dictionary::new();
    existing_fonts.insert("F1", Object::Integer(41));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    pdf.set_object(ObjectRef::new(8, 0), Object::Dictionary(existing_fonts));
    pdf.set_object(
        ObjectRef::new(12, 0),
        Object::Reference(ObjectRef::new(13, 0)),
    );
    pdf.set_object(
        ObjectRef::new(13, 0),
        Object::Dictionary(appearance_resources),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Reference(ObjectRef::new(5, 0)));
    let mut widget = Dictionary::new();
    widget.insert("Subtype", Object::Name(b"Widget".to_vec()));
    widget.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    widget.insert("AP", Object::Dictionary(ap));
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(widget));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    let mut fonts = Dictionary::new();
    fonts.insert("Helv", Object::Integer(42));
    let mut dr = Dictionary::new();
    dr.insert("Font", Object::Dictionary(fonts));
    pdf.set_object(
        ObjectRef::new(10, 0),
        Object::Reference(ObjectRef::new(11, 0)),
    );
    pdf.set_object(ObjectRef::new(11, 0), Object::Dictionary(dr));
    let mut acroform = Dictionary::new();
    acroform.insert("DR", Object::Reference(ObjectRef::new(10, 0)));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    pdf.set_object(ObjectRef::new(9, 0), Object::Dictionary(acroform));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("AcroForm", Object::Reference(ObjectRef::new(6, 0)));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Stream(appearance) = pdf.resolve(ObjectRef::new(5, 0)).unwrap() else {
        panic!("appearance must remain a stream");
    };
    let Some(Object::Reference(resources_ref)) = appearance.dict.get("Resources") else {
        panic!("appearance must retain its live resources reference");
    };
    assert_eq!(*resources_ref, ObjectRef::new(12, 0));
    let Object::Dictionary(resources) = pdf.resolve(ObjectRef::new(13, 0)).unwrap() else {
        panic!("referenced resources must remain a dictionary");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("referenced resources must retain font resources");
    };
    assert_eq!(fonts.get("F1"), Some(&Object::Integer(41)));
    assert_eq!(fonts.get("Helv"), Some(&Object::Integer(42)));
}

#[test]
fn helper_flatten_annotations_keeps_need_appearances_with_unread_dr() {
    let mut pdf = open(pdf_with_need_appearances_and_unread_default_resources());

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(
        page.get("Annots").is_some(),
        "qpdf skips widgets before it needs their malformed /AcroForm/DR"
    );
}

#[test]
fn helper_flatten_annotations_defers_widget_rect_validation_past_resource_merge() {
    let mut pdf = open(build_pdf(
        "/Annots [5 0 R]",
        &[
            (
                4,
                b"4 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << >> /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
            ),
            (
                5,
                b"5 0 obj\n<< /Type /Annot /Subtype /Widget /F 2 /Rect 7 0 R /AP << /N 4 0 R >> >>\nendobj\n".to_vec(),
            ),
            (6, b"6 0 obj\n<< >>\nendobj\n".to_vec()),
            (
                7,
                b"7 0 obj\n[0 0 /malformed 20]\nendobj\n".to_vec(),
            ),
        ],
    ));

    let mut acroform = Dictionary::new();
    acroform.insert("DR", Object::Reference(ObjectRef::new(6, 0)));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("AcroForm", Object::Dictionary(acroform));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    let annotation = pdf.get_object_handle(ObjectRef::new(5, 0));
    pdf.resolve_object_handle(&annotation).unwrap();
    let rect = annotation.get_key(b"/Rect");
    assert!(!rect.is_resolved());

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x2)
        .unwrap();

    assert!(
        !rect.is_resolved(),
        "qpdf's resource merge must not materialize /Rect before the flags gate"
    );

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(page.get("Annots").is_none());
}

#[test]
fn helper_flatten_annotations_reuses_a_materialized_inline_appearance() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance_resources = Dictionary::new();
    appearance_resources.insert("Font", Object::Dictionary(Dictionary::new()));
    let mut appearance = Dictionary::new();
    appearance.insert("Type", Object::Name(b"XObject".to_vec()));
    appearance.insert("Subtype", Object::Name(b"Form".to_vec()));
    appearance.insert("Resources", Object::Dictionary(appearance_resources));
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Stream(Stream::new(appearance, Vec::new())));
    let mut widget = Dictionary::new();
    widget.insert("Subtype", Object::Name(b"Widget".to_vec()));
    widget.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    widget.insert("AP", Object::Dictionary(ap));
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(widget));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    let mut fonts = Dictionary::new();
    fonts.insert("Helv", Object::Integer(42));
    let mut dr = Dictionary::new();
    dr.insert("Font", Object::Dictionary(fonts));
    let mut acroform = Dictionary::new();
    acroform.insert("DR", Object::Dictionary(dr));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(acroform));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("AcroForm", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    let Some(Object::Dictionary(resources)) = page.get("Resources") else {
        panic!("page must retain resources");
    };
    let Some(Object::Dictionary(xobjects)) = resources.get("XObject") else {
        panic!("flattened appearance must be registered as an XObject");
    };
    let Some(Object::Reference(xobject_ref)) = xobjects.get("Fxo1") else {
        panic!("flattened appearance must use an indirect XObject");
    };
    let Object::Stream(appearance) = pdf.resolve(*xobject_ref).unwrap() else {
        panic!("registered XObject must be the materialized appearance");
    };
    let Some(Object::Dictionary(resources)) = appearance.dict.get("Resources") else {
        panic!("appearance must retain resources");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("appearance must retain font resources");
    };
    assert_eq!(fonts.get("Helv"), Some(&Object::Integer(42)));
}

#[test]
fn helper_flatten_annotations_materializes_indirect_page_resources_without_annotations() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut fonts = Dictionary::new();
    fonts.insert("F1", Object::Integer(42));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(fonts));
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Resources", Object::Reference(ObjectRef::new(4, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    let Some(Object::Dictionary(resources)) = page.get("Resources") else {
        panic!("qpdf flattenAnnotations materializes an indirect /Resources");
    };
    assert!(resources.get("Font").is_some());
}

#[test]
fn helper_flatten_annotations_replaces_invalid_page_resources() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Resources", Object::Integer(42));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(matches!(page.get("Resources"), Some(Object::Dictionary(_))));
}

#[test]
fn helper_flatten_annotations_preserves_indirect_annots_holder_when_an_annotation_remains() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance = Dictionary::new();
    appearance.insert("Type", Object::Name(b"XObject".to_vec()));
    appearance.insert("Subtype", Object::Name(b"Form".to_vec()));
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Reference(ObjectRef::new(5, 0)));
    let mut widget = Dictionary::new();
    widget.insert("Subtype", Object::Name(b"Widget".to_vec()));
    widget.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    widget.insert("AP", Object::Dictionary(ap));
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(widget));
    let mut link = Dictionary::new();
    link.insert("Subtype", Object::Name(b"Link".to_vec()));
    pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(link));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Array(vec![
            Object::Reference(ObjectRef::new(4, 0)),
            Object::Reference(ObjectRef::new(6, 0)),
        ]),
    );
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Annots", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert_eq!(
        page.get("Annots"),
        Some(&Object::Reference(ObjectRef::new(7, 0)))
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(7, 0)).unwrap(),
        Object::Array(vec![Object::Reference(ObjectRef::new(6, 0))])
    );
}

#[test]
fn helper_flatten_annotations_expands_indirect_contents_and_wraps_empty_output() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"old-a\n".to_vec())),
    );
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"old-b\n".to_vec())),
    );
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Array(vec![
            Object::Reference(ObjectRef::new(4, 0)),
            Object::Reference(ObjectRef::new(5, 0)),
        ]),
    );

    // The selected appearance is a stream without /BBox. qpdf removes the
    // annotation and still appends its q/Q wrapper streams, but has no Do.
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Reference(ObjectRef::new(7, 0)));
    let mut annot = Dictionary::new();
    annot.insert("Subtype", Object::Name(b"Text".to_vec()));
    annot.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(10),
            Object::Integer(10),
        ]),
    );
    annot.insert("AP", Object::Dictionary(ap));
    pdf.set_object(ObjectRef::new(8, 0), Object::Dictionary(annot));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(6, 0)));
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(8, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(page.get("Annots").is_none());
    let Some(Object::Array(contents)) = page.get("Contents") else {
        panic!("qpdf addPageContents always writes a direct contents array");
    };
    assert_eq!(contents.len(), 4, "before, both original streams, after");
    assert_eq!(contents[1], Object::Reference(ObjectRef::new(4, 0)));
    assert_eq!(contents[2], Object::Reference(ObjectRef::new(5, 0)));
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
        b"q\nold-a\nold-b\n\nQ\n"
    );
}

#[test]
fn helper_flatten_annotations_wraps_when_non_null_ap_has_no_normal_stream() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"existing\n".to_vec())),
    );

    // qpdf removes an annotation when /AP is non-null even if it does not
    // contain a selectable normal-appearance stream. This specifically takes
    // the no-placement branch, which must still add q/Q page-content wrappers.
    let mut annot = Dictionary::new();
    annot.insert("AP", Object::Integer(1));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(annot));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(5, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert!(page.get("Annots").is_none());
    let Some(Object::Array(contents)) = page.get("Contents") else {
        panic!("qpdf addPageContents always writes a direct contents array");
    };
    assert_eq!(contents.len(), 3, "before, original stream, after");
    assert_eq!(contents[1], Object::Reference(ObjectRef::new(4, 0)));
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
        b"q\nexisting\n\nQ\n"
    );
}

#[test]
fn helper_flatten_annotations_keeps_an_indirect_null_appearance() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Reference(ObjectRef::new(7, 0)),
    );
    pdf.set_object(ObjectRef::new(7, 0), Object::Null);
    let mut annot = Dictionary::new();
    annot.insert("AP", Object::Reference(ObjectRef::new(6, 0)));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(annot));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(5, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert_eq!(
        page.get("Annots"),
        Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
            5, 0
        ))])),
        "qpdf preserves an annotation whose /AP resolves to null"
    );
}

#[test]
fn helper_flatten_annotations_prunes_a_chained_annots_holder() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut removable = Dictionary::new();
    removable.insert("AP", Object::Integer(1));
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(removable));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(Dictionary::new()));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Reference(ObjectRef::new(7, 0)),
    );
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Array(vec![
            Object::Reference(ObjectRef::new(4, 0)),
            Object::Reference(ObjectRef::new(5, 0)),
        ]),
    );
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Annots", Object::Reference(ObjectRef::new(6, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert_eq!(
        page.get("Annots"),
        Some(&Object::Reference(ObjectRef::new(6, 0)))
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(6, 0)).unwrap(),
        Object::Array(vec![Object::Reference(ObjectRef::new(5, 0))]),
        "qpdf replaces the outer /Annots holder with the retained annotations"
    );
}

#[test]
fn helper_flatten_annotations_follows_chained_appearance_holders() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance = Dictionary::new();
    appearance.insert("BBox", Object::Reference(ObjectRef::new(9, 0)));
    appearance.insert("Matrix", Object::Reference(ObjectRef::new(11, 0)));
    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Reference(ObjectRef::new(10, 0)),
    );
    pdf.set_object(
        ObjectRef::new(10, 0),
        Object::Array(vec![
            Object::Reference(ObjectRef::new(13, 0)),
            Object::Reference(ObjectRef::new(14, 0)),
            Object::Reference(ObjectRef::new(15, 0)),
            Object::Reference(ObjectRef::new(16, 0)),
        ]),
    );
    pdf.set_object(ObjectRef::new(13, 0), Object::Integer(0));
    pdf.set_object(ObjectRef::new(14, 0), Object::Integer(0));
    pdf.set_object(ObjectRef::new(15, 0), Object::Integer(100));
    pdf.set_object(ObjectRef::new(16, 0), Object::Integer(20));
    pdf.set_object(
        ObjectRef::new(11, 0),
        Object::Reference(ObjectRef::new(12, 0)),
    );
    pdf.set_object(
        ObjectRef::new(12, 0),
        Object::Array(vec![
            Object::Reference(ObjectRef::new(17, 0)),
            Object::Reference(ObjectRef::new(18, 0)),
            Object::Reference(ObjectRef::new(19, 0)),
            Object::Reference(ObjectRef::new(20, 0)),
            Object::Reference(ObjectRef::new(21, 0)),
            Object::Reference(ObjectRef::new(22, 0)),
        ]),
    );
    pdf.set_object(ObjectRef::new(17, 0), Object::Integer(1));
    pdf.set_object(ObjectRef::new(18, 0), Object::Integer(0));
    pdf.set_object(ObjectRef::new(19, 0), Object::Integer(0));
    pdf.set_object(ObjectRef::new(20, 0), Object::Integer(1));
    pdf.set_object(ObjectRef::new(21, 0), Object::Integer(0));
    pdf.set_object(ObjectRef::new(22, 0), Object::Integer(0));
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );
    pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(ap));
    let mut annot = Dictionary::new();
    annot.insert("AP", Object::Reference(ObjectRef::new(5, 0)));
    annot.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annot));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let content = flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
    assert!(
        String::from_utf8_lossy(&content).contains("/Fxo1 Do"),
        "terminal /AP/N stream must be drawn"
    );
}

#[test]
fn helper_flatten_annotations_looks_up_non_utf8_appearance_state_bytes() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance = Dictionary::new();
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    let mut states = Dictionary::new();
    states.insert(vec![0xff], Object::Reference(ObjectRef::new(6, 0)));
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Dictionary(states));
    let mut annot = Dictionary::new();
    annot.insert("AP", Object::Dictionary(ap));
    annot.insert("AS", Object::Name(vec![0xff]));
    annot.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annot));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let content = flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
    assert!(
        String::from_utf8_lossy(&content).contains("/Fxo1 Do"),
        "a raw non-UTF-8 /AS name must select the matching /AP/N state"
    );
}

#[test]
fn helper_flatten_annotations_applies_no_rotate_using_leaf_rotate() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut appearance = Dictionary::new();
    appearance.insert("Type", Object::Name(b"XObject".to_vec()));
    appearance.insert("Subtype", Object::Name(b"Form".to_vec()));
    appearance.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(appearance, Vec::new())),
    );
    let mut ap = Dictionary::new();
    ap.insert("N", Object::Reference(ObjectRef::new(4, 0)));
    let mut annot = Dictionary::new();
    annot.insert("Subtype", Object::Name(b"Widget".to_vec()));
    annot.insert("F", Object::Integer(0x10));
    annot.insert(
        "Rect",
        Object::Array(vec![
            Object::Integer(10),
            Object::Integer(20),
            Object::Integer(110),
            Object::Integer(40),
        ]),
    );
    annot.insert("AP", Object::Dictionary(ap));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(annot));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Rotate", Object::Integer(90));
    page.insert(
        "Annots",
        Object::Array(vec![Object::Reference(ObjectRef::new(5, 0))]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations(0, 0x3)
        .unwrap();

    let contents = flpdf::pages::page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
    assert!(
        String::from_utf8_lossy(&contents).contains("0 1 -1 0 30 40 cm"),
        "qpdf's NoRotate 90-degree matrix must be applied"
    );
}

#[test]
fn get_all_pages_rejects_a_pages_tree_cycle() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut pages) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    pages.insert(
        "Kids",
        Object::Array(vec![Object::Reference(ObjectRef::new(2, 0))]),
    );
    pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(pages));

    let error = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap_err();
    assert!(
        error.to_string().contains("cycle"),
        "qpdf rejects page-tree loops: {error}"
    );
}

#[test]
fn get_all_pages_rejects_a_revisited_pages_subtree_like_qpdf() {
    let mut pdf = open(build_n_page_pdf(1));

    let mut interior = Dictionary::new();
    interior.insert("Type", Object::Name(b"Pages".to_vec()));
    interior.insert(
        "Kids",
        Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]),
    );
    interior.insert("Count", Object::Integer(1));
    pdf.set_object(ObjectRef::new(11, 0), Object::Dictionary(interior));

    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    root.insert(
        "Kids",
        Object::Array(vec![
            Object::Reference(ObjectRef::new(11, 0)),
            Object::Reference(ObjectRef::new(11, 0)),
        ]),
    );
    root.insert("Count", Object::Integer(2));
    pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

    let error = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap_err();
    assert!(
        error.to_string().contains("cycle"),
        "qpdf's traversal-global visited set rejects a repeated /Pages node: {error}"
    );
}

#[test]
fn get_all_pages_traverses_a_direct_intermediate_pages_node() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut indirect_interior = Dictionary::new();
    indirect_interior.insert("Type", Object::Name(b"Pages".to_vec()));
    indirect_interior.insert(
        "Kids",
        Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]),
    );
    indirect_interior.insert("Count", Object::Integer(1));
    pdf.set_object(ObjectRef::new(11, 0), Object::Dictionary(indirect_interior));
    pdf.set_object(ObjectRef::new(12, 0), Object::Integer(12));

    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    let mut direct_leaf = Dictionary::new();
    direct_leaf.insert("Type", Object::Name(b"NotAPage".to_vec()));
    let mut direct_interior = Dictionary::new();
    direct_interior.insert("Type", Object::Name(b"NotPages".to_vec()));
    direct_interior.insert(
        "Kids",
        Object::Array(vec![
            Object::Dictionary(direct_leaf),
            Object::Integer(42),
            Object::Reference(ObjectRef::new(11, 0)),
            Object::Reference(ObjectRef::new(12, 0)),
            Object::Reference(ObjectRef::new(3, 0)),
        ]),
    );
    direct_interior.insert("Count", Object::Integer(3));
    let mut direct_outer = Dictionary::new();
    direct_outer.insert("Type", Object::Name(b"NotPages".to_vec()));
    direct_outer.insert(
        "Kids",
        Object::Array(vec![Object::Dictionary(direct_interior)]),
    );
    direct_outer.insert("Count", Object::Integer(3));
    root.insert(
        "Kids",
        Object::Array(vec![Object::Dictionary(direct_outer)]),
    );
    pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

    assert_eq!(
        PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap(),
        vec![
            ObjectRef::new(13, 0),
            ObjectRef::new(14, 0),
            ObjectRef::new(3, 0),
            ObjectRef::new(12, 0),
            ObjectRef::new(15, 0),
        ],
        "qpdf traverses direct nodes in place, promotes direct leaves, retains indirect scalar leaves, and clones duplicate leaves"
    );

    let Object::Dictionary(minted_leaf) = pdf.resolve(ObjectRef::new(13, 0)).unwrap() else {
        panic!("direct leaf must be made indirect");
    };
    assert_eq!(
        minted_leaf.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ])),
        "qpdf supplies the default MediaBox before retaining the repaired leaf"
    );
    assert_eq!(
        minted_leaf.get("Type"),
        Some(&Object::Name(b"Page".to_vec()))
    );

    assert_eq!(
        pdf.resolve(ObjectRef::new(14, 0)).unwrap(),
        Object::Integer(42),
        "qpdf promotes a direct scalar kid without changing its value"
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(12, 0)).unwrap(),
        Object::Integer(12),
        "qpdf includes an indirect scalar kid in the flattened page order"
    );
    let Object::Dictionary(cloned_leaf) = pdf.resolve(ObjectRef::new(15, 0)).unwrap() else {
        panic!("duplicate page must be copied as a dictionary");
    };
    assert_eq!(
        cloned_leaf.get("Type"),
        Some(&Object::Name(b"Page".to_vec())),
        "qpdf repairs the duplicate copy as a page"
    );

    let Object::Dictionary(root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must remain a dictionary");
    };
    let Some(Object::Array(kids)) = root.get("Kids") else {
        panic!("pages root must retain /Kids");
    };
    assert!(
        matches!(kids.first(), Some(Object::Dictionary(_))),
        "qpdf leaves a direct intermediate /Pages dictionary direct"
    );
}

#[test]
fn get_all_pages_resolves_an_indirect_kids_holder_under_a_direct_pages_node() {
    let bytes = pdf_with_direct_pages_node_kids_holder();
    if Command::new("qpdf").arg("--version").output().is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("kids-holder.pdf");
        fs::write(&input_path, &bytes).unwrap();
        let output = Command::new("qpdf")
            .arg("--show-pages")
            .arg(&input_path)
            .output()
            .unwrap();
        assert!(
            matches!(output.status.code(), Some(0 | 3)),
            "qpdf failed to inspect the indirect /Kids-holder fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("page 1: 7 0 R"),
            "qpdf must dereference the indirect /Kids holder before page classification: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    let mut pdf = open(bytes);
    assert_eq!(
        PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap(),
        vec![ObjectRef::new(7, 0)],
        "direct-node children must use the canonical resolver for indirect /Kids holders"
    );
}

#[test]
fn get_all_pages_rejects_an_overdeep_direct_pages_tree() {
    let mut pdf = open(build_n_page_pdf(1));
    let mut child = Object::Reference(ObjectRef::new(3, 0));
    for _ in 0..=flpdf::pages::DEFAULT_MAX_PAGE_TREE_DEPTH {
        let mut direct_interior = Dictionary::new();
        direct_interior.insert("Type", Object::Name(b"Pages".to_vec()));
        direct_interior.insert("Kids", Object::Array(vec![child]));
        child = Object::Dictionary(direct_interior);
    }
    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    root.insert("Kids", Object::Array(vec![child]));
    pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

    let error = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap_err();
    assert!(
        error.to_string().contains("depth exceeds"),
        "qpdf-compatible direct traversal must enforce the page-tree depth bound: {error}"
    );
}

#[test]
fn get_all_pages_ignores_a_direct_pages_node_with_non_array_kids() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    let mut direct_interior = Dictionary::new();
    direct_interior.insert("Type", Object::Name(b"Pages".to_vec()));
    direct_interior.insert("Kids", Object::Integer(42));
    root.insert(
        "Kids",
        Object::Array(vec![Object::Dictionary(direct_interior)]),
    );
    pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

    assert!(
        PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .unwrap()
            .is_empty(),
        "qpdf's getArrayNItems treats a non-array direct /Kids value as empty"
    );
}

#[test]
fn push_inherited_attributes_materializes_rotate_on_leaf() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut pages) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    pages.insert("Rotate", Object::Integer(90));
    pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(pages));

    PageDocumentHelper::new(&mut pdf)
        .push_inherited_attributes_to_pages()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert_eq!(page.get("Rotate"), Some(&Object::Integer(90)));
}

#[test]
fn push_inherited_attributes_traverses_a_direct_catalog_pages_root() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut pages) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    pages.insert("Rotate", Object::Integer(90));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Dictionary(pages));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .push_inherited_attributes_to_pages()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert_eq!(page.get("Rotate"), Some(&Object::Integer(90)));
    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    assert!(matches!(catalog.get("Pages"), Some(Object::Dictionary(_))));
}

#[test]
fn push_inherited_attributes_traverses_direct_pages_descendants() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    let mut child = Dictionary::new();
    child.insert("Type", Object::Name(b"Pages".to_vec()));
    child.insert(
        "Kids",
        Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]),
    );
    child.insert("Count", Object::Integer(1));
    child.insert("Rotate", Object::Integer(90));
    root.insert("Kids", Object::Array(vec![Object::Dictionary(child)]));
    root.insert("Count", Object::Integer(1));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Dictionary(root));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .push_inherited_attributes_to_pages()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    assert_eq!(page.get("Rotate"), Some(&Object::Integer(90)));
}

#[test]
fn push_inherited_attributes_ignores_non_dictionary_direct_kids() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    root.insert("Kids", Object::Array(vec![Object::Integer(42)]));
    root.insert("Count", Object::Integer(0));
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Dictionary(root));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .push_inherited_attributes_to_pages()
        .unwrap();
}

#[test]
fn remove_unreferenced_resources_prunes_unused_font_on_page() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"BT /F1 12 Tf ET".to_vec())),
    );
    let mut fonts = Dictionary::new();
    fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(fonts));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    let Object::Dictionary(resources) = page.get("Resources").cloned().unwrap() else {
        panic!("qpdf copies shared resources to the page");
    };
    let Object::Dictionary(fonts) = resources.get("Font").cloned().unwrap() else {
        panic!("font dictionary must remain");
    };
    assert!(fonts.get("F1").is_some());
    assert!(fonts.get("F2").is_none());
}

#[test]
fn remove_unreferenced_resources_preserves_non_dictionary_category() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"q Q".to_vec())),
    );
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Integer(42));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Dictionary(resources));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must remain a dictionary");
    };
    let Some(Object::Dictionary(resources)) = page.get("Resources") else {
        panic!("page resources must remain a dictionary");
    };
    assert_eq!(resources.get("Font"), Some(&Object::Integer(42)));
}

#[test]
fn helper_resource_pruning_accepts_pages_without_content_or_resources() {
    let mut no_content = open(build_n_page_pdf(1));
    PageDocumentHelper::new(&mut no_content)
        .remove_unreferenced_resources()
        .unwrap();

    let mut no_resources = open(build_n_page_pdf(1));
    no_resources.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"q Q".to_vec())),
    );
    let Object::Dictionary(mut page) = no_resources.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    no_resources.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));
    PageDocumentHelper::new(&mut no_resources)
        .remove_unreferenced_resources()
        .unwrap();
}

#[test]
fn helper_resource_pruning_keeps_page_resources_after_undecodable_form() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Good Do".to_vec())),
    );

    // qpdf skips non-Form entries but an undecodable Form makes the page's
    // resource set incomplete, so it retains the page resources unchanged.
    pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(Dictionary::new()));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"ignored".to_vec())),
    );
    let mut no_resources_dict = Dictionary::new();
    no_resources_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Stream(Stream::new(no_resources_dict, b"ignored".to_vec())),
    );
    let mut malformed_resources = Dictionary::new();
    malformed_resources.insert("Font", Object::Integer(42));
    let mut bad_form_xobjects = Dictionary::new();
    bad_form_xobjects.insert("Child", Object::Reference(ObjectRef::new(11, 0)));
    malformed_resources.insert("XObject", Object::Dictionary(bad_form_xobjects));
    let mut undecodable_form = Dictionary::new();
    undecodable_form.insert("Subtype", Object::Name(b"Form".to_vec()));
    undecodable_form.insert("Resources", Object::Dictionary(malformed_resources));
    undecodable_form.insert("Filter", Object::Name(b"UnknownFilter".to_vec()));
    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Stream(Stream::new(undecodable_form, b"ignored".to_vec())),
    );
    let mut valid_form = Dictionary::new();
    valid_form.insert("Subtype", Object::Name(b"Form".to_vec()));
    valid_form.insert("Resources", Object::Dictionary(Dictionary::new()));
    pdf.set_object(
        ObjectRef::new(10, 0),
        Object::Stream(Stream::new(valid_form, b"q Q".to_vec())),
    );
    let mut child_fonts = Dictionary::new();
    child_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    child_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut child_resources = Dictionary::new();
    child_resources.insert("Font", Object::Dictionary(child_fonts));
    let mut child_form = Dictionary::new();
    child_form.insert("Subtype", Object::Name(b"Form".to_vec()));
    child_form.insert("Resources", Object::Dictionary(child_resources));
    pdf.set_object(
        ObjectRef::new(11, 0),
        Object::Stream(Stream::new(child_form, b"BT /F1 12 Tf ET".to_vec())),
    );
    let mut xobjects = Dictionary::new();
    xobjects.insert("Direct", Object::Integer(0));
    xobjects.insert("Dictionary", Object::Reference(ObjectRef::new(6, 0)));
    xobjects.insert("NotForm", Object::Reference(ObjectRef::new(7, 0)));
    xobjects.insert("NoResources", Object::Reference(ObjectRef::new(8, 0)));
    xobjects.insert("Bad", Object::Reference(ObjectRef::new(9, 0)));
    xobjects.insert("Good", Object::Reference(ObjectRef::new(10, 0)));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Integer(99));
    resources.insert("XObject", Object::Dictionary(xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must remain a dictionary");
    };
    assert_eq!(
        page.get("Resources"),
        Some(&Object::Reference(ObjectRef::new(5, 0)))
    );
    let Object::Dictionary(resources) = pdf.resolve(ObjectRef::new(5, 0)).unwrap() else {
        panic!("page resources must remain an indirect dictionary");
    };
    assert_eq!(resources.get("Font"), Some(&Object::Integer(99)));
    let Some(Object::Dictionary(xobjects)) = resources.get("XObject") else {
        panic!("page must retain an XObject category");
    };
    assert_eq!(
        xobjects.iter().count(),
        6,
        "page-level pruning must not run after an undecodable Form"
    );
    assert!(xobjects.get("Good").is_some());
    assert!(xobjects.get("Bad").is_some());

    let Object::Stream(child) = pdf.resolve(ObjectRef::new(11, 0)).unwrap() else {
        panic!("child Form must remain a stream");
    };
    let Some(Object::Dictionary(child_resources)) = child.dict.get("Resources") else {
        panic!("child Form resources must be materialized");
    };
    let Some(Object::Dictionary(child_fonts)) = child_resources.get("Font") else {
        panic!("child Form must retain its font dictionary");
    };
    assert!(child_fonts.get("F1").is_some());
    assert!(child_fonts.get("F2").is_none());
}

#[test]
fn helper_resource_pruning_keeps_a_form_resources_after_a_content_parse_error() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Fm0 Do".to_vec())),
    );
    let mut form_fonts = Dictionary::new();
    form_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    form_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut form_resources = Dictionary::new();
    form_resources.insert("Font", Object::Dictionary(form_fonts));
    let mut form_xobjects = Dictionary::new();
    form_xobjects.insert("Child", Object::Reference(ObjectRef::new(7, 0)));
    form_resources.insert("XObject", Object::Dictionary(form_xobjects));
    let mut form_dict = Dictionary::new();
    form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.insert("Resources", Object::Dictionary(form_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(form_dict, b"<0g>".to_vec())),
    );
    let mut child_fonts = Dictionary::new();
    child_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    child_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut child_resources = Dictionary::new();
    child_resources.insert("Font", Object::Dictionary(child_fonts));
    let mut child_form = Dictionary::new();
    child_form.insert("Subtype", Object::Name(b"Form".to_vec()));
    child_form.insert("Resources", Object::Dictionary(child_resources));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Stream(Stream::new(child_form, b"BT /F1 12 Tf ET".to_vec())),
    );
    let mut xobjects = Dictionary::new();
    xobjects.insert("Fm0", Object::Reference(ObjectRef::new(6, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("XObject", Object::Dictionary(xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(form) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("form must remain a stream");
    };
    let Some(Object::Dictionary(resources)) = form.dict.get("Resources") else {
        panic!("malformed Form content must retain its resources");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("malformed Form content must retain its fonts");
    };
    assert!(fonts.get("F1").is_some());
    assert!(fonts.get("F2").is_some());

    let Object::Stream(child) = pdf.resolve(ObjectRef::new(7, 0)).unwrap() else {
        panic!("child Form must remain a stream");
    };
    let Some(Object::Dictionary(child_resources)) = child.dict.get("Resources") else {
        panic!("child Form resources must be materialized");
    };
    let Some(Object::Dictionary(child_fonts)) = child_resources.get("Font") else {
        panic!("child Form must retain its font dictionary");
    };
    assert!(child_fonts.get("F1").is_some());
    assert!(child_fonts.get("F2").is_none());
}

#[test]
fn helper_resource_pruning_keeps_form_and_page_resources_for_unresolved_form_name() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Fm0 Do".to_vec())),
    );

    let mut form_fonts = Dictionary::new();
    form_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    form_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut form_resources = Dictionary::new();
    form_resources.insert("Font", Object::Dictionary(form_fonts));
    let mut form_dict = Dictionary::new();
    form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.insert("Resources", Object::Dictionary(form_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(
            form_dict,
            b"BT /F1 12 Tf /Missing 12 Tf ET".to_vec(),
        )),
    );

    let mut page_fonts = Dictionary::new();
    page_fonts.insert("P1", Object::Dictionary(Dictionary::new()));
    page_fonts.insert("P2", Object::Dictionary(Dictionary::new()));
    let mut page_xobjects = Dictionary::new();
    page_xobjects.insert("Fm0", Object::Reference(ObjectRef::new(6, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("Font", Object::Dictionary(page_fonts));
    page_resources.insert("XObject", Object::Dictionary(page_xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    if Command::new("qpdf").arg("--version").output().is_ok() {
        let mut input = Vec::new();
        let options = WriterTestSettings {
            static_id: true,
            ..WriterTestSettings::default()
        };
        write_with_settings(&mut pdf, &mut input, &options).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("unresolved-form.pdf");
        let output_path = dir.path().join("qpdf-output.pdf");
        fs::write(&input_path, input).unwrap();
        let output = Command::new("qpdf")
            .arg("--remove-unreferenced-resources=yes")
            .arg(&input_path)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "qpdf failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = fs::read(output_path).unwrap();
        assert!(output.windows(3).any(|window| window == b"/F2"));
        assert!(output.windows(3).any(|window| window == b"/P2"));
    }

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(form) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("form must remain a stream");
    };
    let Some(Object::Dictionary(form_resources)) = form.dict.get("Resources") else {
        panic!("form must retain resources");
    };
    let Some(Object::Dictionary(form_fonts)) = form_resources.get("Font") else {
        panic!("form must retain font resources");
    };
    assert!(form_fonts.get("F1").is_some());
    assert!(form_fonts.get("F2").is_some());

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must remain a dictionary");
    };
    assert_eq!(
        page.get("Resources"),
        Some(&Object::Reference(ObjectRef::new(5, 0)))
    );
    let Object::Dictionary(page_resources) = pdf.resolve(ObjectRef::new(5, 0)).unwrap() else {
        panic!("page resources must remain an indirect dictionary");
    };
    let Some(Object::Dictionary(page_fonts)) = page_resources.get("Font") else {
        panic!("page must retain font resources");
    };
    assert!(page_fonts.get("P1").is_some());
    assert!(page_fonts.get("P2").is_some());
}

mod common;
#[allow(unused_imports)]
use common::{write_default, write_with_settings, WriterTestSettings};

#[test]
fn helper_resource_pruning_handles_form_local_resource_variants() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(
            Dictionary::new(),
            b"/Referenced Do /DirectOwner Do".to_vec(),
        )),
    );

    let mut nested = Dictionary::new();
    nested.insert("Subtype", Object::Name(b"Form".to_vec()));
    pdf.set_object(
        ObjectRef::new(10, 0),
        Object::Stream(Stream::new(nested, b"q Q".to_vec())),
    );
    let mut nested_xobjects = Dictionary::new();
    nested_xobjects.insert("First", Object::Reference(ObjectRef::new(10, 0)));
    nested_xobjects.insert("Second", Object::Reference(ObjectRef::new(10, 0)));
    pdf.set_object(ObjectRef::new(8, 0), Object::Dictionary(nested_xobjects));

    let mut referenced_resources = Dictionary::new();
    referenced_resources.insert("Font", Object::Integer(42));
    referenced_resources.insert("XObject", Object::Reference(ObjectRef::new(8, 0)));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Dictionary(referenced_resources),
    );
    let mut referenced_form = Dictionary::new();
    referenced_form.insert("Subtype", Object::Name(b"Form".to_vec()));
    referenced_form.insert("Resources", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(referenced_form, b"q Q".to_vec())),
    );

    let mut direct_child = Dictionary::new();
    direct_child.insert("Subtype", Object::Name(b"Form".to_vec()));
    let mut direct_child_xobjects = Dictionary::new();
    direct_child_xobjects.insert(
        "Child",
        Object::Stream(Stream::new(direct_child, b"q Q".to_vec())),
    );
    let mut direct_owner_resources = Dictionary::new();
    direct_owner_resources.insert("XObject", Object::Dictionary(direct_child_xobjects));
    let mut direct_owner = Dictionary::new();
    direct_owner.insert("Subtype", Object::Name(b"Form".to_vec()));
    direct_owner.insert("Resources", Object::Dictionary(direct_owner_resources));
    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Stream(Stream::new(direct_owner, b"/Child Do".to_vec())),
    );

    let mut page_xobjects = Dictionary::new();
    page_xobjects.insert("Referenced", Object::Reference(ObjectRef::new(6, 0)));
    page_xobjects.insert("DirectOwner", Object::Reference(ObjectRef::new(9, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("XObject", Object::Dictionary(page_xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(form) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("referenced Form must remain a stream");
    };
    let Some(Object::Dictionary(resources)) = form.dict.get("Resources") else {
        panic!("Form resources must be materialized");
    };
    assert_eq!(resources.get("Font"), Some(&Object::Integer(42)));
    let Some(Object::Dictionary(xobjects)) = resources.get("XObject") else {
        panic!("qpdf retains the category dictionary after pruning its entries");
    };
    assert_eq!(xobjects.iter().count(), 0);
}

#[test]
fn helper_prunes_unused_resources_inside_form_xobjects() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Fm0 Do".to_vec())),
    );

    let mut form_fonts = Dictionary::new();
    form_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    form_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut form_resources = Dictionary::new();
    form_resources.insert("Font", Object::Dictionary(form_fonts));
    let mut form_dict = Dictionary::new();
    form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.insert("Resources", Object::Dictionary(form_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(form_dict, b"BT /F1 12 Tf ET".to_vec())),
    );

    let mut xobjects = Dictionary::new();
    xobjects.insert("Fm0", Object::Reference(ObjectRef::new(6, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("XObject", Object::Dictionary(xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(form) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("form must remain a stream");
    };
    let Some(Object::Dictionary(resources)) = form.dict.get("Resources") else {
        panic!("form must retain a resource dictionary");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("form must retain a font dictionary");
    };
    assert!(fonts.get("F1").is_some());
    assert!(
        fonts.get("F2").is_none(),
        "qpdf prunes unreferenced Form resources too"
    );
}

#[test]
fn helper_prunes_declared_child_form_before_pruning_parent_xobjects() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Fm0 Do".to_vec())),
    );

    let mut child_fonts = Dictionary::new();
    child_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    child_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut child_resources = Dictionary::new();
    child_resources.insert("Font", Object::Dictionary(child_fonts));
    let mut child_dict = Dictionary::new();
    child_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    child_dict.insert("Resources", Object::Dictionary(child_resources));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Stream(Stream::new(child_dict, b"BT /F1 12 Tf ET".to_vec())),
    );

    let mut parent_xobjects = Dictionary::new();
    parent_xobjects.insert("Child", Object::Reference(ObjectRef::new(7, 0)));
    let mut parent_resources = Dictionary::new();
    parent_resources.insert("XObject", Object::Dictionary(parent_xobjects));
    let mut parent_dict = Dictionary::new();
    parent_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    parent_dict.insert("Resources", Object::Dictionary(parent_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(parent_dict, b"q Q".to_vec())),
    );

    let mut page_xobjects = Dictionary::new();
    page_xobjects.insert("Fm0", Object::Reference(ObjectRef::new(6, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("XObject", Object::Dictionary(page_xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(child) = pdf.resolve(ObjectRef::new(7, 0)).unwrap() else {
        panic!("declared child Form must remain a stream");
    };
    let Some(Object::Dictionary(resources)) = child.dict.get("Resources") else {
        panic!("child Form must retain a resource dictionary");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("child Form must retain a font dictionary");
    };
    assert!(fonts.get("F1").is_some());
    assert!(
        fonts.get("F2").is_none(),
        "qpdf visits declared child Forms even when their parent does not invoke them"
    );
}

#[test]
fn helper_prunes_unused_resources_inside_a_holder_chained_form_xobject() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Fm0 Do".to_vec())),
    );

    let mut fonts = Dictionary::new();
    fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut form_resources = Dictionary::new();
    form_resources.insert("Font", Object::Dictionary(fonts));
    let mut form_dict = Dictionary::new();
    form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.insert("Resources", Object::Dictionary(form_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(form_dict, b"BT /F1 12 Tf ET".to_vec())),
    );
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );

    let mut xobjects = Dictionary::new();
    xobjects.insert("Fm0", Object::Reference(ObjectRef::new(7, 0)));
    let mut resources = Dictionary::new();
    resources.insert("XObject", Object::Dictionary(xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(form) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("terminal Form must remain a stream");
    };
    let Some(Object::Dictionary(resources)) = form.dict.get("Resources") else {
        panic!("Form must retain resources");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("Form must retain font resources");
    };
    assert!(fonts.get("F1").is_some());
    assert!(fonts.get("F2").is_none());
}

#[test]
fn helper_prunes_parent_form_resources_not_directly_used_by_resource_less_nested_form() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Outer Do".to_vec())),
    );
    let mut child_dict = Dictionary::new();
    child_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Stream(Stream::new(child_dict, b"BT /F1 12 Tf ET".to_vec())),
    );

    let mut parent_fonts = Dictionary::new();
    parent_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    parent_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut parent_xobjects = Dictionary::new();
    parent_xobjects.insert("Child", Object::Reference(ObjectRef::new(7, 0)));
    let mut parent_resources = Dictionary::new();
    parent_resources.insert("Font", Object::Dictionary(parent_fonts));
    parent_resources.insert("XObject", Object::Dictionary(parent_xobjects));
    let mut parent_dict = Dictionary::new();
    parent_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    parent_dict.insert("Resources", Object::Dictionary(parent_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(parent_dict, b"/Child Do".to_vec())),
    );

    let mut page_xobjects = Dictionary::new();
    page_xobjects.insert("Outer", Object::Reference(ObjectRef::new(6, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("XObject", Object::Dictionary(page_xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(parent) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("parent Form must remain a stream");
    };
    let Some(Object::Dictionary(resources)) = parent.dict.get("Resources") else {
        panic!("parent Form must retain resources");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("parent Form must retain font resources");
    };
    assert!(fonts.get("F1").is_none());
    assert!(fonts.get("F2").is_none());
}

#[test]
fn helper_keeps_nested_form_resource_scopes_isolated() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Outer Do".to_vec())),
    );

    let mut child_dict = Dictionary::new();
    child_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Stream(Stream::new(child_dict, b"BT /F1 12 Tf ET".to_vec())),
    );

    let mut inner_fonts = Dictionary::new();
    inner_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    inner_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut inner_xobjects = Dictionary::new();
    inner_xobjects.insert("Child", Object::Reference(ObjectRef::new(8, 0)));
    let mut inner_resources = Dictionary::new();
    inner_resources.insert("Font", Object::Dictionary(inner_fonts));
    inner_resources.insert("XObject", Object::Dictionary(inner_xobjects));
    let mut inner_dict = Dictionary::new();
    inner_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    inner_dict.insert("Resources", Object::Dictionary(inner_resources));
    pdf.set_object(
        ObjectRef::new(7, 0),
        Object::Stream(Stream::new(inner_dict, b"/Child Do".to_vec())),
    );

    let mut outer_fonts = Dictionary::new();
    outer_fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    outer_fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut outer_xobjects = Dictionary::new();
    outer_xobjects.insert("Inner", Object::Reference(ObjectRef::new(7, 0)));
    let mut outer_resources = Dictionary::new();
    outer_resources.insert("Font", Object::Dictionary(outer_fonts));
    outer_resources.insert("XObject", Object::Dictionary(outer_xobjects));
    let mut outer_dict = Dictionary::new();
    outer_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    outer_dict.insert("Resources", Object::Dictionary(outer_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(outer_dict, b"/Inner Do".to_vec())),
    );

    let mut page_xobjects = Dictionary::new();
    page_xobjects.insert("Outer", Object::Reference(ObjectRef::new(6, 0)));
    let mut page_resources = Dictionary::new();
    page_resources.insert("XObject", Object::Dictionary(page_xobjects));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(page_resources));
    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    page.insert("Resources", Object::Reference(ObjectRef::new(5, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Stream(outer) = pdf.resolve(ObjectRef::new(6, 0)).unwrap() else {
        panic!("outer Form must remain a stream");
    };
    let Some(Object::Dictionary(outer_resources)) = outer.dict.get("Resources") else {
        panic!("outer Form must retain resources");
    };
    let Some(Object::Dictionary(outer_fonts)) = outer_resources.get("Font") else {
        panic!("outer Form must retain font resources");
    };
    assert!(outer_fonts.get("F1").is_none());
    assert!(outer_fonts.get("F2").is_none());

    let Object::Stream(inner) = pdf.resolve(ObjectRef::new(7, 0)).unwrap() else {
        panic!("inner Form must remain a stream");
    };
    let Some(Object::Dictionary(inner_resources)) = inner.dict.get("Resources") else {
        panic!("inner Form must retain resources");
    };
    let Some(Object::Dictionary(inner_fonts)) = inner_resources.get("Font") else {
        panic!("inner Form must retain font resources");
    };
    assert!(inner_fonts.get("F1").is_none());
    assert!(inner_fonts.get("F2").is_none());
}

// ---------------------------------------------------------------------------
// removePage()
// ---------------------------------------------------------------------------

#[test]
fn remove_page_allows_an_empty_document() {
    let mut pdf = open(build_n_page_pdf(1));

    PageDocumentHelper::new(&mut pdf)
        .remove_page(ObjectRef::new(3, 0))
        .unwrap();

    assert!(PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .unwrap()
        .is_empty());
    let Object::Dictionary(root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    assert_eq!(root.get("Kids"), Some(&Object::Array(Vec::new())));
    assert_eq!(root.get("Count"), Some(&Object::Integer(0)));
}

#[test]
fn remove_page_allows_an_empty_direct_catalog_pages_root() {
    let mut pdf = open(build_n_page_pdf(1));
    let Object::Dictionary(pages) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", Object::Dictionary(pages));
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    PageDocumentHelper::new(&mut pdf)
        .remove_page(ObjectRef::new(3, 0))
        .unwrap();

    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    let Some(Object::Dictionary(pages)) = catalog.get("Pages") else {
        panic!("catalog /Pages must remain direct");
    };
    assert_eq!(pages.get("Kids"), Some(&Object::Array(Vec::new())));
    assert_eq!(pages.get("Count"), Some(&Object::Integer(0)));
}

#[test]
fn remove_page_removes_the_selected_page() {
    let mut pdf = open(build_n_page_pdf(3));
    PageDocumentHelper::new(&mut pdf)
        .remove_page(ObjectRef::new(3, 0))
        .unwrap();
    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 2);
    // After removing page 1 (obj 3), remaining pages are obj 4 and 5.
    assert_eq!(pages[0], ObjectRef::new(4, 0));
    assert_eq!(pages[1], ObjectRef::new(5, 0));
}

#[test]
fn remove_page_preserves_direct_catalog_pages_root() {
    let mut pdf = open(build_n_page_pdf(3));
    make_catalog_pages_root_direct(&mut pdf);

    PageDocumentHelper::new(&mut pdf)
        .remove_page(ObjectRef::new(4, 0))
        .unwrap();

    assert_direct_catalog_pages_root(&mut pdf, 2);
}

#[test]
fn remove_page_rejects_a_non_member() {
    let mut pdf = open(build_n_page_pdf(2));
    let err = PageDocumentHelper::new(&mut pdf)
        .remove_page(ObjectRef::new(99, 0))
        .unwrap_err();
    assert!(
        matches!(err, flpdf::Error::Missing(_)),
        "expected Missing, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// addPage() / addPageAt()
// ---------------------------------------------------------------------------

#[test]
fn add_page_first_prepends_page() {
    let mut pdf = open(build_n_page_pdf(3));

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::existing(ObjectRef::new(5, 0)), true)
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 4);
    assert_eq!(
        pages[0],
        ObjectRef::new(6, 0),
        "qpdf shallow-copies a page already present in the target tree"
    );
}

#[test]
fn add_page_last_appends_page() {
    let mut pdf = open(build_n_page_pdf(3));

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::existing(ObjectRef::new(3, 0)), false)
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[3], ObjectRef::new(6, 0));
}

#[test]
fn add_page_preserves_direct_catalog_pages_root() {
    let mut pdf = open(build_n_page_pdf(2));
    make_catalog_pages_root_direct(&mut pdf);

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::existing(ObjectRef::new(3, 0)), false)
        .unwrap();

    assert_direct_catalog_pages_root(&mut pdf, 3);
}

#[test]
fn add_page_materializes_attributes_from_a_direct_parent() {
    let mut pdf = open(build_n_page_pdf(2));
    let mut fonts = Dictionary::new();
    fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(fonts));

    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    root.insert("Resources", Object::Dictionary(resources.clone()));
    root.insert("Rotate", Object::Integer(90));
    root.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(300),
        ]),
    );
    root.insert(
        "CropBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(150),
        ]),
    );
    let direct_parent = Object::Dictionary(root.clone());
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", direct_parent.clone());
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    for page_ref in [ObjectRef::new(3, 0), ObjectRef::new(4, 0)] {
        let Object::Dictionary(mut page) = pdf.resolve(page_ref).unwrap() else {
            panic!("page must be a dictionary");
        };
        page.remove("MediaBox");
        page.insert("Parent", direct_parent.clone());
        pdf.set_object(page_ref, Object::Dictionary(page));
    }

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::existing(ObjectRef::new(3, 0)), false)
        .unwrap();

    let expected_media_box = Object::Array(vec![
        Object::Integer(0),
        Object::Integer(0),
        Object::Integer(200),
        Object::Integer(300),
    ]);
    let expected_crop_box = Object::Array(vec![
        Object::Integer(0),
        Object::Integer(0),
        Object::Integer(100),
        Object::Integer(150),
    ]);
    for page_ref in PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap() {
        let Object::Dictionary(page) = pdf.resolve(page_ref).unwrap() else {
            panic!("page must be a dictionary");
        };
        assert_eq!(
            page.get("Resources"),
            Some(&Object::Dictionary(resources.clone()))
        );
        assert_eq!(page.get("Rotate"), Some(&Object::Integer(90)));
        assert_eq!(page.get("MediaBox"), Some(&expected_media_box));
        assert_eq!(page.get("CropBox"), Some(&expected_crop_box));
    }
}

#[test]
fn helper_prunes_resources_inherited_from_a_direct_parent() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"BT /F1 12 Tf ET".to_vec())),
    );

    let mut fonts = Dictionary::new();
    fonts.insert("F1", Object::Dictionary(Dictionary::new()));
    fonts.insert("F2", Object::Dictionary(Dictionary::new()));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(fonts));

    let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(2, 0)).unwrap() else {
        panic!("pages root must be a dictionary");
    };
    root.insert("Resources", Object::Dictionary(resources));
    let direct_parent = Object::Dictionary(root.clone());
    let Object::Dictionary(mut catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    catalog.insert("Pages", direct_parent.clone());
    pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

    let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    page.insert("Parent", direct_parent);
    page.insert("Contents", Object::Reference(ObjectRef::new(4, 0)));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

    PageDocumentHelper::new(&mut pdf)
        .remove_unreferenced_resources()
        .unwrap();

    let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("page must be a dictionary");
    };
    let Some(Object::Dictionary(resources)) = page.get("Resources") else {
        panic!("inherited resources must be materialized onto the page");
    };
    let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
        panic!("page resources must retain a font dictionary");
    };
    assert!(fonts.get("F1").is_some());
    assert!(fonts.get("F2").is_none());
}

#[test]
fn add_page_at_after_reference_inserts_after_that_page() {
    let mut pdf = open(build_n_page_pdf(3));

    PageDocumentHelper::new(&mut pdf)
        .add_page_at(
            PageInput::existing(ObjectRef::new(5, 0)),
            false,
            ObjectRef::new(3, 0),
        )
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[0], ObjectRef::new(3, 0));
    assert_eq!(
        pages[1],
        ObjectRef::new(6, 0),
        "qpdf shallow-copies a page already present in the target tree"
    );
}

#[test]
fn add_page_at_rejects_reference_outside_document() {
    let mut pdf = open(build_n_page_pdf(3));

    let error = PageDocumentHelper::new(&mut pdf)
        .add_page_at(
            PageInput::existing(ObjectRef::new(3, 0)),
            true,
            ObjectRef::new(99, 0),
        )
        .unwrap_err();

    assert!(matches!(error, flpdf::Error::Missing(_)), "got {error:?}");
    assert_eq!(
        PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .unwrap()
            .len(),
        3
    );
}
