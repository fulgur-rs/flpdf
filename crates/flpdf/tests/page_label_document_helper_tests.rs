//! Integration coverage for [`flpdf::Pdf::page_labels`] via the public API.
//!
//! Covers qpdf's page-label NumberTree traversal and malformed-input behavior.

mod common;

use common::build_pdf;
use flpdf::{LabelStyle, Pdf};
use std::io::Cursor;

#[test]
fn page_label_mutations_use_only_the_canonical_handle_route() {
    let source = include_str!("../src/page_label_document_helper.rs").replace("\r\n", "\n");
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
