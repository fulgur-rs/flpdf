//! Article-thread bead `/P` reference drop after page extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree for a subset extraction, this module walks the article-thread beads
//! (ISO 32000-2 §12.4.3) and updates each bead's `/P` (the page the bead
//! belongs to) to match qpdf's `--pages` behaviour:
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
//! # Reaching the beads
//!
//! qpdf reaches a bead ring through **either** entry point, so this pass seeds
//! its walk from both:
//!
//! - the catalog `/Threads` article list (`/Threads` → thread `/F` → ring), and
//! - each surviving page's `/B` bead array.
//!
//! A removed page kept alive only through a sibling bead reachable via a
//! surviving page's `/B` (with no usable `/Threads`) must still have that
//! bead's `/P` dropped, or the prune cannot collect the page. Indirection is
//! normalized through [`crate::outline_dest_remap::resolve_ref_chain`] at every
//! link (`/Threads`, the thread entry, `/F`, `/N`, `/V`, `/B`, and `/P`), so a
//! reference-to-reference chain, a direct (inline) thread dictionary, or a
//! chained `/P` is handled the same way the single-page extraction path
//! ([`crate::page_extract`]) handles them. The walk is bounded by a visited set
//! keyed on the terminal bead ref (a thread's beads form a cycle).
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! For `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf` over a document whose page 2
//! is referenced only by an article bead's `/P`, qpdf drops that bead's `/P`
//! and the removed page is absent from the output (not emitted as `null`). The
//! bead stays in the thread ring with its `/N`/`/V`/`/T`/`/R` intact. Even when
//! every bead of a thread targets a removed page, qpdf keeps the thread and all
//! its beads (each with `/P` dropped) — it never removes the thread itself. The
//! same drop applies when the ring is reachable only through a surviving page's
//! `/B` array rather than the catalog `/Threads`.

use crate::outline_dest_remap::resolve_ref_chain;
use crate::page_tree_rebuild::RebuildResult;
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
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
/// The walk is seeded from both the catalog `/Threads` article list and every
/// surviving page's `/B` bead array, then follows the ring through `/N` and
/// `/V` (bounded by a visited set, since a thread's beads form a cycle). Each
/// bead's `/P` is remapped when its target page survived and removed when its
/// target page was dropped, so that a removed page referenced by nothing else
/// is garbage-collected by the subsequent subset sweep
/// ([`crate::subset_prune::prune_after_subset`]). Reference-to-reference chains
/// at every link are normalized via
/// [`crate::outline_dest_remap::resolve_ref_chain`]. The function mutates `pdf`
/// in place (same convention as `rebuild_page_tree`) and succeeds silently when
/// the document has no article beads.
///
/// # Errors
///
/// Any error propagated from [`Pdf::resolve`] / [`Pdf::resolve_borrowed`] while
/// resolving the catalog, the `/Threads` array, the threads, the surviving
/// pages, or the beads.
pub fn drop_thread_bead_dangling_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<()> {
    let surviving: BTreeMap<ObjectRef, ObjectRef> = result
        .ref_map
        .iter()
        .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
        .collect();

    // Seed the bead walk from every entry point qpdf reaches a ring through:
    // the catalog /Threads article list and each surviving page's /B array.
    let mut queue: Vec<ObjectRef> = Vec::new();
    seed_from_threads(pdf, &mut queue)?;
    seed_from_surviving_pages(pdf, result, &mut queue)?;

    // Walk the ring(s). resolve_ref_chain normalizes /N//V/F indirection so the
    // visited key and the write-back target are the terminal bead ref (never an
    // intermediate reference holder).
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    while let Some(start_ref) = queue.pop() {
        let (concrete, terminal) = resolve_ref_chain(pdf, &Object::Reference(start_ref))?;
        let bead_ref = terminal.unwrap_or(start_ref);
        if !visited.insert(bead_ref) {
            continue;
        }
        let Some(mut bead) = concrete.into_dict() else {
            continue;
        };
        // Enqueue ring neighbours before any mutation; they are chain-resolved
        // when popped.
        for key in ["N", "V"] {
            if let Some(Object::Reference(r)) = bead.get(key) {
                queue.push(*r);
            }
        }
        if remap_or_drop_bead_p(pdf, &mut bead, &surviving)? {
            pdf.set_object(bead_ref, Object::Dictionary(bead));
        }
    }
    Ok(())
}

/// Push every catalog-`/Threads` thread's first bead (`/F`) onto `queue`.
///
/// `/Threads` (an array of thread dictionaries) may be stored as an indirect
/// reference; each entry may be an indirect reference to a thread dictionary or
/// a direct (inline) one. Returns silently when there is no catalog or no
/// `/Threads`.
fn seed_from_threads<R: Read + Seek>(pdf: &mut Pdf<R>, queue: &mut Vec<ObjectRef>) -> Result<()> {
    let Some(catalog_ref) = pdf.root_ref() else {
        return Ok(()); // No catalog.
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
    // /Threads may be an indirect (possibly multi-hop) reference to the array.
    let (threads_concrete, _) = resolve_ref_chain(pdf, &threads_val)?;
    let Object::Array(threads) = threads_concrete else {
        return Ok(());
    };
    for thread in &threads {
        if let Some(first_bead) = thread_first_bead(pdf, thread)? {
            queue.push(first_bead);
        }
    }
    Ok(())
}

/// Resolve a `/Threads` entry to its thread dictionary and return the terminal
/// ref of its `/F` (first bead), if any.
///
/// The entry may be an indirect reference (chain) to a thread dictionary or a
/// direct (inline) dictionary; `/F` may itself be a reference chain.
fn thread_first_bead<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    thread: &Object,
) -> Result<Option<ObjectRef>> {
    let (concrete, _) = resolve_ref_chain(pdf, thread)?;
    let Some(dict) = concrete.as_dict() else {
        return Ok(None);
    };
    let Some(f_val) = dict.get("F") else {
        return Ok(None);
    };
    let (_, terminal) = resolve_ref_chain(pdf, f_val)?;
    Ok(terminal)
}

/// Push the beads listed in each surviving page's `/B` array onto `queue`.
///
/// The surviving pages are `result.new_kids` (deduplicated, since a duplicated
/// selection lists a page more than once). `/B` may be an indirect reference to
/// the array. Beads on removed pages are still reached from here through the
/// ring's `/N`/`/V` links during the walk.
fn seed_from_surviving_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    queue: &mut Vec<ObjectRef>,
) -> Result<()> {
    let mut seen_pages: BTreeSet<ObjectRef> = BTreeSet::new();
    for &page_ref in &result.new_kids {
        if !seen_pages.insert(page_ref) {
            continue;
        }
        let b_val = {
            let page_obj = pdf.resolve_borrowed(page_ref)?;
            page_obj.as_dict().and_then(|p| p.get("B")).cloned()
        };
        let Some(b_val) = b_val else {
            continue;
        };
        let (b_concrete, _) = resolve_ref_chain(pdf, &b_val)?;
        if let Object::Array(beads) = b_concrete {
            for bead in &beads {
                if let Some(r) = bead.as_ref_id() {
                    queue.push(r);
                }
            }
        }
    }
    Ok(())
}

/// Remap-or-drop a bead's `/P`. Returns `true` when `bead` was modified (so the
/// caller writes it back).
///
/// `/P` is by spec an indirect reference to the page the bead belongs to,
/// possibly through a reference chain. The chain is resolved to its terminal
/// page ref; a `/P` that does not resolve to a `/Type /Page` object (a
/// non-reference, or a non-page target) is malformed and left unchanged. A
/// surviving target is remapped to its new ref when the rebuild changed it; a
/// removed target has the key dropped so the page is garbage-collected.
fn remap_or_drop_bead_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    bead: &mut Dictionary,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<bool> {
    let Some(p_val) = bead.get("P").cloned() else {
        return Ok(false);
    };
    let (p_concrete, p_terminal) = resolve_ref_chain(pdf, &p_val)?;
    let Some(page_ref) = p_terminal else {
        return Ok(false); // Non-reference /P: malformed, left unchanged.
    };
    if !is_page_dict(&p_concrete) {
        return Ok(false); // /P does not resolve to a page: left unchanged.
    }
    match surviving.get(&page_ref) {
        Some(&new) if new != page_ref => {
            bead.insert("P", Object::Reference(new));
            Ok(true)
        }
        Some(_) => Ok(false), // Surviving under the same ref: nothing to change.
        None => {
            bead.remove("P");
            Ok(true)
        }
    }
}

/// `true` when `obj` is a `<< /Type /Page ... >>` dictionary.
fn is_page_dict(obj: &Object) -> bool {
    obj.as_dict()
        .and_then(|d| d.get("Type"))
        .is_some_and(|t| matches!(t, Object::Name(n) if n == b"Page"))
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
        build_pdf_inner(objs, true)
    }

    /// Like [`build_pdf`] but writes a trailer with no `/Root` when
    /// `with_root` is false (so `root_ref()` is `None`).
    fn build_pdf_inner(objs: &BTreeMap<u32, String>, with_root: bool) -> Vec<u8> {
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
        let root = if with_root { " /Root 1 0 R" } else { "" };
        raw.extend_from_slice(
            format!(
                "trailer\n<< /Size {}{root} >>\nstartxref\n{xref_pos}\n%%EOF\n",
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

    /// [`base_objs`] with the pages' `/B` arrays removed, so the only way to
    /// reach the bead ring is through the catalog `/Threads`. Used to isolate
    /// the `/Threads`-seeding branches from the `/B`-seeding fallback.
    fn base_objs_no_b() -> BTreeMap<u32, String> {
        let mut objs = base_objs();
        for n in 3..=5 {
            objs.insert(
                n,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
            );
        }
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

    fn bead_dict(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> Dictionary {
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
    fn seeds_from_surviving_page_b_without_threads() {
        // No catalog /Threads, but the kept pages' /B arrays reach the ring. The
        // removed-page bead must still lose its /P (qpdf 11.9.0 parity), or the
        // removed page would stay reachable via the kept page's /B ring.
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("/B seeding");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            p12.is_none(),
            "bead reached only via a surviving page's /B must have its dangling /P dropped, got {p12:?}"
        );
        // A surviving bead reached the same way keeps its /P.
        let p11 = bead_dict(&mut pdf, 11).get("P").cloned();
        assert!(
            matches!(p11, Some(Object::Reference(r)) if r.number == 3),
            "surviving bead /P reached via /B must be kept, got {p11:?}"
        );
    }

    #[test]
    fn direct_thread_dict_in_threads_is_processed() {
        // /Threads holds a DIRECT (inline) thread dictionary, and the pages have
        // no /B, so the only entry point is the direct dict. Its ring's
        // removed-page bead must still lose its /P.
        let mut objs = base_objs_no_b();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads [ << /Type /Thread /F 11 0 R >> ] >>".into(),
        );
        objs.remove(&10); // the inline thread replaces the indirect one
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("direct thread dict");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            p12.is_none(),
            "a direct (inline) thread dictionary's dangling bead /P must be dropped, got {p12:?}"
        );
    }

    #[test]
    fn bead_p_reference_chain_is_resolved() {
        // Bead 12's /P is a reference-to-reference chain to the removed page 4;
        // bead 11's /P is a chain to the surviving page 3. The chain must be
        // resolved before classifying: 12 dropped, 11 kept.
        let mut objs = base_objs_no_b();
        objs.insert(
            11,
            "<< /Type /Bead /T 10 0 R /N 12 0 R /V 13 0 R /P 21 0 R /R [0 0 100 100] >>".into(),
        );
        objs.insert(
            12,
            "<< /Type /Bead /T 10 0 R /N 13 0 R /V 11 0 R /P 20 0 R /R [0 0 100 100] >>".into(),
        );
        objs.insert(20, "4 0 R".into()); // chain → removed page 4
        objs.insert(21, "3 0 R".into()); // chain → surviving page 3
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("/P chain");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            p12.is_none(),
            "a chained /P to a removed page must be dropped, got {p12:?}"
        );
        let p11 = bead_dict(&mut pdf, 11).get("P").cloned();
        assert!(
            matches!(p11, Some(Object::Reference(r)) if r.number == 21),
            "a chained /P to a surviving page must be kept verbatim, got {p11:?}"
        );
    }

    #[test]
    fn ring_neighbour_reference_chain_is_followed() {
        // Bead 11's /N points at object 22, whose value is the real bead 12
        // reference (a ref-to-ref ring link). With no /B fallback the removed
        // bead 12 is reachable only through this chained /N; it must still be
        // processed.
        let mut objs = base_objs_no_b();
        objs.insert(
            11,
            "<< /Type /Bead /T 10 0 R /N 22 0 R /V 13 0 R /P 3 0 R /R [0 0 100 100] >>".into(),
        );
        objs.insert(22, "12 0 R".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("/N chain");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            p12.is_none(),
            "a bead reached only through a chained /N must have its dangling /P dropped, got {p12:?}"
        );
    }

    #[test]
    fn threads_multi_hop_reference_chain_is_resolved() {
        // /Threads → 14 → 15 → [10 0 R] (two-hop chain). No /B fallback, so the
        // removed-page bead is reachable only by resolving the chain.
        let mut objs = base_objs_no_b();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads 14 0 R >>".into(),
        );
        objs.insert(14, "15 0 R".into());
        objs.insert(15, "[10 0 R]".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("multi-hop /Threads");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            p12.is_none(),
            "a thread reached through a multi-hop /Threads chain must be walked, got {p12:?}"
        );
    }

    #[test]
    fn non_dict_thread_entry_and_thread_without_f_skipped() {
        // /Threads holds a direct non-dictionary entry (42 — skipped), the real
        // thread 10, and a second thread 15 with no /F (skipped). No /B, so only
        // thread 10 reaches the ring.
        let mut objs = base_objs_no_b();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Threads [42 10 0 R 15 0 R] >>".into(),
        );
        objs.insert(15, "<< /Type /Thread >>".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("mixed /Threads");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            p12.is_none(),
            "the real thread's dangling bead /P must still be dropped, got {p12:?}"
        );
    }

    #[test]
    fn thread_f_non_reference_is_skipped() {
        // Thread 10's /F is a non-reference (integer): no first bead, skipped.
        // No /B, so nothing reaches the ring and beads are untouched.
        let mut objs = base_objs_no_b();
        objs.insert(10, "<< /Type /Thread /F 0 >>".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("non-ref /F");

        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "a thread whose /F is not a reference yields no first bead"
        );
    }

    #[test]
    fn non_array_threads_with_no_b_is_a_noop() {
        // A malformed /Threads (not an array, not a ref to one) and no /B: no
        // entry point, so beads are untouched.
        let mut objs = base_objs_no_b();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R /Threads 42 >>".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("noop");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "a non-array /Threads with no /B must leave beads untouched"
        );
    }

    #[test]
    fn no_threads_no_b_is_a_noop() {
        // No /Threads and no /B: nothing is seeded, so beads are untouched.
        let mut objs = base_objs_no_b();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("noop");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "with neither /Threads nor /B, beads are untouched"
        );
    }

    #[test]
    fn surviving_page_b_non_array_is_skipped() {
        // A surviving page's /B is not an array (malformed) and there is no
        // /Threads: nothing is seeded.
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        objs.insert(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B 99 >>".into(),
        );
        objs.insert(
            5,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B 99 >>".into(),
        );
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("noop");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_some(),
            "a non-array /B must be ignored"
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

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            matches!(p12, Some(Object::Integer(999))),
            "a non-reference /P must be left unchanged, got {p12:?}"
        );
    }

    #[test]
    fn p_resolving_to_non_page_left_unchanged() {
        // Bead 12's /P points at a non-page object (not in `surviving`, but not
        // a page either): it must be left unchanged, not dropped.
        let mut objs = base_objs();
        objs.insert(
            12,
            "<< /Type /Bead /T 10 0 R /N 13 0 R /V 11 0 R /P 30 0 R /R [0 0 100 100] >>".into(),
        );
        objs.insert(30, "<< /Type /Whatever >>".into());
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("non-page /P");

        let p12 = bead_dict(&mut pdf, 12).get("P").cloned();
        assert!(
            matches!(p12, Some(Object::Reference(r)) if r.number == 30),
            "a /P resolving to a non-page object must be left unchanged, got {p12:?}"
        );
    }

    #[test]
    fn bead_without_p_is_left_unchanged() {
        // A bead that carries no /P at all is walked (for its ring links) but
        // needs no change.
        let mut objs = base_objs();
        objs.insert(
            12,
            "<< /Type /Bead /T 10 0 R /N 13 0 R /V 11 0 R /R [0 0 100 100] >>".into(),
        );
        let mut pdf = open(&objs);

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("bead without /P");

        let bead12 = bead_dict(&mut pdf, 12);
        assert!(bead12.get("P").is_none(), "bead 12 still has no /P");
        // The rest of the ring is still processed (bead 11 kept, 13 kept).
        assert!(
            bead_dict(&mut pdf, 11).get("P").is_some(),
            "the rest of the ring is still walked"
        );
    }

    #[test]
    fn non_dict_bead_in_ring_skipped() {
        // A /N neighbour that resolves to a non-dictionary is skipped without
        // error; the rest of the ring is still processed. No /B, so the chained
        // entry is the thread /F.
        let mut objs = base_objs_no_b();
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
    fn non_dict_catalog_still_seeds_from_b() {
        // /Root resolves to a non-dictionary: the /Threads walk bails out, but
        // the /B seeding (independent of the catalog) still drops the dangling
        // bead /P.
        let mut objs = base_objs();
        objs.insert(1, "42".into());
        let mut pdf = open(&objs);
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("non-dict catalog");
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_none(),
            "a non-dictionary catalog still permits /B-seeded bead /P drop"
        );
    }

    #[test]
    fn no_root_still_seeds_from_b() {
        // A trailer without /Root: root_ref() is None, so the /Threads walk
        // bails out, but /B seeding (driven by the rebuild result, not the
        // catalog) still drops the dangling bead /P.
        let mut objs = base_objs();
        objs.remove(&1); // drop the catalog
        let raw = build_pdf_inner(&objs, false);
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("open no-root fixture");
        assert!(pdf.root_ref().is_none(), "fixture must have no /Root");

        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("no-root /B seeding");

        assert!(
            bead_dict(&mut pdf, 12).get("P").is_none(),
            "a missing catalog still permits /B-seeded bead /P drop"
        );
    }

    #[test]
    fn non_dict_surviving_page_is_skipped() {
        // A surviving page ref that resolves to a non-dictionary (malformed
        // selection) is skipped during /B seeding without error.
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        objs.insert(3, "42".into()); // surviving "page" is not a dict
        let mut pdf = open(&objs);

        // new_kids lists page 3 (now a non-dict) and page 5; only 5 has a /B.
        drop_thread_bead_dangling_p(&mut pdf, &keep_3_and_5()).expect("non-dict surviving page");

        // Page 5's /B still reaches the ring and drops bead 12's dangling /P.
        assert!(
            bead_dict(&mut pdf, 12).get("P").is_none(),
            "a non-dict surviving page is skipped but the others are still walked"
        );
    }

    #[test]
    fn duplicate_surviving_page_ref_in_new_kids_is_deduped() {
        // new_kids lists the same surviving page ref twice; the /B-seeding dedup
        // must visit each page once. (No /Threads, so /B is the only entry.)
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        let mut pdf = open(&objs);
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(3, 0)]);
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(5, 0)]);
        let result = RebuildResult {
            new_kids: vec![
                ObjectRef::new(3, 0),
                ObjectRef::new(3, 0),
                ObjectRef::new(5, 0),
            ],
            ref_map,
        };

        drop_thread_bead_dangling_p(&mut pdf, &result).expect("dedup");

        assert!(
            bead_dict(&mut pdf, 12).get("P").is_none(),
            "a duplicated page ref in new_kids must not break /B seeding"
        );
    }
}
