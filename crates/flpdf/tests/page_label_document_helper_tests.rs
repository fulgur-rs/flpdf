//! Integration coverage for [`flpdf::Pdf::page_labels`] via the public API.
//!
//! Focuses on holder-chain robustness: a `/Nums` label-range value reached
//! through a multi-hop indirect chain (`ref -> ref -> dict`) must still be read.

mod common;

use common::build_pdf;
use flpdf::{LabelRange, LabelStyle, Object, ObjectRef, Pdf};
use std::io::Cursor;

/// A `/Nums` label-range value stored behind a two-hop holder chain
/// (`Ref(6) -> Ref(7) -> << /S /D /St 4 >>`) must resolve to its terminal
/// label dictionary. A single-hop resolve would see the intermediate
/// `Object::Reference` (not a dictionary) and silently drop the range.
#[test]
fn ranges_follows_two_hop_holder_chain_for_label_dict() {
    // Catalog -> /PageLabels (obj 4) -> /Nums [0 6 0 R].
    // 6 0 R is itself a reference to 7 0 R (the carrier hop), and 7 0 obj is
    // the actual label dictionary. This is a genuine two-hop chain.
    let pdf_bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels 4 0 R >>".into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
            (4, "<< /Nums [0 6 0 R] >>".into()),
            // Holder chain: 6 -> 7 -> label dict.
            (6, "7 0 R".into()),
            (7, "<< /S /D /St 4 >>".into()),
        ],
        1,
    );

    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Reference(ObjectRef::new(7, 0)),
    );
    let mut h = pdf.page_labels();
    let ranges = h.ranges().expect("read ranges");

    assert_eq!(
        ranges.len(),
        1,
        "the two-hop holder-chain label range must be read, not dropped"
    );
    assert_eq!(ranges[0].0, 0, "range starts at page index 0");
    assert_eq!(ranges[0].1.style, LabelStyle::Decimal, "/S /D");
    assert_eq!(ranges[0].1.start, 4, "/St 4");
    // The rendered label for page 0 confirms the range is fully wired.
    assert_eq!(h.label_string_for_page(0).expect("label"), "4");
}

#[test]
fn set_range_mutates_existing_page_labels_root() {
    let pdf_bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels 4 0 R >>".into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
            (4, "<< /Nums [0 << /S /D >>] >>".into()),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    let page_labels_root = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve_object_handle(&page_labels_root)
        .expect("resolve page-label root");

    pdf.page_labels()
        .set_range(
            5,
            LabelRange {
                style: LabelStyle::RomanUpper,
                prefix: "A".to_string(),
                start: 2,
            },
        )
        .expect("set range");

    let catalog_ref = pdf.root_ref().expect("catalog ref");
    let catalog = pdf
        .resolve(catalog_ref)
        .expect("catalog")
        .into_dict()
        .expect("catalog dict");
    assert_eq!(
        catalog.get_ref("PageLabels"),
        Some(ObjectRef::new(4, 0)),
        "qpdf NumberTree insertion mutates the existing root"
    );
    assert_eq!(
        pdf.page_labels().label_string_for_page(5).expect("label"),
        "AII"
    );
    let nums = page_labels_root
        .get_key(b"/Nums")
        .as_array()
        .expect("/Nums present");
    assert!(nums.iter().any(|value| value.as_integer() == Some(5)));
}

#[test]
fn page_label_mutations_use_only_the_canonical_handle_route() {
    let source = include_str!("../src/page_label_document_helper.rs");
    let production = source
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("page-label production source");

    for forbidden in ["crate::NumberTree::", "resolve_borrowed(", ".set_object("] {
        assert!(
            !production.contains(forbidden),
            "page-label production route still contains {forbidden}"
        );
    }
}

#[test]
fn remove_range_mutates_existing_nonempty_page_labels_root() {
    let pdf_bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels 4 0 R >>".into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
            (4, "<< /Nums [0 << /S /D >> 5 << /S /r /P (x) >>] >>".into()),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");

    assert!(pdf.page_labels().remove_range(5).expect("remove range"));

    let catalog_ref = pdf.root_ref().expect("catalog ref");
    let catalog = pdf
        .resolve(catalog_ref)
        .expect("catalog")
        .into_dict()
        .expect("catalog dict");
    assert_eq!(catalog.get_ref("PageLabels"), Some(ObjectRef::new(4, 0)));
    assert_eq!(
        pdf.page_labels().label_string_for_page(0).expect("label"),
        "1"
    );
}

#[test]
fn ranges_repairs_and_reads_direct_number_tree_kid() {
    let pdf_bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /PageLabels << /Kids [ << /Limits [0 0] \
                 /Nums [0 << /S /D /St 3 >>] >> ] >> >>"
                    .into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");

    let ranges = pdf.page_labels().ranges().expect("ranges");

    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].0, 0);
    assert_eq!(ranges[0].1.style, LabelStyle::Decimal);
    assert_eq!(ranges[0].1.start, 3);
}

#[test]
fn write_labels_uses_qpdf_sixteen_seventeen_split_order() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");
    let ranges = (0..33)
        .map(|index| {
            (
                index,
                LabelRange {
                    style: LabelStyle::Decimal,
                    prefix: String::new(),
                    start: 1,
                },
            )
        })
        .collect::<Vec<_>>();

    pdf.page_labels()
        .write_labels(&ranges)
        .expect("write labels");

    let catalog_ref = pdf.root_ref().expect("catalog ref");
    let catalog = pdf
        .resolve(catalog_ref)
        .expect("catalog")
        .into_dict()
        .expect("catalog dict");
    let root_ref = catalog.get_ref("PageLabels").expect("page labels root");
    let root = pdf
        .resolve(root_ref)
        .expect("root")
        .into_dict()
        .expect("root dict");
    let kids = root
        .get("Kids")
        .and_then(Object::as_array)
        .expect("split kids");
    let first_ref = kids[0].as_ref_id().expect("first kid ref");
    let first = pdf
        .resolve(first_ref)
        .expect("first leaf")
        .into_dict()
        .expect("first leaf dict");
    let first_items = first
        .get("Nums")
        .and_then(Object::as_array)
        .expect("first nums");
    assert_eq!(first_items.len() / 2, 16);
}

#[test]
fn set_range_creates_tree_and_remove_missing_range_is_false() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");

    pdf.page_labels()
        .set_range(
            0,
            LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: 1,
            },
        )
        .expect("set");

    assert!(!pdf.page_labels().remove_range(99).expect("remove missing"));
}

#[test]
fn set_range_propagates_malformed_number_tree_error() {
    let pdf_bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0] >> >>".into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");

    let error = pdf
        .page_labels()
        .set_range(
            0,
            LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: 1,
            },
        )
        .expect_err("malformed tree must fail");

    assert!(error.to_string().contains("items array is too short"));
}

#[test]
fn remove_range_without_catalog_root_is_false() {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let object_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref = bytes.len() as u64;
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \n\
             trailer\n<< /Size 2 >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");

    assert!(!pdf.page_labels().remove_range(0).expect("remove"));
}
