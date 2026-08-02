//! Integration tests for [`flpdf::PageDocumentHelper`].
//!
//! All tests build in-memory PDFs without touching the filesystem.  They use
//! `PageDocumentHelper` for all page-list access rather than calling
//! `pages::page_refs` or touching raw [`Object`] values directly.

use flpdf::{
    write_pdf, Dictionary, FlattenMode, Object, ObjectRef, PageDocumentHelper, PageRange, Pdf,
    RotateMode, Stream,
};
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
// pages() / iter() / get()
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
fn pages_forwards_to_repair_aware_enumeration() {
    let mut pdf = open(pdf_with_catalog_pages_pointing_to_leaf());

    assert_eq!(
        PageDocumentHelper::new(&mut pdf).pages().unwrap(),
        vec![ObjectRef::new(3, 0)]
    );
    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0)))
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
fn pages_returns_correct_count() {
    let mut pdf = open(build_n_page_pdf(3));
    let mut helper = PageDocumentHelper::new(&mut pdf);
    let pages = helper.pages().unwrap();
    assert_eq!(pages.len(), 3);
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
fn rotate_uses_repair_aware_page_list() {
    let mut pdf = open(pdf_with_catalog_pages_pointing_to_leaf());
    let range = PageRange::parse("").unwrap();

    PageDocumentHelper::new(&mut pdf)
        .rotate(&range, 90, RotateMode::Add)
        .unwrap();

    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0)))
    );
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
fn insert_uses_repair_aware_page_list() {
    let mut pdf = open(pdf_with_catalog_pages_pointing_to_leaf());

    PageDocumentHelper::new(&mut pdf)
        .insert(1, ObjectRef::new(3, 0))
        .unwrap();

    let Object::Dictionary(catalog) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0)))
    );
    assert_eq!(
        PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .unwrap()
            .len(),
        2
    );
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
