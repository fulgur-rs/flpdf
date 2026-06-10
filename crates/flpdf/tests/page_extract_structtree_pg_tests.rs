//! End-to-end `--pages` struct-tree `/Pg` drop parity with qpdf.
//!
//! Drives the subset pipeline (`rebuild_page_tree` -> `remap_outline_and_dests`
//! -> `drop_struct_elem_dangling_pg` -> `prune_after_subset` -> inspection) and
//! asserts the qpdf 11.9.0 behaviour for the structural-reference *drop* family:
//! a structure element whose `/Pg` points at a removed page has the `/Pg` key
//! dropped (not nulled), and the page — once unreferenced — is garbage-collected
//! entirely. This is the opposite of the annotation/outline null-out family,
//! where the reference is kept verbatim and the page object becomes `null`.

use flpdf::{
    drop_struct_elem_dangling_pg, prune_after_subset, rebuild_page_tree, remap_outline_and_dests,
    Object, ObjectRef, Pdf, RemoveUnreferencedResources,
};
use std::collections::BTreeMap;
use std::io::Cursor;

/// 3-page document with a structure tree.
///
/// Pages: 3=p1, 4=p2, 5=p3. Catalog `/StructTreeRoot` 10 0 R, whose `/K` is the
/// document-level StructElem 20 0 R with two StructElem kids: 21 0 R
/// (`/Pg 4 0 R` = p2, its only reference besides the page tree) and 22 0 R
/// (`/Pg 3 0 R` = p1).
fn build_fixture() -> Vec<u8> {
    let mut objs: BTreeMap<u32, String> = BTreeMap::new();
    objs.insert(
        1,
        "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>".into(),
    );
    objs.insert(
        2,
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into(),
    );
    for n in 3..=5 {
        objs.insert(
            n,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
    }
    objs.insert(10, "<< /Type /StructTreeRoot /K 20 0 R >>".into());
    objs.insert(
        20,
        "<< /Type /StructElem /S /Document /P 10 0 R /K [21 0 R 22 0 R] >>".into(),
    );
    objs.insert(
        21,
        "<< /Type /StructElem /S /P /P 20 0 R /Pg 4 0 R /K 0 >>".into(),
    );
    objs.insert(
        22,
        "<< /Type /StructElem /S /P /P 20 0 R /Pg 3 0 R /K 1 >>".into(),
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
    let mut pdf = Pdf::open(Cursor::new(build_fixture())).expect("open fixture");
    let result = rebuild_page_tree(&mut pdf, pages).expect("rebuild");
    remap_outline_and_dests(&mut pdf, &result).expect("remap");
    drop_struct_elem_dangling_pg(&mut pdf, &result).expect("pg drop");
    prune_after_subset(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");
    pdf
}

#[test]
fn dangling_pg_dropped_and_page_gced() {
    // Keep pages 1 and 3 (obj 3, 5). Removed: p2 (obj 4), whose only
    // non-page-tree reference is StructElem 21's /Pg.
    let mut pdf = run_subset(&[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);

    // The StructElem pointing at the removed page loses its /Pg key entirely.
    let elem = pdf.resolve(ObjectRef::new(21, 0)).expect("elem 21");
    let elem = elem.as_dict().expect("elem 21 is a dict");
    assert!(
        elem.get("Pg").is_none(),
        "StructElem 21 /Pg (removed page) must be dropped, got {:?}",
        elem.get("Pg")
    );

    // The /Pg drop leaves the removed page unreferenced, so the existing GC
    // sweeps it: absent from the output, not emitted as `null` (qpdf parity).
    let live = pdf.live_object_refs();
    assert!(
        !live.contains(&ObjectRef::new(4, 0)),
        "removed page 2 must be garbage-collected after /Pg drop"
    );

    // The StructElem pointing at a surviving page keeps its /Pg.
    let elem = pdf.resolve(ObjectRef::new(22, 0)).expect("elem 22");
    let elem = elem.as_dict().expect("elem 22 is a dict");
    assert!(
        matches!(elem.get("Pg"), Some(Object::Reference(r)) if r.number == 3),
        "StructElem 22 /Pg (surviving page 1) must be kept, got {:?}",
        elem.get("Pg")
    );
}
