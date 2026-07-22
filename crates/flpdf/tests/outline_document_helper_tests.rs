//! Integration tests for [`flpdf::OutlineDocumentHelper`].

use flpdf::{write_pdf, Dictionary, Object, ObjectRef, OutlineItem, Pdf};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

/// Build a minimal cross-reffed PDF from `(objnum, body)` pairs.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=max {
        match offsets.get(&n) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn page_dest(page: u32) -> Object {
    Object::Array(vec![
        Object::Reference(ObjectRef::new(page, 0)),
        Object::Name(b"Fit".to_vec()),
    ])
}

fn root_items(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<OutlineItem> {
    let tree = pdf.outline().get_tree().unwrap();
    tree.roots().iter().map(|&id| tree[id].clone()).collect()
}

/// Catalog + pages + a two-level outline:
///   root(4) -> First A(5)
///   A(5)    -> First A1(6); A1 has dest [3 0 R /Fit]
///   A(5)    -> Next  B(7);  B has /Count 2
fn outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 7 0 R /Count 2 >>"),
            (
                5,
                "<< /Title (A) /Parent 4 0 R /First 6 0 R /Last 6 0 R /Next 7 0 R /Count 1 >>",
            ),
            (6, "<< /Title (A1) /Parent 5 0 R /Dest [3 0 R /Fit] >>"),
            (7, "<< /Title (B) /Parent 4 0 R /Prev 5 0 R /Count 2 >>"),
        ],
        1,
    )
}

fn page_index_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /Dests << /same [3 0 R /Fit] >> /Names << /Dests 20 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (5, "<< /Type /Outlines /First 6 0 R /Last 15 0 R >>"),
            (
                6,
                "<< /Title (A) /Dest [3 0 R /Fit] /First 8 0 R /Next 7 0 R >>",
            ),
            (
                7,
                "<< /Title (B) /Dest /same /First 10 0 R /Next 12 0 R >>",
            ),
            (8, "<< /Title (A1) /Dest [3 0 R /Fit] /Next 9 0 R >>"),
            (9, "<< /Title (A2) /Dest [4 0 R /Fit] >>"),
            (10, "<< /Title (B1) /Dest (modern) >>"),
            (12, "<< /Title (No dest) /Next 13 0 R >>"),
            (
                13,
                "<< /Title (Integer dest) /Dest 42 /Next 14 0 R >>",
            ),
            (
                14,
                "<< /Title (Direct page operand) /Dest [<< /Type /Page >> /Fit] /Next 15 0 R >>",
            ),
            (15, "<< /Title (Zero reference) /Dest [0 0 R /Fit] >>"),
            (20, "<< /Names [(modern) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn outlines_for_page_uses_qpdf_breadth_first_order() {
    let mut pdf = Pdf::open(Cursor::new(page_index_outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    let titles: Vec<_> = tree
        .outlines_for_page(Some(ObjectRef::new(3, 0)))
        .map(|(_id, item)| item.title.as_str())
        .collect();

    assert_eq!(titles, ["A", "B", "A1", "B1"]);
}

#[test]
fn outlines_for_page_none_matches_qpdf_objgen_zero_bucket() {
    let mut pdf = Pdf::open(Cursor::new(page_index_outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    let titles: Vec<_> = tree
        .outlines_for_page(None)
        .map(|(_id, item)| item.title.as_str())
        .collect();

    assert_eq!(
        titles,
        [
            "No dest",
            "Integer dest",
            "Direct page operand",
            "Zero reference"
        ]
    );

    let zero_ref_titles: Vec<_> = tree
        .outlines_for_page(Some(ObjectRef::new(0, 0)))
        .map(|(_id, item)| item.title.as_str())
        .collect();
    assert_eq!(zero_ref_titles, titles);
}

fn no_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// Catalog with an `/Outlines` dict present but with no `/First` child.
fn outline_present_but_empty_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /Count 0 >>"),
        ],
        1,
    )
}

fn single_outline_with_item_fields(fields: &str) -> Vec<u8> {
    let item = format!("<< {fields} /Parent 4 0 R >>");
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, item.as_str()),
        ],
        1,
    )
}

fn single_outline_with_title(title_object: &str) -> Vec<u8> {
    single_outline_with_item_fields(&format!("/Title {title_object}"))
}

fn single_outline_without_title() -> Vec<u8> {
    single_outline_with_item_fields("")
}

fn single_outline_with_count(count_object: &str) -> Vec<u8> {
    single_outline_with_item_fields(&format!("/Count {count_object}"))
}

fn warning_messages(pdf: &Pdf<Cursor<Vec<u8>>>) -> Vec<&str> {
    pdf.repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

fn direct_dests_root(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Dictionary {
    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).unwrap() else {
        panic!("catalog must be a dictionary");
    };
    let Object::Dictionary(names) = catalog.get("Names").unwrap() else {
        panic!("/Names must be a direct dictionary");
    };
    let Object::Dictionary(dests) = names.get("Dests").unwrap() else {
        panic!("/Dests must be a direct dictionary");
    };
    dests.clone()
}

#[test]
fn titles_match_qpdf_get_utf8_value() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("(plain)", "plain", None),
        ("<95>", "Ł", None),
        ("<FEFF540D524D>", "名前", None),
        ("<FFFE0D544D52>", "名前", None),
        ("<EFBBBFE5908D>", "名", None),
        ("<EFBBBFFF>", "�", None),
        ("<FEFF0041D800>", "A", None),
        (
            "42",
            "",
            Some(
                "operation for string attempted on object of type integer: returning empty string",
            ),
        ),
    ];

    for &(title_object, expected, expected_warning) in cases {
        let mut pdf = Pdf::open(Cursor::new(single_outline_with_title(title_object))).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        assert_eq!(tree[tree.roots()[0]].title, expected, "{title_object}");
        assert_eq!(
            warning_messages(&pdf),
            expected_warning.into_iter().collect::<Vec<_>>(),
            "{title_object}"
        );
    }

    let mut pdf = Pdf::open(Cursor::new(single_outline_without_title())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].title, "");
    assert!(warning_messages(&pdf).is_empty());
}

#[test]
fn counts_match_qpdf_get_int_value_as_int() {
    let cases = [
        (
            "-2147483649",
            i32::MIN,
            Some("requested value of integer is too small; returning INT_MIN"),
        ),
        ("-2147483648", i32::MIN, None),
        ("7", 7, None),
        ("2147483647", i32::MAX, None),
        (
            "2147483648",
            i32::MAX,
            Some("requested value of integer is too big; returning INT_MAX"),
        ),
        (
            "(wrong type)",
            0,
            Some("operation for integer attempted on object of type string: returning 0"),
        ),
    ];

    for (count_object, expected, expected_warning) in cases {
        let mut pdf = Pdf::open(Cursor::new(single_outline_with_count(count_object))).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        assert_eq!(tree[tree.roots()[0]].count, expected, "{count_object}");
        let warning_messages = warning_messages(&pdf);
        match expected_warning {
            Some(expected_warning) => {
                assert!(
                    warning_messages.contains(&expected_warning),
                    "{count_object}"
                )
            }
            None => assert!(warning_messages.is_empty(), "{count_object}"),
        }
    }

    let mut pdf = Pdf::open(Cursor::new(single_outline_with_item_fields(""))).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].count, 0);
    assert!(warning_messages(&pdf).is_empty());
}

#[test]
fn present_wrong_type_scalar_warnings_use_qpdf_object_type_names() {
    let cases = [
        ("null", "null"),
        ("true", "boolean"),
        ("42", "integer"),
        ("1.5", "real"),
        ("/value", "name"),
        ("(value)", "string"),
        ("[]", "array"),
        ("<<>>", "dictionary"),
    ];

    for (object, type_name) in cases {
        if type_name != "string" {
            let mut pdf = Pdf::open(Cursor::new(single_outline_with_title(object))).unwrap();
            let tree = pdf.outline().get_tree().unwrap();
            assert_eq!(tree[tree.roots()[0]].title, "", "title {object}");
            assert_eq!(
                warning_messages(&pdf),
                [format!(
                    "operation for string attempted on object of type {type_name}: returning empty string"
                )],
                "title {object}"
            );
        }

        if type_name != "integer" {
            let mut pdf = Pdf::open(Cursor::new(single_outline_with_count(object))).unwrap();
            let tree = pdf.outline().get_tree().unwrap();
            assert_eq!(tree[tree.roots()[0]].count, 0, "count {object}");
            assert_eq!(
                warning_messages(&pdf),
                [format!(
                    "operation for integer attempted on object of type {type_name}: returning 0"
                )],
                "count {object}"
            );
        }
    }

    let stream_body = "<< /Length 0 >>\nstream\n\nendstream";
    for key in ["Title", "Count"] {
        let item = format!("<< /{key} 8 0 R /Parent 4 0 R >>");
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
                (5, item.as_str()),
                (8, stream_body),
            ],
            1,
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        let expected = if key == "Title" {
            assert_eq!(tree[tree.roots()[0]].title, "");
            "operation for string attempted on object of type stream: returning empty string"
        } else {
            assert_eq!(tree[tree.roots()[0]].count, 0);
            "operation for integer attempted on object of type stream: returning 0"
        };
        assert_eq!(warning_messages(&pdf), [expected], "{key}");
    }

    for key in ["Title", "Count"] {
        let item = format!("<< /{key} 8 0 R /Parent 4 0 R >>");
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
                (5, item.as_str()),
                (8, "9 0 R"),
                (9, "42"),
            ],
            1,
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        let expected = if key == "Title" {
            assert_eq!(tree[tree.roots()[0]].title, "");
            "operation for string attempted on object of type reference: returning empty string"
        } else {
            assert_eq!(tree[tree.roots()[0]].count, 0);
            "operation for integer attempted on object of type reference: returning 0"
        };
        assert_eq!(warning_messages(&pdf), [expected], "{key}");
    }
}

#[test]
fn has_outlines_true_when_present() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    assert!(pdf.outline().has_outlines().unwrap());
}

#[test]
fn has_outlines_false_when_absent() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(!pdf.outline().has_outlines().unwrap());
}

#[test]
fn missing_or_non_dictionary_catalog_has_no_outline_tree() {
    let mut missing_root = no_outline_pdf();
    let marker = b"/Root 1 0 R";
    let start = missing_root
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    missing_root[start + 1..start + 5].copy_from_slice(b"Info");

    let non_dictionary_catalog = build_pdf(&[(1, "42")], 1);
    for bytes in [missing_root, non_dictionary_catalog] {
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        assert!(!pdf.outline().has_outlines().unwrap());
        assert!(pdf.outline().get_tree().unwrap().roots().is_empty());
    }
}

#[test]
fn has_outlines_false_when_outline_dict_has_no_first() {
    let mut pdf = Pdf::open(Cursor::new(outline_present_but_empty_pdf())).unwrap();
    assert!(!pdf.outline().has_outlines().unwrap());
}

#[test]
fn direct_outlines_first_and_next_are_materialized() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First << /Title (A) /Next << /Title (B) >> >> >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert!(pdf.outline().has_outlines().unwrap());
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree.roots().len(), 2);
    assert_eq!(tree[tree.roots()[0]].source_ref, None);
    assert_eq!(tree[tree.roots()[0]].title, "A");
    assert_eq!(tree[tree.roots()[1]].source_ref, None);
    assert_eq!(tree[tree.roots()[1]].title, "B");

    // qpdf 11.9.0 `--json=2 --json-key=outlines` on
    // `/tmp/direct-outline-fixture.pdf` reports two direct roots. The first raw
    // `object` contains /Count, /Dest, /Next, and /Title, the second is only
    // `{\"/Title\":\"u:Direct B\"}`, and neither is represented as `0 0 R`.
}

#[test]
fn mixed_direct_and_indirect_items_keep_identity_and_parent_ids() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                5,
                "<< /Title (Parent) /First << /Title (Direct child) /Next 6 0 R >> >>",
            ),
            (6, "<< /Title (Indirect child) >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let parent = tree.roots()[0];
    let direct = tree[parent].kids[0];
    let indirect = tree[parent].kids[1];

    assert_eq!(tree[parent].source_ref, Some(ObjectRef::new(5, 0)));
    assert_eq!(tree[direct].source_ref, None);
    assert_eq!(tree[indirect].source_ref, Some(ObjectRef::new(6, 0)));
    assert_eq!(tree[direct].parent, Some(parent));
    assert_eq!(tree[indirect].parent, Some(parent));
}

#[test]
fn non_dictionary_first_is_still_an_outline_item_with_default_accessors() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 42 >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let id = tree.roots()[0];

    assert_eq!(tree[id].object, Object::Integer(42));
    assert_eq!(tree[id].title, "");
    assert_eq!(tree[id].count, 0);
    assert_eq!(tree[id].dest, Object::Null);
    assert!(tree[id].kids.is_empty());
}

#[test]
fn indirect_null_first_has_no_outlines_and_materializes_no_item() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "null"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert!(!pdf.outline().has_outlines().unwrap());
    assert!(pdf.outline().get_tree().unwrap().roots().is_empty());
}

#[test]
fn has_outlines_is_true_when_indirect_first_resolves_to_non_null_scalar() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "42"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert!(pdf.outline().has_outlines().unwrap());
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].object, Object::Integer(42));
}

#[test]
fn indirect_null_next_terminates_the_root_sibling_chain() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Title (A) /Next 6 0 R >>"),
            (6, "null"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    assert_eq!(tree.roots().len(), 1);
    assert_eq!(tree[tree.roots()[0]].title, "A");
}

#[test]
fn construction_resolves_a_bare_reference_item_exactly_once() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "6 0 R"),
            (6, "<< /Title (Must not be followed) >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        pdf.resolve(ObjectRef::new(5, 0)).unwrap(),
        Object::Reference(ObjectRef::new(6, 0))
    );
    let tree = pdf.outline().get_tree().unwrap();
    let item = &tree[tree.roots()[0]];
    assert_eq!(tree.roots().len(), 1);
    assert_eq!(item.source_ref, Some(ObjectRef::new(5, 0)));
    assert_eq!(item.object, Object::Reference(ObjectRef::new(6, 0)));
    assert_eq!(item.title, "");
}

#[test]
fn top_level_indirect_next_cycle_stops_before_duplicate_root() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Title (A) /Next 6 0 R >>"),
            (6, "<< /Title (B) /Next 5 0 R >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    assert_eq!(tree.roots().len(), 2);
    assert_eq!(tree[tree.roots()[0]].title, "A");
    assert_eq!(tree[tree.roots()[1]].title, "B");
}

#[test]
fn child_first_back_to_seen_indirect_ancestor_is_materialized_without_expansion() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Title (Ancestor) /First 5 0 R >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let ancestor = tree.roots()[0];
    let repeated = tree[ancestor].kids[0];

    assert_eq!(tree[repeated].source_ref, Some(ObjectRef::new(5, 0)));
    assert_eq!(tree[repeated].parent, Some(ancestor));
    assert!(tree[repeated].kids.is_empty());
}

#[test]
fn equal_direct_dictionary_values_in_separate_positions_are_materialized_twice() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                5,
                "<< /Title (A) /First << /Title (Repeated) >> /Next 6 0 R >>",
            ),
            (6, "<< /Title (B) /First << /Title (Repeated) >> >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let first = tree[tree.roots()[0]].kids[0];
    let second = tree[tree.roots()[1]].kids[0];

    assert_ne!(first, second);
    assert_eq!(tree[first].source_ref, None);
    assert_eq!(tree[second].source_ref, None);
    assert_eq!(tree[first].object, tree[second].object);
}

#[test]
fn get_tree_materializes_tree_with_titles_counts_parents() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let roots = tree.roots();

    // Two top-level nodes: A, B.
    assert_eq!(roots.len(), 2);
    assert_eq!(tree[roots[0]].title, "A");
    assert_eq!(tree[roots[0]].parent, None);
    assert_eq!(tree[roots[0]].count, 1);
    assert_eq!(tree[roots[1]].title, "B");
    assert_eq!(tree[roots[1]].count, 2);

    // A has one child A1.
    assert_eq!(tree[roots[0]].kids.len(), 1);
    let a1 = tree[roots[0]].kids[0];
    assert_eq!(tree[a1].title, "A1");
    assert_eq!(tree[a1].parent, Some(roots[0]));
    assert_eq!(tree[a1].count, 0); // /Count absent -> 0 (qpdf)
    assert_eq!(tree[a1].source_ref, Some(ObjectRef::new(6, 0)));
}

#[test]
fn get_tree_empty_when_no_outline() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(pdf.outline().get_tree().unwrap().roots().is_empty());
}

#[test]
fn null_and_non_dictionary_outline_containers_are_empty() {
    for outlines in ["null", "42", "<< >>", "<< /First null >>"] {
        let catalog = format!("<< /Type /Catalog /Pages 2 0 R /Outlines {outlines} >>");
        let bytes = build_pdf(
            &[
                (1, catalog.as_str()),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
            1,
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert!(!pdf.outline().has_outlines().unwrap(), "{outlines}");
        assert!(
            pdf.outline().get_tree().unwrap().roots().is_empty(),
            "{outlines}"
        );
    }
}

#[test]
fn indirect_item_seen_as_a_child_is_materialized_again_as_a_root_without_expansion() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Title (A) /First 6 0 R /Next 6 0 R >>"),
            (6, "<< /Title (B) /First 7 0 R >>"),
            (7, "<< /Title (C) >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    assert_eq!(tree.roots().len(), 2);
    let a = tree.roots()[0];
    let b_as_child = tree[a].kids[0];
    let b_as_root = tree.roots()[1];
    assert_eq!(tree[b_as_child].source_ref, Some(ObjectRef::new(6, 0)));
    assert_eq!(tree[b_as_child].kids.len(), 1);
    assert_eq!(tree[b_as_root].source_ref, Some(ObjectRef::new(6, 0)));
    assert!(tree[b_as_root].kids.is_empty());
}

#[test]
fn preorder_yields_lossless_arena_items() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let titles: Vec<&str> = tree
        .preorder()
        .map(|(_depth, _id, item)| item.title.as_str())
        .collect();
    assert_eq!(titles, vec!["A", "A1", "B"]); // pre-order: A, its child A1, then B

    let seen: Vec<(&str, usize, usize)> = tree
        .preorder()
        .map(|(depth, _id, item)| (item.title.as_str(), depth, item.kids.len()))
        .collect();
    assert_eq!(seen, vec![("A", 1, 1), ("A1", 2, 0), ("B", 1, 0),]);
}

/// Build a linear chain of `n` nested outline items (each is the sole child of
/// the previous). Object numbers: catalog 1, pages 2, page 3, outlines 4,
/// items 5..5+n. Returns PDF bytes.
fn deep_outline_pdf(n: u32) -> Vec<u8> {
    let mut objs: Vec<(u32, String)> = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>".to_string(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
    ];
    // outline root (4) points First/Last at first item (5).
    objs.push((
        4,
        "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_string(),
    ));
    for i in 0..n {
        let num = 5 + i;
        let parent = if i == 0 { 4 } else { num - 1 };
        let mut body = format!("<< /Title (L{i}) /Parent {parent} 0 R");
        if i + 1 < n {
            let child = num + 1;
            body.push_str(&format!(" /First {child} 0 R /Last {child} 0 R"));
        }
        body.push_str(" >>");
        objs.push((num, body));
    }
    let refs: Vec<(u32, &str)> = objs.iter().map(|(n, s)| (*n, s.as_str())).collect();
    build_pdf(&refs, 1)
}

#[test]
fn deep_outline_walks_to_full_depth() {
    let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(30))).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let count = tree.preorder().count();
    assert_eq!(count, 30);
    // The arena's public preorder depth is one-based.
    let max_depth = tree
        .preorder()
        .map(|(depth, _id, _item)| depth)
        .max()
        .unwrap();
    assert_eq!(max_depth, 30);
}

#[test]
fn qpdf_depth_50_boundary_materializes_depth_51_without_expanding_it() {
    for (input_levels, expected_levels) in [(50, 50), (51, 51), (52, 51)] {
        let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(input_levels))).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        let visits: Vec<_> = tree.preorder().collect();

        assert_eq!(visits.len(), expected_levels);
        assert_eq!(visits.first().unwrap().0, 1);
        assert_eq!(visits.last().unwrap().0, expected_levels);
        if input_levels == 52 {
            assert!(visits.last().unwrap().2.kids.is_empty());
        }
    }
}

#[test]
fn qpdf_depth_50_boundary_returns_no_depth_error() {
    let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(52))).unwrap();

    assert!(pdf.outline().has_outlines().unwrap());
    assert_eq!(pdf.outline().get_tree().unwrap().preorder().count(), 51);
}

/// Outline with a /Next cycle: 5 -> Next 6 -> Next 5 ...
fn cyclic_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >>"),
            (5, "<< /Title (X) /Parent 4 0 R /Next 6 0 R >>"),
            (6, "<< /Title (Y) /Parent 4 0 R /Next 5 0 R >>"), // cycle back to 5
        ],
        1,
    )
}

#[test]
fn cyclic_outline_terminates() {
    let mut pdf = Pdf::open(Cursor::new(cyclic_outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let titles: Vec<&str> = tree
        .preorder()
        .map(|(_depth, _id, item)| item.title.as_str())
        .collect();
    // Visits X and Y once each, then the cycle back to 5 is cut by `visited`.
    assert_eq!(titles, vec!["X", "Y"]);
}

#[test]
fn dest_from_explicit_dest_array() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let roots = tree.roots();
    let a1 = tree[roots[0]].kids[0]; // A1 has /Dest [3 0 R /Fit]
    assert_eq!(tree[a1].dest, page_dest(3));
    assert_eq!(
        tree[a1].dest_page(),
        Object::Reference(ObjectRef::new(3, 0))
    );
    // Nodes without a destination have qpdf's null sentinel.
    assert_eq!(tree[roots[1]].dest, Object::Null); // B
}

/// Outline item whose destination is a GoTo action: /A << /S /GoTo /D [3 0 R /Fit] >>.
fn action_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S /GoTo /D [3 0 R /Fit] >> >>",
            ),
        ],
        1,
    )
}

#[test]
fn dest_from_goto_action() {
    let mut pdf = Pdf::open(Cursor::new(action_dest_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].dest, page_dest(3));
    assert_eq!(
        roots[0].dest_page(),
        Object::Reference(ObjectRef::new(3, 0))
    );
}

/// Outline item whose /Dest is an INDIRECT ref (obj 8) to an explicit array.
fn indirect_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Ind) /Parent 4 0 R /Dest 8 0 R >>"),
            (8, "[3 0 R /Fit]"),
        ],
        1,
    )
}

#[test]
fn dest_from_indirect_dest_reference() {
    let mut pdf = Pdf::open(Cursor::new(indirect_dest_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].dest, page_dest(3));
}

/// Outline item whose /Dest points at a dict whose /D points back at itself:
/// 8 0 obj << /D 8 0 R >>. qpdf preserves the raw dictionary shape.
fn cyclic_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Cyc) /Parent 4 0 R /Dest 8 0 R >>"),
            (8, "<< /D 8 0 R >>"),
        ],
        1,
    )
}

#[test]
fn cyclic_dest_preserves_dictionary_shape() {
    let mut pdf = Pdf::open(Cursor::new(cyclic_dest_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert!(matches!(roots[0].dest, Object::Dictionary(_)));
}

/// Modern named dest: outline /Dest (string) resolved via catalog /Names /Dests
/// name tree. Name tree leaf maps (mydest) -> [3 0 R /Fit].
fn named_dest_nametree_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (N) /Parent 4 0 R /Dest (mydest) >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(mydest) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn dest_from_named_nametree() {
    let mut pdf = Pdf::open(Cursor::new(named_dest_nametree_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].dest, page_dest(3));
}

fn deep_named_dest_nametree_pdf(kid_levels: u32) -> Vec<u8> {
    let mut objects = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests 8 0 R >> >>"
                .to_string(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (
            4,
            "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_string(),
        ),
        (
            5,
            "<< /Title (Deep) /Parent 4 0 R /Dest (deep) >>".to_string(),
        ),
    ];

    for level in 0..kid_levels {
        let object_number = 8 + level;
        let next = object_number + 1;
        let limits = if level == 0 {
            ""
        } else {
            " /Limits [(deep) (deep)]"
        };
        objects.push((object_number, format!("<< /Kids [{next} 0 R]{limits} >>")));
    }
    objects.push((
        8 + kid_levels,
        "<< /Limits [(deep) (deep)] /Names [(deep) [3 0 R /Fit]] >>".to_string(),
    ));

    let refs: Vec<(u32, &str)> = objects
        .iter()
        .map(|(number, body)| (*number, body.as_str()))
        .collect();
    build_pdf(&refs, 1)
}

#[test]
fn named_destination_lookup_has_no_hidden_tree_depth_limit() {
    let mut pdf = Pdf::open(Cursor::new(deep_named_dest_nametree_pdf(101))).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].dest, page_dest(3));
}

#[test]
fn named_destination_lookup_selects_only_the_kid_covering_the_key() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests 8 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >>"),
            (5, "<< /Title (A) /Dest (a) /Next 6 0 R >>"),
            (6, "<< /Title (Target) /Dest (target) >>"),
            (8, "<< /Kids [9 0 R 10 0 R 11 0 R] >>"),
            (9, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
            (10, "<< /Limits [(h) (h)] /Names [(h) [3 0 R /Fit]] >>"),
            (
                11,
                "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].dest, page_dest(3));
    assert_eq!(tree[tree.roots()[1]].dest, page_dest(3));
    assert!(warning_messages(&pdf).is_empty());
}

#[test]
fn cyclic_modern_name_tree_lookup_terminates_without_a_destination() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests 8 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Cycle) /Dest (deep) >>"),
            (8, "<< /Kids [9 0 R] >>"),
            (9, "<< /Limits [(deep) (deep)] /Kids [9 0 R] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].dest, Object::Null);
}

#[test]
#[ignore = "live qpdf 11.9.0 oracle"]
fn qpdf_deep_named_destination_oracle_resolves_target() {
    use std::io::Write;
    use std::process::Command;

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&deep_named_dest_nametree_pdf(101)).unwrap();
    let output = Command::new("qpdf")
        .args(["--json=2", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["outlines"][0]["dest"][0], "3 0 R");
}

/// Legacy named dest: /Dest is a Name (/mydest) resolved via catalog /Dests
/// dictionary whose value is << /D [3 0 R /Fit] >>.
fn named_dest_legacy_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Dests 8 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (L) /Parent 4 0 R /Dest /mydest >>"),
            (8, "<< /mydest << /D [3 0 R /Fit] >> >>"),
        ],
        1,
    )
}

#[test]
fn dest_from_named_legacy() {
    let mut pdf = Pdf::open(Cursor::new(named_dest_legacy_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert!(matches!(roots[0].dest, Object::Dictionary(_)));
    assert_eq!(roots[0].dest_page(), Object::Null);
}

/// Legacy /Dests with a NAME->NAME cycle: /a -> /b, /b -> /a. qpdf performs
/// only one named lookup, so `/a` materializes as the raw alias `/b`.
fn cyclic_named_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Dests 8 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Cyc) /Parent 4 0 R /Dest /a >>"),
            (8, "<< /a /b /b /a >>"),
        ],
        1,
    )
}

#[test]
fn cyclic_named_dest_preserves_first_alias() {
    let mut pdf = Pdf::open(Cursor::new(cyclic_named_dest_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].dest, Object::Name(b"b".to_vec()));
}

/// The same dest name exists in BOTH the modern name tree and legacy /Dests.
/// The modern name-tree entry must win (it is resolved first).
fn named_dest_collision_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R /Dests 10 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (C) /Parent 4 0 R /Dest (dup) >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(dup) [3 0 R /Fit]] >>"),
            (10, "<< /dup [2 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn named_dest_modern_wins_over_legacy() {
    let mut pdf = Pdf::open(Cursor::new(named_dest_collision_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    // Modern name-tree entry ([3 0 R ...]) wins over legacy /Dests ([2 0 R ...]).
    assert_eq!(roots[0].dest, page_dest(3));
}

/// Outline item whose /Title is an INDIRECT reference (obj 9) to a string.
fn indirect_title_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title 9 0 R /Parent 4 0 R >>"),
            (9, "(RealTitle)"),
        ],
        1,
    )
}

#[test]
fn title_resolves_indirect_reference() {
    let mut pdf = Pdf::open(Cursor::new(indirect_title_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].title, "RealTitle");
}

/// The outline root's `/First` resolves to a non-dictionary object (a stray
/// integer): the walk must break out of that chain gracefully instead of
/// panicking or erroring.
fn outline_first_not_a_dict_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "42"),
        ],
        1,
    )
}

#[test]
fn get_tree_non_dict_first_item_materializes_raw_value() {
    let mut pdf = Pdf::open(Cursor::new(outline_first_not_a_dict_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].object, Object::Integer(42));
}

// -----------------------------------------------------------------------
// Raw outline `/A` destination, round-trip, and remap coverage
// -----------------------------------------------------------------------
//
// `remap_outline_and_dests` already remaps a `/A /GoTo /D` destination (see
// `outline_dest_remap.rs`, `remap_item_dest`) from earlier work on this
// epic. The regression coverage below keeps the surviving-page GoTo remap
// case without exposing a typed action API or changing the remapper itself.

/// Build a single-item outline whose lone item's `/A` is the literal
/// `action_body` (already wrapped in `<< ... >>` or a bare reference).
///
/// This helper reserves object numbers 1–5. If a test needs to embed
/// additional indirect objects (an indirect `/A` dict, an indirect `/D`
/// destination array, and so on), call `build_pdf` directly with obj
/// numbers ≥ 6 to avoid colliding with the fixed layout above. Existing
/// `action_goto_indirect_*_pdf` helpers pick obj 8/9 with 6-7 skipped
/// so the helper's own layout has room to grow before renumbering the
/// tests, but any free number ≥ 6 works.
fn action_pdf(action_body: &str) -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                &format!("<< /Title (Act) /Parent 4 0 R /A {action_body} >>"),
            ),
        ],
        1,
    )
}

fn single_outline_with_catalog(
    catalog_entries: &str,
    item_entries: &str,
    extra: &[(u32, &str)],
) -> Vec<u8> {
    let mut owned = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R {catalog_entries} >>"),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (
            4,
            "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_string(),
        ),
        (
            5,
            format!("<< /Title (One) /Parent 4 0 R {item_entries} >>"),
        ),
    ];
    owned.extend(
        extra
            .iter()
            .map(|(number, body)| (*number, (*body).to_string())),
    );
    let borrowed: Vec<(u32, &str)> = owned
        .iter()
        .map(|(number, body)| (*number, body.as_str()))
        .collect();
    build_pdf(&borrowed, 1)
}

#[test]
fn named_destination_lookup_handles_qpdf_node_shapes() {
    let cases = [
        (
            "/Names << /Dests << /Names [(shape) [3 0 R /Fit]] >> >>",
            &[][..],
            page_dest(3),
        ),
        (
            "/Names << /Dests << /Names [] /Kids [8 0 R] >> >>",
            &[(
                8,
                "<< /Limits [(shape) (shape)] /Names [(shape) [3 0 R /Fit]] >>",
            )][..],
            page_dest(3),
        ),
        ("/Names << /Dests << >> >>", &[][..], Object::Null),
        ("/Names << /Dests << /Kids [] >> >>", &[][..], Object::Null),
        ("/Names << /Dests 42 >>", &[][..], Object::Null),
        (
            "/Names << /Dests << /Names [42 [3 0 R /Fit]] >> >>",
            &[][..],
            Object::Null,
        ),
        (
            "/Names << /Dests << /Names [(a) [3 0 R /Fit]] >> >>",
            &[][..],
            Object::Null,
        ),
        (
            "/Names << /Dests << /Kids [8 0 R] >> >>",
            &[(8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>")][..],
            Object::Null,
        ),
        (
            "/Names << /Dests << /Kids [42] >> >>",
            &[][..],
            Object::Null,
        ),
        (
            "/Names << /Dests << /Kids [8 0 R] >> >>",
            &[(8, "<< /Names [42 [3 0 R /Fit]] >>")][..],
            Object::Null,
        ),
        (
            "/Names << /Dests << /Kids [8 0 R] >> >>",
            &[(8, "<< /Limits [42 42] /Names [(shape) [3 0 R /Fit]] >>")][..],
            page_dest(3),
        ),
    ];

    for (catalog_entries, extra, expected) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, "/Dest (shape)", extra);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        assert_eq!(root_items(&mut pdf)[0].dest, expected, "{catalog_entries}");
    }
}

#[test]
fn malformed_name_tree_structural_errors_warn_repair_and_retry() {
    let cases = [
        (
            "invalid leaf key",
            "/Names << /Dests << /Names [(a) [3 0 R /Fit] 42 [3 0 R /Fit] (z) [3 0 R /Fit]] >> >>",
            &[][..],
            "/Dest (m)",
            "attempting to repair after error: Name/Number tree node: item at index 2 is not the right type",
        ),
        (
            "targeted cycle",
            "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(m) (m)] /Kids [9 0 R] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "attempting to repair after error: Name/Number tree node (object 9): loop detected in find",
        ),
        (
            "bad node",
            "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(m) (m)] /Names [] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "attempting to repair after error: Name/Number tree node (object 9): bad node during find",
        ),
        (
            "binary search no candidate",
            "/Names << /Dests << /Kids [8 0 R 9 0 R] >> >>",
            &[
                (8, "<< /Limits [(m) (m)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (b)",
            "attempting to repair after error: Name/Number tree node: unexpected -1 from binary search of kids; limits may by wrong",
        ),
    ];

    for (label, catalog_entries, extra, item_entries, expected_warning) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, item_entries, extra);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert_eq!(root_items(&mut pdf)[0].dest, Object::Null, "{label}");
        assert_eq!(
            warning_messages(&pdf).first().copied(),
            Some(expected_warning),
            "{label}"
        );
        let dests = direct_dests_root(&mut pdf);
        assert_eq!(dests.get("Kids"), None, "{label}");
        assert!(
            matches!(dests.get("Names"), Some(Object::Array(_))),
            "{label}"
        );
    }
}

#[test]
fn malformed_name_tree_invalid_kid_is_skipped_while_valid_entries_are_retained() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R 42 9 0 R] >> >>",
        "/Dest (target)",
        &[
            (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
            (
                9,
                "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
            ),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    assert_eq!(
        warning_messages(&pdf),
        [
            "attempting to repair after error: Name/Number tree node: invalid kid at index 1",
            "skipping over invalid kid at index 1",
        ]
    );
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Kids"), None);
    assert_eq!(
        dests.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"a".to_vec()),
            page_dest(3),
            Object::String(b"target".to_vec()),
            page_dest(3),
        ]))
    );
}

#[test]
fn malformed_name_tree_zero_entry_repair_still_installs_an_empty_names_array() {
    let bytes =
        single_outline_with_catalog("/Names << /Dests << /Kids [42] >> >>", "/Dest (shape)", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
    assert_eq!(
        warning_messages(&pdf),
        [
            "attempting to repair after error: Name/Number tree node: invalid kid at index 0",
            "skipping over invalid kid at index 0",
        ]
    );
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Kids"), None);
    assert_eq!(dests.get("Names"), Some(&Object::Array(Vec::new())));
}

#[test]
#[ignore = "live qpdf 11.9.0 structural-repair oracle"]
fn qpdf_malformed_name_tree_structural_matrix_warns_and_repairs() {
    use std::io::Write;
    use std::process::Command;

    let cases = [
        (
            "/Names << /Dests << /Kids [8 0 R 42 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "invalid kid at index 1",
        ),
        (
            "/Names << /Dests << /Names [(a) [3 0 R /Fit] 42 [3 0 R /Fit] (z) [3 0 R /Fit]] >> >>",
            &[][..],
            "/Dest (m)",
            "item at index 2 is not the right type",
        ),
        (
            "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(m) (m)] /Kids [9 0 R] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "loop detected in find",
        ),
        (
            "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(m) (m)] /Names [] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "bad node during find",
        ),
        (
            "/Names << /Dests << /Kids [8 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(m) (m)] /Names [(a) [3 0 R /Fit]] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (b)",
            "unexpected -1 from binary search of kids; limits may by wrong",
        ),
    ];

    for (catalog_entries, extra, item_entries, expected_suffix) in cases {
        let mut input = tempfile::NamedTempFile::new().unwrap();
        input
            .write_all(&single_outline_with_catalog(
                catalog_entries,
                item_entries,
                extra,
            ))
            .unwrap();
        let output = Command::new("qpdf")
            .args(["--json=2", "--json-key=outlines", "--json-key=qpdf"])
            .arg(input.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(3),
            "{expected_suffix}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_suffix),
            "{expected_suffix}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let repaired_root = &json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"];
        assert!(repaired_root.get("/Kids").is_none(), "{expected_suffix}");
        assert!(repaired_root["/Names"].is_array(), "{expected_suffix}");
    }
}

fn missing_name_tree_limits_pdf() -> Vec<u8> {
    single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R] >> >>",
        "/Dest (shape)",
        &[(8, "<< /Names [(shape) [3 0 R /Fit]] >>")],
    )
}

#[test]
fn missing_name_tree_limits_repairs_and_mutates_the_existing_direct_root() {
    let mut pdf = Pdf::open(Cursor::new(missing_name_tree_limits_pdf())).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    assert_eq!(
        warning_messages(&pdf),
        vec![
            "attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits"
        ]
    );

    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    let Object::Dictionary(names) = catalog.get("Names").unwrap() else {
        panic!("direct /Names must remain direct");
    };
    let Object::Dictionary(dests) = names.get("Dests").unwrap() else {
        panic!("direct /Dests root must remain direct");
    };
    assert_eq!(dests.get("Kids"), None);
    assert_eq!(
        dests.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"shape".to_vec()),
            page_dest(3),
        ]))
    );
    assert!(matches!(
        pdf.resolve(ObjectRef::new(8, 0)).unwrap(),
        Object::Dictionary(_)
    ));

    let mut serialized = Vec::new();
    write_pdf(&mut pdf, &mut serialized).unwrap();
    let mut reopened = Pdf::open(Cursor::new(serialized)).unwrap();
    let catalog_ref = reopened.root_ref().unwrap();
    let Object::Dictionary(catalog) = reopened.resolve(catalog_ref).unwrap() else {
        panic!("reopened catalog must be a dictionary");
    };
    let Object::Dictionary(names) = catalog.get("Names").unwrap() else {
        panic!("reopened direct /Names must remain direct");
    };
    let Object::Dictionary(dests) = names.get("Dests").unwrap() else {
        panic!("reopened direct /Dests root must remain direct");
    };
    assert_eq!(dests.get("Kids"), None);
    assert!(matches!(dests.get("Names"), Some(Object::Array(_))));
    assert!(matches!(
        reopened.resolve(ObjectRef::new(8, 0)).unwrap(),
        Object::Dictionary(_)
    ));
}

#[test]
#[ignore = "live qpdf 11.9.0 oracle"]
fn qpdf_missing_name_tree_limits_oracle_repairs_the_lookup() {
    use std::io::Write;
    use std::process::Command;

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&missing_name_tree_limits_pdf()).unwrap();
    let output = Command::new("qpdf")
        .args(["--json=2", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("attempting to repair after error:"));
    assert!(stderr.contains("(Name/Number tree node (object 8)): node is missing /Limits"));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["outlines"][0]["dest"][0], "3 0 R");
}

#[test]
fn missing_name_tree_limits_repairs_the_terminal_indirect_root_without_collapsing_holders() {
    let bytes = single_outline_with_catalog(
        "/Names 20 0 R",
        "/Dest (shape)",
        &[
            (8, "<< /Names [(shape) [3 0 R /Fit]] >>"),
            (20, "<< /Dests 21 0 R >>"),
            (21, "22 0 R"),
            (22, "<< /Kids [8 0 R] >>"),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    assert_eq!(
        warning_messages(&pdf),
        vec![
            "attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits"
        ]
    );

    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Names"),
        Some(&Object::Reference(ObjectRef::new(20, 0)))
    );
    let Object::Dictionary(names) = pdf.resolve(ObjectRef::new(20, 0)).unwrap() else {
        panic!("indirect /Names holder must remain a dictionary");
    };
    assert_eq!(
        names.get("Dests"),
        Some(&Object::Reference(ObjectRef::new(21, 0)))
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(21, 0)).unwrap(),
        Object::Reference(ObjectRef::new(22, 0))
    );
    let Object::Dictionary(dests) = pdf.resolve(ObjectRef::new(22, 0)).unwrap() else {
        panic!("terminal /Dests root must remain a dictionary");
    };
    assert_eq!(dests.get("Kids"), None);
    assert!(matches!(dests.get("Names"), Some(Object::Array(_))));
}

#[test]
fn malformed_name_tree_repair_enumerates_all_reachable_branches_and_terminates_cycles() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R 11 0 R] >> >>",
        "/Dest (target)",
        &[
            (8, "42"),
            (9, "<< /Limits [(a) (a)] /Kids [9 0 R] >>"),
            (10, "<< /Names [(target) [3 0 R /Fit]] >>"),
            (11, "<< /Limits [(z) (z)] /Names [42 [3 0 R /Fit]] >>"),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    assert_eq!(
        warning_messages(&pdf),
        vec![
            "attempting to repair after error: Name/Number tree node (object 10): node is missing /Limits",
            "skipping over invalid kid at index 0",
            "loop detected while traversing name/number tree",
            "item 0 has the wrong type",
        ]
    );
}

fn malformed_name_tree_split_pdf() -> Vec<u8> {
    let pairs = (0..33)
        .map(|index| format!("(k{index:02}) [3 0 R /Fit]"))
        .collect::<Vec<_>>()
        .join(" ");
    let leaf = format!("<< /Names [{pairs}] >>");
    single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R] >> >>",
        "/Dest (k17)",
        &[(8, leaf.as_str())],
    )
}

#[test]
fn malformed_name_tree_repair_rebuilds_more_than_one_leaf() {
    let mut pdf = Pdf::open(Cursor::new(malformed_name_tree_split_pdf())).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    let dests = direct_dests_root(&mut pdf);
    let Some(Object::Array(kids)) = dests.get("Kids") else {
        panic!("repaired root must contain /Kids");
    };
    assert_eq!(
        kids.as_slice(),
        &[
            Object::Reference(ObjectRef::new(9, 0)),
            Object::Reference(ObjectRef::new(10, 0)),
        ]
    );
    assert_eq!(dests.get("Names"), None);

    let Object::Dictionary(first) = pdf.resolve(ObjectRef::new(9, 0)).unwrap() else {
        panic!("first repaired leaf must be a dictionary");
    };
    let Object::Dictionary(second) = pdf.resolve(ObjectRef::new(10, 0)).unwrap() else {
        panic!("second repaired leaf must be a dictionary");
    };
    assert!(matches!(first.get("Names"), Some(Object::Array(names)) if names.len() == 32));
    assert!(matches!(second.get("Names"), Some(Object::Array(names)) if names.len() == 34));
    assert_eq!(
        first.get("Limits"),
        Some(&Object::Array(vec![
            Object::String(b"k00".to_vec()),
            Object::String(b"k15".to_vec()),
        ]))
    );
    assert_eq!(
        second.get("Limits"),
        Some(&Object::Array(vec![
            Object::String(b"k16".to_vec()),
            Object::String(b"k32".to_vec()),
        ]))
    );
}

#[test]
fn malformed_name_tree_repair_reproduces_qpdf_parent_split_order() {
    let pairs = (0..529)
        .map(|index| format!("(k{index:04}) [3 0 R /Fit]"))
        .collect::<Vec<_>>()
        .join(" ");
    let leaf = format!("<< /Names [{pairs}] >>");
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R] >> >>",
        "/Dest (k0528)",
        &[(8, leaf.as_str())],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    let dests = direct_dests_root(&mut pdf);
    let Some(Object::Array(root_kids)) = dests.get("Kids") else {
        panic!("repaired root must contain /Kids");
    };
    assert_eq!(root_kids.len(), 2);
    let Object::Reference(first_parent_ref) = root_kids[0] else {
        panic!("first repaired parent must be indirect");
    };
    let Object::Reference(second_parent_ref) = root_kids[1] else {
        panic!("second repaired parent must be indirect");
    };
    let Object::Dictionary(first_parent) = pdf.resolve(first_parent_ref).unwrap() else {
        panic!("first repaired parent must be a dictionary");
    };
    let Object::Dictionary(second_parent) = pdf.resolve(second_parent_ref).unwrap() else {
        panic!("second repaired parent must be a dictionary");
    };
    assert!(matches!(first_parent.get("Kids"), Some(Object::Array(kids)) if kids.len() == 16));
    assert!(matches!(second_parent.get("Kids"), Some(Object::Array(kids)) if kids.len() == 17));
    assert_eq!(
        first_parent.get("Limits"),
        Some(&Object::Array(vec![
            Object::String(b"k0000".to_vec()),
            Object::String(b"k0255".to_vec()),
        ]))
    );
    assert_eq!(
        second_parent.get("Limits"),
        Some(&Object::Array(vec![
            Object::String(b"k0256".to_vec()),
            Object::String(b"k0528".to_vec()),
        ]))
    );
}

#[test]
fn malformed_name_tree_repair_warns_for_a_short_names_array_and_visits_an_empty_leaf() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R 9 0 R] >> >>",
        "/Dest (shape)",
        &[
            (8, "<< /Names [(shape) [3 0 R /Fit] (dangling)] >>"),
            (9, "<< /Limits [(z) (z)] /Names [] >>"),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    assert_eq!(
        warning_messages(&pdf),
        [
            "attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits",
            "items array doesn't have enough elements",
        ]
    );
}

#[test]
#[ignore = "live qpdf 11.9.0 full-object oracle"]
fn qpdf_malformed_name_tree_repair_splits_33_pairs_as_16_then_17() {
    use std::io::Write;
    use std::process::Command;

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&malformed_name_tree_split_pdf()).unwrap();
    let output = Command::new("qpdf")
        .args(["--json=2", "--json-key=outlines", "--json-key=qpdf"])
        .arg(input.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["outlines"][0]["dest"][0], "3 0 R");
    let objects = json["qpdf"][1].as_object().unwrap();
    let repaired_root = &objects["obj:1 0 R"]["value"]["/Names"]["/Dests"];
    assert_eq!(repaired_root["/Kids"].as_array().unwrap().len(), 2);
    assert!(repaired_root.get("/Names").is_none());

    let mut leaves = objects
        .values()
        .filter_map(|object| {
            let value = &object["value"];
            let names = value.get("/Names")?.as_array()?;
            let limits = value.get("/Limits")?.as_array()?;
            Some((names.len(), limits.clone()))
        })
        .collect::<Vec<_>>();
    leaves.sort_by(|left, right| left.1[0].as_str().cmp(&right.1[0].as_str()));
    assert_eq!(
        leaves,
        vec![
            (
                32,
                vec![
                    serde_json::Value::String("u:k00".to_string()),
                    serde_json::Value::String("u:k15".to_string()),
                ],
            ),
            (
                34,
                vec![
                    serde_json::Value::String("u:k16".to_string()),
                    serde_json::Value::String("u:k32".to_string()),
                ],
            ),
        ]
    );
}

#[test]
fn malformed_name_tree_repair_updates_a_direct_root_inside_indirect_names() {
    let bytes = single_outline_with_catalog(
        "/Names 20 0 R",
        "/Dest (shape)",
        &[
            (8, "<< /Names [(shape) [3 0 R /Fit]] >>"),
            (20, "<< /Dests << /Kids [8 0 R] >> >>"),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve(catalog_ref).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Names"),
        Some(&Object::Reference(ObjectRef::new(20, 0)))
    );
    let Object::Dictionary(names) = pdf.resolve(ObjectRef::new(20, 0)).unwrap() else {
        panic!("indirect /Names must remain a dictionary");
    };
    let Object::Dictionary(dests) = names.get("Dests").unwrap() else {
        panic!("direct /Dests root must remain direct");
    };
    assert_eq!(dests.get("Kids"), None);
    assert!(matches!(dests.get("Names"), Some(Object::Array(_))));
}

fn qpdf_destination_matrix_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 7 0 R /Count 3 >>"),
            (5, "<< /Title (Array) /Parent 4 0 R /Next 6 0 R /A [10 0 R] >>"),
            (6, "<< /Title (Integer) /Parent 4 0 R /Prev 5 0 R /Next 7 0 R /Dest 42 /A << /S /GoTo /D [3 0 R /Fit] >> >>"),
            (7, "<< /Title (GoTo) /Parent 4 0 R /Prev 6 0 R /A << /S /GoTo /D [3 0 R /Fit] >> >>"),
            (10, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn qpdf_destination_matrix_matches_raw_objects() {
    let mut pdf = Pdf::open(Cursor::new(qpdf_destination_matrix_pdf())).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].dest, Object::Null);
    assert_eq!(roots[1].dest, Object::Integer(42));
    assert_eq!(roots[2].dest, page_dest(3));
}

#[test]
#[ignore = "live qpdf 11.9.0 oracle"]
fn qpdf_outline_destination_oracle_matches_expected_matrix() {
    use std::io::Write;
    use std::process::Command;

    let bytes = qpdf_destination_matrix_pdf();
    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&bytes).unwrap();

    let output = Command::new("qpdf")
        .args(["--json", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let dests: Vec<serde_json::Value> = json["outlines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|outline| outline["dest"].clone())
        .collect();
    assert_eq!(
        dests,
        vec![
            serde_json::Value::Null,
            serde_json::json!(42),
            serde_json::json!(["3 0 R", "/Fit"]),
        ]
    );
}

#[test]
fn dest_key_presence_suppresses_valid_action_fallback() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (P) /Parent 4 0 R /Dest 42 /A << /S /GoTo /D [3 0 R /Fit] >> >>",
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, Object::Integer(42));
}

#[test]
fn root_action_array_is_not_an_action_dictionary() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (A) /Parent 4 0 R /A [10 0 R] >>"),
            (10, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
}

#[test]
fn candidate_type_selects_only_qpdf_named_destination_store() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R /Dests 10 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >>"),
            (
                5,
                "<< /Title (Name) /Parent 4 0 R /Next 6 0 R /Dest /dup >>",
            ),
            (
                6,
                "<< /Title (String) /Parent 4 0 R /Prev 5 0 R /Dest (dup) >>",
            ),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(dup) [3 0 R /Fit]] >>"),
            (10, "<< /dup [2 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(roots[0].dest, page_dest(2));
    assert_eq!(roots[1].dest, page_dest(3));
}

#[test]
fn malformed_or_non_goto_actions_have_null_destination() {
    for action in [
        "<< /S /GoTo >>",
        "<< /D [3 0 R /Fit] >>",
        "<< /S 42 /D [3 0 R /Fit] >>",
        "<< /S /URI /D [3 0 R /Fit] >>",
        "<< /S /GoTo /D null >>",
        "<< /S /GoTo /SD [3 0 R /Fit] >>",
        "(not a dictionary)",
    ] {
        let mut pdf = Pdf::open(Cursor::new(action_pdf(action))).unwrap();
        assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
    }
}

#[test]
fn unresolved_dest_name_suppresses_action_fallback() {
    let bytes = single_outline_with_catalog(
        "/Dests << >>",
        "/Dest /missing /A << /S /GoTo /D [3 0 R /Fit] >>",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
}

#[test]
fn missing_named_candidate_store_paths_have_null_destination() {
    let cases = [
        (
            "Name candidate with no legacy /Dests",
            "/Names << /Dests 8 0 R >>",
            "/Dest /onlymodern",
            (8, "<< /Names [(onlymodern) [3 0 R /Fit]] >>"),
        ),
        (
            "String candidate with no /Names",
            "/Dests << /onlylegacy [3 0 R /Fit] >>",
            "/Dest (onlylegacy)",
            (8, "null"),
        ),
        (
            "String candidate with /Names but no /Dests",
            "/Names << /Other 8 0 R >> /Dests << /onlylegacy [3 0 R /Fit] >>",
            "/Dest (onlylegacy)",
            (8, "null"),
        ),
        (
            "String candidate missing from the /Dests name tree",
            "/Names << /Dests 8 0 R >>",
            "/Dest (missing)",
            (8, "<< /Names [(other) [3 0 R /Fit]] >>"),
        ),
    ];

    for (label, catalog_entries, item_entries, extra) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, item_entries, &[extra]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        assert_eq!(root_items(&mut pdf)[0].dest, Object::Null, "{label}");
    }
}

#[test]
fn utf16_string_key_uses_qpdf_utf8_value() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests 8 0 R >>",
        "/Dest <FEFF540D524D>",
        &[(8, "<< /Names [<FEFF540D524D> [3 0 R /Fit]] >>")],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
}

/// qpdf keeps bytes after an explicit UTF-8 BOM raw for both the outline
/// candidate and stored name-tree key. Lookup normalizes only the candidate
/// through `newUnicodeString`, so the two identical malformed byte strings do
/// not compare equal (`U+FFFD` needle versus raw `0xff` stored key).
fn malformed_explicit_utf8_named_dest_pdf() -> Vec<u8> {
    single_outline_with_catalog(
        "/Names << /Dests 8 0 R >>",
        "/Dest <EFBBBFFF>",
        &[(8, "<< /Names [<EFBBBFFF> [3 0 R /Fit]] >>")],
    )
}

#[test]
fn malformed_explicit_utf8_candidate_does_not_resolve_same_raw_key() {
    let mut pdf = Pdf::open(Cursor::new(malformed_explicit_utf8_named_dest_pdf())).unwrap();
    assert_eq!(
        root_items(&mut pdf)[0].dest,
        Object::Null,
        "candidate normalization must not create a match against the raw malformed stored key"
    );
}

#[test]
#[ignore = "live qpdf 11.9.0 oracle"]
fn qpdf_malformed_explicit_utf8_named_dest_oracle_is_null() {
    use std::io::Write;
    use std::process::Command;

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input
        .write_all(&malformed_explicit_utf8_named_dest_pdf())
        .unwrap();
    let output = Command::new("qpdf")
        .args(["--json", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["outlines"][0]["dest"], serde_json::Value::Null);
}

#[test]
fn named_destination_preserves_dictionary_shape() {
    let bytes = single_outline_with_catalog(
        "/Dests << /dict << /D [3 0 R /Fit] >> >>",
        "/Dest /dict",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let node = root_items(&mut pdf).remove(0);
    assert!(matches!(node.dest, Object::Dictionary(_)));
    assert_eq!(node.dest_page(), Object::Null);
}

#[test]
fn empty_destination_array_has_null_dest_page() {
    let bytes = single_outline_with_catalog("", "/Dest []", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let node = root_items(&mut pdf).remove(0);
    assert_eq!(node.dest, Object::Array(Vec::new()));
    assert_eq!(node.dest_page(), Object::Null);
}

#[test]
fn named_destination_materializes_indirect_result_holder() {
    let bytes = single_outline_with_catalog(
        "/Dests << /held 8 0 R >>",
        "/Dest /held",
        &[(8, "[3 0 R /Fit]")],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
}

fn raw_action(pdf: &mut Pdf<Cursor<Vec<u8>>>, item_ref: ObjectRef) -> Object {
    let Object::Dictionary(item) = pdf.resolve(item_ref).unwrap() else {
        panic!("outline item must be a dictionary");
    };
    item.get("A").cloned().unwrap_or(Object::Null)
}

fn resolved_raw_action(pdf: &mut Pdf<Cursor<Vec<u8>>>, item_ref: ObjectRef) -> Object {
    let mut value = raw_action(pdf, item_ref);
    let mut seen = BTreeSet::new();
    while let Object::Reference(reference) = value {
        assert!(seen.insert(reference), "cycle in test action holder");
        value = pdf.resolve(reference).unwrap();
    }
    value
}

#[test]
fn action_goto_direct_d_is_the_node_destination() {
    let mut pdf = Pdf::open(Cursor::new(action_pdf("<< /S /GoTo /D [3 0 R /Fit] >>"))).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
}

/// GoTo action whose `/D` is an INDIRECT reference (obj 8, using the ≥6
/// reserved range documented on `action_pdf`) to the dest array.
fn action_goto_indirect_d_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S /GoTo /D 8 0 R >> >>",
            ),
            (8, "[3 0 R /Fit]"),
        ],
        1,
    )
}

#[test]
fn action_goto_indirect_d_is_the_node_destination() {
    let mut pdf = Pdf::open(Cursor::new(action_goto_indirect_d_pdf())).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
}

/// The outline item's `/A` itself is an INDIRECT reference (obj 9) to the
/// action dictionary, per review rule 2 ("/A は間接参照で来うる").
fn action_indirect_a_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Act) /Parent 4 0 R /A 9 0 R >>"),
            (9, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn action_indirect_a_contributes_the_node_destination() {
    let mut pdf = Pdf::open(Cursor::new(action_indirect_a_pdf())).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
}

/// Regression: `/A /S` stored as an indirect reference (obj 8) to a Name.
/// The destination fallback path must see through the holder reference.
#[test]
fn resolve_node_dest_follows_indirect_s_name() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S 8 0 R /D [3 0 R /Fit] >> >>",
            ),
            (8, "/GoTo"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let root = root_items(&mut pdf);
    assert_eq!(
        root[0].dest_page(),
        Object::Reference(ObjectRef::new(3, 0)),
        "GoTo /D must be picked up even when /S is an indirect ref"
    );
}

#[test]
fn action_non_dict_value_has_null_destination() {
    let mut pdf = Pdf::open(Cursor::new(action_pdf("(not a dict)"))).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
}

/// A non-standard action subtype (`/SubmitForm`) with arbitrary keys,
/// including an indirect `/F` pointing at an unrelated dictionary.
fn action_unknown_subtype_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S /SubmitForm /F 9 0 R /Flags 4 >> >>",
            ),
            (9, "<< /FS /URL /F (https://example.com/submit) >>"),
        ],
        1,
    )
}

// ── Round-trip ───────────────────────────────────────────────────────────

/// Five-item outline, one item per action subtype (GoTo/GoToR/URI/Launch/Named).
fn multi_action_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 9 0 R /Count 5 >>"),
            (
                5,
                "<< /Title (GoTo) /Parent 4 0 R /Next 6 0 R \
                 /A << /S /GoTo /D [3 0 R /Fit] >> >>",
            ),
            (
                6,
                "<< /Title (GoToR) /Parent 4 0 R /Prev 5 0 R /Next 7 0 R \
                 /A << /S /GoToR /F (other.pdf) /D [0 /Fit] >> >>",
            ),
            (
                7,
                "<< /Title (URI) /Parent 4 0 R /Prev 6 0 R /Next 8 0 R \
                 /A << /S /URI /URI (https://example.com) >> >>",
            ),
            (
                8,
                "<< /Title (Launch) /Parent 4 0 R /Prev 7 0 R /Next 9 0 R \
                 /A << /S /Launch /F (app.exe) >> >>",
            ),
            (
                9,
                "<< /Title (Named) /Parent 4 0 R /Prev 8 0 R \
                 /A << /S /Named /N /NextPage >> >>",
            ),
        ],
        1,
    )
}

#[test]
fn action_round_trip_through_write_pdf_unmodified() {
    let mut pdf = Pdf::open(Cursor::new(multi_action_pdf())).unwrap();
    let refs: Vec<ObjectRef> = root_items(&mut pdf)
        .into_iter()
        .map(|item| item.source_ref.expect("fixture items are indirect"))
        .collect();
    let before: Vec<Object> = refs.iter().map(|&r| raw_action(&mut pdf, r)).collect();
    assert_eq!(refs.len(), 5, "sanity: fixture has 5 outline items");

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let after: Vec<Object> = refs.iter().map(|&r| raw_action(&mut reopened, r)).collect();
    assert_eq!(
        before, after,
        "every raw /A object must round-trip unmodified through write_pdf"
    );
}

// ── GoTo remap on page renumber ──────────────────────────────────────────

/// Two-page document; the outline item's `/A /GoTo /D` targets the SECOND
/// page (obj 30) explicitly by reference.
fn action_goto_two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 30 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                30,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S /GoTo /D [30 0 R /Fit] >> >>",
            ),
        ],
        1,
    )
}

/// Selecting a page more than once (e.g. `qpdf --pages . 1,1`) clones the
/// second-and-later occurrences to fresh object numbers, while the first
/// occurrence keeps the source page's original ref (see
/// `page_tree_rebuild.rs`'s "First occurrence: mutate the existing object in
/// place" branch — [`crate`]-level `rebuild_page_tree` never renumbers a
/// singly-selected page). Selecting page 30 twice below is what makes this
/// test meaningful: it proves a GoTo action's `/D` is remapped to the FIRST
/// occurrence, not silently left pointing at (or accidentally rewritten to)
/// the second, unrelated clone — the same property
/// `duplicate_selection_uses_first_new_ref` in `outline_dest_remap.rs`
/// verifies for a plain `/Dest`.
#[test]
fn action_goto_dest_remapped_to_first_occurrence_of_duplicated_page() {
    let mut pdf = Pdf::open(Cursor::new(action_goto_two_page_pdf())).unwrap();
    let result = flpdf::rebuild_page_tree(
        &mut pdf,
        &[
            ObjectRef::new(3, 0),
            ObjectRef::new(30, 0),
            ObjectRef::new(30, 0),
        ],
    )
    .unwrap();
    assert_eq!(
        result.ref_map[&ObjectRef::new(30, 0)].len(),
        2,
        "sanity: page 30 was selected twice"
    );
    let first_new = result.ref_map[&ObjectRef::new(30, 0)][0];
    flpdf::remap_outline_and_dests(&mut pdf, &result).unwrap();

    assert_eq!(
        root_items(&mut pdf)[0].dest,
        Object::Array(vec![
            Object::Reference(first_new),
            Object::Name(b"Fit".to_vec()),
        ]),
        "a GoTo action /D must remap to the first occurrence of a duplicated page"
    );
}

/// Same two-page shape, but the GoTo action's `/D` is a NAMED destination
/// (a string naming an entry in the `/Names /Dests` tree) rather than an
/// explicit array.
fn action_goto_named_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 30 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                30,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S /GoTo /D (mydest) >> >>",
            ),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(mydest) [30 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn action_goto_named_dest_kept_verbatim_while_name_tree_remaps() {
    let mut pdf = Pdf::open(Cursor::new(action_goto_named_dest_pdf())).unwrap();
    let result = flpdf::rebuild_page_tree(&mut pdf, &[ObjectRef::new(30, 0)]).unwrap();
    let new_p2 = result.ref_map[&ObjectRef::new(30, 0)][0];
    flpdf::remap_outline_and_dests(&mut pdf, &result).unwrap();

    let Object::Dictionary(action) = resolved_raw_action(&mut pdf, ObjectRef::new(5, 0)) else {
        panic!("/A must resolve to a dictionary");
    };
    assert_eq!(
        action.get("D"),
        Some(&Object::String(b"mydest".to_vec())),
        "the named GoTo action keeps the literal destination name"
    );
    assert_eq!(
        root_items(&mut pdf)[0].dest,
        Object::Array(vec![
            Object::Reference(new_p2),
            Object::Name(b"Fit".to_vec()),
        ]),
        "the named GoTo destination must resolve through the remapped name tree"
    );

    // The name tree's raw "mydest" destination array is what gets remapped.
    let Object::Dictionary(dests) = pdf.resolve(ObjectRef::new(9, 0)).unwrap() else {
        panic!("/Names /Dests leaf must remain a dictionary");
    };
    let Object::Array(entries) = dests.get("Names").unwrap() else {
        panic!("name-tree leaf must retain its raw /Names array");
    };
    let Object::Array(dest) = &entries[1] else {
        panic!("mydest value must remain a raw destination array");
    };
    assert_eq!(dest[0], Object::Reference(new_p2));
}

/// GoToR's `/D` looks like a local page reference (`30 0 R`), but a remote
/// destination must never be remapped even when that ref happens to also be
/// a page in THIS document that survives the rebuild.
fn action_gotor_two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 30 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                30,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R \
                 /A << /S /GoToR /F (other.pdf) /D [30 0 R /Fit] >> >>",
            ),
        ],
        1,
    )
}

#[test]
fn action_gotor_dest_left_unchanged_after_page_rebuild() {
    let mut pdf = Pdf::open(Cursor::new(action_gotor_two_page_pdf())).unwrap();
    let result = flpdf::rebuild_page_tree(&mut pdf, &[ObjectRef::new(30, 0)]).unwrap();
    flpdf::remap_outline_and_dests(&mut pdf, &result).unwrap();

    let Object::Dictionary(action) = resolved_raw_action(&mut pdf, ObjectRef::new(5, 0)) else {
        panic!("/A must resolve to a dictionary");
    };
    assert_eq!(action.get("S"), Some(&Object::Name(b"GoToR".to_vec())));
    assert_eq!(
        action.get("F"),
        Some(&Object::String(b"other.pdf".to_vec()))
    );
    assert_eq!(
        action.get("D").unwrap().as_array().unwrap()[0],
        Object::Reference(ObjectRef::new(30, 0)),
        "a GoToR /D is never a local destination and must be left verbatim"
    );
}

/// A URI action's target must be preserved byte-for-byte across a page
/// rebuild — it never carries a page reference at all.
#[test]
fn action_uri_left_unchanged_after_page_rebuild() {
    let mut pdf = Pdf::open(Cursor::new(action_pdf(
        "<< /S /URI /URI (https://example.com/x) >>",
    )))
    .unwrap();
    let result = flpdf::rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
    flpdf::remap_outline_and_dests(&mut pdf, &result).unwrap();

    let Object::Dictionary(action) = resolved_raw_action(&mut pdf, ObjectRef::new(5, 0)) else {
        panic!("/A must resolve to a dictionary");
    };
    assert_eq!(action.get("S"), Some(&Object::Name(b"URI".to_vec())));
    assert_eq!(
        action.get("URI"),
        Some(&Object::String(b"https://example.com/x".to_vec()))
    );
}

/// An unknown-subtype action's fields (including an indirect `/F` to an
/// unrelated dictionary) must never be touched by the page-rebuild remap.
#[test]
fn action_unknown_subtype_unchanged_after_page_rebuild() {
    let mut pdf = Pdf::open(Cursor::new(action_unknown_subtype_pdf())).unwrap();
    let result = flpdf::rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
    flpdf::remap_outline_and_dests(&mut pdf, &result).unwrap();

    let Object::Dictionary(action) = resolved_raw_action(&mut pdf, ObjectRef::new(5, 0)) else {
        panic!("/A must resolve to a dictionary");
    };
    assert_eq!(action.get("S"), Some(&Object::Name(b"SubmitForm".to_vec())));
    assert_eq!(action.get_ref("F"), Some(ObjectRef::new(9, 0)));
}

/// A multi-hop holder chain on outline `/A` still contributes its GoTo `/D`.
#[test]
fn outline_destination_resolves_through_multi_hop_action_holder_chain() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Act) /Parent 4 0 R /A 8 0 R >>"),
            (8, "9 0 R"),
            (9, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, page_dest(3));
}

#[test]
fn outline_action_null_d_has_null_destination() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (N) /Parent 4 0 R /A << /S /GoTo /D null >> >>",
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
}

/// qpdf ignores `/SD` when a GoTo action has no `/D`.
#[test]
fn outline_action_sd_without_d_has_null_destination() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (S) /Parent 4 0 R /A << /S /GoTo /SD [3 0 R /Fit] >> >>",
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    assert_eq!(root_items(&mut pdf)[0].dest, Object::Null);
}
