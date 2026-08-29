//! qpdf correspondence: QPDFJob.cc removed-page nulling plus QPDFWriter.cc null-key visibility specialized for structure elements.
//! Structure-tree `/Pg` reference drop after page extraction.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree for a subset extraction, this module updates the structure tree
//! (catalog `/StructTreeRoot`, ISO 32000-2 §14.7) to match qpdf's `--pages`
//! behaviour for structure elements:
//!
//! - A structure element whose `/Pg` points at a **surviving** page keeps the
//!   entry, remapped to the page's new [`ObjectRef`] when the rebuild changed it.
//! - A structure element whose `/Pg` points at a **removed** page has the
//!   `/Pg` key **dropped**. A removed page referenced by nothing else is then
//!   garbage-collected by the subsequent subset sweep
//!   (the subsequent job subset sweep) and is absent from the output.
//!
//! This is the structural-reference *drop* family: the opposite of the
//! outline/named-destination/annotation handling
//! ([`crate::job::remap_outline_and_dests`]), where qpdf keeps the reference verbatim
//! and replaces the removed page object with `null`.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! For `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf` over a document whose page 2
//! is referenced only by a structure element's `/Pg`, qpdf drops that `/Pg`
//! entry and the removed page is absent from the output (not emitted as
//! `null`). The structure element itself — and the rest of the structure tree
//! — is otherwise left unchanged. The same drop applies to a `/Pg` carried by a
//! marked-content reference (`/Type /MCR`) or object reference (`/Type /OBJR`)
//! kid: qpdf drops the dangling `/Pg` key (keeping an OBJR's `/Obj`), and the
//! now-unreferenced page is garbage-collected.
//!
//! # Scope
//!
//! The `/Pg` entry of structure elements and of their marked-content reference
//! (`/Type /MCR`) and object reference (`/Type /OBJR`) kids is handled. The
//! number tree under `/ParentTree` carries no page references — its values are
//! structure-element references (the structure parent tree, ISO 32000-2, 14.7),
//! which survive via `/K` and are remapped by the writer — so it needs no
//! handling here.

use crate::object_handle::ObjectHandle;
use crate::pages::tree_rebuild::RebuildResult;
use crate::{Error, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Maximum structure-tree nesting depth accepted by
/// [`drop_struct_elem_dangling_pg`] before the walk fails.
///
/// Bounds recursion over `/K` so a malformed or adversarial document cannot
/// overflow the stack.
pub const DEFAULT_MAX_STRUCT_TREE_DEPTH: usize = 100;

/// Mutable accumulator threaded through the structure-tree walk.
///
/// `visited` deduplicates shared/cyclic kids; `objr_obj_targets` collects the
/// `/Obj` reference of every object-reference (`/Type /OBJR`) kid for the
/// follow-on annotation `/P` drop pass ([`crate::objr_obj_annot_p`]).
#[derive(Default)]
struct WalkState {
    visited: BTreeSet<ObjectRef>,
    objr_obj_targets: Vec<ObjectRef>,
}

fn child_if_present(parent: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    if parent.try_has_key(key)? {
        Ok(Some(parent.try_get_key(key)?))
    } else {
        Ok(None)
    }
}

/// Return a dictionary entry without applying qpdf's null-valued key
/// visibility filter. The page-reference identity must be inspected before a
/// removed page is replaced with a null value.
fn raw_child(parent: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    Ok(parent
        .try_as_dictionary()?
        .and_then(|entries| entries.get(key).cloned()))
}

/// Drop dangling structure-element `/Pg` references after a page-tree rebuild
/// (qpdf `--pages` parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::pages::tree_rebuild::rebuild_page_tree`]. Its `ref_map` encodes the
/// old → new page reference mapping: a page absent from the map was removed; a
/// page present maps to `ref_map[old][0]` (first new occurrence).
///
/// Walks the structure tree from the catalog `/StructTreeRoot` through `/K`.
/// Each structure element's `/Pg` is remapped when its target page survived
/// and removed when its target page was dropped, so that a removed page
/// referenced by nothing else is garbage-collected by the subsequent subset
/// sweep ([`crate::job::QPDFJob::prune_after_subset`]). The function mutates
/// `pdf` in place (same convention as `rebuild_page_tree`) and succeeds
/// silently when the document has no `/StructTreeRoot`.
///
/// The same remap-or-drop is applied to a `/Pg` carried by a marked-content
/// reference (`/Type /MCR`) or object reference (`/Type /OBJR`) kid; an OBJR's
/// `/Obj` and an MCR's other entries are left unchanged.
///
/// Returns the OBJR `/Obj` target refs gathered during the same walk, for the
/// [`crate::objr_obj_annot_p`] `/P` drop pass (the object reached through an
/// OBJR `/Obj` survives the prune via that reference, so its dangling `/P` is
/// dropped separately).
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`Error::Unsupported`] when the structure-tree depth limit
///   ([`DEFAULT_MAX_STRUCT_TREE_DEPTH`]) is exceeded.
pub fn drop_struct_elem_dangling_pg<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<Vec<ObjectRef>> {
    drop_struct_elem_dangling_pg_with_max_depth(pdf, result, DEFAULT_MAX_STRUCT_TREE_DEPTH)
}

/// Like [`drop_struct_elem_dangling_pg`] but with a caller-supplied depth limit.
///
/// Returns the OBJR `/Obj` target refs gathered during the same walk (see
/// [`drop_struct_elem_dangling_pg`] for how they are consumed).
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`Error::Unsupported`] when the structure-tree depth exceeds `max_depth`.
pub fn drop_struct_elem_dangling_pg_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
) -> Result<Vec<ObjectRef>> {
    let surviving: BTreeMap<ObjectRef, ObjectRef> = result
        .ref_map
        .iter()
        .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
        .collect();
    let removed_pages = &result.removed_pages;
    let mut state = WalkState::default();

    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(Vec::new()), // No catalog, nothing to do.
    };
    let catalog = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog)?;
    if catalog.try_as_dictionary()?.is_none() {
        return Ok(state.objr_obj_targets);
    }

    let Some(root) = child_if_present(&catalog, b"/StructTreeRoot")? else {
        return Ok(state.objr_obj_targets);
    };
    if root.try_as_dictionary()?.is_none() {
        return Ok(state.objr_obj_targets);
    }
    if let Some(root_ref) = root.object_ref() {
        // Pre-mark the root so a malformed /K back-edge to the root object is
        // not re-walked as if it were a structure element.
        state.visited.insert(root_ref);
    }
    if let Some(k) = child_if_present(&root, b"/K")? {
        walk_kids(pdf, &k, &surviving, removed_pages, 0, max_depth, &mut state)?;
    }
    Ok(state.objr_obj_targets)
}

/// Walk a `/K` value (single kid, kid reference, or array of kids), processing
/// every structure element reachable from it.
///
/// Every indirect handle is resolved once and every dictionary or array child
/// is then processed through the same live handle graph. Direct children remain
/// embedded in their existing parent; no snapshot is rebuilt.
fn walk_kids<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    k: &ObjectHandle,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    removed_pages: &BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
    state: &mut WalkState,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "structure tree depth exceeds maximum of {max_depth}"
        )));
    }
    if let Some(kid_ref) = k.object_ref() {
        if !state.visited.insert(kid_ref) {
            return Ok(());
        }
    }
    if let Some(items) = k.try_as_array()? {
        for item in items {
            walk_kids(
                pdf,
                &item,
                surviving,
                removed_pages,
                depth,
                max_depth,
                state,
            )?;
        }
        return Ok(());
    }
    if k.try_as_dictionary()?.is_some() {
        process_elem_dict(pdf, k, surviving, removed_pages, depth, max_depth, state)?;
    }
    Ok(())
}

/// Remap-or-drop the `/Pg` of one structure element dictionary, then recurse
/// into its `/K` kids. Mutations are applied directly to the live handle.
///
/// The `/Pg` remap-or-drop applies uniformly to structure elements,
/// marked-content references (`/Type /MCR`) and object references
/// (`/Type /OBJR`): qpdf 11.9.0 drops a dangling `/Pg` on any of them. Only
/// true structure elements carry struct-tree `/K` kids, so the `/K` recursion
/// is restricted to non-MCR/OBJR dictionaries (an MCR's `/Stm`/`/StmOwn` and
/// an OBJR's `/Obj` are not structure kids and must not be walked).
fn process_elem_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    removed_pages: &BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
    state: &mut WalkState,
) -> Result<()> {
    let mut changed = false;

    // /Pg is by spec an indirect reference to a page object; any other form is
    // malformed and left unchanged. A surviving target is remapped to its new
    // ref; only an original page-tree leaf in removed_pages is dropped.
    if let Some(pg) = raw_child(dict, b"/Pg")? {
        if let Some(pg_ref) = pg.object_ref() {
            match surviving.get(&pg_ref) {
                Some(&new) if new != pg_ref => {
                    dict.replace_key(b"/Pg", pdf.get_object_handle(new))?;
                    changed = true;
                }
                None if removed_pages.contains(&pg_ref) => {
                    dict.remove_key(b"/Pg");
                    changed = true;
                }
                _ => {}
            }
        }
    }

    // Collect an object-reference (/Type /OBJR) kid's /Obj target. The object
    // reached through /Obj (an annotation) survives the prune via this
    // reference; a separate pass (objr_obj_annot_p) drops its dangling /P
    // back-reference to a removed page. /Obj is by spec an indirect reference;
    // keep the object identity from the live handle. A direct /Obj is malformed
    // and ignored. Collection is gated on /Type /OBJR specifically so a
    // private/extension /Obj key on any other dictionary (a plain structure
    // element, or even an /Type /MCR) is not pulled into the OBJR-only /P-drop
    // scope.
    if let Some(obj) = raw_child(dict, b"/Obj")? {
        if is_objr(dict)? {
            if let Some(obj_ref) = obj.object_ref() {
                state.objr_obj_targets.push(obj_ref);
            }
        }
    }

    // Recurse only into a real structure element's /K kids. Classifying a dict
    // as MCR/OBJR resolves its /Type (possibly I/O-bound), so defer that check
    // until a /K is actually present to walk: a /K-less dictionary — which every
    // MCR/OBJR is — has nothing to recurse into regardless.
    if let Some(k) = raw_child(dict, b"/K")? {
        if !is_mcr_or_objr(dict)? {
            walk_kids(
                pdf,
                &k,
                surviving,
                removed_pages,
                depth + 1,
                max_depth,
                state,
            )?;
        }
    }

    if changed {
        pdf.mark_object_handle_dirty(dict)?;
    }
    Ok(())
}

/// Whether `dict` is a marked-content reference (`/Type /MCR`) or object
/// reference (`/Type /OBJR`) dictionary. `/Type` may itself be stored as an
/// indirect reference, so it is resolved before matching.
fn is_mcr_or_objr(dict: &ObjectHandle) -> Result<bool> {
    let Some(type_handle) = raw_child(dict, b"/Type")? else {
        return Ok(false);
    };
    Ok(matches!(
        type_handle.try_as_name()?.as_deref(),
        Some(b"MCR" | b"OBJR")
    ))
}

/// Whether `dict`'s `/Type` resolves to `/OBJR`. `/Type` may be stored as an
/// indirect reference, so it is resolved before matching. Gates `/Obj` target
/// collection to true object-reference kids (only OBJR carries `/Obj`), keeping
/// the follow-on `/P`-drop pass within its OBJR-only scope.
fn is_objr(dict: &ObjectHandle) -> Result<bool> {
    let Some(type_handle) = raw_child(dict, b"/Type")? else {
        return Ok(false);
    };
    Ok(matches!(
        type_handle.try_as_name()?.as_deref(),
        Some(b"OBJR")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectHandle, Pdf};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Test PDF builder
    // -----------------------------------------------------------------------

    /// Serialize `objs` (object number → body) into a classic-xref PDF with
    /// `/Root 1 0 R`.
    fn build_pdf(objs: &BTreeMap<u32, String>) -> Vec<u8> {
        build_pdf_inner(objs, true)
    }

    /// Like [`build_pdf`] but writes a trailer with no `/Root` when `with_root`
    /// is false (so `root_ref()` is `None`).
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

    /// Base skeleton: catalog (1), pages root (2), three pages (3, 4, 5) and a
    /// `/StructTreeRoot` (10) whose `/K` points at StructElem 20.
    fn base_objs() -> BTreeMap<u32, String> {
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
            removed_pages: [ObjectRef::new(4, 0)].into_iter().collect(),
        }
    }

    fn elem_dict(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> ObjectHandle {
        let elem = pdf.get_object_handle(ObjectRef::new(num, 0));
        pdf.resolve(&elem).expect("resolve elem");
        assert!(
            elem.as_dictionary().is_some(),
            "object {num} is not a dictionary"
        );
        elem
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn surviving_pg_remapped_to_new_ref() {
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /P /P 10 0 R /Pg 3 0 R >>".into(),
        );
        let mut pdf = open(&objs);

        // Page 3 survives but under a new ref (7 0 R), as a duplicate-page
        // selection can produce.
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(7, 0)],
            ref_map,
            ..Default::default()
        };

        drop_struct_elem_dangling_pg(&mut pdf, &result).expect("pg drop");
        let elem = elem_dict(&mut pdf, 20);
        assert!(
            elem.get_key(b"/Pg").object_ref() == Some(ObjectRef::new(7, 0)),
            "surviving /Pg must be remapped to the new ref"
        );
    }

    #[test]
    fn mcr_and_objr_dangling_pg_dropped() {
        // StructElem 20 has an inline MCR kid and an indirect OBJR kid (21),
        // both with /Pg pointing at the removed page 4. qpdf 11.9.0 drops a
        // dangling /Pg on MCR/OBJR kids too (so the page is garbage-collected);
        // an OBJR's /Obj is kept.
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /P /P 10 0 R \
             /K [ << /Type /MCR /Pg 4 0 R /MCID 0 >> 21 0 R ] >>"
                .into(),
        );
        objs.insert(21, "<< /Type /OBJR /Pg 4 0 R /Obj 5 0 R >>".into());
        let mut pdf = open(&objs);

        let targets = drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");
        assert!(
            targets.contains(&ObjectRef::new(5, 0)),
            "OBJR /Obj target (object 5) must be collected, got {targets:?}"
        );

        let elem = elem_dict(&mut pdf, 20);
        let kids = elem.get_key(b"/K").as_array().expect("kids");
        let mcr = kids[0].clone();
        assert!(
            mcr.as_dictionary().is_some(),
            "inline MCR must be a dictionary"
        );
        assert!(!mcr.has_key(b"/Pg"), "MCR dangling /Pg must be dropped");
        let objr = elem_dict(&mut pdf, 21);
        assert!(!objr.has_key(b"/Pg"), "OBJR dangling /Pg must be dropped");
        assert!(
            objr.get_key(b"/Obj").object_ref() == Some(ObjectRef::new(5, 0)),
            "OBJR /Obj must be kept"
        );
    }

    #[test]
    fn non_objr_obj_key_not_collected() {
        // A non-OBJR structure dictionary carrying a private/extension /Obj key
        // must NOT contribute an /Obj target: collection is gated on /Type /OBJR
        // so it stays within the OBJR-only /P-drop scope. Object 20 is a
        // /Type /StructElem with an /Obj key; object 5 must not be collected.
        let mut objs = base_objs();
        objs.insert(20, "<< /Type /StructElem /S /P /Obj 5 0 R >>".into());
        let mut pdf = open(&objs);

        let targets = drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("ok");
        assert!(
            !targets.contains(&ObjectRef::new(5, 0)),
            "a non-OBJR /Obj key must not be collected, got {targets:?}"
        );
    }

    #[test]
    fn objr_with_indirect_type_obj_collected() {
        // An OBJR whose /Type is itself an indirect reference is still recognized
        // by the is_objr gate (which resolves /Type), so its /Obj is collected.
        let mut objs = base_objs();
        objs.insert(20, "<< /Type /StructElem /S /Document /K 21 0 R >>".into());
        objs.insert(21, "<< /Type 22 0 R /Obj 5 0 R >>".into());
        objs.insert(22, "/OBJR".into());
        let mut pdf = open(&objs);

        let targets = drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("ok");
        assert!(
            targets.contains(&ObjectRef::new(5, 0)),
            "an OBJR with an indirect /Type must collect its /Obj, got {targets:?}"
        );
    }

    #[test]
    fn typeless_obj_key_not_collected() {
        // A dictionary carrying an /Obj key but no /Type is not an OBJR, so its
        // /Obj target is not collected (exercises the is_objr no-/Type arm).
        let mut objs = base_objs();
        objs.insert(20, "<< /S /P /Obj 5 0 R /K 21 0 R >>".into());
        objs.insert(21, "7".into());
        let mut pdf = open(&objs);

        let targets = drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("ok");
        assert!(
            !targets.contains(&ObjectRef::new(5, 0)),
            "a /Obj key on a dictionary with no /Type must not be collected, got {targets:?}"
        );
    }

    #[test]
    fn mcr_and_objr_surviving_pg_remapped() {
        // The remap branch for MCR/OBJR kids: a /Pg pointing at a page that
        // survives under a new ref is remapped, not dropped.
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /P /P 10 0 R \
             /K [ << /Type /MCR /Pg 3 0 R /MCID 0 >> 21 0 R ] >>"
                .into(),
        );
        objs.insert(21, "<< /Type /OBJR /Pg 3 0 R /Obj 5 0 R >>".into());
        let mut pdf = open(&objs);

        // Page 3 survives under a new ref (7 0 R); page 4 removed.
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(5, 0)]);
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(7, 0), ObjectRef::new(5, 0)],
            ref_map,
            ..Default::default()
        };

        drop_struct_elem_dangling_pg(&mut pdf, &result).expect("pg remap");

        let elem = elem_dict(&mut pdf, 20);
        let kids = elem.get_key(b"/K").as_array().expect("kids");
        let mcr = kids[0].clone();
        assert!(
            mcr.as_dictionary().is_some(),
            "inline MCR must be a dictionary"
        );
        assert!(
            mcr.get_key(b"/Pg").object_ref() == Some(ObjectRef::new(7, 0)),
            "MCR surviving /Pg must be remapped to the new ref"
        );
        let objr = elem_dict(&mut pdf, 21);
        assert!(
            objr.get_key(b"/Pg").object_ref() == Some(ObjectRef::new(7, 0)),
            "OBJR surviving /Pg must be remapped to the new ref"
        );
    }

    #[test]
    fn pg_resolving_to_non_page_left_unchanged() {
        let mut objs = base_objs();
        objs.insert(20, "<< /Type /StructElem /S /P /Pg 30 0 R >>".into());
        objs.insert(30, "<< /Type /Whatever >>".into());
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("non-page /Pg");

        let elem = elem_dict(&mut pdf, 20);
        assert_eq!(
            elem.get_key(b"/Pg").object_ref(),
            Some(ObjectRef::new(30, 0))
        );
    }

    #[test]
    fn pg_resolving_to_orphan_page_left_unchanged() {
        let mut objs = base_objs();
        objs.insert(20, "<< /Type /StructElem /S /P /Pg 30 0 R >>".into());
        objs.insert(
            30,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("orphan-page /Pg");

        let elem = elem_dict(&mut pdf, 20);
        assert_eq!(
            elem.get_key(b"/Pg").object_ref(),
            Some(ObjectRef::new(30, 0))
        );
    }

    #[test]
    fn no_struct_tree_root_is_a_noop() {
        let mut objs = base_objs();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        objs.remove(&10);
        let mut pdf = open(&objs);
        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("noop");
    }

    #[test]
    fn direct_dict_kid_change_written_back_to_root() {
        // /StructTreeRoot /K holds a *direct* StructElem dict whose /Pg points
        // at the removed page: the drop must be persisted through the root.
        let mut objs = base_objs();
        objs.insert(
            10,
            "<< /Type /StructTreeRoot \
             /K << /Type /StructElem /S /P /Pg 4 0 R >> >>"
                .into(),
        );
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");

        let root = elem_dict(&mut pdf, 10);
        let kid = root.get_key(b"/K");
        assert!(
            kid.as_dictionary().is_some(),
            "direct kid must be a dictionary"
        );
        assert!(
            !kid.has_key(b"/Pg"),
            "direct-dict kid's dangling /Pg must be dropped and written back"
        );
    }

    #[test]
    fn indirect_kid_array_processed() {
        // /K is an indirect reference to an *array* of kid refs.
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /Document /P 10 0 R /K 25 0 R >>".into(),
        );
        objs.insert(25, "[ 21 0 R ]".into());
        objs.insert(
            21,
            "<< /Type /StructElem /S /P /P 20 0 R /Pg 4 0 R >>".into(),
        );
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");

        let elem = elem_dict(&mut pdf, 21);
        assert!(
            !elem.has_key(b"/Pg"),
            "kid reached through an indirect /K array must have /Pg dropped"
        );
    }

    #[test]
    fn kid_cycle_terminates() {
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /P /P 10 0 R /Pg 4 0 R /K 21 0 R >>".into(),
        );
        objs.insert(
            21,
            "<< /Type /StructElem /S /P /P 20 0 R /K 20 0 R >>".into(),
        );
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("cycle must terminate");
        let elem = elem_dict(&mut pdf, 20);
        assert!(!elem.has_key(b"/Pg"), "dangling /Pg dropped despite cycle");
    }

    #[test]
    fn direct_struct_tree_root_on_catalog_written_back() {
        // /StructTreeRoot held as a *direct* dictionary on the catalog: the
        // dangling-/Pg drop in its direct kid must be persisted through the
        // catalog object.
        let mut objs = base_objs();
        objs.insert(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot \
             << /Type /StructTreeRoot /K << /Type /StructElem /S /P /Pg 4 0 R >> >> >>"
                .into(),
        );
        objs.remove(&10);
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");

        let catalog = elem_dict(&mut pdf, 1);
        let root = catalog.get_key(b"/StructTreeRoot");
        assert!(
            root.as_dictionary().is_some(),
            "direct root must be a dictionary"
        );
        let kid = root.get_key(b"/K");
        assert!(
            kid.as_dictionary().is_some(),
            "direct kid must be a dictionary"
        );
        assert!(
            !kid.has_key(b"/Pg"),
            "dangling /Pg under a catalog-direct /StructTreeRoot must be dropped"
        );
    }

    #[test]
    fn non_dict_struct_tree_root_is_a_noop() {
        let mut objs = base_objs();
        objs.insert(10, "42".into());
        let mut pdf = open(&objs);
        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("noop");
    }

    #[test]
    fn non_dict_catalog_is_a_noop() {
        // The /Root points at a non-dictionary object: the walk has no catalog
        // dictionary to read, so it returns early (collecting nothing).
        let mut objs = base_objs();
        objs.insert(1, "42".into());
        let mut pdf = open(&objs);
        let targets = drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5())
            .expect("non-dict catalog is a noop");
        assert!(targets.is_empty());
    }

    #[test]
    fn no_catalog_is_a_noop() {
        // A trailer without /Root: root_ref() is None, so the pass returns an
        // empty target list and makes no changes.
        let pdf_bytes = build_pdf_inner(&base_objs(), false);
        let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open rootless fixture");
        assert!(pdf.root_ref().is_none(), "fixture must have no catalog");
        let targets = drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("noop");
        assert!(
            targets.is_empty(),
            "no catalog => no /Obj targets collected"
        );
    }

    #[test]
    fn typeless_elem_processed_and_indirect_type_resolved() {
        // Elem 21 has no /Type (legal for structure elements): it must still be
        // processed and its dangling /Pg dropped. Elem 22's /Type is an
        // *indirect reference* to /MCR: the indirect /Type must resolve so the
        // dict is recognized as an MCR. Its own dangling /Pg is still dropped
        // (drop applies to MCR/OBJR kids), but its /K is NOT walked — a
        // (malformed) struct-elem kid 23 under it keeps its /Pg, proving the
        // indirect /Type resolved to MCR and short-circuited the /K recursion.
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [21 0 R 22 0 R] >>".into(),
        );
        objs.insert(21, "<< /S /P /P 20 0 R /Pg 4 0 R >>".into());
        objs.insert(22, "<< /Type 30 0 R /Pg 4 0 R /MCID 0 /K 23 0 R >>".into());
        objs.insert(
            23,
            "<< /Type /StructElem /S /P /P 22 0 R /Pg 4 0 R >>".into(),
        );
        objs.insert(30, "/MCR".into());
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");

        let typeless = elem_dict(&mut pdf, 21);
        assert!(
            !typeless.has_key(b"/Pg"),
            "typeless StructElem must still have its dangling /Pg dropped"
        );
        let mcr = elem_dict(&mut pdf, 22);
        assert!(
            !mcr.has_key(b"/Pg"),
            "MCR (indirect /Type) dangling /Pg must be dropped"
        );
        let unwalked_kid = elem_dict(&mut pdf, 23);
        assert!(
            unwalked_kid.get_key(b"/Pg").object_ref() == Some(ObjectRef::new(4, 0)),
            "kid under an MCR (indirect /Type) must not be walked, so its /Pg stays"
        );
    }

    #[test]
    fn indirect_kid_array_with_direct_dict_kid_written_back() {
        // /K is an indirect reference to an array holding a *direct* StructElem
        // dict (plus non-kid noise entries): the drop must be persisted by
        // rewriting the array object, and the noise must round-trip unchanged.
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /Document /P 10 0 R /K 25 0 R >>".into(),
        );
        objs.insert(
            25,
            "[ << /Type /StructElem /S /P /Pg 4 0 R >> (noise) 26 0 R ]".into(),
        );
        objs.insert(26, "7".into()); // kid ref resolving to a non-dict, non-array
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");

        let arr_handle = pdf.get_object_handle(ObjectRef::new(25, 0));
        pdf.resolve(&arr_handle).expect("array");
        let arr = arr_handle.as_array().expect("object 25 is not an array");
        let kid = arr[0].clone();
        assert!(
            kid.as_dictionary().is_some(),
            "direct kid must be a dictionary"
        );
        assert!(
            !kid.has_key(b"/Pg"),
            "direct-dict kid in an indirect /K array must have /Pg dropped"
        );
        assert!(
            arr[1].as_string() == Some(b"noise".to_vec()),
            "non-kid array entry must round-trip unchanged"
        );
    }

    #[test]
    fn direct_dict_kid_error_propagates_from_array() {
        // A direct dictionary inside a `/K` array can itself contain a nested
        // `/K`; an error from that nested walk must propagate through the
        // direct-dictionary array arm.
        let mut objs = base_objs();
        objs.insert(
            10,
            "<< /Type /StructTreeRoot /K [ << /Type /StructElem /S /P /K 42 >> ] >>".into(),
        );
        let mut pdf = open(&objs);

        let err = drop_struct_elem_dangling_pg_with_max_depth(&mut pdf, &keep_3_and_5(), 1)
            .expect_err("nested direct array kid must hit the depth limit");
        assert!(
            matches!(err, Error::Unsupported(_)),
            "over-deep direct array kid must surface Unsupported, got {err:?}"
        );
    }

    #[test]
    fn depth_limit_exceeded_is_unsupported() {
        // Direct-dict /K nesting deeper than the limit (no refs, so the
        // visited set cannot bound it — only the depth limit can).
        let mut objs = base_objs();
        let mut nested = "<< /Type /StructElem /S /P /Pg 4 0 R >>".to_string();
        for _ in 0..5 {
            nested = format!("<< /Type /StructElem /S /P /K {nested} >>");
        }
        objs.insert(10, format!("<< /Type /StructTreeRoot /K {nested} >>"));
        let mut pdf = open(&objs);

        let err =
            drop_struct_elem_dangling_pg_with_max_depth(&mut pdf, &keep_3_and_5(), 3).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "over-deep tree must surface Unsupported, got {err:?}"
        );
    }
}
