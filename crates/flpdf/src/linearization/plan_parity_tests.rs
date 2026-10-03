//! Source-derived plan traversal tests that exercise writer-private data.

use crate::linearization::plan::LinearizationPlan;
use crate::{ObjectRef, ObjectStreamMode, Pdf};
use std::io::Cursor;
use std::path::Path;

/// A three-page document where object 6 is an ordinary object of page 1 and
/// page 2's `/Thumb`. qpdf gives object 6 both `ou_page(1)` and `ou_thumb(2)`,
/// so its non-zero thumbnail count demotes it from Part 7 to Part 9.
fn other_page_object_also_thumbnail_pdf() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = [0u64; 7];
    let mut push = |number: usize, body: &str| {
        offsets[number] = pdf.len() as u64;
        pdf.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    push(1, "<< /Type /Catalog /Pages 2 0 R >>");
    push(2, "<< /Type /Pages /Count 3 /Kids [3 0 R 4 0 R 5 0 R] >>");
    push(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
    push(
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> >>",
    );
    push(
        5,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Thumb 6 0 R >>",
    );
    push(
        6,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Name /F1 >>",
    );
    let xref_start = pdf.len() as u64;
    pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// Build a classic-xref PDF whose object numbers are the 1-based indices of
/// `bodies`. It keeps the malformed-but-parseable shapes below local to their
/// coverage tests rather than committing fixtures that have no qpdf oracle.
fn pdf_from_object_bodies(bodies: &[String]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        let number = index + 1;
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = pdf.len() as u64;
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn thumbnail_user_demotes_other_page_private_object_to_part9() {
    let mut pdf = Pdf::open(Cursor::new(other_page_object_also_thumbnail_pdf())).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let font = ObjectRef::new(6, 0);

    assert!(
        plan.part4_rest.contains(&font),
        "other-page object with a thumbnail user must be Part 9; part4_rest={:?}",
        plan.part4_rest
    );
    assert!(
        !plan.part4_other_pages_private.contains(&font),
        "thumbnail user must exclude the object from Part 7; part4_other_pages_private={:?}",
        plan.part4_other_pages_private
    );
}

#[test]
fn bogus_leaf_parent_cycle_is_ignored_by_user_traversal() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 4 0 R /MediaBox [0 0 612 792] >>".to_owned(),
        "<< /Type /Pages /Parent 3 0 R >>".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    LinearizationPlan::from_pdf(&mut pdf, false)
        .expect("ordered page-user traversal must skip the leaf /Parent");
}

#[test]
fn deep_bogus_leaf_parent_chain_is_ignored_like_qpdf() {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 4 0 R /MediaBox [0 0 612 792] >>".to_owned(),
    ];
    for number in 4..=104 {
        let parent = if number == 104 {
            String::new()
        } else {
            format!(" /Parent {} 0 R", number + 1)
        };
        bodies.push(format!("<< /Type /Pages{parent} >>"));
    }
    let mut pdf = Pdf::open(Cursor::new(pdf_from_object_bodies(&bodies))).unwrap();
    LinearizationPlan::from_pdf(&mut pdf, false)
        .expect("qpdf ignores a leaf /Parent outside the real /Kids page tree");
}

#[test]
fn detached_parent_attributes_do_not_become_page_users() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 4 0 R /MediaBox [0 0 612 792] >>".to_owned(),
        "<< /Type /Pages /Resources << /Font << /Bogus 5 0 R >> >> >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let detached_font = ObjectRef::new(5, 0);

    assert!(
        !plan.part2_objects.contains(&detached_font)
            && !plan.part3_objects.contains(&detached_font),
        "an attribute on a detached leaf /Parent must not become a page user"
    );
    assert!(
        plan.part4_rest.contains(&detached_font),
        "the reachable detached attribute remains an ordinary Part 9 object"
    );
}

#[test]
fn non_dictionary_leaf_parent_is_ignored_by_user_traversal() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 4 0 R /MediaBox [0 0 612 792] >>".to_owned(),
        "null".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    LinearizationPlan::from_pdf(&mut pdf, false)
        .expect("ordered page-user traversal must not resolve the leaf /Parent");
}

#[test]
fn direct_page_parent_keeps_the_exact_reachability_fallback() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent << /Type /Pages /Count 1 /Kids [3 0 R] >> /MediaBox [0 0 612 792] >>".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    LinearizationPlan::from_pdf(&mut pdf, false)
        .expect("a direct page /Parent must use the exact writer reachability walk");
}

#[test]
fn preserve_source_objstm_keeps_the_exact_reachability_fallback() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/objstm-lin-firstpage-private-before-shared-bearing.pdf");
    let file = std::fs::File::open(path).unwrap();
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();
    LinearizationPlan::from_pdf_with_object_stream_mode(&mut pdf, ObjectStreamMode::Preserve)
        .expect("source-backed Preserve must use the exact writer reachability walk");
}

#[test]
fn deeply_nested_direct_page_value_is_accepted_by_optimization_up_to_parser_limit() {
    let nested = format!("{}{}", "[".repeat(257), "]".repeat(257));
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Nested {nested} >>"),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    LinearizationPlan::from_pdf(&mut pdf, false)
        .expect("qpdf accepts direct nesting below its parser limit");
}

#[test]
fn nested_page_reference_is_a_user_traversal_boundary() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Zzz 4 0 R >>".to_owned(),
        "<< /Type /Page /MediaBox [0 0 612 792] >>".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let nested_page = ObjectRef::new(4, 0);
    assert!(
        !plan.part2_objects.contains(&nested_page),
        "a non-top /Page must not become a first-page object user"
    );
}

#[test]
fn direct_nested_page_dictionary_is_a_user_traversal_boundary() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Zzz << /Type /Page /MediaBox [0 0 612 792] >> >>".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    assert_eq!(
        plan.part2_objects,
        vec![ObjectRef::new(3, 0)],
        "a direct nested /Page must not contribute indirect object users"
    );
}

#[test]
fn missing_thumbnail_reference_is_retracted_without_an_array_edge_on_that_page() {
    let bytes = pdf_from_object_bodies(&[
        "<< /Type /Catalog /Pages 2 0 R /Other [99 0 R] >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Thumb 99 0 R >>".to_owned(),
    ]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let missing = ObjectRef::new(99, 0);
    assert!(
        !plan.part2_objects.contains(&missing),
        "a thumbnail-only missing ref must not become a page user"
    );
}
