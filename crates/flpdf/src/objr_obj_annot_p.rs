//! Annotation `/P` reference drop for annotations kept alive only through a
//! structure-tree object reference (`/Type /OBJR`) `/Obj`, after page
//! extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree for a subset extraction, an annotation on a removed page is normally
//! garbage-collected with that page. But when a structure-tree object reference
//! (`/Type /OBJR`, ISO 32000-2 §14.7.4.4) keeps the annotation alive through its
//! `/Obj`, the annotation survives — and if its `/P` (the page the annotation is
//! on, §12.5.2) still points at the removed page, that back-reference keeps the
//! page alive too, leaving an orphan `/Type /Page` in the output.
//!
//! This pass updates each such annotation's `/P` to match qpdf's `--pages`
//! behaviour:
//!
//! - A `/P` pointing at a **surviving** page keeps the entry, remapped to the
//!   page's new [`ObjectRef`] when the rebuild changed it.
//! - A `/P` pointing at a **removed** page has the `/P` key **dropped**. The
//!   annotation itself (and the OBJR `/Obj` reaching it) is retained; the
//!   now-unreferenced page is garbage-collected by the subsequent subset sweep
//!   ([`crate::subset_prune`]) and is absent from the output.
//!
//! This is the structural-reference *drop* family, alongside the structure-tree
//! `/Pg` handling ([`crate::struct_tree_pg`]) and the article-thread bead `/P`
//! handling ([`crate::thread_bead_p`]): the reference is removed rather than
//! replaced with `null`.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! For `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf` over a document whose page 2
//! is referenced only by an OBJR `/Obj` annotation's `/P`, qpdf drops that
//! annotation's `/P` and the removed page is absent from the output (not emitted
//! as `null`). The annotation survives via the OBJR `/Obj`, which qpdf keeps.
//!
//! # Scope
//!
//! Only the `/P` of annotations reached through a structure-tree OBJR `/Obj` is
//! handled here. Out of scope:
//!
//! - Annotations on surviving pages (their `/P` is the page they live on, kept
//!   by the writer's reference remap).
//! - AcroForm widget annotations, handled by the field/widget prune.
//! - A direct (inline) `/Obj` dictionary: `/Obj` is by spec an indirect
//!   reference, so an inline object is malformed and left unchanged.
//! - An OBJR `/Obj` target without a `/P`, or whose `/P` is not a reference.
//! - A `/P` that does not resolve to a page object (e.g. a non-annotation OBJR
//!   `/Obj` target whose `/P` names a different relationship) is left unchanged.

use crate::page_tree_rebuild::RebuildResult;
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Drop dangling `/P` references on annotations kept alive through a
/// structure-tree OBJR `/Obj`, after a page-tree rebuild (qpdf `--pages`
/// parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::page_tree_rebuild::rebuild_page_tree`]; its `ref_map` encodes the
/// old → new page mapping (a page absent from the map was removed; a page
/// present maps to `ref_map[old][0]`). `objr_obj_targets` are the OBJR `/Obj`
/// references collected during the structure-tree walk
/// ([`crate::struct_tree_pg::drop_struct_elem_dangling_pg`]).
///
/// Each target is resolved (reference-to-reference chains normalized to their
/// terminal ref and deduplicated by a visited set). When the target is a
/// dictionary whose `/P` is a reference to a removed page, the `/P` key is
/// dropped so the page is garbage-collected by the subsequent subset sweep
/// ([`crate::subset_prune::prune_after_subset`]); when `/P` points at a
/// surviving page it is remapped to the page's new ref. A target with no `/P`,
/// or a `/P` that is not a reference, is left unchanged. The function mutates
/// `pdf` in place and succeeds silently when `objr_obj_targets` is empty.
///
/// # Errors
///
/// Any error propagated from [`Pdf::resolve`] / [`Pdf::resolve_borrowed`] while
/// resolving a target annotation or its `/P` chain.
pub fn drop_objr_obj_annot_dangling_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    objr_obj_targets: &[ObjectRef],
) -> Result<()> {
    let surviving: BTreeMap<ObjectRef, ObjectRef> = result
        .ref_map
        .iter()
        .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
        .collect();

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    for &start in objr_obj_targets {
        // Skip a duplicate start ref before resolving the (potentially I/O-bound,
        // decryption-involving) chain.
        if !visited.insert(start) {
            continue;
        }
        // Normalize a reference chain so the visited key and the write-back
        // target are the terminal annotation ref, never an intermediate holder.
        let (concrete, terminal) = resolve_ref_chain(pdf, &Object::Reference(start))?;
        let annot_ref = terminal.unwrap_or(start);
        // A distinct start that resolves to an already-processed terminal is
        // skipped too (two OBJR /Obj entries can reach the same annotation).
        if annot_ref != start && !visited.insert(annot_ref) {
            continue;
        }
        let Some(mut annot) = concrete.into_dict() else {
            continue;
        };
        if remap_or_drop_annot_p(pdf, &mut annot, &surviving)? {
            pdf.set_object(annot_ref, Object::Dictionary(annot));
        }
    }
    Ok(())
}

/// Remap-or-drop the `/P` of one annotation dictionary. Returns whether the
/// dictionary changed.
///
/// `/P` is by spec an indirect reference to the page the annotation is on; any
/// other form is malformed and left unchanged. A surviving target is remapped to
/// its new ref (an identity remap in a single-document rebuild); a removed target
/// has the key dropped so the page is garbage-collected.
fn remap_or_drop_annot_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot: &mut Dictionary,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<bool> {
    let p_ref = match annot.get("P") {
        Some(Object::Reference(r)) => *r,
        _ => return Ok(false),
    };
    // Normalize a possible reference-to-reference chain to the terminal page ref.
    let (p_concrete, terminal) = resolve_ref_chain(pdf, &Object::Reference(p_ref))?;
    let page_ref = terminal.unwrap_or(p_ref);
    // /P must resolve to a page. An OBJR /Obj can reference a non-annotation
    // object whose /P means something else (e.g. a /Type /StructElem whose /P is
    // the parent structure element); such a /P is left unchanged.
    if !is_page_dict(&p_concrete) {
        return Ok(false);
    }
    match surviving.get(&page_ref) {
        Some(&new) => {
            if new != page_ref {
                annot.insert("P", Object::Reference(new));
                return Ok(true);
            }
            Ok(false)
        }
        None => {
            annot.remove("P");
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

    fn open(objs: &BTreeMap<u32, String>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(build_pdf(objs))).expect("open fixture")
    }

    /// Base: catalog (1), pages root (2) /Kids [3 4 5], three pages (3,4,5).
    /// The annotation under test is object 30.
    fn base() -> BTreeMap<u32, String> {
        let mut objs: BTreeMap<u32, String> = BTreeMap::new();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
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
        objs
    }

    /// `RebuildResult` keeping pages 3 and 5 (page 4 removed), identity refs.
    fn keep_3_and_5() -> RebuildResult {
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(3, 0)]);
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(5, 0)]);
        RebuildResult {
            new_kids: vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)],
            ref_map,
        }
    }

    fn annot(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> Dictionary {
        pdf.resolve(ObjectRef::new(num, 0))
            .expect("resolve annot")
            .into_dict()
            .expect("annot object is a dictionary")
    }

    #[test]
    fn dangling_p_to_removed_page_dropped() {
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 4 0 R /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(
            annot(&mut pdf, 30).get("P").is_none(),
            "removed-page /P must be dropped"
        );
    }

    #[test]
    fn p_to_surviving_page_kept() {
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 3),
            "surviving-page /P must be kept",
        );
    }

    #[test]
    fn p_to_surviving_page_remapped_to_new_ref() {
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        // Page 3 survives under a new ref (7 0 R), as a duplicate selection can produce.
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(7, 0)],
            ref_map,
        };
        drop_objr_obj_annot_dangling_p(&mut pdf, &result, &[ObjectRef::new(30, 0)]).expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 7),
            "surviving-page /P must be remapped to the new ref",
        );
    }

    #[test]
    fn target_without_p_left_unchanged() {
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        let a = annot(&mut pdf, 30);
        assert!(
            a.get("P").is_none() && a.get("Subtype").is_some(),
            "non-/P annot untouched"
        );
    }

    #[test]
    fn empty_targets_is_noop() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 4 0 R >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[]).expect("noop");
        assert!(
            annot(&mut pdf, 30).get("P").is_some(),
            "no targets ⇒ no change"
        );
    }

    #[test]
    fn chained_obj_and_p_normalized() {
        // Target is reached via a reference chain (40 → 30), and /P is itself a
        // chain (50 → 4 removed). Both terminals must be resolved.
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 50 0 R /Rect [0 0 10 10] >>".into(),
        );
        objs.insert(40, "30 0 R".into());
        objs.insert(50, "4 0 R".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(40, 0)])
            .expect("drop");
        assert!(
            annot(&mut pdf, 30).get("P").is_none(),
            "chained /P to removed page must drop"
        );
    }

    #[test]
    fn shared_target_deduped_non_identity() {
        // Same annot ref supplied twice with a NON-identity page remap (3 -> 7),
        // so the visited dedup guard is load-bearing. Pass 1 remaps /P 3 -> 7;
        // pass 2 must be skipped by the dedup guard. Without the guard, pass 2
        // re-reads the already-remapped /P 7, finds no surviving entry keyed by
        // 7, and would erroneously DROP /P.
        let mut objs = base();
        // Page 3 survives under a new ref (7 0 R). Object 7 must exist as a real
        // page so the second chain-resolution of /P does not hit a missing
        // object.
        objs.insert(
            7,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(7, 0)],
            ref_map,
        };
        drop_objr_obj_annot_dangling_p(
            &mut pdf,
            &result,
            &[ObjectRef::new(30, 0), ObjectRef::new(30, 0)],
        )
        .expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 7),
            "remapped /P 7 must survive the duplicate target; dedup guard prevents re-drop",
        );
    }

    #[test]
    fn distinct_starts_to_same_terminal_deduped_non_identity() {
        // Two DISTINCT start refs reach the same annotation: a direct one (30)
        // and a holder chain (40 -> 30). With a NON-identity page remap (3 -> 7),
        // the terminal-ref dedup is load-bearing. Pass for start 30 remaps /P
        // 3 -> 7; the start-40 pass resolves to the already-processed terminal 30
        // and must be skipped. Without the terminal dedup, the start-40 pass
        // re-reads the remapped /P 7, finds no surviving entry keyed by 7, and
        // would erroneously DROP /P.
        let mut objs = base();
        objs.insert(
            7,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into(),
        );
        objs.insert(40, "30 0 R".into());
        let mut pdf = open(&objs);
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(7, 0)],
            ref_map,
        };
        drop_objr_obj_annot_dangling_p(
            &mut pdf,
            &result,
            &[ObjectRef::new(30, 0), ObjectRef::new(40, 0)],
        )
        .expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 7),
            "remapped /P 7 must survive a distinct start reaching the same terminal",
        );
    }

    #[test]
    fn non_dict_target_skipped() {
        // An OBJR /Obj target that resolves to a non-dictionary (malformed) is
        // skipped without error and left in place.
        let mut objs = base();
        objs.insert(30, "42".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("non-dict target skipped");
        assert_eq!(
            pdf.resolve(ObjectRef::new(30, 0)).expect("resolve"),
            Object::Integer(42),
            "a non-dict OBJR /Obj target must be left unchanged",
        );
    }

    #[test]
    fn non_reference_p_left_unchanged() {
        // A /P that is not an indirect reference (an integer) is malformed and
        // must be left unchanged, per the documented scope.
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 999 /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Integer(999))),
            "a non-reference /P must be left unchanged",
        );
    }

    #[test]
    fn p_resolving_to_non_page_left_unchanged() {
        // A structure-tree OBJR /Obj can reference a non-annotation object whose
        // /P means something other than "the page this is on" (here a
        // /Type /StructElem whose /P is the parent structure element). Object 60
        // is not a page and is not in `surviving`, so without the is_page_dict
        // guard the /P would be wrongly dropped. The guard leaves it unchanged.
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 60 0 R /Rect [0 0 10 10] >>".into(),
        );
        objs.insert(60, "<< /Type /StructElem /S /P >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 60),
            "a /P resolving to a non-page object must be left unchanged",
        );
    }

    #[test]
    fn stream_target_skipped() {
        // An OBJR /Obj can reference a stream (e.g. an XObject). Object::into_dict
        // returns None for a stream, so the target is skipped via the
        // `else { continue; }` arm with no stream-body corruption and no error.
        let mut objs = base();
        objs.insert(30, "<< /Length 3 >>\nstream\nabc\nendstream".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("stream target skipped without error");
        assert!(
            matches!(pdf.resolve(ObjectRef::new(30, 0)), Ok(Object::Stream(_))),
            "a stream OBJR /Obj target must be left unchanged",
        );
    }
}
