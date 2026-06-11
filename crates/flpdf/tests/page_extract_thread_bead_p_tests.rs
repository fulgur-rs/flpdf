//! End-to-end `--pages` article-thread bead `/P` drop parity with qpdf.
//!
//! Drives the subset pipeline in the production order used by
//! `run_page_extraction` (`rebuild_page_tree` -> `remap_outline_and_dests` ->
//! `drop_struct_elem_dangling_pg` -> `drop_thread_bead_dangling_p` ->
//! `prune_after_subset` -> `prune_acroform_after_subset` -> inspection) and
//! asserts the qpdf 11.9.0 behaviour for the structural-reference *drop* family:
//! a bead whose `/P` points at a removed page has the `/P` key dropped (not
//! nulled), the bead and its ring links are kept, and the page — once
//! unreferenced — is garbage-collected entirely. This is the opposite of the
//! annotation/outline null-out family, where the reference is kept verbatim and
//! the page object becomes `null`.

use flpdf::{
    drop_struct_elem_dangling_pg, drop_thread_bead_dangling_p, extract_pages, pages,
    prune_acroform_after_subset, prune_after_subset, rebuild_page_tree, remap_outline_and_dests,
    Object, ObjectRef, Pdf, RemoveUnreferencedResources,
};
use std::collections::BTreeMap;
use std::io::Cursor;

/// 3-page document with one article thread.
///
/// Pages: 3=p1, 4=p2, 5=p3, each carrying a `/B` bead array. Catalog
/// `/Threads [10 0 R]`; thread 10's `/F` is bead 11. The bead ring is
/// 11→12→13 (via `/N`): bead 11 on p1, bead 12 on p2, bead 13 on p3. Page 2's
/// only non-page-tree reference is bead 12's `/P`.
fn build_fixture() -> Vec<u8> {
    build_fixture_inner(true)
}

/// Like [`build_fixture`] but with no catalog `/Threads`, so the bead ring is
/// reachable only through the surviving pages' `/B` arrays.
fn build_fixture_b_only() -> Vec<u8> {
    build_fixture_inner(false)
}

fn build_fixture_inner(with_threads: bool) -> Vec<u8> {
    let mut objs: BTreeMap<u32, String> = BTreeMap::new();
    let catalog = if with_threads {
        "<< /Type /Catalog /Pages 2 0 R /Threads [10 0 R] >>"
    } else {
        "<< /Type /Catalog /Pages 2 0 R >>"
    };
    objs.insert(1, catalog.into());
    objs.insert(
        2,
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into(),
    );
    objs.insert(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [11 0 R] >>".into(),
    );
    objs.insert(
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [12 0 R] >>".into(),
    );
    objs.insert(
        5,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [13 0 R] >>".into(),
    );
    objs.insert(10, "<< /Type /Thread /F 11 0 R >>".into());
    objs.insert(
        11,
        "<< /Type /Bead /T 10 0 R /N 12 0 R /V 13 0 R /P 3 0 R /R [0 0 100 100] >>".into(),
    );
    objs.insert(
        12,
        "<< /Type /Bead /T 10 0 R /N 13 0 R /V 11 0 R /P 4 0 R /R [0 0 100 100] >>".into(),
    );
    objs.insert(
        13,
        "<< /Type /Bead /T 10 0 R /N 11 0 R /V 12 0 R /P 5 0 R /R [0 0 100 100] >>".into(),
    );

    let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in &objs {
        offs.insert(*n, raw.len());
        raw.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let max_num = *objs.keys().max().unwrap();
    let xref_pos = raw.len();
    raw.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        if let Some(&off) = offs.get(&i) {
            raw.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        } else {
            raw.extend_from_slice(b"0000000000 65535 f \n");
        }
    }
    raw.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );
    raw
}

fn run_subset(pages: &[ObjectRef]) -> Pdf<Cursor<Vec<u8>>> {
    run_subset_bytes(build_fixture(), pages)
}

fn run_subset_bytes(bytes: Vec<u8>, pages: &[ObjectRef]) -> Pdf<Cursor<Vec<u8>>> {
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open fixture");
    let result = rebuild_page_tree(&mut pdf, pages).expect("rebuild");
    remap_outline_and_dests(&mut pdf, &result).expect("remap");
    drop_struct_elem_dangling_pg(&mut pdf, &result).expect("pg drop");
    drop_thread_bead_dangling_p(&mut pdf, &result).expect("bead /P drop");
    prune_after_subset(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");
    prune_acroform_after_subset(&mut pdf, &result).expect("acroform prune");
    pdf
}

#[test]
fn dangling_bead_p_dropped_and_page_gced() {
    // Keep pages 1 and 3 (obj 3, 5). Removed: p2 (obj 4), whose only
    // non-page-tree reference is bead 12's /P.
    let mut pdf = run_subset(&[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);

    // The bead pointing at the removed page loses its /P key entirely.
    let bead = pdf.resolve(ObjectRef::new(12, 0)).expect("bead 12");
    let bead = bead.as_dict().expect("bead 12 is a dict");
    assert!(
        bead.get("P").is_none(),
        "bead 12 /P (removed page) must be dropped, got {:?}",
        bead.get("P")
    );
    // The bead itself and its ring links survive (qpdf keeps the bead).
    assert!(
        matches!(bead.get("N"), Some(Object::Reference(r)) if r.number == 13),
        "bead 12 /N must be kept, got {:?}",
        bead.get("N")
    );

    // The /P drop leaves the removed page unreferenced, so the existing GC
    // sweeps it: absent from the output, not emitted as `null` (qpdf parity).
    let live = pdf.live_object_refs();
    assert!(
        !live.contains(&ObjectRef::new(4, 0)),
        "removed page 2 must be garbage-collected after bead /P drop"
    );
    // The bead, by contrast, is still live (kept in the thread ring).
    assert!(
        live.contains(&ObjectRef::new(12, 0)),
        "bead 12 must stay live in the thread ring after its /P drop"
    );

    // Beads on surviving pages keep their /P.
    let bead = pdf.resolve(ObjectRef::new(11, 0)).expect("bead 11");
    let bead = bead.as_dict().expect("bead 11 is a dict");
    assert!(
        matches!(bead.get("P"), Some(Object::Reference(r)) if r.number == 3),
        "bead 11 /P (surviving page 1) must be kept, got {:?}",
        bead.get("P")
    );
}

#[test]
fn duplicate_selection_shares_bead_and_p_points_at_first_occurrence() {
    // extract_pages(&[0, 0]): page 0 (obj 3) carries /B [11 0 R] and bead 11's
    // /P points back at that page. The duplicate is a shallow clone, so both
    // copies share the SAME bead object, and the single bead's /P must be the
    // FIRST occurrence's ref — not the duplicate's, not dropped (qpdf 11.9.0
    // duplicate-page bead /P parity).
    let mut src = Pdf::open(Cursor::new(build_fixture())).expect("open fixture");
    let mut out = extract_pages(&mut src, &[0, 0]).expect("extract duplicate selection");

    let page_refs = pages::page_refs(&mut out).expect("output page refs");
    assert_eq!(page_refs.len(), 2, "duplicate selection yields two pages");
    assert_ne!(
        page_refs[0], page_refs[1],
        "duplicate kids must be distinct page objects"
    );

    // Both copies' /B reference the same bead object (shallow clone shares /B).
    let bead_ref_of = |doc: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef| -> ObjectRef {
        let page = doc.resolve(r).expect("page").into_dict().expect("dict");
        match page.get("B") {
            Some(Object::Array(a)) => a[0].as_ref_id().expect("/B[0] is an indirect ref"),
            other => panic!("expected /B array, got {other:?}"),
        }
    };
    let bead0 = bead_ref_of(&mut out, page_refs[0]);
    let bead1 = bead_ref_of(&mut out, page_refs[1]);
    assert_eq!(
        bead0, bead1,
        "both duplicate pages must share the SAME bead object"
    );

    // The single shared bead's /P targets the FIRST occurrence.
    let bead = out.resolve(bead0).expect("bead").into_dict().expect("dict");
    assert_eq!(
        bead.get("P"),
        Some(&Object::Reference(page_refs[0])),
        "shared bead /P must point at the first occurrence's page ref"
    );
}

#[test]
fn dangling_bead_p_dropped_and_page_gced_via_b_array_without_threads() {
    // Same parity, but the catalog has no /Threads: the bead ring is reachable
    // only through the surviving pages' /B arrays. Without dropping the
    // removed-page bead's /P here, the removed page would stay reachable via the
    // kept page's /B ring and the prune could not collect it (qpdf 11.9.0
    // garbage-collects it).
    let mut pdf = run_subset_bytes(
        build_fixture_b_only(),
        &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)],
    );

    let bead = pdf.resolve(ObjectRef::new(12, 0)).expect("bead 12");
    let bead = bead.as_dict().expect("bead 12 is a dict");
    assert!(
        bead.get("P").is_none(),
        "bead 12 /P (removed page) must be dropped via /B seeding, got {:?}",
        bead.get("P")
    );

    let live = pdf.live_object_refs();
    assert!(
        !live.contains(&ObjectRef::new(4, 0)),
        "removed page 2 must be garbage-collected after the /B-seeded bead /P drop"
    );
    assert!(
        live.contains(&ObjectRef::new(12, 0)),
        "bead 12 must stay live in the ring after its /P drop"
    );
}
