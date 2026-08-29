//! qpdf correspondence: QPDFJob.cc removed-page nulling plus QPDFWriter.cc null-key visibility specialized for OBJR annotations.
//! Annotation `/P` reference drop for annotations kept alive only through a
//! structure-tree object reference (`/Type /OBJR`) `/Obj`, after page
//! extraction.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has rebuilt the page
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
//!   (the subsequent job subset sweep) and is absent from the output.
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
//! qpdf drops the annotation's `/P` even when the removed page is *also*
//! referenced by a surviving outline item or named destination: there the page
//! object is kept as `null` (the destination still points at it), but the
//! annotation's `/P` is still dropped.
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
//! - A `/P` that resolves to a live non-page object (e.g. a non-annotation OBJR
//!   `/Obj` target whose `/P` names a different relationship, such as a
//!   structure element's parent) is left unchanged. A `/P` resolving to `null`
//!   *is* treated as a removed page and dropped (see the note above).

use crate::object_handle::ObjectHandle;
use crate::pages::tree_rebuild::RebuildResult;
use crate::{ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Return a dictionary entry without hiding an indirect value that currently
/// resolves to null; its object reference is the page-membership identity.
fn raw_child(parent: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    Ok(parent
        .try_as_dictionary()?
        .and_then(|entries| entries.get(key).cloned()))
}

/// Drop dangling `/P` references on annotations kept alive through a
/// structure-tree OBJR `/Obj`, after a page-tree rebuild (qpdf `--pages`
/// parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::pages::tree_rebuild::rebuild_page_tree`]; its `ref_map` encodes the
/// old → new mapping for surviving page-tree leaves, while `removed_pages`
/// identifies the exact original page-tree leaves removed by the rebuild.
/// `objr_obj_targets` are the OBJR `/Obj`
/// references collected during the structure-tree walk
/// ([`crate::struct_tree_pg::drop_struct_elem_dangling_pg`]).
///
/// Each target is resolved through its canonical handle and deduplicated by its
/// object reference. When the target is a dictionary whose `/P` is a reference to a removed page, the `/P` key is
/// dropped so the page is garbage-collected by the subsequent subset sweep
/// ([`crate::job::QPDFJob::prune_after_subset`]); when `/P` points at a
/// surviving page it is remapped to the page's new ref. A `/P` pointing at a
/// non-page object or a page-like object outside the original page tree is left
/// unchanged. A target with no `/P`, or a `/P` that is not a reference, is also
/// left unchanged. The function mutates
/// `pdf` in place and succeeds silently when `objr_obj_targets` is empty.
///
/// # Errors
///
/// Any error propagated from [`Pdf::resolve`] while resolving a target
/// annotation or its `/P` value.
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
    let removed_pages = &result.removed_pages;

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    for &start in objr_obj_targets {
        // Skip a duplicate start ref before resolving the (potentially I/O-bound,
        // decryption-involving) chain.
        if !visited.insert(start) {
            continue;
        }
        let annot = pdf.get_object_handle(start);
        pdf.resolve(&annot)?;
        if annot.try_as_dictionary()?.is_none() {
            continue;
        }
        remap_or_drop_annot_p(pdf, &annot, &surviving, removed_pages)?;
    }
    Ok(())
}

/// Remap-or-drop the `/P` of one annotation dictionary. Returns whether the
/// dictionary changed.
///
/// `/P` is by spec an indirect reference to the page the annotation is on; any
/// other form is malformed and left unchanged. A surviving target is remapped to
/// its new ref (an identity remap in a single-document rebuild); a target in
/// `removed_pages` has the key dropped so the page is garbage-collected. A
/// reference that is neither a surviving nor a removed original page-tree leaf
/// is left unchanged, even if it resolves to a `/Type /Page` dictionary.
fn remap_or_drop_annot_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot: &ObjectHandle,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    removed_pages: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    let Some(p) = raw_child(annot, b"/P")? else {
        return Ok(false);
    };
    let Some(page_ref) = p.object_ref() else {
        return Ok(false);
    };
    if !surviving.contains_key(&page_ref) && !removed_pages.contains(&page_ref) {
        return Ok(false);
    }
    match surviving.get(&page_ref) {
        Some(&new) => {
            if new != page_ref {
                annot.replace_key(b"/P", pdf.get_object_handle(new))?;
                pdf.mark_object_handle_dirty(annot)?;
                return Ok(true);
            }
            Ok(false)
        }
        None => {
            annot.remove_key(b"/P");
            pdf.mark_object_handle_dirty(annot)?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectHandle, Pdf};
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
            removed_pages: [ObjectRef::new(4, 0)].into_iter().collect(),
        }
    }

    fn annot(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> ObjectHandle {
        let annot = pdf.get_object_handle(ObjectRef::new(num, 0));
        pdf.resolve(&annot).expect("resolve annot");
        assert!(
            annot.as_dictionary().is_some(),
            "annot object is not a dictionary"
        );
        annot
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
            !annot(&mut pdf, 30).has_key(b"/P"),
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
            annot(&mut pdf, 30).get_key(b"/P").object_ref() == Some(ObjectRef::new(3, 0)),
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
            ..Default::default()
        };
        drop_objr_obj_annot_dangling_p(&mut pdf, &result, &[ObjectRef::new(30, 0)]).expect("drop");
        assert!(
            annot(&mut pdf, 30).get_key(b"/P").object_ref() == Some(ObjectRef::new(7, 0)),
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
            !a.has_key(b"/P") && a.has_key(b"/Subtype"),
            "non-/P annot untouched"
        );
    }

    #[test]
    fn empty_targets_is_noop() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 4 0 R >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[]).expect("noop");
        assert!(annot(&mut pdf, 30).has_key(b"/P"), "no targets ⇒ no change");
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
            ..Default::default()
        };
        drop_objr_obj_annot_dangling_p(
            &mut pdf,
            &result,
            &[ObjectRef::new(30, 0), ObjectRef::new(30, 0)],
        )
        .expect("drop");
        assert!(
            annot(&mut pdf, 30).get_key(b"/P").object_ref() == Some(ObjectRef::new(7, 0)),
            "remapped /P 7 must survive the duplicate target; dedup guard prevents re-drop",
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
        let target = pdf.get_object_handle(ObjectRef::new(30, 0));
        pdf.resolve(&target).expect("resolve");
        assert_eq!(
            target.as_integer(),
            Some(42),
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
            annot(&mut pdf, 30).get_key(b"/P").as_integer() == Some(999),
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
            annot(&mut pdf, 30).get_key(b"/P").object_ref() == Some(ObjectRef::new(60, 0)),
            "a /P resolving to a non-page object must be left unchanged",
        );
    }

    #[test]
    fn p_resolving_to_orphan_page_left_unchanged() {
        let mut objs = base();
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 60 0 R /Rect [0 0 10 10] >>".into(),
        );
        objs.insert(
            60,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
        let mut pdf = open(&objs);

        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("orphan-page /P");

        assert!(
            annot(&mut pdf, 30).get_key(b"/P").object_ref() == Some(ObjectRef::new(60, 0)),
            "a /P to a page outside the original page tree must be left unchanged",
        );
    }

    #[test]
    fn p_to_nulled_removed_page_still_dropped() {
        // A removed page that a surviving outline / named destination still
        // references is replaced with `null` in place by the earlier null-out
        // pass (crate::job::remap_outline_and_dests) BEFORE this pass runs. The /P then
        // resolves to a null object rather than a /Type /Page dict — but it is
        // still a removed page, and qpdf drops the annotation's /P. Object 4 is
        // `null` and is not in `surviving`, so /P must be dropped (matching qpdf;
        // requiring /Type /Page here would wrongly keep the dangling /P).
        let mut objs = base();
        objs.insert(4, "null".into());
        objs.insert(
            30,
            "<< /Type /Annot /Subtype /Text /P 4 0 R /Rect [0 0 10 10] >>".into(),
        );
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(
            !annot(&mut pdf, 30).has_key(b"/P"),
            "a /P to a removed page nulled by the dest null-out pass must still be dropped",
        );
    }

    #[test]
    fn stream_target_skipped() {
        // An OBJR /Obj can reference a stream (e.g. an XObject). A stream is
        // not a dictionary, so the target is skipped via the `else { continue; }`
        // arm with no stream-body corruption and no error.
        let mut objs = base();
        objs.insert(30, "<< /Length 3 >>\nstream\nabc\nendstream".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("stream target skipped without error");
        let target = pdf.get_object_handle(ObjectRef::new(30, 0));
        pdf.resolve(&target).expect("resolve stream target");
        assert!(
            target.as_stream_dict().is_some(),
            "a stream OBJR /Obj target must be left unchanged"
        );
    }
}
