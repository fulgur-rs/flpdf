//! Article-thread bead `/P` reference drop after page extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree for a subset extraction, this module walks the article threads (catalog
//! `/Threads` → thread `/F` → bead ring, ISO 32000-2 §12.4.3) and updates each
//! bead's `/P` (the page the bead belongs to) to match qpdf's `--pages`
//! behaviour:
//!
//! - A bead whose `/P` points at a **surviving** page keeps the entry, remapped
//!   to the page's new [`ObjectRef`] when the rebuild changed it.
//! - A bead whose `/P` points at a **removed** page has the `/P` key
//!   **dropped**. The bead itself — and its `/N`/`/V` ring links, `/T`, and
//!   `/R` — is retained; the now-unreferenced page is garbage-collected by the
//!   subsequent subset sweep ([`crate::subset_prune`]) and is absent from the
//!   output.
//!
//! This is the structural-reference *drop* family, alongside the structure-tree
//! `/Pg` handling ([`crate::struct_tree_pg`]): the reference is removed rather
//! than replaced with `null` (the opposite of the outline / named-destination /
//! annotation null-out family in [`crate::outline_dest_remap`]).
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! For `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf` over a document whose page 2
//! is referenced only by an article bead's `/P`, qpdf drops that bead's `/P`
//! and the removed page is absent from the output (not emitted as `null`). The
//! bead stays in the thread ring with its `/N`/`/V`/`/T`/`/R` intact. Even when
//! every bead of a thread targets a removed page, qpdf keeps the thread and all
//! its beads (each with `/P` dropped) — it never removes the thread itself.
//!
//! # Scope
//!
//! Only the page-valued bead `/P` is handled. The ring links (`/N`, `/V`), the
//! thread back-pointer (`/T`), the bead rectangle (`/R`), the thread `/F`, and
//! the surviving pages' `/B` arrays carry no removed-page reference that needs
//! dropping and are left unchanged.

use crate::page_tree_rebuild::RebuildResult;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Drop dangling article-thread bead `/P` references after a page-tree rebuild
/// (qpdf `--pages` parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::page_tree_rebuild::rebuild_page_tree`]. Its `ref_map` encodes the
/// old → new page reference mapping: a page absent from the map was removed; a
/// page present maps to `ref_map[old][0]` (first new occurrence).
///
/// Walks the article threads from the catalog `/Threads` array: each thread's
/// `/F` seeds a bead ring whose neighbours are followed through `/N` and `/V`
/// (bounded by a visited set, since a thread's beads form a cycle). Each bead's
/// `/P` is remapped when its target page survived and removed when its target
/// page was dropped, so that a removed page referenced by nothing else is
/// garbage-collected by the subsequent subset sweep
/// ([`crate::subset_prune::prune_after_subset`]). The function mutates `pdf` in
/// place (same convention as `rebuild_page_tree`) and succeeds silently when
/// the document has no `/Threads`.
///
/// # Errors
///
/// Any error propagated from [`Pdf::resolve`] / [`Pdf::resolve_borrowed`] while
/// resolving the catalog, the `/Threads` array, the threads, or the beads.
pub fn drop_thread_bead_dangling_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<()> {
    let surviving: BTreeMap<ObjectRef, ObjectRef> = result
        .ref_map
        .iter()
        .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
        .collect();

    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()), // No catalog, nothing to do.
    };
    let threads_val = {
        let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
        let Some(catalog) = catalog_obj.as_dict() else {
            return Ok(());
        };
        // Shared borrow of the catalog: clone the (small) /Threads value out.
        catalog.get("Threads").cloned()
    };
    let Some(threads_val) = threads_val else {
        return Ok(()); // No article threads.
    };
    // /Threads is an array of thread dictionaries; it may itself be stored as
    // an indirect reference to that array.
    let threads = match threads_val {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve(r)? {
            Object::Array(arr) => arr,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    // Seed the walk with each thread's /F (first bead). The ring is then
    // traversed through /N and /V; a thread's beads form a cycle, so the
    // visited set is what bounds the walk (beads are always indirect objects).
    let mut queue: Vec<ObjectRef> = Vec::new();
    for thread in &threads {
        let Some(thread_ref) = thread.as_ref_id() else {
            continue;
        };
        let first_bead = {
            let thread_obj = pdf.resolve_borrowed(thread_ref)?;
            thread_obj
                .as_dict()
                .and_then(|d| d.get("F"))
                .and_then(Object::as_ref_id)
        };
        if let Some(first_bead) = first_bead {
            queue.push(first_bead);
        }
    }

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    while let Some(bead_ref) = queue.pop() {
        if !visited.insert(bead_ref) {
            continue;
        }
        // `resolve` returns an owned object; move out its dictionary (a non-dict
        // /N/V neighbour is malformed and skipped).
        let Some(mut bead) = pdf.resolve(bead_ref)?.into_dict() else {
            continue;
        };
        // Enqueue ring neighbours before any mutation.
        for key in ["N", "V"] {
            if let Some(Object::Reference(r)) = bead.get(key) {
                queue.push(*r);
            }
        }
        // /P is by spec an indirect reference to the page the bead belongs to;
        // any other form is malformed and left unchanged. A surviving target is
        // remapped to its new ref; a removed target has the key dropped so the
        // page is garbage-collected. Only a changed bead is rewritten (a kept,
        // unchanged bead's body is never re-serialized).
        if let Some(Object::Reference(pg)) = bead.get("P") {
            match surviving.get(pg) {
                Some(&new) if new != *pg => {
                    bead.insert("P", Object::Reference(new));
                    pdf.set_object(bead_ref, Object::Dictionary(bead));
                }
                Some(_) => {} // Surviving under the same ref: nothing to change.
                None => {
                    bead.remove("P");
                    pdf.set_object(bead_ref, Object::Dictionary(bead));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    /// Serialize `objs` (object number → body) into a classic-xref PDF with
    /// `/Root 1 0 R`.
    fn build_pdf(objs: &BTreeMap<u32, String>) -> Vec<u8> {
        let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
        for (n, body) in objs {
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

    /// Base skeleton: catalog (1) with `/Threads [10 0 R]`, pages root (2),
    /// three pages (3, 4, 5) each carrying a `/B` bead array, a thread (10)
    /// whose `/F` is bead 11, and a 3-bead ring 11→12→13 (via `/N`) where bead
    /// 11 is on page 3, bead 12 on page 4, bead 13 on page 5.
    fn base_objs() -> BTreeMap<u32, String> {
        let mut objs: BTreeMap<u32, String> = BTreeMap::new();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads [10 0 R] >>".into(),
        );
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
        objs
    }

    fn open(objs: &BTreeMap<u32, String>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(build_pdf(objs))).expect("open fixture")
    }

    /// A `RebuildResult` keeping pages 3 and 5 under their original refs
    /// (page 4 removed).
    fn keep_3_and_5() -> RebuildResult {
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(3, 0)]);
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(5, 0)]);
        RebuildResult {
            new_kids: vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)],
            ref_map,
        }
    }

    fn bead_dict(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> crate::Dictionary {
        pdf.resolve(ObjectRef::new(num, 0))
            .expect("resolve bead")
            .into_dict()
            .expect("bead object is a dictionary")
    }

    #[test]
    fn removed_bead_p_dropped_and_bead_kept() {
        // Keep pages 3 and 5; page 4 removed. Bead 12 (on page 4) loses /P; the
        // bead itself and its ring links stay. Beads 11/13 keep their /P.
        let mut pdf = open(&base_objs());
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("bead /P drop");

        let bead12 = bead_dict(&mut pdf, 12);
        let p12 = bead12.get("P");
        assert!(
            p12.is_none(),
            "bead 12 /P (removed page) must be dropped, got {p12:?}"
        );
        // Ring links, thread back-pointer, and rectangle are retained.
        let n12 = bead12.get("N");
        assert!(
            matches!(n12, Some(Object::Reference(r)) if r.number == 13),
            "bead 12 /N must be kept, got {n12:?}"
        );
        let v12 = bead12.get("V");
        assert!(
            matches!(v12, Some(Object::Reference(r)) if r.number == 11),
            "bead 12 /V must be kept, got {v12:?}"
        );
        assert!(bead12.get("T").is_some(), "bead 12 /T must be kept");
        assert!(bead12.get("R").is_some(), "bead 12 /R must be kept");

        // Beads on surviving pages keep their /P.
        let bead11 = bead_dict(&mut pdf, 11);
        let p11 = bead11.get("P");
        assert!(
            matches!(p11, Some(Object::Reference(r)) if r.number == 3),
            "bead 11 /P (surviving page 3) must be kept, got {p11:?}"
        );
        let bead13 = bead_dict(&mut pdf, 13);
        let p13 = bead13.get("P");
        assert!(
            matches!(p13, Some(Object::Reference(r)) if r.number == 5),
            "bead 13 /P (surviving page 5) must be kept, got {p13:?}"
        );
    }

    #[test]
    fn surviving_bead_p_remapped_to_new_ref() {
        // Page 3 survives under a new ref (7 0 R), as a duplicate-page selection
        // can produce: bead 11's /P must be remapped to the new ref, not dropped.
        let mut pdf = open(&base_objs());
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(5, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(7, 0), ObjectRef::new(5, 0)],
            ref_map,
        };

        drop_thread_bead_dangling_p(&mut pdf, &result).expect("bead /P remap");

        let bead11 = bead_dict(&mut pdf, 11);
        let p11 = bead11.get("P");
        assert!(
            matches!(p11, Some(Object::Reference(r)) if r.number == 7),
            "bead 11 /P (surviving page under new ref) must be remapped, got {p11:?}"
        );
        // The surviving page under its original ref is left unchanged.
        let bead13 = bead_dict(&mut pdf, 13);
        let p13 = bead13.get("P");
        assert!(
            matches!(p13, Some(Object::Reference(r)) if r.number == 5),
            "bead 13 /P (surviving page, identity) must be kept, got {p13:?}"
        );
    }

    /// Skeleton like [`base_objs`] but with a fourth, bead-less page (6) so a
    /// selection can keep a page while every bead targets a removed page.
    fn objs_with_beadless_page() -> BTreeMap<u32, String> {
        let mut objs = base_objs();
        objs.insert(
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>".into(),
        );
        objs.insert(
            6,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
        objs
    }

    #[test]
    fn all_beads_dangling_keeps_thread_and_ring_and_terminates() {
        // Keep only the bead-less page 6, so every bead (on pages 3/4/5) targets
        // a removed page. qpdf keeps the thread and every bead, dropping each
        // bead's /P. The cyclic ring (11→12→13→11) must terminate via `visited`.
        let mut pdf = open(&objs_with_beadless_page());
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(6, 0), vec![ObjectRef::new(6, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(6, 0)],
            ref_map,
        };

        drop_thread_bead_dangling_p(&mut pdf, &result).expect("all-dangling drop");

        for num in [11, 12, 13] {
            let bead = bead_dict(&mut pdf, num);
            let p = bead.get("P");
            assert!(
                p.is_none(),
                "bead {num} /P must be dropped (all dangling), got {p:?}"
            );
            // Ring links survive so the thread stays traversable.
            assert!(bead.get("N").is_some(), "bead {num} /N must be kept");
            assert!(bead.get("V").is_some(), "bead {num} /V must be kept");
        }
        // The thread itself is retained with its /F first-bead pointer.
        let thread = bead_dict(&mut pdf, 10);
        let f = thread.get("F");
        assert!(
            matches!(f, Some(Object::Reference(r)) if r.number == 11),
            "thread /F must be kept, got {f:?}"
        );
    }

    #[test]
    fn no_threads_is_a_noop() {
        // A catalog without /Threads: nothing to walk, succeeds silently.
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("noop");
        // Beads are untouched (no thread root reached them).
        let bead12 = bead_dict(&mut pdf, 12);
        assert!(
            bead12.get("P").is_some(),
            "without /Threads no bead /P is touched"
        );
    }

    #[test]
    fn indirect_threads_array_is_walked() {
        // /Threads stored as an indirect reference to the array object.
        let mut objs = base_objs();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads 14 0 R >>".into(),
        );
        objs.insert(14, "[10 0 R]".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("indirect /Threads");

        let bead12 = bead_dict(&mut pdf, 12);
        let p12 = bead12.get("P");
        assert!(
            p12.is_none(),
            "bead reached through an indirect /Threads array must have /P dropped, got {p12:?}"
        );
    }

    #[test]
    fn non_array_threads_is_a_noop() {
        // A malformed /Threads (not an array, not a ref to one) is ignored.
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R /Threads 42 >>".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("noop");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "a non-array /Threads must leave beads untouched"
        );
    }

    #[test]
    fn indirect_threads_ref_to_non_array_is_a_noop() {
        // /Threads is an indirect reference, but the target is not an array.
        let mut objs = base_objs();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads 14 0 R >>".into(),
        );
        objs.insert(14, "42".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("noop");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "an indirect /Threads resolving to a non-array must leave beads untouched"
        );
    }

    #[test]
    fn non_ref_thread_entry_and_thread_without_f_skipped() {
        // /Threads holds a direct (non-reference) entry — skipped — plus a real
        // thread 10 whose first bead is processed, and a second thread 15 with
        // no /F — skipped without error.
        let mut objs = base_objs();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads [42 10 0 R 15 0 R] >>".into(),
        );
        objs.insert(15, "<< /Type /Thread >>".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("mixed /Threads");

        assert!(
            bead_dict(&mut pdf, 12).get("P").is_none(),
            "the real thread's dangling bead /P must still be dropped"
        );
    }

    #[test]
    fn malformed_non_ref_p_left_unchanged() {
        // A bead whose /P is not an indirect reference (malformed) is left as-is.
        let mut objs = base_objs();
        objs.insert(
            12,
            "<< /Type /Bead /T 10 0 R /N 13 0 R /V 11 0 R /P 999 /R [0 0 100 100] >>".into(),
        );
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("malformed /P");

        let bead12 = bead_dict(&mut pdf, 12);
        let p12 = bead12.get("P");
        assert!(
            matches!(p12, Some(Object::Integer(999))),
            "a non-reference /P must be left unchanged, got {p12:?}"
        );
    }

    #[test]
    fn non_dict_bead_in_ring_skipped() {
        // A /N neighbour that resolves to a non-dictionary is skipped without
        // error; the rest of the ring is still processed.
        let mut objs = base_objs();
        // Bead 11's /N points at object 16, which is not a dictionary.
        objs.insert(
            11,
            "<< /Type /Bead /T 10 0 R /N 16 0 R /V 13 0 R /P 3 0 R /R [0 0 100 100] >>".into(),
        );
        objs.insert(16, "42".into());
        // Thread /F points at bead 12 directly so the removed-page bead is still
        // reached even though the 11→16 link is broken.
        objs.insert(10, "<< /Type /Thread /F 12 0 R >>".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("non-dict neighbour");

        assert!(
            bead_dict(&mut pdf, 12).get("P").is_none(),
            "bead 12 dangling /P must be dropped despite a non-dict ring neighbour"
        );
    }

    #[test]
    fn non_dict_catalog_is_a_noop() {
        // /Root resolves to a non-dictionary object: the walk bails out cleanly
        // before reaching any thread.
        let mut objs = base_objs();
        objs.insert(1, "42".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("non-dict catalog noop");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "a non-dictionary catalog must leave beads untouched"
        );
    }

    #[test]
    fn no_root_in_trailer_is_a_noop() {
        // A trailer without /Root: root_ref() is None, so the walk bails out.
        let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let off1 = raw.len();
        raw.extend_from_slice(b"1 0 obj\n<< /Type /Bead /P 3 0 R >>\nendobj\n");
        let xref_pos = raw.len();
        raw.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n\
                 trailer\n<< /Size 2 >>\nstartxref\n{xref_pos}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("open no-root fixture");
        assert!(pdf.root_ref().is_none(), "fixture must have no /Root");
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("no-root noop");
    }
}
