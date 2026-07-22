//! Integration coverage for [`flpdf::Pdf::page_labels`] via the public API.
//!
//! Focuses on holder-chain robustness: a `/Nums` label-range value reached
//! through a multi-hop indirect chain (`ref -> ref -> dict`) must still be read.

mod common;

use common::build_pdf;
use flpdf::{LabelStyle, Object, ObjectRef, Pdf};
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
