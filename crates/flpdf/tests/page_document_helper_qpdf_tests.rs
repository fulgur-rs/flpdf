use flpdf::{
    Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageInput, Pdf, PdfOpenOptions,
    QpdfErrorCode, QpdfObjGen,
};
use std::io::Cursor;

mod common;
use common::build_pdf;

fn one_page_nested_tree_with_unknown_key() -> Vec<u8> {
    let objects: [&[u8]; 4] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /UserUnit 2 >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn removing_the_last_page_flattens_intermediate_pages_with_qpdf_warnings() {
    let mut pdf = Pdf::open(Cursor::new(one_page_nested_tree_with_unknown_key())).unwrap();
    PageDocumentHelper::new(&mut pdf)
        .remove_page(ObjectRef::new(4, 0))
        .expect("qpdf-style final page removal");

    assert!(
        pdf.repair_diagnostics()
            .entries()
            .iter()
            .any(
                |diagnostic| String::from_utf8_lossy(diagnostic.get_message_detail())
                    .contains("Unknown key /UserUnit")
            ),
        "flattening the intermediate /Pages node must retain qpdf's warning"
    );
}

#[test]
fn removing_an_already_removed_page_preserves_qpdf_exception_context() {
    let mut pdf = Pdf::open_with_options(
        Cursor::new(one_page_nested_tree_with_unknown_key()),
        PdfOpenOptions {
            description: b"page_api_1.pdf".to_vec(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();
    let page = ObjectRef::new(4, 0);

    PageDocumentHelper::new(&mut pdf)
        .remove_page(page)
        .expect("the first removal should succeed");
    let error = PageDocumentHelper::new(&mut pdf)
        .remove_page(page)
        .expect_err("the second removal should raise the page exception");

    assert_eq!(
        error.to_string(),
        "page_api_1.pdf (page object: object 4 0): page object not referenced in /Pages tree"
    );
}

#[test]
fn page_tree_cycle_description_follows_the_last_parsed_kid_not_the_first() {
    // A single-page cycle cannot tell the two candidate semantics apart: the
    // last parsed kid and the first kid are the same object. qpdf reports
    // `m->last_object_description`, which `setLastObjectDescription` updates
    // only while parsing (`QPDF.cc:1298-1310`), so a second page must move the
    // description to `object 4 0`. Real qpdf 11.9.0 prints
    // `ERROR: pages-loop-two.pdf (object 4 0): Loop detected in /Pages
    // structure (getAllPages)` for this shape.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (
                2,
                "<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R 2 0 R] >>".to_owned(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            description: b"pages-loop-two.pdf".to_vec(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("cyclic page-tree fixture should open");

    let error = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect_err("a repeated /Pages node must raise qpdf_e_pages");
    let Error::QpdfExc(exception) = error else {
        panic!("expected structured QPDFExc, got {error:?}");
    };

    assert_eq!(exception.get_object(), b"object 4 0");
    assert_eq!(
        exception.what_bytes(),
        b"pages-loop-two.pdf (object 4 0): Loop detected in /Pages structure (getAllPages)"
    );
}

#[test]
fn page_tree_cycle_raises_qpdf_pages_exception_with_last_object_description() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (
                2,
                "<< /Type /Pages /Count 1 /Kids [3 0 R 2 0 R] >>".to_owned(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            description: b"pages-loop.pdf".to_vec(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("cyclic page-tree fixture should open");

    let error = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect_err("a repeated /Pages node must raise qpdf_e_pages");
    let Error::QpdfExc(exception) = error else {
        panic!("expected structured QPDFExc, got {error:?}");
    };

    assert_eq!(exception.get_error_code(), QpdfErrorCode::Pages);
    assert_eq!(exception.get_filename(), b"pages-loop.pdf");
    assert_eq!(exception.get_object(), b"object 3 0");
    assert_eq!(exception.get_file_position(), 0);
    assert_eq!(
        exception.get_message_detail(),
        b"Loop detected in /Pages structure (getAllPages)"
    );
    assert_eq!(
        exception.what_bytes(),
        b"pages-loop.pdf (object 3 0): Loop detected in /Pages structure (getAllPages)"
    );
}

#[test]
fn raw_generation_page_tree_leaf_is_returned_as_a_qpdf_page_handle() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (2, "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("page fixture should open");
    let raw_ref = ObjectRef::new(17, 65_535);
    pdf.replace_object(
        raw_ref,
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (
                b"/MediaBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(612),
                    ObjectHandle::integer(792),
                ]),
            ),
        ]),
    )
    .expect("install raw-generation page");
    let pages = pdf.get_object_handle(ObjectRef::new(2, 0));
    pages
        .replace_key(
            b"/Kids",
            ObjectHandle::array(vec![pdf.get_object_handle(raw_ref)]),
        )
        .expect("attach raw-generation page");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("raw-generation page should remain in qpdf's page list");
    assert_eq!(pages.len(), 1);
    let raw_page = &pages[0];
    assert_eq!(raw_page.get_obj_gen(), QpdfObjGen::new(17, 65_535));
    assert!(raw_page.is_indirect());
    assert_eq!(raw_page.unparse(), b"17 65535 R");
    assert_eq!(raw_page.object_ref(), None);
}

#[test]
fn add_page_at_inserts_a_direct_page_before_its_reference_page() {
    let mut pdf = Pdf::empty().expect("empty document should be constructible");
    let first_page = ObjectHandle::dictionary(vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (
            b"/MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(612),
                ObjectHandle::integer(792),
            ]),
        ),
    ]);
    let first_page_ref = PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::direct(first_page), false)
        .expect("initial page should be added")
        .new_kids[0];

    let inserted_page = ObjectHandle::dictionary(vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (
            b"/MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(612),
                ObjectHandle::integer(792),
            ]),
        ),
    ]);
    let inserted_page_ref = PageDocumentHelper::new(&mut pdf)
        .add_page_at(PageInput::direct(inserted_page), true, first_page_ref)
        .expect("page should be inserted before the reference page")
        .new_kids[0];

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("page list should be readable");
    assert_eq!(
        pages
            .iter()
            .map(ObjectHandle::object_ref)
            .collect::<Vec<_>>(),
        vec![Some(inserted_page_ref), Some(first_page_ref)]
    );
}
