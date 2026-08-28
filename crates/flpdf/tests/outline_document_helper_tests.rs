//! Integration tests for [`flpdf::OutlineDocumentHelper`].

use flpdf::job::{JsonJobOptions, JsonJobOutput, JsonStreamData, QPDFJob};
use flpdf::json_inspect::{DecodeLevel, JsonKey};
use flpdf::{Dictionary, Error, Object, ObjectHandle, ObjectRef, OutlineItem, Pdf};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

mod common;
use common::PdfCanonicalTestExt;
use common::{remap_object_refs, write_with_settings_and_mapping, WriterTestSettings};

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

fn materialized(handle: &ObjectHandle) -> Object {
    handle.materialize().unwrap()
}

fn outline_object(item: &OutlineItem) -> Object {
    materialized(&item.object)
}

fn outline_dest(item: &OutlineItem, pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Object {
    let mut helper = pdf.outline();
    materialized(&item.get_dest(&mut helper).unwrap())
}

fn outline_dest_page(item: &OutlineItem, pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Object {
    let mut helper = pdf.outline();
    let page = item.get_dest_page(&mut helper).unwrap();
    page.object_ref()
        .map(Object::Reference)
        .unwrap_or_else(|| materialized(&page))
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
fn get_outlines_for_page_uses_qpdf_breadth_first_order() {
    let mut pdf = Pdf::open(Cursor::new(page_index_outline_pdf())).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();

    let titles: Vec<_> = tree
        .get_outlines_for_page(&mut helper, Some(ObjectRef::new(3, 0)))
        .unwrap()
        .map(|(_id, item)| item.get_title(&mut helper).unwrap())
        .collect();

    assert_eq!(titles, ["A", "B", "A1", "B1"]);
}

#[test]
fn get_outlines_for_page_none_matches_qpdf_objgen_zero_bucket() {
    let mut pdf = Pdf::open(Cursor::new(page_index_outline_pdf())).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();

    let titles: Vec<_> = tree
        .get_outlines_for_page(&mut helper, None)
        .unwrap()
        .map(|(_id, item)| item.get_title(&mut helper).unwrap())
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
        .get_outlines_for_page(&mut helper, Some(ObjectRef::new(0, 0)))
        .unwrap()
        .map(|(_id, item)| item.get_title(&mut helper).unwrap())
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

fn warning_messages(pdf: &Pdf<Cursor<Vec<u8>>>) -> Vec<String> {
    // Owned rather than borrowed: `repair_diagnostics` hands back a snapshot,
    // so a `Vec<&str>` here would borrow from a temporary this function drops.
    pdf.repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect()
}

fn write_outlines_json(pdf: &mut Pdf<Cursor<Vec<u8>>>) {
    let keys = [JsonKey::Outlines];
    let options = JsonJobOptions {
        decode_level: DecodeLevel::Generalized,
        stream_data: JsonStreamData::None,
        stream_prefix: None,
        keys: &keys,
        objects: &[],
    };
    let mut job = QPDFJob::new();
    job.set_suppress_warnings(true);
    let mut output = Vec::new();
    job.write_json(pdf, options, JsonJobOutput::Stdout(&mut output))
        .expect("outline JSON should be written");
}

fn direct_dests_root(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Dictionary {
    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve_canonical_object(catalog_ref).unwrap() else {
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
        let mut helper = pdf.outline();
        let tree = helper.get_tree().unwrap();
        assert_eq!(
            tree[tree.roots()[0]].get_title(&mut helper).unwrap(),
            expected,
            "{title_object}"
        );
        let warnings = warning_messages(&pdf);
        match expected_warning {
            Some(expected_warning) => {
                assert_eq!(warnings.len(), 1, "{title_object}");
                assert!(warnings[0].ends_with(expected_warning), "{title_object}");
            }
            None => assert!(warnings.is_empty(), "{title_object}"),
        }
    }

    let mut pdf = Pdf::open(Cursor::new(single_outline_without_title())).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "");
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
        let mut helper = pdf.outline();
        let tree = helper.get_tree().unwrap();
        assert_eq!(
            tree[tree.roots()[0]].get_count(&mut helper).unwrap(),
            expected,
            "{count_object}"
        );
        let warning_messages = warning_messages(&pdf);
        match expected_warning {
            Some(expected_warning) => {
                assert!(
                    warning_messages
                        .iter()
                        .any(|m| m.ends_with(expected_warning)),
                    "{count_object}"
                )
            }
            None => assert!(warning_messages.is_empty(), "{count_object}"),
        }
    }

    let mut pdf = Pdf::open(Cursor::new(single_outline_with_item_fields(""))).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].get_count(&mut helper).unwrap(), 0);
    assert!(warning_messages(&pdf).is_empty());
}

#[test]
fn present_wrong_type_scalar_warnings_use_qpdf_object_type_names() {
    let cases = [
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
            let mut helper = pdf.outline();
            let tree = helper.get_tree().unwrap();
            assert_eq!(
                tree[tree.roots()[0]].get_title(&mut helper).unwrap(),
                "",
                "title {object}"
            );
            let warnings = warning_messages(&pdf);
            assert_eq!(warnings.len(), 1, "title {object}");
            assert!(
                warnings[0].ends_with(&format!(
                    "operation for string attempted on object of type {type_name}: returning empty string"
                )),
                "title {object}"
            );
        }

        if type_name != "integer" {
            let mut pdf = Pdf::open(Cursor::new(single_outline_with_count(object))).unwrap();
            let mut helper = pdf.outline();
            let tree = helper.get_tree().unwrap();
            assert_eq!(
                tree[tree.roots()[0]].get_count(&mut helper).unwrap(),
                0,
                "count {object}"
            );
            let warnings = warning_messages(&pdf);
            assert_eq!(warnings.len(), 1, "count {object}");
            assert!(
                warnings[0].ends_with(&format!(
                    "operation for integer attempted on object of type {type_name}: returning 0"
                )),
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
        let mut helper = pdf.outline();
        let tree = helper.get_tree().unwrap();
        let expected = if key == "Title" {
            assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "");
            "operation for string attempted on object of type stream: returning empty string"
        } else {
            assert_eq!(tree[tree.roots()[0]].get_count(&mut helper).unwrap(), 0);
            "operation for integer attempted on object of type stream: returning 0"
        };
        let warnings = warning_messages(&pdf);
        assert_eq!(warnings.len(), 1, "{key}");
        assert!(warnings[0].ends_with(expected), "{key}: {warnings:?}");
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
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Reference(ObjectRef::new(9, 0)),
        );
        let mut helper = pdf.outline();
        let tree = helper.get_tree().unwrap();
        let expected = if key == "Title" {
            assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "");
            "operation for string attempted on object of type unresolved: returning empty string"
        } else {
            assert_eq!(tree[tree.roots()[0]].get_count(&mut helper).unwrap(), 0);
            "operation for integer attempted on object of type unresolved: returning 0"
        };
        let warnings = warning_messages(&pdf);
        assert_eq!(warnings.len(), 1, "{key}");
        assert!(warnings[0].ends_with(expected), "{key}: {warnings:?}");
    }
}

/// A literal `null` `/Title` or `/Count` is indistinguishable from a missing
/// key: `QPDF_Dictionary::hasKey` (`libqpdf/QPDF_Dictionary.cc:97-100`)
/// treats a null-valued entry as absent, so
/// `QPDFOutlineObjectHelper::getTitle`/`getCount`
/// (`libqpdf/QPDFOutlineObjectHelper.cc:79-86,94-100`), both gated on
/// `hasKey`, never reach the type-checked accessor and never warn. Confirmed
/// against qpdf 11.9.0: `qpdf --json --json-key=outlines` on a `/Title null`
/// outline exits 0 with empty stderr and `"title": ""`.
#[test]
fn present_null_scalar_is_treated_as_a_missing_key_like_qpdf_haskey() {
    let mut pdf = Pdf::open(Cursor::new(single_outline_with_title("null"))).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "");
    assert!(warning_messages(&pdf).is_empty());

    let mut pdf = Pdf::open(Cursor::new(single_outline_with_count("null"))).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree[tree.roots()[0]].get_count(&mut helper).unwrap(), 0);
    assert!(warning_messages(&pdf).is_empty());
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
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree.roots().len(), 2);
    assert_eq!(tree[tree.roots()[0]].source_ref, None);
    assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "A");
    assert_eq!(tree[tree.roots()[1]].source_ref, None);
    assert_eq!(tree[tree.roots()[1]].get_title(&mut helper).unwrap(), "B");

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
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let id = tree.roots()[0];

    assert_eq!(outline_object(&tree[id]), Object::Integer(42));
    assert_eq!(tree[id].get_title(&mut helper).unwrap(), "");
    assert_eq!(tree[id].get_count(&mut helper).unwrap(), 0);
    assert_eq!(outline_dest(&tree[id], &mut pdf), Object::Null);
    assert!(tree[id].kids.is_empty());
}

/// Regression test for flpdf-t1wr: `get_title`/`get_count`/`get_dest` must
/// each emit qpdf's `hasKey`/`getKey` type warning when the outline item
/// itself resolves to a non-dictionary object, matching
/// `getTitle`/`getCount`/`getDest` (`libqpdf/QPDFOutlineObjectHelper.cc:47-98`)
/// calling `hasKey`/`getKey` directly on `this->oh` with no upfront
/// `isDictionary()` guard. Confirmed against live qpdf 11.9.0: running
/// `qpdf --json --json-key=outlines` on this same fixture emits exactly 8
/// warnings (4x "returning false for a key containment request" from
/// `hasKey(/Title)`, `hasKey(/Dest)` x2, `hasKey(/Count)`; 4x "returning
/// null for attempted key retrieval" from `get_tree`'s `/First`/`/Next`
/// chase and `getKey(/A)` x2 inside the two `getDest()` calls that
/// `addOutlinesToJson` makes for "dest" and "destpageposfrom1").
#[test]
fn non_dictionary_outline_item_emits_qpdf_has_key_get_key_warnings_on_accessors() {
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
    let id;
    let tree = {
        let mut helper = pdf.outline();
        let tree = helper.get_tree().unwrap();
        id = tree.roots()[0];
        tree
    };
    // `get_tree`'s own /First and /Next chase over the non-dictionary 42
    // already emits two qpdf-matching getKey warnings before any accessor runs.
    let after_tree = warning_messages(&pdf);
    assert_eq!(after_tree.len(), 2, "{after_tree:?}");
    for message in &after_tree {
        assert!(
            message.ends_with(
                "operation for dictionary attempted on object of type integer: \
                 returning null for attempted key retrieval"
            ),
            "{after_tree:?}"
        );
    }

    {
        let mut helper = pdf.outline();
        assert_eq!(tree[id].get_title(&mut helper).unwrap(), "");
    }
    let after_title = warning_messages(&pdf);
    assert_eq!(after_title.len(), 3, "{after_title:?}");
    assert!(
        after_title[2].ends_with(
            "operation for dictionary attempted on object of type integer: \
             returning false for a key containment request"
        ),
        "{after_title:?}"
    );

    {
        let mut helper = pdf.outline();
        assert_eq!(tree[id].get_count(&mut helper).unwrap(), 0);
    }
    let after_count = warning_messages(&pdf);
    assert_eq!(after_count.len(), 4, "{after_count:?}");
    assert!(
        after_count[3].ends_with(
            "operation for dictionary attempted on object of type integer: \
             returning false for a key containment request"
        ),
        "{after_count:?}"
    );

    let dest = {
        let mut helper = pdf.outline();
        tree[id].get_dest(&mut helper).unwrap()
    };
    assert!(dest.is_null());
    let after_dest = warning_messages(&pdf);
    assert_eq!(after_dest.len(), 6, "{after_dest:?}");
    assert!(
        after_dest[4].ends_with(
            "operation for dictionary attempted on object of type integer: \
             returning false for a key containment request"
        ),
        "{after_dest:?}: hasKey(/Dest)"
    );
    assert!(
        after_dest[5].ends_with(
            "operation for dictionary attempted on object of type integer: \
             returning null for attempted key retrieval"
        ),
        "{after_dest:?}: getKey(/A)"
    );
}

#[test]
fn outline_json_warning_order_matches_qpdf_add_outlines_to_json() {
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

    write_outlines_json(&mut pdf);

    let kinds = warning_messages(&pdf)
        .into_iter()
        .map(|message| {
            if message.ends_with("returning false for a key containment request") {
                "F"
            } else if message.ends_with("returning null for attempted key retrieval") {
                "N"
            } else {
                panic!("unexpected warning: {message}");
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["N", "N", "F", "F", "N", "F", "F", "N"]);
}

#[test]
fn nested_outline_json_warning_order_keeps_kids_after_parent_fields() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (parent) /Parent 4 0 R /First 42 0 R /Last 42 0 R /Count 1 >>",
            ),
            (42, "42"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    write_outlines_json(&mut pdf);

    let kinds = warning_messages(&pdf)
        .into_iter()
        .map(|message| {
            if message.ends_with("returning false for a key containment request") {
                "F"
            } else if message.ends_with("returning null for attempted key retrieval") {
                "N"
            } else {
                panic!("unexpected warning: {message}");
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["N", "N", "F", "F", "N", "F", "F", "N"]);
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
    assert_eq!(outline_object(&tree[tree.roots()[0]]), Object::Integer(42));
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
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();

    assert_eq!(tree.roots().len(), 1);
    assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "A");
}

#[test]
fn construction_integerizes_a_bare_reference_item() {
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
        pdf.resolve_canonical_object(ObjectRef::new(5, 0)).unwrap(),
        Object::Integer(6)
    );
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let item = &tree[tree.roots()[0]];
    assert_eq!(tree.roots().len(), 1);
    assert_eq!(item.source_ref, Some(ObjectRef::new(5, 0)));
    assert_eq!(outline_object(item), Object::Integer(6));
    assert_eq!(item.get_title(&mut helper).unwrap(), "");
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
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();

    assert_eq!(tree.roots().len(), 2);
    assert_eq!(tree[tree.roots()[0]].get_title(&mut helper).unwrap(), "A");
    assert_eq!(tree[tree.roots()[1]].get_title(&mut helper).unwrap(), "B");
}

#[test]
fn nested_indirect_next_cycle_stops_before_duplicate_child() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Title (Root) /First 6 0 R >>"),
            (6, "<< /Title (Child A) /Next 7 0 R >>"),
            (7, "<< /Title (Child B) /Next 6 0 R >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let root = tree.roots()[0];

    assert_eq!(tree[root].kids.len(), 2);
    assert_eq!(
        tree[tree[root].kids[0]].get_title(&mut helper).unwrap(),
        "Child A"
    );
    assert_eq!(
        tree[tree[root].kids[1]].get_title(&mut helper).unwrap(),
        "Child B"
    );
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
    assert_eq!(outline_object(&tree[first]), outline_object(&tree[second]));
}

#[test]
fn get_tree_materializes_tree_with_titles_counts_parents() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let roots = tree.roots();

    // Two top-level nodes: A, B.
    assert_eq!(roots.len(), 2);
    assert_eq!(tree[roots[0]].get_title(&mut helper).unwrap(), "A");
    assert_eq!(tree[roots[0]].parent, None);
    assert_eq!(tree[roots[0]].get_count(&mut helper).unwrap(), 1);
    assert_eq!(tree[roots[1]].get_title(&mut helper).unwrap(), "B");
    assert_eq!(tree[roots[1]].get_count(&mut helper).unwrap(), 2);

    // A has one child A1.
    assert_eq!(tree[roots[0]].kids.len(), 1);
    let a1 = tree[roots[0]].kids[0];
    assert_eq!(tree[a1].get_title(&mut helper).unwrap(), "A1");
    assert_eq!(tree[a1].parent, Some(roots[0]));
    assert_eq!(tree[a1].get_count(&mut helper).unwrap(), 0); // /Count absent -> 0 (qpdf)
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
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let titles: Vec<String> = tree
        .preorder()
        .map(|(_depth, _id, item)| item.get_title(&mut helper).unwrap())
        .collect();
    assert_eq!(titles, vec!["A", "A1", "B"]); // pre-order: A, its child A1, then B

    let seen: Vec<(String, usize, usize)> = tree
        .preorder()
        .map(|(depth, _id, item)| (item.get_title(&mut helper).unwrap(), depth, item.kids.len()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("A".to_string(), 1, 1),
            ("A1".to_string(), 2, 0),
            ("B".to_string(), 1, 0),
        ]
    );
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
    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let titles: Vec<String> = tree
        .preorder()
        .map(|(_depth, _id, item)| item.get_title(&mut helper).unwrap())
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
    assert_eq!(outline_dest(&tree[a1], &mut pdf), page_dest(3));
    assert_eq!(
        outline_dest_page(&tree[a1], &mut pdf),
        Object::Reference(ObjectRef::new(3, 0))
    );
    // Nodes without a destination have qpdf's null sentinel.
    assert_eq!(outline_dest(&tree[roots[1]], &mut pdf), Object::Null); // B
}

#[test]
fn outline_items_retain_canonical_object_handle_identity() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let item = &tree[tree.roots()[0]];
    let canonical = pdf.get_object_handle(ObjectRef::new(5, 0));

    assert!(item.object.is_same_object_as(&canonical));
    assert_eq!(item.object.object_ref(), Some(ObjectRef::new(5, 0)));
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
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(3));
    assert_eq!(
        outline_dest_page(&roots[0], &mut pdf),
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
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(3));
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
    assert!(matches!(
        outline_dest(&roots[0], &mut pdf),
        Object::Dictionary(_)
    ));
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
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(3));
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
    assert_eq!(outline_dest(&tree[tree.roots()[0]], &mut pdf), page_dest(3));
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
    assert_eq!(outline_dest(&tree[tree.roots()[0]], &mut pdf), page_dest(3));
    assert_eq!(outline_dest(&tree[tree.roots()[1]], &mut pdf), page_dest(3));
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
    assert_eq!(outline_dest(&tree[tree.roots()[0]], &mut pdf), Object::Null);
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node (object 9): loop detected while traversing name/number tree"]
    );
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
    assert!(matches!(
        outline_dest(&roots[0], &mut pdf),
        Object::Dictionary(_)
    ));
    assert_eq!(outline_dest_page(&roots[0], &mut pdf), Object::Null);
}

#[test]
fn non_dictionary_legacy_dests_resolve_to_null() {
    let bytes = single_outline_with_catalog("/Dests 42", "/Dest /missing", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let roots = root_items(&mut pdf);
    assert_eq!(outline_dest(&roots[0], &mut pdf), Object::Null);
}

/// Legacy named dest whose `/Dests` entry (`/held`) is an indirect reference
/// to object 8, and object 8's *value* is later replaced in place with
/// `Pdf::set_object(8, Object::Reference(9))` — the same reference-to-reference
/// redirect bridge `OutlineDocumentHelper::resolve_value_handle`'s own doc
/// describes (`Pdf::set_object` can install this state in a canonical slot;
/// a normal indirect child parsed from a real PDF never does). The other
/// dest-resolution call sites (`OutlineItem::get_dest` in
/// outline_object_helper.rs, `resolve_named_dest_by_string`) already chase
/// this redirect to its terminal value via `resolve_value_handle`; the legacy
/// `/Dests` dictionary lookup (`resolve_named_dest_by_name`) must reach the
/// same object 9 array rather than exposing the still-redirected object 8
/// handle.
fn named_dest_legacy_redirect_chain_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Dests 6 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (L) /Parent 4 0 R /Dest /held >>"),
            (6, "<< /held 8 0 R >>"),
            (8, "null"),
        ],
        1,
    )
}

#[test]
fn dest_from_named_legacy_chases_a_set_object_redirect_chain() {
    let mut pdf = Pdf::open(Cursor::new(named_dest_legacy_redirect_chain_pdf())).unwrap();
    // Overwrite object 8's value with a further indirect reference to
    // object 9, then give object 9 the real destination array. This is
    // qpdf-oracle-inapplicable by construction: qpdf has no notion of an
    // object whose own parsed value is another indirect reference (a raw
    // "M H R" body at the top level of "N G obj ... endobj" does not parse
    // as a reference at all — confirmed against live qpdf 11.9.0, which
    // reads only the leading integer and warns "expected endobj" on the
    // trailing " R"). This is purely an flpdf `Pdf::set_object` legacy-API
    // artifact, so the oracle here is this module's own three sibling
    // call sites, plus the pre-`ObjectHandle`-migration implementation,
    // which called `resolve_terminal_object` on exactly this value.
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    pdf.set_object(ObjectRef::new(9, 0), page_dest(3));

    let roots = root_items(&mut pdf);
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(3));
    assert_eq!(
        outline_dest_page(&roots[0], &mut pdf),
        Object::Reference(ObjectRef::new(3, 0))
    );
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
    assert_eq!(
        outline_dest(&roots[0], &mut pdf),
        Object::Name(b"b".to_vec())
    );
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
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(3));
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
    let mut helper = pdf.outline();
    assert_eq!(roots[0].get_title(&mut helper).unwrap(), "RealTitle");
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
    assert_eq!(outline_object(&roots[0]), Object::Integer(42));
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
fn outline_named_destination_lookup_uses_only_shared_nntree_engine() {
    const SOURCE: &str = include_str!("../src/outline_document_helper.rs");

    assert!(
        SOURCE.contains("NameTree::new("),
        "outline named-destination lookup must construct the shared canonical NameTree"
    );

    for private_algorithm in [
        "enum NameTreeLookup",
        "struct NameTreeStructuralError",
        "fn find_name_tree_value<",
        "fn name_tree_begin_preflight<",
        "fn name_tree_node<",
        "fn find_name_tree_leaf_value(",
        "fn select_name_tree_kid<",
        "fn qpdf_name_tree_binary_search<",
        "fn name_tree_kid_ordering<",
        "fn enumerate_name_tree_entries<",
        "fn repair_name_tree<",
        "fn build_repaired_name_tree_root<",
        "enum RepairedNameTreeNodeKind",
        "fn split_repaired_name_tree_node(",
        "fn repaired_name_tree_dictionary(",
        "fn repaired_name_tree_limit(",
    ] {
        assert!(
            !SOURCE.contains(private_algorithm),
            "outline_document_helper.rs still owns private NNTree algorithm: {private_algorithm}"
        );
    }
}

#[test]
fn named_destination_lookup_handles_qpdf_nonfatal_node_shapes() {
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
            &[(8, "<< /Limits [42 42] /Names [(shape) [3 0 R /Fit]] >>")][..],
            page_dest(3),
        ),
    ];

    for (catalog_entries, extra, expected) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, "/Dest (shape)", extra);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        assert_eq!(
            outline_dest(&root_items(&mut pdf)[0], &mut pdf),
            expected,
            "{catalog_entries}"
        );
    }
}

#[test]
fn short_first_name_tree_pair_is_fatal_after_the_repair_warning() {
    let cases = [
        (
            "direct root",
            "/Names << /Dests << /Names [(m)] >> >>",
            &[][..],
            "Name/Number tree node: update ivalue: items array is too short",
        ),
        (
            "indirect root",
            "/Names << /Dests 8 0 R >>",
            &[(8, "<< /Names [(m)] >>")][..],
            "Name/Number tree node (object 8): update ivalue: items array is too short",
        ),
    ];

    for (label, catalog_entries, extra, expected_message) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, "/Dest (m)", extra);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let warning_context = match extra {
            [] => "Name/Number tree node".to_string(),
            [(object_number, _)] => format!("Name/Number tree node (object {object_number})"),
            _ => unreachable!(),
        };

        // qpdf's `QPDFOutlineDocumentHelper` constructor never touches
        // `/Dest`, so `get_tree()` (which mirrors it) succeeds here; the
        // named-tree lookup only runs, and only then can fail, when
        // `getDest()`/`dest()` is actually called on an item — matching
        // `qpdf --json=2 --json-key=outlines`'s own failure point (it exits
        // 2 while building the JSON `dest` field, not while constructing the
        // document helper; see the neighboring `#[ignore]`d
        // `qpdf_short_first_name_tree_pair_is_fatal_after_repair_warning`
        // oracle test).
        let items = root_items(&mut pdf);
        let mut helper = pdf.outline();
        let error = items[0].get_dest(&mut helper).unwrap_err();
        match error {
            Error::Parse { offset, message } => {
                assert_eq!(offset, 0, "{label}");
                assert_eq!(message, expected_message, "{label}");
            }
            other => panic!("{label}: expected parse error, got {other}"),
        }
        assert_eq!(
            warning_messages(&pdf),
            [format!(
                "{warning_context}: attempting to repair after error: {expected_message}"
            )],
            "{label}"
        );

        match extra {
            [] => assert_eq!(
                direct_dests_root(&mut pdf).get("Names"),
                Some(&Object::Array(vec![Object::String(b"m".to_vec())])),
                "{label}"
            ),
            [(object_number, _)] => {
                let Object::Dictionary(root) = pdf
                    .resolve_canonical_object(ObjectRef::new(*object_number, 0))
                    .unwrap()
                else {
                    panic!("{label}: indirect root must remain a dictionary");
                };
                assert_eq!(
                    root.get("Names"),
                    Some(&Object::Array(vec![Object::String(b"m".to_vec())])),
                    "{label}"
                );
            }
            _ => unreachable!(),
        }
    }
}

fn direct_first_child_short_name_tree_pdf() -> Vec<u8> {
    single_outline_with_catalog(
        "/Names << /Dests << /Kids [<< /Names [(m)] >>] >> >>",
        "/Dest (m)",
        &[],
    )
}

#[test]
fn direct_first_child_short_pair_repairs_from_the_mutated_root() {
    let mut pdf = Pdf::open(Cursor::new(direct_first_child_short_name_tree_pdf())).unwrap();

    // See `short_first_name_tree_pair_is_fatal_after_the_repair_warning`:
    // `get_tree()` never touches `/Dest`, so the fatal named-tree error only
    // surfaces once `dest()` is actually called on the item.
    let items = root_items(&mut pdf);
    let mut helper = pdf.outline();
    let error = items[0].get_dest(&mut helper).unwrap_err();
    match error {
        Error::Parse { offset, message } => {
            assert_eq!(offset, 0);
            assert_eq!(
                message,
                "Name/Number tree node (object 6): update ivalue: items array is too short"
            );
        }
        other => panic!("expected parse error, got {other}"),
    }
    assert_eq!(
        warning_messages(&pdf),
        [
            "Name/Number tree node: converting kid number 0 to an indirect object",
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 6): update ivalue: items array is too short",
        ]
    );

    let dests = direct_dests_root(&mut pdf);
    assert_eq!(
        dests.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
            6, 0
        ))]))
    );
    let Object::Dictionary(child) = pdf.resolve_canonical_object(ObjectRef::new(6, 0)).unwrap()
    else {
        panic!("converted first child must be an indirect dictionary");
    };
    assert_eq!(
        child.get("Names"),
        Some(&Object::Array(vec![Object::String(b"m".to_vec())]))
    );
}

#[test]
#[ignore = "live qpdf 11.9.0 direct-child short-pair oracle"]
fn qpdf_direct_first_child_short_pair_preserves_converted_object_context() {
    use std::io::Write;
    use std::process::Command;

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input
        .write_all(&direct_first_child_short_name_tree_pdf())
        .unwrap();
    let output = Command::new("qpdf")
        .args(["--json=2", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let conversion = stderr
        .find("converting kid number 0 to an indirect object")
        .unwrap_or_else(|| panic!("missing conversion warning in {stderr}"));
    let repair = stderr
        .find("attempting to repair after error:")
        .unwrap_or_else(|| panic!("missing repair warning in {stderr}"));
    let fatal = stderr
        .rfind("update ivalue: items array is too short")
        .unwrap_or_else(|| panic!("missing fatal error in {stderr}"));
    assert!(conversion < repair && repair < fatal, "{stderr}");
    assert_eq!(stderr.matches("object 6").count(), 2, "{stderr}");
    assert_eq!(
        stderr
            .matches("update ivalue: items array is too short")
            .count(),
        2,
        "{stderr}"
    );
    assert!(!output.stdout.is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err());
}

fn first_invalid_name_tree_key_pdf(indirect_root: bool) -> Vec<u8> {
    let catalog_entries = if indirect_root {
        "/Names << /Dests 8 0 R >>"
    } else {
        "/Names << /Dests << /Kids [8 0 R] >> >>"
    };
    let extra = if indirect_root {
        vec![
            (8, "<< /Kids [9 0 R] >>"),
            (9, "<< /Names [42 [3 0 R /Fit] (m) [3 0 R /Fit]] >>"),
        ]
    } else {
        vec![(8, "<< /Names [42 [3 0 R /Fit] (m) [3 0 R /Fit]] >>")]
    };
    single_outline_with_catalog(catalog_entries, "/Dest (m)", &extra)
}

#[test]
#[ignore = "live qpdf 11.9.0 warning-context and initial-invalid-key oracle"]
fn qpdf_first_invalid_name_tree_key_oracle() {
    use std::io::Write;
    use std::process::Command;

    for (label, indirect_root) in [("direct root", false), ("indirect root", true)] {
        let mut input = tempfile::NamedTempFile::new().unwrap();
        input
            .write_all(&first_invalid_name_tree_key_pdf(indirect_root))
            .unwrap();
        let output = Command::new("qpdf")
            .args(["--json=2", "--json-key=outlines"])
            .arg(input.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "{label} stderr:\n{stderr}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );

        assert_eq!(output.status.code(), Some(2), "{label}: {stderr}");
        let repair_context = if indirect_root {
            "(Name/Number tree node (object 8)): attempting to repair after error:"
        } else {
            "(Name/Number tree node): attempting to repair after error:"
        };
        let inner_context = if indirect_root {
            "(Name/Number tree node (object 9)): node is missing /Limits"
        } else {
            "(Name/Number tree node (object 8)): node is missing /Limits"
        };
        let repair = stderr
            .find(repair_context)
            .unwrap_or_else(|| panic!("{label}: missing repair context in {stderr}"));
        let inner = stderr
            .find(inner_context)
            .unwrap_or_else(|| panic!("{label}: missing inner context in {stderr}"));
        let fatal = stderr
            .find("(Name/Number tree node): item at index 0 is not the right type")
            .unwrap_or_else(|| panic!("{label}: missing fatal direct-root context in {stderr}"));
        assert!(repair < inner && inner < fatal, "{label}: {stderr}");
        assert!(
            !stderr.contains("item 0 has the wrong type"),
            "{label}: qpdf must not turn the initial invalid key into a skip warning: {stderr}"
        );
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err(),
            "{label}: repair error must leave partial JSON"
        );
    }
}

#[test]
fn first_invalid_name_tree_key_fails_during_qpdf_style_repair() {
    for (label, indirect_root, expected_warning) in [
        (
            "direct root",
            false,
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits",
        ),
        (
            "indirect root",
            true,
            "Name/Number tree node (object 8): attempting to repair after error: Name/Number tree node (object 9): node is missing /Limits",
        ),
    ] {
        let mut pdf = Pdf::open(Cursor::new(first_invalid_name_tree_key_pdf(indirect_root))).unwrap();

        // See `short_first_name_tree_pair_is_fatal_after_the_repair_warning`:
        // `get_tree()` never touches `/Dest`, so the fatal named-tree error
        // only surfaces once `dest()` is actually called on the item.
        let items = root_items(&mut pdf);
        let mut helper = pdf.outline();
        let error = items[0].get_dest(&mut helper).unwrap_err();
        assert_eq!(
            error.to_string(),
            "parse error at byte 0: Name/Number tree node: item at index 0 is not the right type",
            "{label}"
        );
        assert_eq!(warning_messages(&pdf), [expected_warning], "{label}");
    }
}

#[test]
fn scalar_name_tree_dests_are_silent_and_resolve_to_null() {
    let cases = [
        ("direct scalar", "/Names << /Dests 42 >>", &[][..]),
        (
            "indirect scalar",
            "/Names << /Dests 8 0 R >>",
            &[(8, "42")][..],
        ),
    ];

    for (label, catalog_entries, extra) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, "/Dest (m)", extra);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert_eq!(
            outline_dest(&root_items(&mut pdf)[0], &mut pdf),
            Object::Null,
            "{label}"
        );
        assert!(warning_messages(&pdf).is_empty(), "{label}");
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 short first-pair oracle"]
fn qpdf_short_first_name_tree_pair_is_fatal_after_repair_warning() {
    use std::io::Write;
    use std::process::Command;

    let cases = [
        (
            "direct root",
            "/Names << /Dests << /Names [(m)] >> >>",
            &[][..],
        ),
        (
            "indirect root",
            "/Names << /Dests 8 0 R >>",
            &[(8, "<< /Names [(m)] >>")][..],
        ),
    ];

    for (label, catalog_entries, extra) in cases {
        let mut input = tempfile::NamedTempFile::new().unwrap();
        input
            .write_all(&single_outline_with_catalog(
                catalog_entries,
                "/Dest (m)",
                extra,
            ))
            .unwrap();
        let output = Command::new("qpdf")
            .args(["--json=2", "--json-key=outlines"])
            .arg(input.path())
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2), "{label}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let repair = stderr
            .find("attempting to repair after error:")
            .unwrap_or_else(|| panic!("{label}: missing repair warning in {stderr}"));
        let fatal = stderr
            .rfind("update ivalue: items array is too short")
            .unwrap_or_else(|| panic!("{label}: missing fatal error in {stderr}"));
        assert!(repair < fatal, "{label}: {stderr}");
        assert_eq!(
            stderr.matches("attempting to repair after error:").count(),
            1,
            "{label}"
        );
        assert_eq!(
            stderr
                .matches("update ivalue: items array is too short")
                .count(),
            2,
            "{label}"
        );
        assert!(!output.stdout.is_empty(), "{label}");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err(),
            "{label}: qpdf must leave partial, incomplete JSON"
        );
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 scalar /Dests oracle"]
fn qpdf_scalar_name_tree_dests_are_silent_and_null() {
    use std::io::Write;
    use std::process::Command;

    let cases = [
        ("direct scalar", "/Names << /Dests 42 >>", &[][..]),
        (
            "indirect scalar",
            "/Names << /Dests 8 0 R >>",
            &[(8, "42")][..],
        ),
    ];

    for (label, catalog_entries, extra) in cases {
        let mut input = tempfile::NamedTempFile::new().unwrap();
        input
            .write_all(&single_outline_with_catalog(
                catalog_entries,
                "/Dest (m)",
                extra,
            ))
            .unwrap();
        let output = Command::new("qpdf")
            .args(["--json=2", "--json-key=outlines"])
            .arg(input.path())
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0), "{label}");
        assert!(
            output.stderr.is_empty(),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            json["outlines"][0]["dest"],
            serde_json::Value::Null,
            "{label}"
        );
    }
}

#[test]
fn qpdf_binary_search_finds_last_leaf_pair_before_visiting_an_invalid_middle_key() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Names [(a) [3 0 R /Fit] 42 [3 0 R /Fit] (target) [3 0 R /Fit]] >> >>",
        "/Dest (target)",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert!(warning_messages(&pdf).is_empty());
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Kids"), None);
    assert_eq!(
        dests.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"a".to_vec()),
            page_dest(3),
            Object::Integer(42),
            page_dest(3),
            Object::String(b"target".to_vec()),
            page_dest(3),
        ]))
    );
}

#[test]
fn name_tree_begin_lower_bound_skips_an_invalid_middle_leaf_key() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Names [(m) [3 0 R /Fit] 42 [3 0 R /Fit] (z) [3 0 R /Fit]] >> >>",
        "/Dest (a)",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert!(warning_messages(&pdf).is_empty());
    assert_eq!(
        direct_dests_root(&mut pdf).get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"m".to_vec()),
            page_dest(3),
            Object::Integer(42),
            page_dest(3),
            Object::String(b"z".to_vec()),
            page_dest(3),
        ]))
    );
}

#[test]
fn qpdf_binary_search_finds_last_kid_before_visiting_an_invalid_middle_kid() {
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

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert!(warning_messages(&pdf).is_empty());
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Names"), None);
    assert_eq!(
        dests.get("Kids"),
        Some(&Object::Array(vec![
            Object::Reference(ObjectRef::new(8, 0)),
            Object::Integer(42),
            Object::Reference(ObjectRef::new(9, 0)),
        ]))
    );
}

#[test]
fn name_tree_begin_lower_bound_converts_the_direct_first_path_before_skipping_search() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [<< /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >>] >> 42 9 0 R] >> >>",
        "/Dest (a)",
        &[(
            9,
            "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>",
        )],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        [
            "Name/Number tree node: converting kid number 0 to an indirect object",
            "Name/Number tree node (object 10): converting kid number 0 to an indirect object",
        ]
    );

    let dests = direct_dests_root(&mut pdf);
    assert_eq!(
        dests.get("Kids"),
        Some(&Object::Array(vec![
            Object::Reference(ObjectRef::new(10, 0)),
            Object::Integer(42),
            Object::Reference(ObjectRef::new(9, 0)),
        ]))
    );
    let Object::Dictionary(first_parent) =
        pdf.resolve_canonical_object(ObjectRef::new(10, 0)).unwrap()
    else {
        panic!("converted first parent must be a dictionary");
    };
    assert_eq!(
        first_parent.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
            11, 0,
        ))]))
    );
    let Object::Dictionary(first_leaf) =
        pdf.resolve_canonical_object(ObjectRef::new(11, 0)).unwrap()
    else {
        panic!("converted first leaf must be a dictionary");
    };
    assert_eq!(
        first_leaf.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"m".to_vec()),
            page_dest(3),
        ]))
    );

    let (serialized, mapping) = write_with_settings_and_mapping(
        &mut pdf,
        &WriterTestSettings::default(),
        &[
            ObjectRef::new(8, 0),
            ObjectRef::new(9, 0),
            ObjectRef::new(10, 0),
            ObjectRef::new(11, 0),
        ],
    )
    .unwrap();
    let mapped_first_parent = mapping[&ObjectRef::new(10, 0)];
    let mapped_first_leaf = mapping[&ObjectRef::new(11, 0)];
    let mut reopened = Pdf::open(Cursor::new(serialized)).unwrap();
    let dests = direct_dests_root(&mut reopened);
    assert!(matches!(
        dests.get("Kids"),
        Some(Object::Array(kids)) if kids.first() == Some(&Object::Reference(mapped_first_parent))
    ));
    let Object::Dictionary(first_parent) = reopened
        .resolve_canonical_object(mapped_first_parent)
        .unwrap()
    else {
        panic!("reopened first parent must be a dictionary");
    };
    assert_eq!(
        first_parent.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(mapped_first_leaf)]))
    );
}

#[test]
fn name_tree_begin_converts_a_direct_first_kid_under_an_indirect_root() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests 8 0 R >>",
        "/Dest (a)",
        &[(
            8,
            "<< /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >>] >>",
        )],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node (object 8): converting kid number 0 to an indirect object"]
    );
    let Object::Dictionary(root) = pdf.resolve_canonical_object(ObjectRef::new(8, 0)).unwrap()
    else {
        panic!("indirect destination root must remain a dictionary");
    };
    assert_eq!(
        root.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
            9, 0,
        ))]))
    );
    assert!(matches!(
        pdf.resolve_canonical_object(ObjectRef::new(9, 0)).unwrap(),
        Object::Dictionary(_)
    ));

    let (serialized, mapping) = write_with_settings_and_mapping(
        &mut pdf,
        &WriterTestSettings::default(),
        &[ObjectRef::new(8, 0), ObjectRef::new(9, 0)],
    )
    .unwrap();
    let mapped_root = mapping[&ObjectRef::new(8, 0)];
    let mapped_leaf = mapping[&ObjectRef::new(9, 0)];
    let mut reopened = Pdf::open(Cursor::new(serialized)).unwrap();
    let Object::Dictionary(root) = reopened.resolve_canonical_object(mapped_root).unwrap() else {
        panic!("reopened indirect destination root must remain a dictionary");
    };
    assert_eq!(
        root.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(mapped_leaf)]))
    );
}

#[test]
fn name_tree_begin_updates_a_direct_root_inside_an_indirect_names_holder() {
    let bytes = single_outline_with_catalog(
        "/Names 8 0 R",
        "/Dest (a)",
        &[(
            8,
            "<< /Dests << /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >>] >> >>",
        )],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node: converting kid number 0 to an indirect object"]
    );
    let Object::Dictionary(names) = pdf.resolve_canonical_object(ObjectRef::new(8, 0)).unwrap()
    else {
        panic!("indirect /Names holder must remain a dictionary");
    };
    let Some(Object::Dictionary(root)) = names.get("Dests") else {
        panic!("destination root must remain direct");
    };
    assert_eq!(
        root.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
            9, 0,
        ))]))
    );

    let (serialized, mapping) = write_with_settings_and_mapping(
        &mut pdf,
        &WriterTestSettings::default(),
        &[ObjectRef::new(8, 0), ObjectRef::new(9, 0)],
    )
    .unwrap();
    let mapped_names = mapping[&ObjectRef::new(8, 0)];
    let mapped_leaf = mapping[&ObjectRef::new(9, 0)];
    let mut reopened = Pdf::open(Cursor::new(serialized)).unwrap();
    let Object::Dictionary(names) = reopened.resolve_canonical_object(mapped_names).unwrap() else {
        panic!("reopened indirect /Names holder must remain a dictionary");
    };
    let Some(Object::Dictionary(root)) = names.get("Dests") else {
        panic!("reopened destination root must remain direct");
    };
    assert_eq!(
        root.get("Kids"),
        Some(&Object::Array(vec![Object::Reference(mapped_leaf)]))
    );
}

#[test]
fn invalid_first_name_tree_key_is_fatal_without_mutating_the_direct_root() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Names [42 [3 0 R /Fit] (m) [3 0 R /Fit] (z) [3 0 R /Fit]] >> >>",
        "/Dest (a)",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    // See `short_first_name_tree_pair_is_fatal_after_the_repair_warning`:
    // `get_tree()` never touches `/Dest`, so the fatal named-tree error only
    // surfaces once `dest()` is actually called on the item.
    let items = root_items(&mut pdf);
    let mut helper = pdf.outline();
    let error = items[0].get_dest(&mut helper).unwrap_err();
    assert_eq!(
        error.to_string(),
        "parse error at byte 0: Name/Number tree node: item at index 0 is not the right type"
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node: attempting to repair after error: Name/Number tree node: item at index 0 is not the right type"]
    );
    assert_eq!(
        direct_dests_root(&mut pdf).get("Names"),
        Some(&Object::Array(vec![
            Object::Integer(42),
            page_dest(3),
            Object::String(b"m".to_vec()),
            page_dest(3),
            Object::String(b"z".to_vec()),
            page_dest(3),
        ]))
    );
}

#[test]
fn after_last_lookup_runs_targeted_search_without_a_last_descent() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >> 42 << /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>] >> >>",
        "/Dest (zz)",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node: converting kid number 0 to an indirect object"]
    );
    let dests = direct_dests_root(&mut pdf);
    let Some(Object::Array(kids)) = dests.get("Kids") else {
        panic!("destination root must retain /Kids");
    };
    assert_eq!(kids[0], Object::Reference(ObjectRef::new(6, 0)));
    assert_eq!(kids[1], Object::Integer(42));
    assert!(matches!(kids[2], Object::Dictionary(_)));
}

#[test]
fn name_tree_begin_indirects_a_direct_scalar_before_reporting_it_as_non_dictionary() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [42 8 0 R] >> >>",
        "/Dest (target)",
        &[(
            8,
            "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
        )],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        [
            "Name/Number tree node: converting kid number 0 to an indirect object",
            "Name/Number tree node (object 9): non-dictionary node while traversing name/number tree",
        ]
    );
    assert_eq!(
        direct_dests_root(&mut pdf).get("Kids"),
        Some(&Object::Array(vec![
            Object::Reference(ObjectRef::new(9, 0)),
            Object::Reference(ObjectRef::new(8, 0)),
        ]))
    );
    assert_eq!(
        pdf.resolve_canonical_object(ObjectRef::new(9, 0)).unwrap(),
        Object::Integer(42)
    );
}

#[test]
fn name_tree_begin_reports_an_indirect_scalar_without_repairing_the_tree() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [8 0 R 9 0 R] >> >>",
        "/Dest (target)",
        &[
            (8, "42"),
            (
                9,
                "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
            ),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node (object 8): non-dictionary node while traversing name/number tree"]
    );
    assert_eq!(
        direct_dests_root(&mut pdf).get("Kids"),
        Some(&Object::Array(vec![
            Object::Reference(ObjectRef::new(8, 0)),
            Object::Reference(ObjectRef::new(9, 0)),
        ]))
    );
}

#[test]
#[ignore = "live qpdf 11.9.0 begin/deepen full-object oracle"]
fn qpdf_name_tree_begin_lower_bound_and_direct_kid_full_object_oracle() {
    use std::io::Write;
    use std::process::Command;

    let cases = [
        (
            "deep direct first path",
            "/Names << /Dests << /Kids [<< /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >>] >> 42 9 0 R] >> >>",
            &[(
                9,
                "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>",
            )][..],
            "/Dest (a)",
            Some(3),
            &[
                "converting kid number 0 to an indirect object",
                "Name/Number tree node (object 10)",
                "converting kid number 0 to an indirect object",
            ][..],
        ),
        (
            "after last",
            "/Names << /Dests << /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >> 42 << /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>] >> >>",
            &[][..],
            "/Dest (zz)",
            Some(3),
            &["converting kid number 0 to an indirect object"][..],
        ),
        (
            "indirect root",
            "/Names << /Dests 8 0 R >>",
            &[(
                8,
                "<< /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >>] >>",
            )][..],
            "/Dest (a)",
            Some(3),
            &["Name/Number tree node (object 8)", "converting kid number 0"][..],
        ),
        (
            "direct scalar",
            "/Names << /Dests << /Kids [42 8 0 R] >> >>",
            &[(
                8,
                "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
            )][..],
            "/Dest (target)",
            Some(3),
            &[
                "converting kid number 0 to an indirect object",
                "Name/Number tree node (object 9)",
                "non-dictionary node while traversing name/number tree",
            ][..],
        ),
        (
            "indirect scalar",
            "/Names << /Dests << /Kids [8 0 R 9 0 R] >> >>",
            &[
                (8, "42"),
                (
                    9,
                    "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
                ),
            ][..],
            "/Dest (target)",
            Some(3),
            &[
                "Name/Number tree node (object 8)",
                "non-dictionary node while traversing name/number tree",
            ][..],
        ),
        (
            "leaf lower bound",
            "/Names << /Dests << /Names [(m) [3 0 R /Fit] 42 [3 0 R /Fit] (z) [3 0 R /Fit]] >> >>",
            &[][..],
            "/Dest (a)",
            Some(0),
            &[][..],
        ),
        (
            "indirect names holder",
            "/Names 8 0 R",
            &[(
                8,
                "<< /Dests << /Kids [<< /Limits [(m) (m)] /Names [(m) [3 0 R /Fit]] >>] >> >>",
            )][..],
            "/Dest (a)",
            Some(3),
            &["converting kid number 0 to an indirect object"][..],
        ),
        (
            "first path cycle",
            "/Names << /Dests << /Kids [8 0 R] >> >>",
            &[(8, "<< /Limits [(m) (m)] /Kids [8 0 R] >>")][..],
            "/Dest (m)",
            Some(3),
            &[
                "Name/Number tree node (object 8)",
                "loop detected while traversing name/number tree",
            ][..],
        ),
    ];

    for (label, catalog_entries, extra, item_entries, expected_status, ordered_warnings) in cases {
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
            expected_status,
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut warning_offset = 0;
        for warning in ordered_warnings {
            let relative = stderr[warning_offset..]
                .find(warning)
                .unwrap_or_else(|| panic!("{label}: missing {warning:?} in {stderr}"));
            warning_offset += relative + warning.len();
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let objects = json["qpdf"][1].as_object().unwrap();
        match label {
            "deep direct first path" => {
                let kids = json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"]["/Kids"]
                    .as_array()
                    .unwrap();
                assert_eq!(kids[0], "10 0 R");
                assert_eq!(kids[1], 42);
                assert_eq!(kids[2], "9 0 R");
                assert_eq!(objects["obj:10 0 R"]["value"]["/Kids"][0], "11 0 R");
                assert_eq!(
                    objects["obj:11 0 R"]["value"]["/Names"]
                        .as_array()
                        .unwrap()
                        .len(),
                    2
                );
            }
            "after last" => {
                let kids = json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"]["/Kids"]
                    .as_array()
                    .unwrap();
                assert_eq!(kids[0], "6 0 R");
                assert_eq!(kids[1], 42);
                assert!(kids[2].is_object(), "last direct kid must remain direct");
                assert!(objects.get("obj:7 0 R").is_none());
            }
            "indirect root" => {
                assert_eq!(objects["obj:8 0 R"]["value"]["/Kids"][0], "9 0 R");
                assert!(objects["obj:9 0 R"]["value"].is_object());
            }
            "direct scalar" => {
                let kids = json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"]["/Kids"]
                    .as_array()
                    .unwrap();
                assert_eq!(kids[0], "9 0 R");
                assert_eq!(kids[1], "8 0 R");
                assert_eq!(objects["obj:9 0 R"]["value"], 42);
            }
            "indirect scalar" => {
                let kids = json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"]["/Kids"]
                    .as_array()
                    .unwrap();
                assert_eq!(kids[0], "8 0 R");
                assert_eq!(kids[1], "9 0 R");
                assert!(objects.get("obj:10 0 R").is_none());
            }
            "leaf lower bound" => {
                let names = json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"]["/Names"]
                    .as_array()
                    .unwrap();
                assert_eq!(names.len(), 6);
                assert!(objects.get("obj:6 0 R").is_none());
            }
            "indirect names holder" => {
                assert_eq!(objects["obj:8 0 R"]["value"]["/Dests"]["/Kids"][0], "9 0 R");
                assert!(objects["obj:9 0 R"]["value"].is_object());
            }
            "first path cycle" => {
                assert_eq!(objects["obj:8 0 R"]["value"]["/Kids"][0], "8 0 R");
                assert!(objects.get("obj:9 0 R").is_none());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn empty_root_name_tree_warns_as_traversal_missing_without_repair_or_mutation() {
    let bytes = single_outline_with_catalog("/Names << /Dests << >> >>", "/Dest (shape)", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node: name/number tree node has neither non-empty /Names nor /Kids"]
    );
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Names"), None);
    assert_eq!(dests.get("Kids"), None);
}

#[test]
fn empty_names_and_empty_kids_root_is_allowed_without_warning_or_mutation() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Names [] /Kids [] >> >>",
        "/Dest (shape)",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    assert!(warning_messages(&pdf).is_empty());
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Names"), Some(&Object::Array(Vec::new())));
    assert_eq!(dests.get("Kids"), Some(&Object::Array(Vec::new())));
}

#[test]
#[ignore = "live qpdf 11.9.0 search-order and empty-root oracle"]
fn qpdf_binary_search_and_empty_root_full_object_oracle() {
    use std::io::Write;
    use std::process::Command;

    let cases = [
        (
            "/Names << /Dests << /Names [(a) [3 0 R /Fit] 42 [3 0 R /Fit] (target) [3 0 R /Fit]] >> >>",
            &[][..],
            "/Dest (target)",
            Some(0),
            None,
            "Names",
        ),
        (
            "/Names << /Dests << /Kids [8 0 R 42 9 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (
                    9,
                    "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
                ),
            ][..],
            "/Dest (target)",
            Some(0),
            None,
            "Kids",
        ),
        (
            "/Names << /Dests << >> >>",
            &[][..],
            "/Dest (shape)",
            Some(3),
            Some("name/number tree node has neither non-empty /Names nor /Kids"),
            "Empty",
        ),
    ];

    for (catalog_entries, extra, item_entries, expected_status, warning, shape) in cases {
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
            expected_status,
            "{shape}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        match warning {
            Some(warning) => assert!(stderr.contains(warning), "{shape}: {stderr}"),
            None => assert!(stderr.is_empty(), "{shape}: {stderr}"),
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let root = &json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"];
        match shape {
            "Names" => {
                assert_eq!(root["/Names"].as_array().unwrap().len(), 6);
                assert!(root.get("/Kids").is_none());
            }
            "Kids" => {
                assert_eq!(root["/Kids"].as_array().unwrap().len(), 3);
                assert!(root.get("/Names").is_none());
            }
            "Empty" => assert!(root.as_object().unwrap().is_empty()),
            _ => unreachable!(),
        }
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
            "Name/Number tree node: attempting to repair after error: Name/Number tree node: item at index 2 is not the right type",
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
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 9): loop detected in find",
        ),
        (
            "bad node",
            "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(m) (m)] /Kids [] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 9): bad node during find",
        ),
        (
            "empty leaf bad node",
            "/Names << /Dests << /Kids [8 0 R 9 0 R 10 0 R] >> >>",
            &[
                (8, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(m) (m)] /Names [] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 9): bad node during find",
        ),
        (
            "binary search no candidate",
            "/Names << /Dests << /Kids [8 0 R 9 0 R] >> >>",
            &[
                (8, "<< /Limits [(m) (m)] /Names [(a) [3 0 R /Fit]] >>"),
                (9, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (b)",
            "Name/Number tree node: attempting to repair after error: Name/Number tree node: unexpected -1 from binary search of kids; limits may by wrong",
        ),
    ];

    for (label, catalog_entries, extra, item_entries, expected_warning) in cases {
        let bytes = single_outline_with_catalog(catalog_entries, item_entries, extra);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert_eq!(
            outline_dest(&root_items(&mut pdf)[0], &mut pdf),
            Object::Null,
            "{label}"
        );
        assert_eq!(
            warning_messages(&pdf).first().map(String::as_str),
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
            (
                8,
                "<< /Limits [(target) (target)] /Names [(target) [3 0 R /Fit]] >>",
            ),
            (9, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert_eq!(
        warning_messages(&pdf),
        [
            "Name/Number tree node: attempting to repair after error: Name/Number tree node: invalid kid at index 1",
            "Name/Number tree node: skipping over invalid kid at index 1",
        ]
    );
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Kids"), None);
    assert_eq!(
        dests.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"target".to_vec()),
            page_dest(3),
            Object::String(b"z".to_vec()),
            page_dest(3),
        ]))
    );
}

#[test]
fn malformed_name_tree_with_only_an_initial_invalid_key_is_fatal() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Names [42 [3 0 R /Fit]] >> >>",
        "/Dest (shape)",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    // See `short_first_name_tree_pair_is_fatal_after_the_repair_warning`:
    // `get_tree()` never touches `/Dest`, so the fatal named-tree error only
    // surfaces once `dest()` is actually called on the item.
    let items = root_items(&mut pdf);
    let mut helper = pdf.outline();
    let error = items[0].get_dest(&mut helper).unwrap_err();
    assert_eq!(
        error.to_string(),
        "parse error at byte 0: Name/Number tree node: item at index 0 is not the right type"
    );
    assert_eq!(
        warning_messages(&pdf),
        ["Name/Number tree node: attempting to repair after error: Name/Number tree node: item at index 0 is not the right type"]
    );
    let dests = direct_dests_root(&mut pdf);
    assert_eq!(dests.get("Kids"), None);
    assert_eq!(
        dests.get("Names"),
        Some(&Object::Array(vec![Object::Integer(42), page_dest(3)]))
    );
}

fn nul_name_tree_repair_pdf() -> Vec<u8> {
    single_outline_with_catalog(
        "/Names << /Dests << /Names [<00> [3 0 R /Fit] 42 [3 0 R /Fit] (z) [3 0 R /Fit]] >> >>",
        "/Dest (m)",
        &[],
    )
}

#[test]
fn malformed_name_tree_repair_preserves_nul_as_pdfdoc_byte_zero() {
    let mut pdf = Pdf::open(Cursor::new(nul_name_tree_repair_pdf())).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
    let names = direct_dests_root(&mut pdf)
        .get("Names")
        .cloned()
        .expect("repair installs /Names");
    let Object::Array(names) = names else {
        panic!("repaired /Names must be an array");
    };
    assert_eq!(names.first(), Some(&Object::String(vec![0x00])));
}

#[test]
#[ignore = "live qpdf 11.9.0 NUL destination repair oracle"]
fn qpdf_malformed_name_tree_repair_preserves_nul_as_pdfdoc_byte_zero() {
    use std::io::Write;
    use std::process::Command;

    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&nul_name_tree_repair_pdf()).unwrap();
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
    let repaired_names = &json["qpdf"][1]["obj:1 0 R"]["value"]["/Names"]["/Dests"]["/Names"];
    assert_eq!(
        repaired_names[0],
        serde_json::Value::String("b:00".to_string())
    );
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
                (9, "<< /Limits [(m) (m)] /Kids [] >>"),
                (10, "<< /Limits [(z) (z)] /Names [(z) [3 0 R /Fit]] >>"),
            ][..],
            "/Dest (m)",
            "bad node during find",
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

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert_eq!(
        warning_messages(&pdf),
        vec![
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits"
        ]
    );

    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve_canonical_object(catalog_ref).unwrap() else {
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
        pdf.resolve_canonical_object(ObjectRef::new(8, 0)).unwrap(),
        Object::Dictionary(_)
    ));

    let serialized = {
        let mut out = Vec::new();
        common::write_default(&mut pdf, &mut out).unwrap();
        out
    };
    let mut reopened = Pdf::open(Cursor::new(serialized)).unwrap();
    let catalog_ref = reopened.root_ref().unwrap();
    let Object::Dictionary(catalog) = reopened.resolve_canonical_object(catalog_ref).unwrap()
    else {
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
    let warning = stderr
        .find("attempting to repair after error:")
        .unwrap_or_else(|| panic!("missing repair warning in {stderr}"));
    let summary = stderr
        .find("qpdf: operation succeeded with warnings")
        .unwrap_or_else(|| panic!("missing warning summary in {stderr}"));
    assert!(warning < summary, "{stderr}");
    assert_eq!(
        stderr.matches("attempting to repair after error:").count(),
        1
    );
    assert_eq!(
        stderr
            .matches("qpdf: operation succeeded with warnings")
            .count(),
        1
    );
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
    pdf.set_object(
        ObjectRef::new(21, 0),
        Object::Reference(ObjectRef::new(22, 0)),
    );

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert_eq!(
        warning_messages(&pdf),
        vec![
            "Name/Number tree node (object 22): attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits"
        ]
    );

    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve_canonical_object(catalog_ref).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Names"),
        Some(&Object::Reference(ObjectRef::new(20, 0)))
    );
    let Object::Dictionary(names) = pdf.resolve_canonical_object(ObjectRef::new(20, 0)).unwrap()
    else {
        panic!("indirect /Names holder must remain a dictionary");
    };
    assert_eq!(
        names.get("Dests"),
        Some(&Object::Reference(ObjectRef::new(21, 0)))
    );
    assert_eq!(
        pdf.resolve_canonical_object(ObjectRef::new(21, 0)).unwrap(),
        Object::Reference(ObjectRef::new(22, 0))
    );
    let Object::Dictionary(dests) = pdf.resolve_canonical_object(ObjectRef::new(22, 0)).unwrap()
    else {
        panic!("terminal /Dests root must remain a dictionary");
    };
    assert_eq!(dests.get("Kids"), None);
    assert!(matches!(dests.get("Names"), Some(Object::Array(_))));
}

#[test]
fn malformed_name_tree_repair_enumerates_all_reachable_branches_and_terminates_cycles() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests << /Kids [12 0 R 8 0 R 9 0 R 10 0 R 11 0 R] >> >>",
        "/Dest (target)",
        &[
            (8, "42"),
            (9, "<< /Limits [(m) (m)] /Kids [9 0 R] >>"),
            (10, "<< /Names [(target) [3 0 R /Fit]] >>"),
            (11, "<< /Limits [(z) (z)] /Names [42 [3 0 R /Fit]] >>"),
            (12, "<< /Limits [(a) (a)] /Names [(a) [3 0 R /Fit]] >>"),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert_eq!(
        warning_messages(&pdf),
        vec![
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 10): node is missing /Limits",
            "Name/Number tree node: skipping over invalid kid at index 1",
            "Name/Number tree node (object 9): loop detected while traversing name/number tree",
            "Name/Number tree node (object 11): item 0 has the wrong type",
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

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
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

    let Object::Dictionary(first) = pdf.resolve_canonical_object(ObjectRef::new(9, 0)).unwrap()
    else {
        panic!("first repaired leaf must be a dictionary");
    };
    let Object::Dictionary(second) = pdf.resolve_canonical_object(ObjectRef::new(10, 0)).unwrap()
    else {
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

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
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
    let Object::Dictionary(first_parent) = pdf.resolve_canonical_object(first_parent_ref).unwrap()
    else {
        panic!("first repaired parent must be a dictionary");
    };
    let Object::Dictionary(second_parent) =
        pdf.resolve_canonical_object(second_parent_ref).unwrap()
    else {
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

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    assert_eq!(
        warning_messages(&pdf),
        [
            "Name/Number tree node: attempting to repair after error: Name/Number tree node (object 8): node is missing /Limits",
            "Name/Number tree node (object 8): items array doesn't have enough elements",
            "Name/Number tree node (object 9): name/number tree node has neither non-empty /Names nor /Kids",
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

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
    let catalog_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve_canonical_object(catalog_ref).unwrap() else {
        panic!("catalog must remain a dictionary");
    };
    assert_eq!(
        catalog.get("Names"),
        Some(&Object::Reference(ObjectRef::new(20, 0)))
    );
    let Object::Dictionary(names) = pdf.resolve_canonical_object(ObjectRef::new(20, 0)).unwrap()
    else {
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
    assert_eq!(outline_dest(&roots[0], &mut pdf), Object::Null);
    assert_eq!(outline_dest(&roots[1], &mut pdf), Object::Integer(42));
    assert_eq!(outline_dest(&roots[2], &mut pdf), page_dest(3));
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Integer(42)
    );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
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
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(2));
    assert_eq!(outline_dest(&roots[1], &mut pdf), page_dest(3));
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
        assert_eq!(
            outline_dest(&root_items(&mut pdf)[0], &mut pdf),
            Object::Null
        );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
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
        assert_eq!(
            outline_dest(&root_items(&mut pdf)[0], &mut pdf),
            Object::Null,
            "{label}"
        );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
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
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
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
    assert!(matches!(
        outline_dest(&node, &mut pdf),
        Object::Dictionary(_)
    ));
    assert_eq!(outline_dest_page(&node, &mut pdf), Object::Null);
}

#[test]
fn empty_destination_array_has_null_dest_page() {
    let bytes = single_outline_with_catalog("", "/Dest []", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let node = root_items(&mut pdf).remove(0);
    assert_eq!(outline_dest(&node, &mut pdf), Object::Array(Vec::new()));
    assert_eq!(outline_dest_page(&node, &mut pdf), Object::Null);
}

#[test]
fn named_destination_materializes_indirect_result_holder() {
    let bytes = single_outline_with_catalog(
        "/Dests << /held 8 0 R >>",
        "/Dest /held",
        &[(8, "[3 0 R /Fit]")],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
}

fn raw_action(pdf: &mut Pdf<Cursor<Vec<u8>>>, item_ref: ObjectRef) -> Object {
    let Object::Dictionary(item) = pdf.resolve_canonical_object(item_ref).unwrap() else {
        panic!("outline item must be a dictionary");
    };
    item.get("A").cloned().unwrap_or(Object::Null)
}

fn resolved_raw_action(pdf: &mut Pdf<Cursor<Vec<u8>>>, item_ref: ObjectRef) -> Object {
    let mut value = raw_action(pdf, item_ref);
    let mut seen = BTreeSet::new();
    while let Object::Reference(reference) = value {
        assert!(seen.insert(reference), "cycle in test action holder");
        value = pdf.resolve_canonical_object(reference).unwrap();
    }
    value
}

#[test]
fn action_goto_direct_d_is_the_node_destination() {
    let mut pdf = Pdf::open(Cursor::new(action_pdf("<< /S /GoTo /D [3 0 R /Fit] >>"))).unwrap();
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
}

/// Regression: `/A /S` stored as an indirect reference (obj 8) to a Name.
/// The destination fallback path must see through the holder reference.
#[test]
fn get_dest_follows_indirect_s_name() {
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
        outline_dest_page(&root[0], &mut pdf),
        Object::Reference(ObjectRef::new(3, 0)),
        "GoTo /D must be picked up even when /S is an indirect ref"
    );
}

#[test]
fn action_non_dict_value_has_null_destination() {
    let mut pdf = Pdf::open(Cursor::new(action_pdf("(not a dict)"))).unwrap();
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
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

    let mut source_refs = vec![ObjectRef::new(3, 0)];
    source_refs.extend(refs.iter().copied());

    let (out, mapping) =
        write_with_settings_and_mapping(&mut pdf, &WriterTestSettings::default(), &source_refs)
            .unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let after: Vec<Object> = refs
        .iter()
        .map(|r| raw_action(&mut reopened, mapping[r]))
        .collect();
    let mut expected = before.clone();
    for value in &mut expected {
        remap_object_refs(value, &mapping);
    }
    assert_eq!(
        expected, after,
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
/// `pages/tree_rebuild.rs`'s "First occurrence: mutate the existing object in
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
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
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
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Array(vec![
            Object::Reference(new_p2),
            Object::Name(b"Fit".to_vec()),
        ]),
        "the named GoTo destination must resolve through the remapped name tree"
    );

    // The name tree's raw "mydest" destination array is what gets remapped.
    let Object::Dictionary(dests) = pdf.resolve_canonical_object(ObjectRef::new(9, 0)).unwrap()
    else {
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
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
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
    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        Object::Null
    );
}

// -----------------------------------------------------------------------
// PR #796 codex round-2 findings: redirect chases and direct-handle
// identity gaps reachable only through public `ObjectHandle` mutation APIs
// (`Pdf::set_object`, `ObjectHandle::shallow_copy`, `replace_key`), never
// from parsing real PDF bytes.
// -----------------------------------------------------------------------

/// A GoTo action whose `/S` is stored as an INDIRECT holder (obj 8), later
/// redirected in place with `Pdf::set_object(8, Object::Reference(9))` to a
/// freshly allocated obj 9 holding the real `/GoTo` name. This is the same
/// flpdf-internal `Pdf::set_object` redirect-bridge shape
/// `resolve_value_handle`'s own doc describes, and that this file's other
/// dest-resolution call sites (`OutlineItem::get_dest`'s `candidate`,
/// `resolve_named_dest_by_name`'s `value`, `resolve_named_dest_by_string`'s
/// `found`, and `goto_action_dest`'s own `action` holder — see
/// `outline_destination_resolves_through_multi_hop_action_holder_chain`
/// above) already chase via `resolve_value_handle` before inspecting the
/// resolved value. `goto_action_dest`'s `/S` subtype check did not: it
/// dereferenced the holder exactly once and compared the intermediate
/// `ObjectValue::Reference(9)` against `ObjectValue::Name("GoTo")`, which
/// never matches. qpdf-oracle-inapplicable by construction — see
/// `dest_from_named_legacy_chases_a_set_object_redirect_chain`'s doc: qpdf
/// has no notion of an object whose own parsed value is another indirect
/// reference.
fn action_subtype_redirect_chain_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Act) /Parent 4 0 R /A << /S 8 0 R /D [3 0 R /Fit] >> >>",
            ),
            (8, "null"),
        ],
        1,
    )
}

#[test]
fn goto_action_subtype_chases_a_set_object_redirect_chain() {
    let mut pdf = Pdf::open(Cursor::new(action_subtype_redirect_chain_pdf())).unwrap();
    pdf.set_object(
        ObjectRef::new(8, 0),
        Object::Reference(ObjectRef::new(9, 0)),
    );
    pdf.set_object(ObjectRef::new(9, 0), Object::Name(b"GoTo".to_vec()));

    assert_eq!(
        outline_dest(&root_items(&mut pdf)[0], &mut pdf),
        page_dest(3)
    );
}

/// `/First` installed as a DIRECT handle whose own resolved value is itself
/// a bare `Object::Reference` — built the way the codex review describes:
/// `Pdf::set_object` redirects a spare holder (obj 9) to the real target
/// (obj 8, an ordinary outline item dict), then `shallow_copy` on that
/// resolved holder handle produces a direct copy (`object_ref() == None`)
/// whose *value* is still `ObjectValue::Reference(8, 0)` — and that direct
/// copy is installed on the Outlines dict's `/First` via the public
/// `ObjectHandle::replace_key` API, never going through `Pdf::set_object`
/// itself. `materialize_item`'s `resolve_handle` (a thin
/// `ObjectHandle::try_dereference` wrapper) is a no-op for a direct handle
/// and never inspects `as_reference()`, so the un-chased cursor's own value
/// (a bare reference, not a dictionary) materialized as a titleless scalar
/// instead of reaching object 8's real `<< /Title (Target) ... >>` content.
fn direct_reference_valued_first_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>"),
            (8, "<< /Title (Target) /Parent 4 0 R /Dest [3 0 R /Fit] >>"),
            (9, "null"),
        ],
        1,
    )
}

#[test]
fn direct_reference_valued_cursor_is_chased_to_its_target() {
    let mut pdf = Pdf::open(Cursor::new(direct_reference_valued_first_pdf())).unwrap();

    // Redirect a spare holder (obj 9) to the real outline item (obj 8), then
    // shallow_copy its resolved handle to obtain a DIRECT handle whose value
    // is still `ObjectValue::Reference(8, 0)`.
    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    let holder = pdf.get_object_handle(ObjectRef::new(9, 0));
    pdf.resolve(&holder).unwrap();
    let direct_reference = holder.shallow_copy().unwrap();
    assert!(direct_reference.object_ref().is_none());
    assert_eq!(direct_reference.as_reference(), Some(ObjectRef::new(8, 0)));

    // Install it as the Outlines dict's /First, bypassing Pdf::set_object.
    let outlines = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve(&outlines).unwrap();
    outlines.replace_key(b"/First", direct_reference).unwrap();

    let roots = root_items(&mut pdf);
    assert_eq!(roots.len(), 1);
    let mut helper = pdf.outline();
    assert_eq!(roots[0].get_title(&mut helper).unwrap(), "Target");
    assert_eq!(outline_dest(&roots[0], &mut pdf), page_dest(3));
}

/// Two DIRECT outline dictionaries (`object_ref() == None`, obtained via
/// `shallow_copy`) whose `/Next` keys reciprocally point at each other,
/// installed as the Outlines dict's `/First` via the public
/// `ObjectHandle::replace_key` API — the exact gap `replace_key`'s own doc
/// records ("does not detect a multi-hop reciprocal cycle built from two or
/// more replace_key calls across distinct direct dictionaries"). Before the
/// direct-identity fix, `get_tree`'s `top_level_seen: BTreeSet<ObjectRef>`
/// never records either node (`object_ref()` is `None` for both), so the
/// `/Next` walk never terminates. Real PDF bytes cannot produce this shape
/// (direct values are inline text; two of them cannot mutually contain each
/// other in a finite file).
fn direct_sibling_cycle_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >>"),
            (5, "<< /Title (A) /Parent 4 0 R >>"),
            (6, "<< /Title (B) /Parent 4 0 R >>"),
        ],
        1,
    )
}

#[test]
fn direct_sibling_next_cycle_terminates_the_top_level_walk() {
    let mut pdf = Pdf::open(Cursor::new(direct_sibling_cycle_pdf())).unwrap();

    let handle_a = pdf.get_object_handle(ObjectRef::new(5, 0));
    pdf.resolve(&handle_a).unwrap();
    let handle_b = pdf.get_object_handle(ObjectRef::new(6, 0));
    pdf.resolve(&handle_b).unwrap();
    let direct_a = handle_a.shallow_copy().unwrap();
    let direct_b = handle_b.shallow_copy().unwrap();
    assert!(direct_a.object_ref().is_none());
    assert!(direct_b.object_ref().is_none());

    direct_a.replace_key(b"/Next", direct_b.clone()).unwrap();
    direct_b.replace_key(b"/Next", direct_a.clone()).unwrap();

    let outlines = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve(&outlines).unwrap();
    outlines.replace_key(b"/First", direct_a).unwrap();

    let roots = root_items(&mut pdf);
    assert_eq!(roots.len(), 2);
    let mut helper = pdf.outline();
    assert_eq!(roots[0].get_title(&mut helper).unwrap(), "A");
    assert_eq!(roots[1].get_title(&mut helper).unwrap(), "B");
}

/// Same reciprocal direct-dictionary `/Next` cycle as
/// `direct_sibling_next_cycle_terminates_the_top_level_walk`, but one level
/// down: the cycle sits among a parent item's CHILDREN (`build_item`'s
/// per-`Frame` `siblings_seen`), not among top-level roots (`get_tree`'s
/// `top_level_seen`) — a distinct loop with its own separate seen set that
/// needs the same direct-identity guard.
fn direct_child_sibling_cycle_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Parent) /Parent 4 0 R /First 6 0 R /Last 7 0 R /Count 2 >>",
            ),
            (6, "<< /Title (A) /Parent 5 0 R >>"),
            (7, "<< /Title (B) /Parent 5 0 R >>"),
        ],
        1,
    )
}

#[test]
fn direct_child_sibling_next_cycle_terminates_the_frame_walk() {
    let mut pdf = Pdf::open(Cursor::new(direct_child_sibling_cycle_pdf())).unwrap();

    let handle_a = pdf.get_object_handle(ObjectRef::new(6, 0));
    pdf.resolve(&handle_a).unwrap();
    let handle_b = pdf.get_object_handle(ObjectRef::new(7, 0));
    pdf.resolve(&handle_b).unwrap();
    let direct_a = handle_a.shallow_copy().unwrap();
    let direct_b = handle_b.shallow_copy().unwrap();

    direct_a.replace_key(b"/Next", direct_b.clone()).unwrap();
    direct_b.replace_key(b"/Next", direct_a.clone()).unwrap();

    let parent = pdf.get_object_handle(ObjectRef::new(5, 0));
    pdf.resolve(&parent).unwrap();
    parent.replace_key(b"/First", direct_a).unwrap();

    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree.roots().len(), 1);
    let parent_item = &tree[tree.roots()[0]];
    assert_eq!(parent_item.get_title(&mut helper).unwrap(), "Parent");
    assert_eq!(parent_item.kids.len(), 2);
    assert_eq!(
        tree[parent_item.kids[0]].get_title(&mut helper).unwrap(),
        "A"
    );
    assert_eq!(
        tree[parent_item.kids[1]].get_title(&mut helper).unwrap(),
        "B"
    );
}

// -----------------------------------------------------------------------
// PR #796 codex round-3 findings: the round-2 fix taught `materialize_item`
// to chase a direct reference-valued cursor, but `get_tree`'s top-level
// `/Next` walk, `build_item`'s per-frame sibling walk, and `has_outlines`
// all inspect a cursor's identity/nullness *before* `materialize_item` ever
// runs, so none of them saw the chased target. qpdf-oracle-inapplicable by
// construction, same as the round-2 findings above: these shapes need
// `Pdf::set_object` + `ObjectHandle::shallow_copy` to construct a direct
// handle whose own value is a bare reference, and qpdf's own
// `QPDF::replaceObject` (`libqpdf/QPDF.cc:1980-1991`) throws
// `std::logic_error` given an indirect handle, so qpdf's object graph can
// never hold this state at all — there is no qpdf byte output to compare
// against, only qpdf's *architectural* precedent that identity is recorded
// before further per-node work runs (`QPDFObjGen::set`-guarded constructor,
// `libqpdf/QPDFOutlineDocumentHelper.cc:16-21`).
// -----------------------------------------------------------------------

/// Same direct reference-valued `/First` shape as
/// `direct_reference_valued_first_pdf`, but the chased target's own `/Next`
/// points back at itself (an ordinary indirect self-loop, reachable from
/// real PDF bytes on its own). Before recording identity *after* chasing,
/// `get_tree`'s first iteration takes the direct-cursor branch on the
/// un-chased wrapper (recording only the wrapper's own identity, since a
/// direct handle's `object_ref()` is always `None`) and only discovers the
/// target's real `ObjectRef` two iterations later, once the raw `/Next`
/// reference itself becomes the cursor — one full extra iteration after the
/// target was already materialized once.
fn direct_reference_valued_first_self_loop_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>"),
            (8, "<< /Title (Target) /Parent 4 0 R /Next 8 0 R >>"),
            (9, "null"),
        ],
        1,
    )
}

#[test]
fn direct_reference_valued_cursor_self_loop_is_recorded_before_a_second_visit() {
    let mut pdf = Pdf::open(Cursor::new(direct_reference_valued_first_self_loop_pdf())).unwrap();

    // Same construction as `direct_reference_valued_cursor_is_chased_to_its_target`:
    // redirect a spare holder (obj 9) to the real outline item (obj 8), then
    // shallow_copy its resolved handle to obtain a DIRECT handle whose value
    // is still `ObjectValue::Reference(8, 0)`.
    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    let holder = pdf.get_object_handle(ObjectRef::new(9, 0));
    pdf.resolve(&holder).unwrap();
    let direct_reference = holder.shallow_copy().unwrap();
    assert!(direct_reference.object_ref().is_none());
    assert_eq!(direct_reference.as_reference(), Some(ObjectRef::new(8, 0)));

    let outlines = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve(&outlines).unwrap();
    outlines.replace_key(b"/First", direct_reference).unwrap();

    // Object 8 must be visited exactly once, not twice: the self-loop is a
    // repeat of the SAME node, not two distinct siblings.
    let roots = root_items(&mut pdf);
    assert_eq!(roots.len(), 1);
    let mut helper = pdf.outline();
    assert_eq!(roots[0].get_title(&mut helper).unwrap(), "Target");
}

/// Same reciprocal-cycle shape as
/// `direct_reference_valued_cursor_self_loop_is_recorded_before_a_second_visit`,
/// but one level down: a parent item's `/First` is the direct
/// reference-valued wrapper, and the resolved child's own `/Next` is an
/// ordinary indirect self-loop back to that child — exercising
/// `build_item`'s per-frame `siblings_seen`/`direct_siblings_seen`, not
/// `get_tree`'s top-level ones.
fn direct_reference_valued_first_child_self_loop_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (Parent) /Parent 4 0 R /First 8 0 R /Last 8 0 R /Count 1 >>",
            ),
            (8, "<< /Title (Child) /Parent 5 0 R /Next 8 0 R >>"),
            (9, "null"),
        ],
        1,
    )
}

#[test]
fn direct_reference_valued_child_cursor_self_loop_is_recorded_before_a_second_visit() {
    let mut pdf = Pdf::open(Cursor::new(
        direct_reference_valued_first_child_self_loop_pdf(),
    ))
    .unwrap();

    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    let holder = pdf.get_object_handle(ObjectRef::new(9, 0));
    pdf.resolve(&holder).unwrap();
    let direct_reference = holder.shallow_copy().unwrap();
    assert!(direct_reference.object_ref().is_none());
    assert_eq!(direct_reference.as_reference(), Some(ObjectRef::new(8, 0)));

    let parent = pdf.get_object_handle(ObjectRef::new(5, 0));
    pdf.resolve(&parent).unwrap();
    parent.replace_key(b"/First", direct_reference).unwrap();

    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    assert_eq!(tree.roots().len(), 1);
    let parent_item = &tree[tree.roots()[0]];
    assert_eq!(parent_item.get_title(&mut helper).unwrap(), "Parent");
    // Object 8 must appear exactly once among the parent's kids.
    assert_eq!(parent_item.kids.len(), 1);
    assert_eq!(
        tree[parent_item.kids[0]].get_title(&mut helper).unwrap(),
        "Child"
    );
}

/// A direct reference-valued `/First` (same construction as
/// `direct_reference_valued_first_pdf`) whose terminal target is null
/// (obj 8, `null`). `has_outlines` read `/First`'s raw value and called
/// `try_is_null` on the un-chased wrapper directly; `try_is_null` only
/// dereferences its own receiver and never follows a bare-reference-valued
/// result, so it saw `ObjectValue::Reference(8, 0)` — not
/// `ObjectValue::Null` — and reported non-null. `get_tree`, which chases
/// the same shape inside `materialize_item` before checking, correctly
/// reports zero roots for this exact document.
fn direct_reference_valued_first_to_null_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines >>"),
            (8, "null"),
            (9, "null"),
        ],
        1,
    )
}

#[test]
fn has_outlines_agrees_with_get_tree_for_a_direct_reference_valued_first_targeting_null() {
    let mut pdf = Pdf::open(Cursor::new(direct_reference_valued_first_to_null_pdf())).unwrap();

    pdf.set_object(
        ObjectRef::new(9, 0),
        Object::Reference(ObjectRef::new(8, 0)),
    );
    let holder = pdf.get_object_handle(ObjectRef::new(9, 0));
    pdf.resolve(&holder).unwrap();
    let direct_reference = holder.shallow_copy().unwrap();
    assert!(direct_reference.object_ref().is_none());
    assert_eq!(direct_reference.as_reference(), Some(ObjectRef::new(8, 0)));

    let outlines = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve(&outlines).unwrap();
    outlines.replace_key(b"/First", direct_reference).unwrap();

    assert!(!pdf.outline().has_outlines().unwrap());
    assert!(pdf.outline().get_tree().unwrap().roots().is_empty());
}

fn two_page_single_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (
                5,
                "<< /Title (One) /Parent 4 0 R /Dest [3 0 R /Fit] /Count 1 >>",
            ),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// qpdf's `getTitle()`/`getCount()`/`getDest()`/`getDestPage()`
/// (`libqpdf/QPDFOutlineObjectHelper.cc:47-98`) hold no cache and re-read
/// `this->oh` fresh on every call — only `getParent()`/`getKids()` are
/// captured once, in the constructor. `OutlineItem::get_title`/`get_count`/
/// `get_dest`/`get_dest_page` mirror that: they read `OutlineItem::object` (a live,
/// shared-identity handle) fresh on every call instead of returning a
/// value frozen at `get_tree()` time. A mutation applied through the public
/// `ObjectHandle::replace_key` API between two calls must be visible on the
/// second call.
#[test]
fn title_count_dest_recompute_live_after_object_mutation() {
    let mut pdf = Pdf::open(Cursor::new(two_page_single_outline_pdf())).unwrap();
    let page_six = pdf.get_object_handle(ObjectRef::new(6, 0));

    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let item = tree[tree.roots()[0]].clone();

    assert_eq!(item.get_title(&mut helper).unwrap(), "One");
    assert_eq!(item.get_count(&mut helper).unwrap(), 1);
    assert_eq!(
        item.get_dest_page(&mut helper).unwrap().object_ref(),
        Some(ObjectRef::new(3, 0))
    );

    item.object
        .replace_key(b"/Title", ObjectHandle::string(b"Two".to_vec()))
        .unwrap();
    item.object
        .replace_key(b"/Count", ObjectHandle::integer(-2))
        .unwrap();
    item.object
        .replace_key(
            b"/Dest",
            ObjectHandle::array(vec![page_six, ObjectHandle::name(b"Fit".to_vec())]),
        )
        .unwrap();

    assert_eq!(
        item.get_title(&mut helper).unwrap(),
        "Two",
        "title() must not return a value frozen before the mutation"
    );
    assert_eq!(
        item.get_count(&mut helper).unwrap(),
        -2,
        "count() must not return a value frozen before the mutation"
    );
    assert_eq!(
        item.get_dest_page(&mut helper).unwrap().object_ref(),
        Some(ObjectRef::new(6, 0)),
        "dest_page() must not return a value frozen before the mutation"
    );
}

/// qpdf's `resolveNamedDest` fetches the catalog's `/Dests` dictionary into
/// `QPDFOutlineDocumentHelper::Members::dest_dict` once and reuses it for
/// the rest of that document helper instance's lifetime
/// (`libqpdf/QPDFOutlineDocumentHelper.cc:60-63`), unlike `getDest()` itself.
/// `OutlineDocumentHelper::cached_dest_dict` mirrors that: swapping the
/// catalog's `/Dests` entry after the cache is already populated must not
/// change what the *same* helper session resolves, while a fresh session
/// (a fresh `pdf.outline()` call) must observe the swap.
#[test]
fn dest_dict_cache_holds_stale_value_within_one_helper_session() {
    let bytes = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Dests << /shape [3 0 R /Fit] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (One) /Parent 4 0 R /Dest /shape >>"),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let page_six = pdf.get_object_handle(ObjectRef::new(6, 0));
    let root_ref = pdf.root_ref().unwrap();
    let catalog = pdf.get_object_handle(root_ref);
    pdf.resolve(&catalog).unwrap();

    let mut helper = pdf.outline();
    let tree = helper.get_tree().unwrap();
    let item = tree[tree.roots()[0]].clone();

    // Populate `dest_dict` from the original catalog `/Dests`.
    assert_eq!(
        item.get_dest_page(&mut helper).unwrap().object_ref(),
        Some(ObjectRef::new(3, 0))
    );

    // Point the catalog at a brand-new `/Dests` dictionary after the cache
    // above already captured the old one.
    let new_dests = ObjectHandle::dictionary(vec![(
        b"/shape".to_vec(),
        ObjectHandle::array(vec![page_six, ObjectHandle::name(b"Fit".to_vec())]),
    )]);
    catalog.replace_key(b"/Dests", new_dests).unwrap();

    // Same helper session: `dest_dict` still holds the old handle.
    assert_eq!(
        item.get_dest_page(&mut helper).unwrap().object_ref(),
        Some(ObjectRef::new(3, 0)),
        "cached dest_dict must not observe a /Dests swap mid-session"
    );

    // A fresh OutlineDocumentHelper session re-fetches /Dests and observes
    // the swap.
    drop(helper);
    let mut fresh_helper = pdf.outline();
    assert_eq!(
        item.get_dest_page(&mut fresh_helper).unwrap().object_ref(),
        Some(ObjectRef::new(6, 0)),
        "a new helper session must observe the swapped /Dests"
    );
}
