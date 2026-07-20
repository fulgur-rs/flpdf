//! End-to-end integration coverage for outline and page-label preservation
//! across `write_pdf` round-trips and page operations (extract/rebuild/merge).
//!
//! This file complements the feature-scoped unit tests in
//! `outline_document_helper_tests.rs`, `name_tree_dests_tests.rs`, and
//! `page_label_document_helper_tests.rs` (each of which drills into a single
//! reader/writer API in isolation) with cross-cutting scenarios: a deep
//! outline actually surviving a `write_pdf` round trip (not just an in-memory
//! walk), both named-destination sources present in the same document,
//! `/PageLabels` reconstruction through [`flpdf::merge_documents`] (verified
//! against real `qpdf` 11.9.0 output — see the oracle notes below), and one
//! fixture that exercises deep outlines, both destination sources, all five
//! `/A` action subtypes, `/SE`, and `/PageLabels` together.

use flpdf::{
    drop_struct_elem_dangling_pg, merge_documents, rebuild_page_tree, remap_outline_and_dests,
    write_pdf, LabelRange, LabelStyle, MergeInput, Object, ObjectRef, OutlineAction, Pdf,
};
use std::collections::BTreeMap;
use std::io::Cursor;

/// Build a minimal cross-reffed PDF from `(objnum, body)` pairs (same
/// convention as `outline_document_helper_tests.rs`'s `build_pdf`).
///
/// Intentionally NOT `crates/flpdf/tests/common/mod.rs`'s `build_pdf`:
/// * that variant emits a `%PDF-1.5` header and takes `&[(u32, String)]`,
///   forcing every literal object body to be owned;
/// * this variant emits `%PDF-1.7` (the version the tests below target for
///   `/Names /Dests`, `/A` action subtypes, and `/StructTreeRoot`) and takes
///   `&[(u32, &str)]`, so a `&refs` slice built from `Vec<(u32, &str)>` and
///   inline literal-array calls compose without an owned-string round trip.
///
/// Consolidating with `common::build_pdf` would require changing every
/// call site here to `.into()` per element and accepting the 1.5 header
/// (or widening `common`'s signature and header, which is out of this
/// subtask's scope).
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

// ---------------------------------------------------------------------------
// 1. Deep outline round-trip through write_pdf
// ---------------------------------------------------------------------------

/// A linear chain of `n` nested outline items (each is the sole child of the
/// previous), each carrying a `/Dest` to the single page. Object numbers:
/// catalog 1, pages 2, page 3, outlines root 4, items 5..5+n.
fn deep_outline_with_dests_pdf(n: u32) -> Vec<u8> {
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
        (
            4,
            "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_string(),
        ),
    ];
    for i in 0..n {
        let num = 5 + i;
        let parent = if i == 0 { 4 } else { num - 1 };
        let mut body = format!("<< /Title (L{i}) /Parent {parent} 0 R /Dest [3 0 R /Fit]");
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

/// A 150-level-deep outline chain survives a full `write_pdf` round trip: the
/// reopened document's depth, per-level titles, and per-level `/Dest`
/// resolution are all preserved — not just the in-memory walk that
/// `deep_outline_walks_1000_levels_with_default_depth` (in
/// `outline_document_helper_tests.rs`) already covers without ever calling
/// [`write_pdf`].
#[test]
fn deep_outline_round_trip_through_write_pdf() {
    let n = 150u32;
    let mut pdf = Pdf::open(Cursor::new(deep_outline_with_dests_pdf(n))).unwrap();

    let before = pdf.outline().iter().unwrap().count();
    assert_eq!(before, n as usize, "sanity: fixture has n levels");

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let roots = reopened.outline().get_root().unwrap();
    assert_eq!(roots.len(), 1, "single top-level root after round trip");

    // Walk the single-child chain end to end, checking every level's title,
    // depth, and dest resolution survived the round trip intact.
    let mut depth = 0usize;
    let mut node = &roots[0];
    loop {
        assert_eq!(node.depth, depth);
        assert_eq!(node.title, format!("L{depth}"));
        let dest = node
            .dest
            .as_ref()
            .unwrap_or_else(|| panic!("level {depth} must keep its /Dest after the round trip"));
        assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
        match node.children.first() {
            Some(child) => {
                node = child;
                depth += 1;
            }
            None => break,
        }
    }
    assert_eq!(depth, (n - 1) as usize, "full depth must survive intact");

    // `iter()` (the flattened preorder view) must also see every level after
    // the round trip, not just the recursive `get_root()` tree.
    let after_count = reopened.outline().iter().unwrap().count();
    assert_eq!(after_count, n as usize, "iter() must see every level too");
}

// ---------------------------------------------------------------------------
// 2. Named destinations: both /Dests (legacy) and /Names /Dests (modern)
//    present together, round-tripping independently.
// ---------------------------------------------------------------------------

/// Catalog carrying BOTH a legacy `/Dests` dictionary and a modern
/// `/Names /Dests` name tree, each with its own distinct keys (no overlap),
/// so a bug that conflated the two trees on write would be visible.
fn combined_named_dests_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Dests 8 0 R /Names 9 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /legacy_only [3 0 R /Fit] >>"),
            (9, "<< /Dests 10 0 R >>"),
            (10, "<< /Names [(modern_only) [4 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn combined_legacy_and_modern_named_dests_round_trip_through_write_pdf() {
    let mut pdf = Pdf::open(Cursor::new(combined_named_dests_pdf())).unwrap();

    let legacy_before = pdf.outline().legacy_dests().unwrap();
    let modern_before = pdf.outline().name_tree_dests().unwrap();
    assert_eq!(legacy_before.len(), 1, "sanity: one legacy entry");
    assert_eq!(modern_before.len(), 1, "sanity: one modern entry");

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let legacy_after = reopened.outline().legacy_dests().unwrap();
    let modern_after = reopened.outline().name_tree_dests().unwrap();

    assert_eq!(
        legacy_before, legacy_after,
        "legacy /Dests must round-trip unchanged even with a modern tree present"
    );
    assert_eq!(
        modern_before, modern_after,
        "modern /Names /Dests must round-trip unchanged even with a legacy dict present"
    );
    assert_eq!(legacy_after[0].0, b"legacy_only");
    assert_eq!(modern_after[0].0, b"modern_only");
    assert_eq!(
        legacy_after[0].1.as_ref().unwrap().page(),
        Some(ObjectRef::new(3, 0))
    );
    assert_eq!(
        modern_after[0].1.as_ref().unwrap().page(),
        Some(ObjectRef::new(4, 0))
    );
}

// ---------------------------------------------------------------------------
// 3. /A action /Next chain round-trip through write_pdf
// ---------------------------------------------------------------------------

/// Outline item whose `/A` (obj 10) chains two further actions via `/Next`
/// (obj 11, obj 12): `action_chain` already covers the in-memory walk
/// (`outline_document_helper_tests.rs`); this proves the whole chain survives
/// serialization and reopening too.
fn action_chain_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Chained) /Parent 4 0 R /A 10 0 R >>"),
            (10, "<< /S /Named /N /FirstPage /Next 11 0 R >>"),
            (11, "<< /S /Named /N /NextPage /Next 12 0 R >>"),
            (12, "<< /S /Named /N /LastPage >>"),
        ],
        1,
    )
}

#[test]
fn action_chain_with_next_round_trips_through_write_pdf() {
    let mut pdf = Pdf::open(Cursor::new(action_chain_pdf())).unwrap();
    let before = pdf.outline().action_chain(ObjectRef::new(5, 0)).unwrap();
    assert_eq!(before.len(), 3, "sanity: 3-action /Next chain");

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();
    let after = reopened
        .outline()
        .action_chain(ObjectRef::new(5, 0))
        .unwrap();
    assert_eq!(
        before, after,
        "the entire /Next action chain must survive a write_pdf round trip"
    );
}

// ---------------------------------------------------------------------------
// 4. Combined fixture: deep-ish outline + both dest sources + all five /A
//    action subtypes + /SE + /PageLabels, all through one write_pdf round trip.
// ---------------------------------------------------------------------------

/// A single fixture exercising every area this subtask covers at once:
///  - 20-level nested outline chain (deep, but small enough to keep the test
///    fast) rooted at item 100.
///  - Five SIBLING items at the top level (obj 200..204), one per `/A`
///    action subtype (GoTo/GoToR/URI/Launch/Named).
///  - One sibling (obj 205) carrying `/SE` into a `/StructTreeRoot`.
///  - Catalog-level legacy `/Dests` AND modern `/Names /Dests`.
///  - Catalog-level `/PageLabels` (roman then decimal).
fn combined_e2e_pdf() -> Vec<u8> {
    let mut objs: Vec<(u32, String)> = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Dests 50 0 R /Names 51 0 R \
             /StructTreeRoot 60 0 R \
             /PageLabels << /Nums [0 << /S /r >> 2 << /S /D /St 1 >>] >> >>"
                .to_string(),
        ),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 30 0 R 31 0 R 32 0 R] /Count 4 >>".to_string(),
        ),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (
            30,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (
            31,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (
            32,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        // Outline root: /First is the deep chain's root (100); /Last is item 205.
        (
            4,
            "<< /Type /Outlines /First 100 0 R /Last 205 0 R /Count 26 >>".to_string(),
        ),
    ];

    // 20-level nested chain, obj 100..119, /Next chains to 200 at the tail.
    let depth_n = 20u32;
    for i in 0..depth_n {
        let num = 100 + i;
        let parent = if i == 0 { 4 } else { num - 1 };
        let mut body = format!("<< /Title (Deep{i}) /Parent {parent} 0 R");
        if i == 0 {
            body.push_str(" /Next 200 0 R");
        }
        if i + 1 < depth_n {
            let child = num + 1;
            body.push_str(&format!(" /First {child} 0 R /Last {child} 0 R"));
        }
        body.push_str(" >>");
        objs.push((num, body));
    }

    // Five action-subtype siblings (200..204), then the /SE sibling (205).
    objs.push((
        200,
        "<< /Title (GoTo) /Parent 4 0 R /Prev 100 0 R /Next 201 0 R \
         /A << /S /GoTo /D [3 0 R /Fit] >> >>"
            .to_string(),
    ));
    objs.push((
        201,
        "<< /Title (GoToR) /Parent 4 0 R /Prev 200 0 R /Next 202 0 R \
         /A << /S /GoToR /F (other.pdf) /D [0 /Fit] >> >>"
            .to_string(),
    ));
    objs.push((
        202,
        "<< /Title (URI) /Parent 4 0 R /Prev 201 0 R /Next 203 0 R \
         /A << /S /URI /URI (https://example.com/combined) >> >>"
            .to_string(),
    ));
    objs.push((
        203,
        "<< /Title (Launch) /Parent 4 0 R /Prev 202 0 R /Next 204 0 R \
         /A << /S /Launch /F (app.exe) >> >>"
            .to_string(),
    ));
    objs.push((
        204,
        "<< /Title (Named) /Parent 4 0 R /Prev 203 0 R /Next 205 0 R \
         /A << /S /Named /N /NextPage >> >>"
            .to_string(),
    ));
    objs.push((
        205,
        "<< /Title (WithSE) /Parent 4 0 R /Prev 204 0 R /SE 61 0 R \
         /Dest [30 0 R /Fit] >>"
            .to_string(),
    ));

    // Named destinations: legacy /Dests (50) and modern /Names /Dests (51->52).
    objs.push((50, "<< /combined_legacy [31 0 R /Fit] >>".to_string()));
    objs.push((51, "<< /Dests 52 0 R >>".to_string()));
    objs.push((
        52,
        "<< /Names [(combined_modern) [32 0 R /Fit]] >>".to_string(),
    ));

    // Structure tree: root (60) with one struct elem (61) targeting page 3.
    objs.push((60, "<< /Type /StructTreeRoot /K [61 0 R] >>".to_string()));
    objs.push((
        61,
        "<< /Type /StructElem /S /P /P 60 0 R /Pg 3 0 R >>".to_string(),
    ));

    let refs: Vec<(u32, &str)> = objs.iter().map(|(n, s)| (*n, s.as_str())).collect();
    build_pdf(&refs, 1)
}

#[test]
fn combined_fixture_round_trips_every_area_through_write_pdf() {
    let mut pdf = Pdf::open(Cursor::new(combined_e2e_pdf())).unwrap();

    // Sanity on the freshly opened document before the round trip.
    let roots_before = pdf.outline().get_root().unwrap();
    assert_eq!(
        roots_before.len(),
        7,
        "7 top-level siblings: deep chain root + 5 action items + /SE item"
    );

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();
    let mut reopened = Pdf::open(Cursor::new(out)).unwrap();

    // -- Deep chain (20 levels) preserved --
    let roots = reopened.outline().get_root().unwrap();
    assert_eq!(roots.len(), 7);
    let mut depth = 0usize;
    let mut node = &roots[0];
    loop {
        assert_eq!(node.title, format!("Deep{depth}"));
        match node.children.first() {
            Some(child) => {
                node = child;
                depth += 1;
            }
            None => break,
        }
    }
    assert_eq!(depth, 19, "20-level deep chain must survive intact");

    // -- All five action subtypes preserved, in sibling order --
    let actions: Vec<Option<OutlineAction>> =
        roots[1..=5].iter().map(|n| n.action.clone()).collect();
    match actions[0].as_ref().unwrap() {
        OutlineAction::GoTo { d } => assert_eq!(
            d.as_array().unwrap()[0],
            Object::Reference(ObjectRef::new(3, 0))
        ),
        other => panic!("expected GoTo, got {other:?}"),
    }
    match actions[1].as_ref().unwrap() {
        OutlineAction::GoToR { f, .. } => {
            assert_eq!(f, &Object::String(b"other.pdf".to_vec()))
        }
        other => panic!("expected GoToR, got {other:?}"),
    }
    match actions[2].as_ref().unwrap() {
        OutlineAction::Uri { uri } => assert_eq!(uri, b"https://example.com/combined"),
        other => panic!("expected Uri, got {other:?}"),
    }
    match actions[3].as_ref().unwrap() {
        OutlineAction::Launch { f } => assert_eq!(f, &Object::String(b"app.exe".to_vec())),
        other => panic!("expected Launch, got {other:?}"),
    }
    match actions[4].as_ref().unwrap() {
        OutlineAction::Named { n } => assert_eq!(n, b"NextPage"),
        other => panic!("expected Named, got {other:?}"),
    }

    // -- /SE link preserved and still resolves to a live /StructElem --
    let se_item = &roots[6];
    assert_eq!(se_item.title, "WithSE");
    let se_ref = se_item.se.expect("/SE must survive the round trip");
    match reopened.resolve(se_ref).unwrap() {
        Object::Dictionary(dict) => {
            assert_eq!(
                dict.get("Type"),
                Some(&Object::Name(b"StructElem".to_vec()))
            );
        }
        other => panic!("/SE target must still be a /StructElem dict, got {other:?}"),
    }

    // -- Both named-destination sources preserved independently --
    let legacy = reopened.outline().legacy_dests().unwrap();
    let modern = reopened.outline().name_tree_dests().unwrap();
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].0, b"combined_legacy");
    assert_eq!(
        legacy[0].1.as_ref().unwrap().page(),
        Some(ObjectRef::new(31, 0))
    );
    assert_eq!(modern.len(), 1);
    assert_eq!(modern[0].0, b"combined_modern");
    assert_eq!(
        modern[0].1.as_ref().unwrap().page(),
        Some(ObjectRef::new(32, 0))
    );

    // -- /PageLabels preserved --
    let mut h = reopened.page_labels();
    assert_eq!(h.label_string_for_page(0).unwrap(), "i");
    assert_eq!(h.label_string_for_page(1).unwrap(), "ii");
    assert_eq!(h.label_string_for_page(2).unwrap(), "1");
    assert_eq!(h.label_string_for_page(3).unwrap(), "2");
}

// ---------------------------------------------------------------------------
// 5. /SE + /StructTreeRoot interaction with page-tree rebuild: current,
//    verified behaviour (drop_struct_elem_dangling_pg drops only /Pg — the
//    structure element itself, and any outline /SE pointing at it, survive).
// ---------------------------------------------------------------------------

/// Two pages; a `/StructTreeRoot` struct elem (20) targets the SECOND page
/// (31, to be removed); an outline item's `/SE` (5) points at that same
/// struct elem.
fn se_survives_pg_drop_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /StructTreeRoot 10 0 R >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 31 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                31,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (A) /Parent 4 0 R /SE 20 0 R >>"),
            (10, "<< /Type /StructTreeRoot /K [20 0 R] >>"),
            (20, "<< /Type /StructElem /S /P /P 10 0 R /Pg 31 0 R >>"),
        ],
        1,
    )
}

/// Mirrors the CLI `--pages` pipeline
/// (`rebuild_page_tree` → `remap_outline_and_dests` →
/// `drop_struct_elem_dangling_pg`, see `flpdf-cli`'s `main.rs`) over a
/// selection that DROPS the struct element's target page. Per
/// `struct_tree_pg`'s documented qpdf parity, the struct element's `/Pg` is
/// dropped but the struct element itself is left in the tree — it is not
/// garbage-collected (only page objects are, by the subsequent subset
/// sweep). So the outline item's `/SE` reference is never left dangling by
/// this pipeline alone: it still resolves to a live `/StructElem`, just one
/// that no longer names a page. `prune_outline_se` (14.6) exists for callers
/// that DO fully remove struct elements — which this built-in pipeline does
/// not do — and is not invoked automatically by it.
#[test]
fn struct_elem_survives_page_rebuild_pg_drop_and_outline_se_still_resolves() {
    let mut pdf = Pdf::open(Cursor::new(se_survives_pg_drop_pdf())).unwrap();

    // Select only page 3 (drop page 31, the struct elem's /Pg target).
    let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
    remap_outline_and_dests(&mut pdf, &result).unwrap();
    drop_struct_elem_dangling_pg(&mut pdf, &result).unwrap();

    let roots = pdf.outline().get_root().unwrap();
    let se_ref = roots[0].se.expect(
        "the outline /SE reference itself is untouched by this pipeline (no automatic prune)",
    );
    assert_eq!(se_ref, ObjectRef::new(20, 0));

    match pdf.resolve(se_ref).unwrap() {
        Object::Dictionary(dict) => {
            assert_eq!(
                dict.get("Type"),
                Some(&Object::Name(b"StructElem".to_vec())),
                "the struct element itself must survive (only /Pg is dropped)"
            );
            assert!(
                dict.get("Pg").is_none(),
                "the dangling /Pg to the removed page must be dropped"
            );
        }
        other => panic!("/SE target must still resolve to a dictionary, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 6. /PageLabels reconstruction through merge_documents — verified against
//    real qpdf 11.9.0 (`qpdf --empty --pages a.pdf b.pdf -- merged.pdf`,
//    `qpdf --json=2 --json-key=pagelabels merged.pdf`); see page_merge.rs's
//    own doc comment on the accumulating-across-every-input qpdf
//    `handlePageSpecs` parity this exercises end to end via the public API
//    (page_merge_tests.rs, despite its size, never covers /PageLabels).
// ---------------------------------------------------------------------------

/// Two-page document with a roman-lowercase `/PageLabels` range starting at
/// page 0.
fn roman_two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0 << /S /r >>] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// Two-page document with a decimal `/PageLabels` range restarting at 5.
fn decimal_from_five_two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0 << /S /D /St 5 >>] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// Two-page document with NO `/PageLabels` at all.
fn unlabeled_two_page_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// Three inputs — roman(2p), unlabeled(2p), decimal-from-5(2p) — merged in
/// order. qpdf 11.9.0 observed output for the identical fixture shape
/// (`qpdf --empty --pages roman.pdf unlabeled.pdf decimal5.pdf --
/// merged.pdf`) is exactly these three ranges: the unlabeled input's
/// implicit numbering CONTINUES the running merged page count (index 2 gets
/// `/St 3`, not a per-source restart at 1) rather than restarting, and no
/// input's own labels are dropped.
#[test]
fn merge_documents_page_labels_three_inputs_matches_qpdf_oracle() {
    let mut roman = Pdf::open(Cursor::new(roman_two_page_pdf())).unwrap();
    let mut unlabeled = Pdf::open(Cursor::new(unlabeled_two_page_pdf())).unwrap();
    let mut decimal5 = Pdf::open(Cursor::new(decimal_from_five_two_page_pdf())).unwrap();

    let mut inputs = [
        MergeInput {
            source: &mut roman,
            pages: vec![0, 1],
        },
        MergeInput {
            source: &mut unlabeled,
            pages: vec![0, 1],
        },
        MergeInput {
            source: &mut decimal5,
            pages: vec![0, 1],
        },
    ];
    let mut merged = merge_documents(&mut inputs).unwrap();

    let ranges = merged.page_labels().ranges().unwrap();
    assert_eq!(
        ranges,
        vec![
            (
                0,
                LabelRange {
                    style: LabelStyle::RomanLower,
                    prefix: String::new(),
                    start: 1,
                }
            ),
            (
                2,
                LabelRange {
                    style: LabelStyle::None,
                    prefix: String::new(),
                    start: 3,
                }
            ),
            (
                4,
                LabelRange {
                    style: LabelStyle::Decimal,
                    prefix: String::new(),
                    start: 5,
                }
            ),
        ],
        "must match qpdf 11.9.0's observed /PageLabels reconstruction for this exact input shape"
    );

    // Sanity on the rendered strings too (what a viewer would actually show).
    // Pages 2-3 (the unlabeled source's own pages) fall under the fabricated
    // `LabelStyle::None` range: per ISO 32000-2 §12.4.2 "no /S" means no
    // numeric portion at all, so they render as an EMPTY label string, not
    // their /St value — the /St is bookkeeping for the running default
    // sequence, never itself displayed.
    let mut h = merged.page_labels();
    assert_eq!(h.label_string_for_page(0).unwrap(), "i");
    assert_eq!(h.label_string_for_page(1).unwrap(), "ii");
    assert_eq!(h.label_string_for_page(2).unwrap(), "");
    assert_eq!(h.label_string_for_page(3).unwrap(), "");
    assert_eq!(h.label_string_for_page(4).unwrap(), "5");
    assert_eq!(h.label_string_for_page(5).unwrap(), "6");
}

/// When NO input carries real `/PageLabels`, the merged output must gain
/// none at all — verified against qpdf 11.9.0 (`qpdf --empty --pages
/// unlabeled.pdf unlabeled.pdf -- merged.pdf` emits `"pagelabels": []`).
#[test]
fn merge_documents_omits_page_labels_when_no_input_has_them() {
    let mut a = Pdf::open(Cursor::new(unlabeled_two_page_pdf())).unwrap();
    let mut b = Pdf::open(Cursor::new(unlabeled_two_page_pdf())).unwrap();
    let mut inputs = [
        MergeInput {
            source: &mut a,
            pages: vec![0, 1],
        },
        MergeInput {
            source: &mut b,
            pages: vec![0, 1],
        },
    ];
    let mut merged = merge_documents(&mut inputs).unwrap();
    assert!(
        !merged.page_labels().has_page_labels().unwrap(),
        "a merge of two label-less inputs must not gain /PageLabels"
    );
}

// ---------------------------------------------------------------------------
// 7. /PageLabels decode + reformat across extract (split) then merge:
//    each page is extracted into its own single-page document (its label
//    reconstructed per extract_pages's own qpdf parity), then those
//    single-page documents are merged back in original order. The rendered
//    label STRING for every page must match the source document's.
// ---------------------------------------------------------------------------

/// Four-page document: roman lowercase for pages 0-1, decimal (restart at 1)
/// for pages 2-3 — same shape as `page_extract_tests.rs`'s
/// `four_page_pdf_with_labels`, defined locally so this file stays
/// self-contained.
fn four_page_pdf_with_labels() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels \
                 << /Nums [0 << /S /r >> 2 << /S /D /St 1 >>] >> >>",
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

#[test]
fn page_labels_round_trip_through_extract_then_remerge() {
    let src_bytes = four_page_pdf_with_labels();
    let expected: Vec<&str> = vec!["i", "ii", "1", "2"];

    // "Split": extract each page into its own single-page document, exactly
    // as flpdf's split_pages does per chunk (one entry per selected page,
    // reconstructed against that page's own source index).
    let mut singles: Vec<Pdf<Cursor<Vec<u8>>>> = Vec::new();
    for (idx, want) in expected.iter().enumerate() {
        let mut src = Pdf::open(Cursor::new(src_bytes.clone())).unwrap();
        let mut extracted = flpdf::extract_pages(&mut src, &[idx]).unwrap();
        assert_eq!(
            extracted.page_labels().label_string_for_page(0).unwrap(),
            *want,
            "each single-page extract must carry its own correct label"
        );
        singles.push(extracted);
    }

    // "Remerge": feed the four single-page documents back through
    // merge_documents, in original order.
    let mut inputs: Vec<MergeInput<'_, Cursor<Vec<u8>>>> = singles
        .iter_mut()
        .map(|doc| MergeInput {
            source: doc,
            pages: vec![0],
        })
        .collect();
    let mut merged = merge_documents(&mut inputs).unwrap();

    let mut h = merged.page_labels();
    for (idx, want) in expected.iter().enumerate() {
        assert_eq!(
            &h.label_string_for_page(idx as i64).unwrap(),
            want,
            "page {idx}'s label must survive split-then-remerge"
        );
    }
}
