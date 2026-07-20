//! Integration tests for [`flpdf::OutlineDocumentHelper`].

use flpdf::{check_legacy_dests, check_name_tree_dests, write_pdf, ObjectRef, Pdf, Severity};
use std::collections::BTreeMap;
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
fn has_outlines_false_when_outline_dict_has_no_first() {
    let mut pdf = Pdf::open(Cursor::new(outline_present_but_empty_pdf())).unwrap();
    assert!(!pdf.outline().has_outlines().unwrap());
}

#[test]
fn get_root_materializes_tree_with_titles_counts_parents() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();

    // Two top-level nodes: A, B.
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].title, "A");
    assert_eq!(roots[0].depth, 0);
    assert_eq!(roots[0].parent, None); // top-level: /Outlines dict is not an item -> None (qpdf getParent)
    assert_eq!(roots[0].count, 1);
    assert_eq!(roots[1].title, "B");
    assert_eq!(roots[1].count, 2);

    // A has one child A1.
    assert_eq!(roots[0].children.len(), 1);
    let a1 = &roots[0].children[0];
    assert_eq!(a1.title, "A1");
    assert_eq!(a1.depth, 1);
    assert_eq!(a1.parent, Some(ObjectRef::new(5, 0)));
    assert_eq!(a1.count, 0); // /Count absent -> 0 (qpdf)
    assert_eq!(a1.object_ref, ObjectRef::new(6, 0));
}

#[test]
fn get_root_empty_when_no_outline() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(pdf.outline().get_root().unwrap().is_empty());
}

#[test]
fn iter_yields_preorder() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let titles: Vec<String> = pdf.outline().iter().unwrap().map(|n| n.title).collect();
    assert_eq!(titles, vec!["A", "A1", "B"]); // pre-order: A, its child A1, then B

    // iter() yields a flattened view: every node has its children cleared.
    let mut pdf2 = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    assert!(pdf2
        .outline()
        .iter()
        .unwrap()
        .all(|n| n.children.is_empty()));
}

#[test]
fn walk_visits_preorder_with_depth() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let mut seen: Vec<(String, usize, usize)> = Vec::new(); // (title, depth, child_count)
    pdf.outline()
        .walk(|node, depth| seen.push((node.title.clone(), depth, node.children.len())))
        .unwrap();
    assert_eq!(
        seen,
        vec![
            ("A".to_string(), 0, 1), // A has one child (A1) — populated in walk
            ("A1".to_string(), 1, 0),
            ("B".to_string(), 0, 0),
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
    let count = pdf.outline().iter().unwrap().count();
    assert_eq!(count, 30);
    // deepest node is at depth 29
    let max_depth = pdf
        .outline()
        .iter()
        .unwrap()
        .map(|n| n.depth)
        .max()
        .unwrap();
    assert_eq!(max_depth, 29);
}

#[test]
fn depth_cap_is_enforced() {
    let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(10))).unwrap();
    let err = pdf.outline().get_root_with_max_depth(5);
    assert!(err.is_err(), "expected depth-cap error, got {err:?}");
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
    let titles: Vec<String> = pdf.outline().iter().unwrap().map(|n| n.title).collect();
    // Visits X and Y once each, then the cycle back to 5 is cut by `visited`.
    assert_eq!(titles, vec!["X", "Y"]);
}

#[test]
fn dest_from_explicit_dest_array() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    let a1 = &roots[0].children[0]; // A1 has /Dest [3 0 R /Fit]
    let dest = a1.dest.as_ref().expect("A1 should have a dest");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
    // Nodes without a dest stay None.
    assert!(roots[1].dest.is_none()); // B
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
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0]
        .dest
        .as_ref()
        .expect("GoTo action should yield a dest");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
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
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0]
        .dest
        .as_ref()
        .expect("indirect /Dest should resolve");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
}

/// Outline item whose /Dest points at a dict whose /D points back at itself:
/// 8 0 obj << /D 8 0 R >>. Resolution must terminate (depth bound) -> None.
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
fn cyclic_dest_terminates_as_none() {
    let mut pdf = Pdf::open(Cursor::new(cyclic_dest_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    // The cyclic /D bottoms out at the depth bound and resolves to no dest.
    assert!(roots[0].dest.is_none());
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
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0].dest.as_ref().expect("named dest should resolve");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
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
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0]
        .dest
        .as_ref()
        .expect("legacy named dest should resolve");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
}

/// Legacy /Dests with a NAME->NAME cycle: /a -> /b, /b -> /a. Resolution must
/// terminate at the depth bound and yield no dest (not overflow the stack).
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
fn cyclic_named_dest_terminates_as_none() {
    let mut pdf = Pdf::open(Cursor::new(cyclic_named_dest_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    // The /a -> /b -> /a name cycle bottoms out at the depth bound -> no dest.
    assert!(roots[0].dest.is_none());
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
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0]
        .dest
        .as_ref()
        .expect("collision named dest should resolve");
    // Modern name-tree entry ([3 0 R ...]) wins over legacy /Dests ([2 0 R ...]).
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
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
    let roots = pdf.outline().get_root().unwrap();
    assert_eq!(roots[0].title, "RealTitle");
}

// -----------------------------------------------------------------------
// flpdf-9hc.14.1: catalog-level legacy /Dests dictionary read/diagnostics
// -----------------------------------------------------------------------

/// Catalog whose legacy `/Dests` is an INLINE (direct) dictionary on the
/// catalog with three entries, two distinct target pages.
fn legacy_dests_inline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Dests << /alpha [3 0 R /Fit] \
                 /beta [4 0 R /XYZ 0 792 0] /gamma [3 0 R /FitH 792] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

#[test]
fn legacy_dests_reads_inline_dictionary_entries_sorted_by_name() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_inline_pdf())).unwrap();
    let entries = pdf.outline().legacy_dests().unwrap();

    // Dictionary::iter() yields lexicographic key order.
    let names: Vec<Vec<u8>> = entries.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(
        names,
        vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()]
    );

    let alpha = entries[0].1.as_ref().expect("alpha dest should resolve");
    assert_eq!(alpha.page(), Some(ObjectRef::new(3, 0)));
    let beta = entries[1].1.as_ref().expect("beta dest should resolve");
    assert_eq!(beta.page(), Some(ObjectRef::new(4, 0)));
    let gamma = entries[2].1.as_ref().expect("gamma dest should resolve");
    assert_eq!(gamma.page(), Some(ObjectRef::new(3, 0)));
}

/// Catalog whose legacy `/Dests` is an INDIRECT reference (object 8) to the
/// dictionary — the other form permitted by the spec.
fn legacy_dests_indirect_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /solo [3 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn legacy_dests_reads_indirect_dictionary() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_indirect_pdf())).unwrap();
    let entries = pdf.outline().legacy_dests().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"solo");
    assert_eq!(
        entries[0].1.as_ref().unwrap().page(),
        Some(ObjectRef::new(3, 0))
    );
}

#[test]
fn legacy_dests_absent_yields_empty_vec() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(pdf.outline().legacy_dests().unwrap().is_empty());
}

/// Round-trip acceptance: opening a document with a legacy `/Dests`
/// dictionary and rewriting it via [`write_pdf`] (no page operations, no
/// mutation) must preserve every entry unchanged.
#[test]
fn legacy_dests_round_trip_through_write_pdf_unmodified() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_indirect_pdf())).unwrap();
    let before = pdf.outline().legacy_dests().unwrap();
    assert_eq!(before.len(), 1, "sanity: fixture has one dest entry");

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let after = reopened.outline().legacy_dests().unwrap();
    assert_eq!(before, after, "/Dests entries must round-trip unmodified");
}

#[test]
fn check_legacy_dests_no_diagnostics_when_all_targets_exist() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_inline_pdf())).unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().is_empty());
}

/// A `/Dests` entry ("gone") targets object `99 0 R`, which is never defined
/// in this document — a dangling reference. Acceptance: this must produce a
/// diagnostic, not fail the call.
fn legacy_dests_missing_target_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /gone [99 0 R /Fit] /here [3 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn check_legacy_dests_missing_target_is_warning_not_error() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_missing_target_pdf())).unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    let entries = diagnostics.entries();
    assert_eq!(
        entries.len(),
        1,
        "only the dangling entry should be flagged"
    );
    assert_eq!(entries[0].severity, Severity::Warning);
    assert!(entries[0].message.contains("gone"));
    assert!(entries[0].message.contains("99 0 R"));
}

/// A `/Dests` entry targets object `2 0 R`, which exists but is the `/Pages`
/// root, not a `/Page` leaf — also a "missing target page".
fn legacy_dests_non_page_target_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /wrong [2 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn check_legacy_dests_target_not_a_page_is_warning() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_non_page_target_pdf())).unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert_eq!(diagnostics.entries().len(), 1);
    assert_eq!(diagnostics.entries()[0].severity, Severity::Warning);
}

/// `check_legacy_dests` must not fail even when the document has no `/Pages`
/// tree at all to enumerate (downgraded to a warning, matching `check.rs`'s
/// own page-enumeration-failure posture).
#[test]
fn check_legacy_dests_missing_page_tree_downgrades_to_warning() {
    let mut pdf = Pdf::open(Cursor::new(build_pdf(
        &[
            (1, "<< /Type /Catalog /Dests 8 0 R >>"),
            (8, "<< /gone [99 0 R /Fit] >>"),
        ],
        1,
    )))
    .unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().iter().any(
        |d| d.severity == Severity::Warning && d.message.contains("could not enumerate pages")
    ));
}

/// Acceptance: after a page-tree rebuild (e.g. `--pages` subset selection)
/// renumbers pages, a surviving-page legacy `/Dests` entry must read back
/// remapped to its new ref, and a removed-page entry (left verbatim,
/// resolving to the nulled-out page) must be flagged by
/// [`check_legacy_dests`].
#[test]
fn legacy_dests_reflects_remap_after_page_tree_rebuild() {
    let mut pdf = Pdf::open(Cursor::new(build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /keep [3 0 R /Fit] /drop [4 0 R /Fit] >>"),
        ],
        1,
    )))
    .unwrap();

    let result = flpdf::rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
    let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
    flpdf::remap_outline_and_dests(&mut pdf, &result).unwrap();

    let entries = pdf.outline().legacy_dests().unwrap();
    let keep = entries
        .iter()
        .find(|(n, _)| n == b"keep")
        .expect("keep entry present");
    assert_eq!(
        keep.1.as_ref().unwrap().page(),
        Some(new_p1),
        "surviving page's dest should read back remapped to its new ref"
    );

    let drop = entries
        .iter()
        .find(|(n, _)| n == b"drop")
        .expect("drop entry present (qpdf null-out parity: never dropped)");
    assert_eq!(
        drop.1.as_ref().unwrap().page(),
        Some(ObjectRef::new(4, 0)),
        "removed page's dest is left verbatim, now resolving to a nulled object"
    );

    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert_eq!(
        diagnostics.entries().len(),
        1,
        "the nulled removed-page dest should be the only flagged entry"
    );
    assert!(diagnostics.entries()[0].message.contains("drop"));
}

/// `check_legacy_dests` must short-circuit before enumerating the page tree
/// when the catalog carries no legacy `/Dests` dictionary at all.
#[test]
fn check_legacy_dests_returns_empty_when_no_dests_dict() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().is_empty());
}

/// A `/Dests` entry whose value is an array with no resolvable page
/// reference (first element is a name, not an indirect reference). This is a
/// malformed destination, not a "missing target page": `Dest::page()`
/// returns `None`, so no diagnostic is produced for it.
fn legacy_dests_no_page_ref_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /odd [/NotAPageRef /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn check_legacy_dests_skips_entries_without_resolvable_page_ref() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_no_page_ref_pdf())).unwrap();
    // The value parses as a Dest (it is an array), but has no page ref.
    let entries = pdf.outline().legacy_dests().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].1.as_ref().unwrap().page().is_none());

    // check_legacy_dests must not flag it (nothing to validate) and must not error.
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().is_empty());
}

/// Mixed /Dests: one entry has a resolvable page ref (so the early-return
/// short-circuit does NOT fire), a second entry has no resolvable page ref
/// (so the validation loop below must `continue` past it without adding a
/// diagnostic). This covers the in-loop skip for `dest.page().is_none()`.
fn legacy_dests_mixed_page_ref_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /good [3 0 R /Fit] /odd [/NotAPageRef /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn check_legacy_dests_continues_past_entry_without_page_ref_when_others_have_one() {
    let mut pdf = Pdf::open(Cursor::new(legacy_dests_mixed_page_ref_pdf())).unwrap();
    // Sanity: one entry resolves to page 3, the other resolves to None.
    let entries = pdf.outline().legacy_dests().unwrap();
    assert_eq!(entries.len(), 2);
    // Both targets are live/malformed but present in a live document → no
    // diagnostics: the `good` entry hits page 3, the `odd` entry hits the
    // `continue` in the validation loop.
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert!(
        diagnostics.entries().is_empty(),
        "no page is missing and the odd entry has no page ref to validate"
    );
}

/// B1 regression: catalog `/Dests` reached through a multi-hop holder chain
/// (catalog `/Dests 10 0 R`, obj 10 → obj 11 → dict) must still be read.
/// The prior reader stopped after a single hop and returned Vec::new(),
/// silently missing every named destination — and, by extension, every
/// dangling-target warning `check_legacy_dests` would have emitted for
/// them.
#[test]
fn legacy_dests_reads_through_multi_hop_holder_chain() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            // 2-hop holder chain into the /Dests dict.
            (10, "11 0 R"),
            (11, "<< /only [3 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let entries = pdf.outline().legacy_dests().unwrap();
    assert_eq!(entries.len(), 1, "multi-hop /Dests must still resolve");
    assert_eq!(entries[0].0, b"only");
    assert_eq!(
        entries[0].1.as_ref().expect("dest resolves").page(),
        Some(ObjectRef::new(3, 0)),
    );
}

/// B2 regression: a destination whose page operand is stored behind a
/// holder (`/h [30 0 R /Fit]` where `30 0 obj 3 0 R`) points at a live
/// page — `check_legacy_dests` must normalise through the holder chain
/// before comparing against `page_refs`, or it emits a false-positive
/// "not a page" warning.
#[test]
fn check_legacy_dests_follows_page_ref_through_holder() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            // /Dests -> /h [30 0 R /Fit], and obj 30 is a bare ref to page 3.
            (8, "<< /h [30 0 R /Fit] >>"),
            (30, "3 0 R"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    assert!(
        diagnostics.entries().is_empty(),
        "the holder-chain page ref resolves to a live page, no diagnostic: got {:?}",
        diagnostics.entries()
    );
}

/// Malformed catalog: `/Dests` is present but resolves to a non-dictionary
/// value (an integer here). Covers the `let Object::Dictionary(dests) = ..
/// else { return Ok(Vec::new()); }` fallback added alongside the multi-hop
/// holder-chain resolve — matches the sibling `/Names` fallback on sub-2.
#[test]
fn legacy_dests_returns_empty_when_dests_is_not_a_dictionary() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            // /Dests resolves to a plain integer — spec-nonconforming.
            (10, "42"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let entries = pdf.outline().legacy_dests().unwrap();
    assert!(entries.is_empty(), "non-dict /Dests must read as empty");
}

/// Cover the depth-cap fallback in `resolve_page_ref_through_holders`: a
/// destination whose page operand is an unbounded ref chain must terminate
/// (returning the last ref we managed to walk to) instead of looping
/// forever. Uses a chain that exceeds MAX_DEST_RESOLVE_DEPTH via reused
/// object numbers.
#[test]
fn check_legacy_dests_holder_chain_depth_cap_terminates() {
    // Build a 70-object bare-ref chain 100 → 101 → 102 → … → 169 → 3 0 R
    // (page 3). MAX_DEST_RESOLVE_DEPTH is 64, so the walker gives up at
    // hop 64 and returns object 164, which is NOT in live_pages, so this
    // legitimately-if-pathologically-deep target reads as dangling. The
    // point of this test is that it terminates (no infinite loop) and
    // emits a diagnostic instead.
    let mut objs: Vec<(u32, String)> = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R >>".to_string(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (8, "<< /deep [100 0 R /Fit] >>".to_string()),
    ];
    for i in 100..169 {
        objs.push((i, format!("{} 0 R", i + 1)));
    }
    objs.push((169, "3 0 R".to_string()));
    let refs: Vec<(u32, &str)> = objs.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let pdf_bytes = build_pdf(&refs, 1);
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let diagnostics = check_legacy_dests(&mut pdf).unwrap();
    // Test succeeded by not looping forever; the depth-capped ref is not
    // in the page tree, so one warning is emitted.
    assert_eq!(
        diagnostics.entries().len(),
        1,
        "got {:?}",
        diagnostics.entries()
    );
}

/// Codex round-3: alias resolution inside a chained-through `/Dests` dict.
/// `/Dests 10 0 R` → obj 10 = `11 0 R` → obj 11 = `<< /alias /target /target [3 0 R /Fit] >>`.
/// The `alias` entry is a name pointing at `target`. Before the fix,
/// legacy_dests followed the chain (sub-1) but resolve_named_dest still
/// used the single-hop catalog_value, so the alias silently resolved to
/// None and check_legacy_dests skipped the entry entirely.
#[test]
fn legacy_dests_resolves_alias_through_chained_dests_dict() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Dests 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            // Multi-hop holder chain to the dict.
            (10, "11 0 R"),
            (11, "<< /alias /target /target [3 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let entries = pdf.outline().legacy_dests().unwrap();
    // Two entries; both must resolve to the same page (alias → target →
    // page 3), not None for alias.
    assert_eq!(entries.len(), 2, "got {entries:?}");
    for (name, dest) in &entries {
        assert!(
            dest.as_ref().and_then(|d| d.page()).is_some(),
            "entry {:?} must resolve to a page (alias must chase target through chained /Dests)",
            std::str::from_utf8(name).unwrap_or("?")
        );
    }
}

// ── /Names /Dests name tree (modern, PDF 1.2+) ────────────────────────────────
//
// Reader ([`OutlineDocumentHelper::name_tree_dests`]) and diagnostic
// ([`check_name_tree_dests`]) coverage. Mirrors the legacy_dests tests above;
// the writer (insert/delete/rebuild) is covered separately in
// `name_tree_dests_tests.rs`.

/// A flat `/Names /Dests` leaf with two entries, one holding a dict value
/// (`<< /D array >>`) instead of a bare array (both forms are valid per ISO
/// 32000-2 §12.3.2.3).
fn name_tree_dests_single_level_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /Dests 9 0 R >>"),
            (
                9,
                "<< /Names [(alpha) [3 0 R /Fit] (beta) << /D [4 0 R /XYZ 0 792 0] >>] >>",
            ),
        ],
        1,
    )
}

#[test]
fn name_tree_dests_reads_flat_leaf_entries_in_key_order() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_single_level_pdf())).unwrap();
    let entries = pdf.outline().name_tree_dests().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, b"alpha");
    assert_eq!(
        entries[0].1.as_ref().expect("alpha resolves").page(),
        Some(ObjectRef::new(3, 0))
    );
    assert_eq!(entries[1].0, b"beta");
    assert_eq!(
        entries[1].1.as_ref().expect("beta resolves").page(),
        Some(ObjectRef::new(4, 0)),
        "a << /D array >> dict value must resolve just like a bare array"
    );
}

/// A `/Names /Dests` tree nested 5 levels deep via `/Kids` chains, with the
/// sole entry at the deepest leaf. Verifies the reader's depth-first walk
/// reaches entries well past a shallow tree (acceptance: "round-trip
/// preserves ordering and depth").
///
/// Object layout: 10 -> Kids[11] -> Kids[12] -> Kids[13] -> Kids[14] -> leaf
/// (/Names [(deep) [3 0 R /Fit]]).
fn name_tree_dests_deep_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /Dests 10 0 R >>"),
            (10, "<< /Kids [11 0 R] >>"),
            (11, "<< /Kids [12 0 R] >>"),
            (12, "<< /Kids [13 0 R] >>"),
            (13, "<< /Kids [14 0 R] >>"),
            (14, "<< /Names [(deep) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn name_tree_dests_reads_through_five_level_deep_kids_chain() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_deep_pdf())).unwrap();
    let entries = pdf.outline().name_tree_dests().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, b"deep");
    assert_eq!(
        entries[0].1.as_ref().expect("deep entry resolves").page(),
        Some(ObjectRef::new(3, 0))
    );
}

#[test]
fn name_tree_dests_absent_names_key_yields_empty_vec() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(pdf.outline().name_tree_dests().unwrap().is_empty());
}

#[test]
fn name_tree_dests_names_present_but_no_dests_key_yields_empty_vec() {
    // /Names dict exists (e.g. for /EmbeddedFiles) but has no /Dests entry.
    let mut pdf = Pdf::open(Cursor::new(build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
            (8, "<< /EmbeddedFiles 9 0 R >>"),
            (9, "<< /Names [] >>"),
        ],
        1,
    )))
    .unwrap();
    assert!(pdf.outline().name_tree_dests().unwrap().is_empty());
}

/// Regression: `/Names` reached via a 2-hop indirect holder chain (catalog
/// /Names -> ref -> ref -> dict) must not silently return empty. The writer
/// already uses `resolve_ref_chain` for this; before this fix the reader
/// stopped after a single hop and matched the intermediate Reference
/// against Object::Dictionary, dropping every entry on the floor.
#[test]
fn name_tree_dests_reads_through_multi_hop_names_chain() {
    let pdf_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            // 2-hop holder chain into the /Names dict.
            (10, "11 0 R"),
            (11, "<< /Dests 12 0 R >>"),
            (12, "<< /Names [(only) [3 0 R /Fit]] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
    let entries = pdf.outline().name_tree_dests().unwrap();
    assert_eq!(entries.len(), 1, "multi-hop /Names must still resolve");
    assert_eq!(entries[0].0, b"only");
    assert_eq!(
        entries[0].1.as_ref().expect("dest resolves").page(),
        Some(ObjectRef::new(3, 0)),
    );
}

/// Round-trip acceptance: opening a document with a `/Names /Dests` name
/// tree and rewriting it via [`write_pdf`] (no page operations, no mutation)
/// must preserve every entry, in the same order, unchanged.
#[test]
fn name_tree_dests_round_trip_through_write_pdf_unmodified() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_deep_pdf())).unwrap();
    let before = pdf.outline().name_tree_dests().unwrap();
    assert_eq!(before.len(), 1, "sanity: fixture has one dest entry");

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let after = reopened.outline().name_tree_dests().unwrap();
    assert_eq!(
        before, after,
        "/Names /Dests entries must round-trip unmodified"
    );
}

#[test]
fn check_name_tree_dests_no_diagnostics_when_all_targets_exist() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_single_level_pdf())).unwrap();
    let diagnostics = check_name_tree_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().is_empty());
}

/// A `/Names /Dests` entry ("gone") targets object `99 0 R`, which is never
/// defined in this document — a dangling reference. Acceptance: this must
/// produce a diagnostic, not fail the call.
fn name_tree_dests_missing_target_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(gone) [99 0 R /Fit] (here) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn check_name_tree_dests_missing_target_is_warning_not_error() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_missing_target_pdf())).unwrap();
    let diagnostics = check_name_tree_dests(&mut pdf).unwrap();
    let entries = diagnostics.entries();
    assert_eq!(
        entries.len(),
        1,
        "only the dangling entry should be flagged"
    );
    assert_eq!(entries[0].severity, Severity::Warning);
    assert!(entries[0].message.contains("gone"));
    assert!(entries[0].message.contains("99 0 R"));
}

/// A `/Names /Dests` entry targets object `2 0 R`, which exists but is the
/// `/Pages` root, not a `/Page` leaf — also a "missing target page".
fn name_tree_dests_non_page_target_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(wrong) [2 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn check_name_tree_dests_target_not_a_page_is_warning() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_non_page_target_pdf())).unwrap();
    let diagnostics = check_name_tree_dests(&mut pdf).unwrap();
    assert_eq!(diagnostics.entries().len(), 1);
    assert_eq!(diagnostics.entries()[0].severity, Severity::Warning);
}

/// `check_name_tree_dests` must not fail even when the document has no
/// `/Pages` tree at all to enumerate (downgraded to a warning).
#[test]
fn check_name_tree_dests_missing_page_tree_downgrades_to_warning() {
    let mut pdf = Pdf::open(Cursor::new(build_pdf(
        &[
            (1, "<< /Type /Catalog /Names 8 0 R >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(gone) [99 0 R /Fit]] >>"),
        ],
        1,
    )))
    .unwrap();
    let diagnostics = check_name_tree_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().iter().any(
        |d| d.severity == Severity::Warning && d.message.contains("could not enumerate pages")
    ));
}

/// `check_name_tree_dests` must short-circuit before enumerating the page
/// tree when the catalog carries no `/Names /Dests` name tree at all.
#[test]
fn check_name_tree_dests_returns_empty_when_no_dests_tree() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    let diagnostics = check_name_tree_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().is_empty());
}

/// A `/Names /Dests` entry whose value is an array with no resolvable page
/// reference (first element is a name, not an indirect reference). This is a
/// malformed destination, not a "missing target page": `Dest::page()`
/// returns `None`, so no diagnostic is produced for it.
fn name_tree_dests_no_page_ref_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(odd) [/NotAPageRef /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn check_name_tree_dests_skips_entries_without_resolvable_page_ref() {
    let mut pdf = Pdf::open(Cursor::new(name_tree_dests_no_page_ref_pdf())).unwrap();
    let entries = pdf.outline().name_tree_dests().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].1.as_ref().unwrap().page().is_none());

    let diagnostics = check_name_tree_dests(&mut pdf).unwrap();
    assert!(diagnostics.entries().is_empty());
}
