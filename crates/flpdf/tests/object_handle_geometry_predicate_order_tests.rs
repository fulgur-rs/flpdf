use flpdf::{ObjectHandle, Pdf, PdfOpenOptions};
use std::io::Cursor;

fn pdf_with_geometry_arrays(
    rectangle: &str,
    matrix: &str,
    second_object: &str,
) -> Pdf<Cursor<Vec<u8>>> {
    let root =
        format!("<< /Type /Catalog /Pages 11 0 R /Rectangle {rectangle} /Matrix {matrix} >>");
    let objects = [
        (1, root),
        (2, "1".to_owned()),
        (3, second_object.to_owned()),
        (4, "3".to_owned()),
        (5, "4".to_owned()),
        (6, "5".to_owned()),
        (7, "6".to_owned()),
        (8, "7".to_owned()),
        (9, "8".to_owned()),
        (10, "9".to_owned()),
        (11, "<< /Type /Pages /Kids [12 0 R] /Count 1 >>".to_owned()),
        (
            12,
            "<< /Type /Page /Parent 11 0 R /MediaBox [0 0 10 10] >>".to_owned(),
        ),
    ];

    let size = objects.len() + 1;
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![None; size];
    for (number, body) in objects {
        offsets[number] = Some(bytes.len());
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body.as_bytes());
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        let offset = offset.expect("the fixture uses contiguous object numbers");
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );

    Pdf::open_mem_owned_with_options(
        bytes,
        PdfOpenOptions {
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("the synthetic PDF must open")
}

fn array_and_children(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    key: &[u8],
) -> (ObjectHandle, Vec<ObjectHandle>) {
    let root = pdf.root_handle().expect("the catalog must resolve");
    let array = root
        .try_get_key(key)
        .expect("the geometry array must exist");
    let children = array
        .try_get_array_as_vector()
        .expect("the geometry value must be an array");
    (array, children)
}

fn assert_unknown_token_warning(pdf: &Pdf<Cursor<Vec<u8>>>) {
    let diagnostics = pdf.repair_diagnostics();
    assert_eq!(diagnostics.entries().len(), 1, "{diagnostics:?}");
    assert_eq!(
        diagnostics.entries()[0].get_message_detail(),
        b"unknown token while reading object; treating as string"
    );
}

#[test]
fn rectangle_predicate_checks_indirect_components_before_oversized_length() {
    let mut pdf = pdf_with_geometry_arrays(
        "[2 0 R 4 0 R 5 0 R 6 0 R 10]",
        "[2 0 R 4 0 R 5 0 R 6 0 R 7 0 R 8 0 R 10]",
        "2",
    );
    let (rectangle, children) = array_and_children(&mut pdf, b"/Rectangle");
    assert_eq!(children.len(), 5);
    assert!(children[..4].iter().all(|child| !child.is_resolved()));

    assert!(!rectangle
        .try_is_rectangle()
        .expect("an oversized array is not a rectangle"));

    assert!(children[..4].iter().all(ObjectHandle::is_resolved));
    assert!(pdf.repair_diagnostics().entries().is_empty());
}

#[test]
fn rectangle_predicate_checks_available_components_before_missing_slot() {
    let mut pdf = pdf_with_geometry_arrays(
        "[2 0 R 4 0 R 5 0 R]",
        "[2 0 R 4 0 R 5 0 R 6 0 R 7 0 R]",
        "2",
    );
    let (rectangle, children) = array_and_children(&mut pdf, b"/Rectangle");
    assert_eq!(children.len(), 3);
    assert!(children.iter().all(|child| !child.is_resolved()));

    assert!(!rectangle
        .try_is_rectangle()
        .expect("an undersized array is not a rectangle"));

    assert!(children.iter().all(ObjectHandle::is_resolved));
    assert!(pdf.repair_diagnostics().entries().is_empty());
}

#[test]
fn matrix_predicate_checks_indirect_components_before_oversized_length() {
    let mut pdf = pdf_with_geometry_arrays(
        "[2 0 R 4 0 R 5 0 R 6 0 R 10]",
        "[2 0 R 4 0 R 5 0 R 6 0 R 7 0 R 8 0 R 10]",
        "2",
    );
    let (matrix, children) = array_and_children(&mut pdf, b"/Matrix");
    assert_eq!(children.len(), 7);
    assert!(children[..6].iter().all(|child| !child.is_resolved()));

    assert!(!matrix
        .try_is_matrix()
        .expect("an oversized array is not a matrix"));

    assert!(children[..6].iter().all(ObjectHandle::is_resolved));
    assert!(pdf.repair_diagnostics().entries().is_empty());
}

#[test]
fn matrix_predicate_checks_available_components_before_missing_slot() {
    let mut pdf = pdf_with_geometry_arrays(
        "[2 0 R 4 0 R 5 0 R 6 0 R 10]",
        "[2 0 R 4 0 R 5 0 R 6 0 R 7 0 R]",
        "2",
    );
    let (matrix, children) = array_and_children(&mut pdf, b"/Matrix");
    assert_eq!(children.len(), 5);
    assert!(children.iter().all(|child| !child.is_resolved()));

    assert!(!matrix
        .try_is_matrix()
        .expect("an undersized array is not a matrix"));

    assert!(children.iter().all(ObjectHandle::is_resolved));
    assert!(pdf.repair_diagnostics().entries().is_empty());
}

#[test]
fn rectangle_predicate_emits_child_warning_before_rejecting_oversized_length() {
    let mut pdf = pdf_with_geometry_arrays(
        "[2 0 R 3 0 R 4 0 R 5 0 R 10]",
        "[2 0 R 3 0 R 4 0 R 5 0 R 6 0 R 7 0 R 10]",
        "@",
    );
    let (rectangle, children) = array_and_children(&mut pdf, b"/Rectangle");
    assert!(pdf.repair_diagnostics().entries().is_empty());

    assert!(!rectangle
        .try_is_rectangle()
        .expect("the malformed component is not numeric"));

    assert!(children[0].is_resolved());
    assert!(children[1].is_resolved());
    assert!(children[2..4].iter().all(|child| !child.is_resolved()));
    assert_unknown_token_warning(&pdf);
}

#[test]
fn matrix_predicate_emits_child_warning_before_rejecting_oversized_length() {
    let mut pdf = pdf_with_geometry_arrays(
        "[2 0 R 3 0 R 4 0 R 5 0 R 10]",
        "[2 0 R 3 0 R 4 0 R 5 0 R 6 0 R 7 0 R 10]",
        "@",
    );
    let (matrix, children) = array_and_children(&mut pdf, b"/Matrix");
    assert!(pdf.repair_diagnostics().entries().is_empty());

    assert!(!matrix
        .try_is_matrix()
        .expect("the malformed component is not numeric"));

    assert!(children[0].is_resolved());
    assert!(children[1].is_resolved());
    assert!(children[2..6].iter().all(|child| !child.is_resolved()));
    assert_unknown_token_warning(&pdf);
}
