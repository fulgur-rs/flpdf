use flpdf::{Error, ObjectRef, PageDocumentHelper, Pdf, PdfOpenOptions, QpdfErrorCode};
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
