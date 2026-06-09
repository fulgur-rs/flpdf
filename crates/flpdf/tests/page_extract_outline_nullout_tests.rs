//! End-to-end `--pages` outline / named-destination null-out parity with qpdf.
//!
//! Drives the real subset pipeline (`rebuild_page_tree` -> `remap_outline_and_dests`
//! -> `prune_after_subset` -> `write_pdf`) and asserts the qpdf 11.9.0 behaviour:
//! no nav entry is dropped, surviving-page dests are remapped, a removed page
//! still referenced by a kept dest is emitted as `null` (and stays live), and a
//! removed page referenced by nothing is garbage-collected (absent). This is a
//! structural parity check, not a byte-compare against qpdf (qpdf renumbers).

use flpdf::{
    prune_after_subset, rebuild_page_tree, remap_outline_and_dests, write_pdf, Object, ObjectRef,
    Pdf, RemoveUnreferencedResources,
};
use std::collections::BTreeMap;
use std::io::Cursor;

/// 5-page document with an outline and a `/Names /Dests` name tree.
///
/// Pages: 3=p1, 4=p2, 5=p3, 6=p4, 7=p5. `p5` (obj 7) is referenced by no
/// destination or outline item. Named dests: dp1->p1, dp2->p2, dp3->p3,
/// dp4->p4. Outline: 20->p1 /Dest, 21->p2 /Dest, chain 20->21.
fn build_fixture() -> Vec<u8> {
    let mut objs: BTreeMap<u32, String> = BTreeMap::new();
    objs.insert(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Names 11 0 R >>".into(),
    );
    objs.insert(
        2,
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 5 >>".into(),
    );
    for n in 3..=7 {
        objs.insert(
            n,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
    }
    objs.insert(
        10,
        "<< /Type /Outlines /First 20 0 R /Last 21 0 R /Count 2 >>".into(),
    );
    objs.insert(11, "<< /Dests 30 0 R >>".into());
    objs.insert(
        30,
        "<< /Limits [(dp1) (dp4)] /Names [(dp1) [3 0 R /Fit] (dp2) [4 0 R /Fit] \
         (dp3) [5 0 R /Fit] (dp4) [6 0 R /Fit]] >>"
            .into(),
    );
    objs.insert(
        20,
        "<< /Title (P1) /Parent 10 0 R /Next 21 0 R /Dest [3 0 R /Fit] >>".into(),
    );
    objs.insert(
        21,
        "<< /Title (P2) /Parent 10 0 R /Prev 20 0 R /Dest [4 0 R /Fit] >>".into(),
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
    prune_after_subset(&mut pdf, RemoveUnreferencedResources::Yes).expect("prune");
    pdf
}

#[test]
fn referenced_removed_pages_nulled_unreferenced_absent() {
    // Keep pages 1 and 3 (obj 3, 5). Removed: p2(4), p4(6) referenced by dests;
    // p5(7) referenced by nothing.
    let mut pdf = run_subset(&[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);

    // Referenced removed pages are nulled in place AND stay live (reachable via
    // their dests), matching qpdf's `N 0 obj null`.
    assert!(
        matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
        "removed page 2 (referenced by dp2) must be null"
    );
    assert!(
        matches!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null),
        "removed page 4 (referenced by dp4) must be null"
    );
    let live = pdf.live_object_refs();
    assert!(
        live.contains(&ObjectRef::new(4, 0)),
        "nulled-but-referenced page 2 stays live"
    );
    assert!(
        live.contains(&ObjectRef::new(6, 0)),
        "nulled-but-referenced page 4 stays live"
    );

    // The page referenced by nothing is garbage-collected entirely (absent).
    assert!(
        !live.contains(&ObjectRef::new(7, 0)),
        "removed page 5 (referenced by nothing) must be swept, not nulled"
    );
}

#[test]
fn outline_and_names_retained_all_entries_kept() {
    let mut pdf = run_subset(&[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);

    // Catalog keeps /Outlines and /Names.
    let cat = pdf.resolve(pdf.root_ref().unwrap()).unwrap();
    let cat = cat.as_dict().unwrap();
    assert!(cat.get("Outlines").is_some(), "/Outlines retained");
    assert!(cat.get("Names").is_some(), "/Names retained");

    // All four named dests still present; /Limits unchanged.
    let leaf = pdf.resolve(ObjectRef::new(30, 0)).unwrap();
    let leaf = leaf.as_dict().unwrap();
    let names = leaf.get("Names").and_then(Object::as_array).unwrap();
    let keys: Vec<&[u8]> = names
        .iter()
        .step_by(2)
        .filter_map(|o| match o {
            Object::String(b) | Object::Name(b) => Some(b.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(keys, vec![b"dp1".as_slice(), b"dp2", b"dp3", b"dp4"]);
    assert!(
        leaf.get("Limits").is_some(),
        "/Limits not recomputed/removed"
    );

    // Both outline items kept with their chain intact.
    let i20 = pdf.resolve(ObjectRef::new(20, 0)).unwrap();
    assert_eq!(
        i20.as_dict().unwrap().get_ref("Next"),
        Some(ObjectRef::new(21, 0)),
        "outline chain not stitched"
    );
}

#[test]
fn full_rewrite_roundtrip_reopens_and_keeps_nav() {
    let mut pdf = run_subset(&[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);

    // Write (full rewrite renumbers + emits the referenced nulls) and reopen.
    let mut out: Vec<u8> = Vec::new();
    write_pdf(&mut pdf, &mut out).expect("write");
    let mut re = Pdf::open(Cursor::new(out)).expect("reopen rewritten subset");

    // Catalog still carries the navigation structures after the round trip.
    let root = re.root_ref().expect("root");
    let cat = re.resolve(root).unwrap();
    let cat = cat.as_dict().expect("catalog dict").clone();
    assert!(
        cat.get("Outlines").is_some(),
        "/Outlines survives round trip"
    );
    let names_ref = cat.get_ref("Names").expect("/Names survives round trip");

    // The Dests leaf still holds four entries; at least one resolves to null
    // (a removed-but-referenced page emitted as `N 0 obj null`).
    let names_dict = re.resolve(names_ref).unwrap();
    let dests_ref = names_dict.as_dict().unwrap().get_ref("Dests").unwrap();
    let leaf = re.resolve(dests_ref).unwrap();
    let pairs = leaf
        .as_dict()
        .unwrap()
        .get("Names")
        .and_then(Object::as_array)
        .unwrap()
        .to_vec();
    assert_eq!(
        pairs.len(),
        8,
        "all four named dests survive the round trip"
    );
    let mut null_targets = 0;
    for dest in pairs.iter().skip(1).step_by(2) {
        if let Some(first) = dest.as_array().and_then(|a| a.first()) {
            if let Some(r) = first.as_ref_id() {
                if matches!(re.resolve(r).unwrap(), Object::Null) {
                    null_targets += 1;
                }
            }
        }
    }
    assert_eq!(
        null_targets, 2,
        "exactly the two removed-but-referenced page targets are null"
    );
}
