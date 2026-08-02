//! Integration tests for [`flpdf::PageDocumentHelper`].
//!
//! All tests build in-memory PDFs without touching the filesystem.  They use
//! `PageDocumentHelper` for all page-list access rather than calling
//! `pages::page_refs` or touching raw [`Object`] values directly.

use flpdf::{Dictionary, FlattenMode, Object, ObjectRef, PageDocumentHelper, Pdf, Stream};
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

// ---------------------------------------------------------------------------
// getAllPages() / pushInheritedAttributesToPages()
// ---------------------------------------------------------------------------

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
        .flatten_annotations(FlattenMode::All)
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
            ObjectRef::new(3, 0),
            ObjectRef::new(14, 0),
        ],
        "qpdf traverses direct nodes in place, mints direct leaves, and clones duplicate leaves"
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
fn helper_resource_pruning_skips_non_dictionary_categories_and_malformed_forms() {
    let mut pdf = open(build_n_page_pdf(1));
    pdf.set_object(
        ObjectRef::new(4, 0),
        Object::Stream(Stream::new(Dictionary::new(), b"/Good Do".to_vec())),
    );

    // The pre-pass sees every declared XObject. None of these malformed entries
    // is a usable Form; qpdf skips them without preventing page-level pruning.
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
    let Some(Object::Dictionary(resources)) = page.get("Resources") else {
        panic!("page must retain a direct resource dictionary");
    };
    assert_eq!(resources.get("Font"), Some(&Object::Integer(99)));
    let Some(Object::Dictionary(xobjects)) = resources.get("XObject") else {
        panic!("page must retain an XObject category");
    };
    assert_eq!(
        xobjects.iter().count(),
        1,
        "only the invoked XObject remains"
    );
    assert!(xobjects.get("Good").is_some());
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
    let mut form_dict = Dictionary::new();
    form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.insert("Resources", Object::Dictionary(form_resources));
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Stream(Stream::new(form_dict, b"<0g>".to_vec())),
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
}

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
        .add_page(ObjectRef::new(5, 0), true)
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[0], ObjectRef::new(5, 0));
}

#[test]
fn add_page_last_appends_page() {
    let mut pdf = open(build_n_page_pdf(3));

    PageDocumentHelper::new(&mut pdf)
        .add_page(ObjectRef::new(3, 0), false)
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
        .add_page(ObjectRef::new(3, 0), false)
        .unwrap();

    assert_direct_catalog_pages_root(&mut pdf, 3);
}

#[test]
fn add_page_at_after_reference_inserts_after_that_page() {
    let mut pdf = open(build_n_page_pdf(3));

    PageDocumentHelper::new(&mut pdf)
        .add_page_at(ObjectRef::new(5, 0), false, ObjectRef::new(3, 0))
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[0], ObjectRef::new(3, 0));
    assert_eq!(pages[1], ObjectRef::new(5, 0));
}

#[test]
fn add_page_at_rejects_reference_outside_document() {
    let mut pdf = open(build_n_page_pdf(3));

    let error = PageDocumentHelper::new(&mut pdf)
        .add_page_at(ObjectRef::new(3, 0), true, ObjectRef::new(99, 0))
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
