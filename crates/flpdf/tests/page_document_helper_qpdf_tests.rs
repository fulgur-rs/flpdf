use flpdf::{
    Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageInput, Pdf, PdfOpenOptions,
    QpdfErrorCode, QpdfObjGen,
};
use std::io::Cursor;

mod common;
use common::build_pdf;

fn annotation_flags_pdf() -> Vec<u8> {
    let annotation = |flags: i32, appearance: u32| {
        format!(
            "<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] /F {flags} /AP << /N {appearance} 0 R >> >>"
        )
    };
    let appearance =
        "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >> /Length 4 >>\nstream\nq Q\nendstream";
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Annots [4 0 R 6 0 R 8 0 R] >>".to_owned(),
        ),
        (4, annotation(0, 5)),
        (5, appearance.to_owned()),
        (6, annotation(1, 7)),
        (7, appearance.to_owned()),
        (8, annotation(2, 9)),
        (9, appearance.to_owned()),
    ];
    build_pdf(&objects, 1)
}

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
fn page_tree_root_correction_warning_uses_qpdf_damaged_pdf_code() {
    let mut pdf = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/root-pages-points-into-tree.pdf").to_vec(),
    )
    .expect("root-points-into-tree fixture should open");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("qpdf corrects the page-tree root");
    assert_eq!(pages.len(), 1);
    let diagnostics = pdf.repair_diagnostics();
    let warning = diagnostics
        .entries()
        .iter()
        .find(|warning| {
            warning.get_message_detail()
                == b"document page tree root (root -> /Pages) doesn't point to the root of the page tree; attempting to correct"
        })
        .expect("qpdf root-correction warning");
    assert_eq!(warning.get_error_code(), QpdfErrorCode::DamagedPdf);
}

#[test]
fn removing_the_last_page_flattens_intermediate_pages_with_qpdf_warnings() {
    let mut pdf = Pdf::open(Cursor::new(one_page_nested_tree_with_unknown_key())).unwrap();
    let page = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("enumerate nested page")
        .remove(0);
    PageDocumentHelper::new(&mut pdf)
        .remove_page(page)
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
    let page = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("enumerate nested page")
        .remove(0);

    PageDocumentHelper::new(&mut pdf)
        .remove_page(page.clone())
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
fn removing_a_raw_page_handle_outside_the_tree_preserves_its_identity() {
    let mut pdf = Pdf::open_with_options(
        Cursor::new(one_page_nested_tree_with_unknown_key()),
        PdfOpenOptions {
            description: b"page_api_raw.pdf".to_vec(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("nested one-page PDF should parse");
    pdf.replace_object(
        ObjectRef::new(17, 65_535),
        ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Page".to_vec()),
        )]),
    )
    .expect("install a detached raw-generation page");
    let raw_page = pdf.get_object_handle_by_raw_identity(17, 65_535);

    let error = PageDocumentHelper::new(&mut pdf)
        .remove_page(raw_page)
        .expect_err("a page outside the tree must produce qpdf's page error");

    assert!(matches!(error, Error::QpdfExc(_)));
    assert_eq!(
        error.to_string(),
        "page_api_raw.pdf (page object: object 17 65535): page object not referenced in /Pages tree"
    );
}

#[test]
fn remove_page_flattens_with_qpdf_unsigned_count_warnings() {
    for (count, warning) in [
        (
            "-1",
            "unsigned value request for negative number; returning 0",
        ),
        (
            "/bad",
            "operation for integer attempted on object of type name: returning 0",
        ),
    ] {
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (2, format!("<< /Type /Pages /Kids [] /Count {count} >>")),
            ],
            1,
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("empty page tree should parse");

        let error = PageDocumentHelper::new(&mut pdf)
            .remove_page(ObjectHandle::dictionary(Vec::new()))
            .expect_err("the direct page is not a member of the empty page tree");

        assert!(matches!(error, Error::QpdfExc(_)));
        assert!(
            pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
                String::from_utf8_lossy(diagnostic.get_message_detail()).contains(warning)
            }),
            "qpdf /Count conversion warning should be preserved for /Count {count}"
        );
    }
}

#[test]
fn remove_page_rejects_a_mismatched_count_after_flattening() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 2 >>".to_owned()),
            (3, "<< /Type /Page /Parent 2 0 R >>".to_owned()),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("one-page tree should parse");
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));

    let error = PageDocumentHelper::new(&mut pdf)
        .remove_page(page)
        .expect_err("flattening must reject an incorrect /Count");

    assert!(matches!(
        error,
        Error::Internal(message) if message == "/Count is wrong after flattening pages tree"
    ));
}

#[test]
fn remove_page_accepts_a_raw_generation_page_handle() {
    let mut pdf = Pdf::open(Cursor::new(one_page_nested_tree_with_unknown_key()))
        .expect("nested one-page PDF should parse");
    let raw_page_ref = ObjectRef::new(17, 65_535);
    let raw_page = ObjectHandle::dictionary(vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (
            b"/Parent".to_vec(),
            pdf.get_object_handle(ObjectRef::new(3, 0)),
        ),
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
    pdf.replace_object(raw_page_ref, raw_page)
        .expect("install raw-generation page");
    let raw_page = pdf.get_object_handle_by_raw_identity(17, 65_535);
    let intermediate = pdf.get_object_handle(ObjectRef::new(3, 0));
    intermediate
        .replace_key(b"/Kids", ObjectHandle::array(vec![raw_page.clone()]))
        .expect("attach raw page to nested page tree");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("qpdf page-list route retains raw identity");
    assert_eq!(pages[0].get_obj_gen(), QpdfObjGen::new(17, 65_535));

    PageDocumentHelper::new(&mut pdf)
        .remove_page(raw_page.clone())
        .expect("raw-generation page removal should match qpdf");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("enumerate the empty page tree");
    assert!(pages.is_empty());
    let root = pdf.get_object_handle(ObjectRef::new(2, 0));
    assert_eq!(
        root.try_get_key(b"/Count")
            .expect("read /Count")
            .as_integer(),
        Some(0)
    );
    assert_eq!(
        root.try_get_key(b"/Kids")
            .expect("read /Kids")
            .try_get_array_n_items()
            .expect("read /Kids item count"),
        0
    );
}

#[test]
fn repeated_page_removal_reuses_the_flattened_kids_array() {
    let bytes = build_pdf(&[
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 2 >>".to_owned()),
        (
            3,
            "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 /MediaBox [0 0 612 792] >>".to_owned(),
        ),
        (4, "<< /Type /Page /Parent 3 0 R >>".to_owned()),
        (5, "<< /Type /Page /Parent 3 0 R >>".to_owned()),
    ], 1);
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("two-page nested tree should parse");
    let raw_page_ref = ObjectRef::new(17, 65_535);
    let intermediate = pdf.get_object_handle(ObjectRef::new(3, 0));
    pdf.replace_object(
        raw_page_ref,
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"/Parent".to_vec(), intermediate.clone()),
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
    .expect("install raw page");
    let raw_page = pdf.get_object_handle_by_raw_identity(17, 65_535);
    intermediate
        .replace_key(
            b"/Kids",
            ObjectHandle::array(vec![
                raw_page.clone(),
                pdf.get_object_handle(ObjectRef::new(5, 0)),
            ]),
        )
        .expect("replace first page with the raw page");

    PageDocumentHelper::new(&mut pdf)
        .remove_page(raw_page)
        .expect("remove raw first page");
    let pages_root = pdf.get_object_handle(ObjectRef::new(2, 0));
    let kids = pages_root
        .try_get_key(b"/Kids")
        .expect("read flattened /Kids");
    assert_eq!(kids.try_get_array_n_items().unwrap(), 1);

    let last_page = pdf.get_object_handle(ObjectRef::new(5, 0));
    PageDocumentHelper::new(&mut pdf)
        .remove_page(last_page)
        .expect("remove the last remaining page");
    let current_kids = pages_root.try_get_key(b"/Kids").expect("read final /Kids");
    assert!(
        kids.is_same_object_as(&current_kids),
        "qpdf erases the same flattened /Kids array on subsequent removals"
    );
    assert_eq!(kids.try_get_array_n_items().unwrap(), 0);
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
    assert_eq!(raw_page.unparse().unwrap(), b"17 65535 R");
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
    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::target(first_page), false)
        .expect("initial page should be added");
    let first_page = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("read initial page")
        .into_iter()
        .next()
        .expect("initial page should be present");
    let first_page_ref = first_page.object_ref().expect("inserted page is indirect");

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
    PageDocumentHelper::new(&mut pdf)
        .add_page_at(PageInput::target(inserted_page), true, first_page)
        .expect("page should be inserted before the reference page");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("page list should be readable");
    let inserted_page_ref = pages[0].object_ref().expect("inserted page is indirect");
    assert_eq!(
        pages
            .iter()
            .map(ObjectHandle::object_ref)
            .collect::<Vec<_>>(),
        vec![Some(inserted_page_ref), Some(first_page_ref)]
    );
}

fn page_dictionary(parent: Option<ObjectHandle>) -> ObjectHandle {
    let mut entries = vec![
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
    ];
    if let Some(parent) = parent {
        entries.push((b"/Parent".to_vec(), parent));
    }
    ObjectHandle::dictionary(entries)
}

#[test]
fn add_page_accepts_a_raw_generation_handle_and_duplicates_by_raw_identity() {
    let mut pdf = Pdf::empty().expect("empty document should be constructible");
    let raw_page_ref = ObjectRef::new(17, 65_535);
    pdf.replace_object(raw_page_ref, page_dictionary(None))
        .expect("install raw page in the target object cache");
    let raw_page = pdf.get_object_handle_by_raw_identity(17, 65_535);

    let first_insert = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PageDocumentHelper::new(&mut pdf).add_page(PageInput::target(raw_page.clone()), false)
    }));
    assert!(
        first_insert.is_ok(),
        "raw QpdfObjGen insertion must not panic at an ObjectRef projection boundary"
    );
    first_insert
        .expect("raw page insertion should not panic")
        .expect("raw page insertion should succeed");

    let pages_root = pdf
        .root_handle()
        .expect("read target catalog")
        .try_get_key(b"/Pages")
        .expect("read target /Pages");
    let flattened_kids = pages_root
        .try_get_key(b"/Kids")
        .expect("read flattened /Kids");

    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::target(raw_page.clone()), false)
        .expect("inserting an existing raw page should shallow-copy it");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("read pages after duplicate insertion");
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].get_obj_gen(), QpdfObjGen::new(17, 65_535));
    assert_ne!(pages[1].get_obj_gen(), QpdfObjGen::new(17, 65_535));
    let current_kids = pages_root
        .try_get_key(b"/Kids")
        .expect("read current /Kids");
    assert!(flattened_kids.is_same_object_as(&current_kids));
    assert_eq!(
        pages_root
            .try_get_key(b"/Count")
            .expect("read /Count")
            .as_integer(),
        Some(2)
    );
}

#[test]
fn add_page_at_accepts_a_raw_generation_reference_page_handle() {
    let mut pdf = Pdf::empty().expect("empty document should be constructible");
    let pages_root = pdf
        .root_handle()
        .expect("read target catalog")
        .try_get_key(b"/Pages")
        .expect("read target /Pages");
    let raw_page_ref = ObjectRef::new(17, 65_535);
    pdf.replace_object(raw_page_ref, page_dictionary(Some(pages_root.clone())))
        .expect("install raw page in the target object cache");
    let raw_page = pdf.get_object_handle_by_raw_identity(17, 65_535);
    pages_root
        .try_get_key(b"/Kids")
        .expect("read target /Kids")
        .insert_array_item(0, raw_page.clone())
        .expect("attach raw page to target tree");
    pages_root
        .replace_key(b"/Count", ObjectHandle::integer(1))
        .expect("set target page count");

    let inserted_page = page_dictionary(None);
    let result = PageDocumentHelper::new(&mut pdf).add_page_at(
        PageInput::target(inserted_page),
        true,
        raw_page.clone(),
    );
    assert!(
        result.is_ok(),
        "addPageAt must find the reference page by raw QpdfObjGen"
    );
    result.expect("raw reference page insertion should succeed");

    let pages = PageDocumentHelper::new(&mut pdf)
        .get_all_pages()
        .expect("read pages after insertion");
    assert_eq!(pages.len(), 2);
    assert_ne!(pages[0].get_obj_gen(), raw_page.get_obj_gen());
    assert_eq!(pages[1].get_obj_gen(), QpdfObjGen::new(17, 65_535));
}

#[test]
fn add_page_at_reports_qpdf_page_context_for_a_raw_nonmember_reference() {
    let mut pdf = Pdf::open_with_options(
        Cursor::new(one_page_nested_tree_with_unknown_key()),
        PdfOpenOptions {
            description: b"page_add_at_raw.pdf".to_vec(),
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("nested one-page PDF should parse");
    pdf.replace_object(ObjectRef::new(17, 65_535), page_dictionary(None))
        .expect("install a detached raw-generation page");
    let reference_page = pdf.get_object_handle_by_raw_identity(17, 65_535);

    let error = PageDocumentHelper::new(&mut pdf)
        .add_page_at(
            PageInput::target(page_dictionary(None)),
            true,
            reference_page,
        )
        .expect_err("a reference page outside /Pages must raise qpdf's page error");

    assert!(matches!(error, Error::QpdfExc(_)));
    assert_eq!(
        error.to_string(),
        "page_add_at_raw.pdf (page object: object 17 65535): page object not referenced in /Pages tree"
    );
}

#[test]
fn foreign_input_rejects_a_page_not_owned_by_its_source_pdf() {
    let mut actual_source = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/three-page.pdf").to_vec(),
    )
    .expect("source PDF should parse");
    let source_page = PageDocumentHelper::new(&mut actual_source)
        .get_all_pages()
        .expect("source page list")
        .into_iter()
        .next()
        .expect("source page");
    let mut wrong_source = Pdf::empty().expect("wrong source PDF");
    let mut target = Pdf::empty().expect("target PDF");

    let error = PageDocumentHelper::new(&mut target)
        .add_page(PageInput::foreign(&mut wrong_source, source_page), false)
        .expect_err("foreign input must be paired with the page's owning source");

    assert!(matches!(
        error,
        Error::Unsupported(message) if message == "foreign page handle is not owned by the source PDF"
    ));
    assert!(PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .expect("read target after rejected input")
        .is_empty());
}

#[test]
fn target_input_rejects_an_indirect_page_owned_by_another_pdf() {
    let mut source = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/three-page.pdf").to_vec(),
    )
    .expect("source PDF should parse");
    let source_page = PageDocumentHelper::new(&mut source)
        .get_all_pages()
        .expect("source page list")
        .into_iter()
        .next()
        .expect("source page");
    let mut target = Pdf::empty().expect("target PDF");

    let error = PageDocumentHelper::new(&mut target)
        .add_page(PageInput::target(source_page), false)
        .expect_err("target input must belong to the target PDF");

    assert!(matches!(
        error,
        Error::Unsupported(message)
            if message == "indirect page handle is not owned by the target PDF; use Foreign input"
    ));
    assert!(PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .expect("read target after rejected input")
        .is_empty());
}

#[test]
fn foreign_direct_page_is_promoted_into_the_target_pdf() {
    let mut source = Pdf::empty().expect("source PDF");
    let mut target = Pdf::empty().expect("target PDF");

    PageDocumentHelper::new(&mut target)
        .add_page(
            PageInput::foreign(&mut source, page_dictionary(None)),
            false,
        )
        .expect("direct page handles are promoted in the target");

    let target_pages = PageDocumentHelper::new(&mut target)
        .get_all_pages()
        .expect("read inserted target page");
    assert_eq!(target_pages.len(), 1);
    assert!(target_pages[0].is_indirect());
    assert_eq!(
        PageDocumentHelper::new(&mut source)
            .get_all_pages()
            .expect("source page list should remain empty")
            .len(),
        0
    );
}

#[test]
fn add_page_rejects_an_append_index_out_of_range_after_flattening() {
    let mut pdf = Pdf::empty().expect("empty target PDF");
    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::target(page_dictionary(None)), false)
        .expect("install initial page");
    let pages_root = pdf
        .root_handle()
        .expect("target catalog")
        .try_get_key(b"/Pages")
        .expect("target /Pages");
    pages_root
        .replace_key(b"/Count", ObjectHandle::integer(-1))
        .expect("force qpdf's negative append index after flattening");

    let error = PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::target(page_dictionary(None)), false)
        .expect_err("negative append position must fail after page materialization");

    assert!(matches!(
        error,
        Error::Internal(message) if message == "QPDF::insertPage called with pos out of range"
    ));
    assert_eq!(
        PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("read page list after failed insertion")
            .len(),
        1
    );
}

#[test]
fn flatten_annotations_defaults_to_no_required_flags_and_forbids_invisible_hidden() {
    let bytes = annotation_flags_pdf();
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("open flag fixture");

    PageDocumentHelper::new(&mut pdf)
        .flatten_annotations()
        .expect("the public default call uses qpdf's all-annotations flag mask");

    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    assert!(page.try_get_key(b"/Annots").unwrap().try_is_null().unwrap());
    let xobjects = page
        .try_get_key(b"/Resources")
        .unwrap()
        .try_get_key(b"/XObject")
        .unwrap();
    let names = xobjects.try_get_keys().unwrap();
    assert_eq!(names.len(), 1, "only the F=0 annotation is eligible");
    assert_eq!(
        xobjects.try_get_key(b"/Fxo1").unwrap().object_ref(),
        Some(ObjectRef::new(5, 0)),
        "required_flags defaults to zero and forbidden_flags defaults to 1|2"
    );

    let mut print_only = Pdf::open(Cursor::new(bytes)).expect("reopen flag fixture");
    PageDocumentHelper::new(&mut print_only)
        .flatten_annotations_with_flags(0x4, 0x3)
        .expect("explicit flags remain available through the named variant");
    let page = print_only.get_object_handle(ObjectRef::new(3, 0));
    assert!(page
        .try_get_key(b"/Resources")
        .unwrap()
        .try_get_key(b"/XObject")
        .unwrap()
        .try_is_null()
        .unwrap());
}
