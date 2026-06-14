//! Structure-tree `/Pg` reference drop after page extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree for a subset extraction, this module updates the structure tree
//! (catalog `/StructTreeRoot`, ISO 32000-2 §14.7) to match qpdf's `--pages`
//! behaviour for structure elements:
//!
//! - A structure element whose `/Pg` points at a **surviving** page keeps the
//!   entry, remapped to the page's new [`ObjectRef`] when the rebuild changed it.
//! - A structure element whose `/Pg` points at a **removed** page has the
//!   `/Pg` key **dropped**. A removed page referenced by nothing else is then
//!   garbage-collected by the subsequent subset sweep
//!   ([`crate::subset_prune`]) and is absent from the output.
//!
//! This is the structural-reference *drop* family: the opposite of the
//! outline/named-destination/annotation handling
//! ([`crate::outline_dest_remap`]), where qpdf keeps the reference verbatim
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

use crate::page_tree_rebuild::RebuildResult;
use crate::ref_chain::terminal_ref_of_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
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

/// Drop dangling structure-element `/Pg` references after a page-tree rebuild
/// (qpdf `--pages` parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::page_tree_rebuild::rebuild_page_tree`]. Its `ref_map` encodes the
/// old → new page reference mapping: a page absent from the map was removed; a
/// page present maps to `ref_map[old][0]` (first new occurrence).
///
/// Walks the structure tree from the catalog `/StructTreeRoot` through `/K`.
/// Each structure element's `/Pg` is remapped when its target page survived
/// and removed when its target page was dropped, so that a removed page
/// referenced by nothing else is garbage-collected by the subsequent subset
/// sweep ([`crate::subset_prune::prune_after_subset`]). The function mutates
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
    let mut state = WalkState::default();

    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(Vec::new()), // No catalog, nothing to do.
    };
    let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
    let Some(catalog) = catalog_obj.as_dict() else {
        return Ok(state.objr_obj_targets);
    };

    match catalog.get("StructTreeRoot").cloned() {
        // Usual form: /StructTreeRoot is an indirect dictionary. The root
        // itself carries no /Pg; only its /K kids are walked.
        Some(Object::Reference(root_ref)) => {
            // Pre-mark the root so a malformed /K back-edge to the root object
            // is not re-walked as if it were a structure element.
            state.visited.insert(root_ref);
            let k = {
                let root_obj = pdf.resolve_borrowed(root_ref)?;
                let Some(root) = root_obj.as_dict() else {
                    return Ok(state.objr_obj_targets);
                };
                root.get("K").cloned()
            };
            if let Some(k) = k {
                let (new_k, changed) = walk_kids(pdf, k, &surviving, 0, max_depth, &mut state)?;
                if changed {
                    let root_obj = pdf.resolve_borrowed(root_ref)?;
                    if let Some(root) = root_obj.as_dict() {
                        let mut root = root.clone();
                        root.insert("K", new_k);
                        pdf.set_object(root_ref, Object::Dictionary(root));
                    }
                }
            }
        }
        // Degenerate form: /StructTreeRoot held as a direct dictionary on the
        // catalog. The rebuilt /K is written back through the catalog.
        Some(Object::Dictionary(mut root)) => {
            if let Some(k) = root.remove("K") {
                let (new_k, changed) = walk_kids(pdf, k, &surviving, 0, max_depth, &mut state)?;
                root.insert("K", new_k);
                if changed {
                    let cat_obj = pdf.resolve_borrowed(catalog_ref)?;
                    if let Some(cat) = cat_obj.as_dict() {
                        let mut cat = cat.clone();
                        cat.insert("StructTreeRoot", Object::Dictionary(root));
                        pdf.set_object(catalog_ref, Object::Dictionary(cat));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(state.objr_obj_targets)
}

/// Walk a `/K` value (single kid, kid reference, or array of kids), processing
/// every structure element reachable from it.
///
/// Takes the value by move and returns `(value, changed)`: indirect kids are
/// rewritten in place via [`Pdf::set_object`] (and report `changed = false` to
/// the caller, whose holder needs no rewrite), while direct-dictionary kids are
/// rebuilt into the returned value with `changed = true` so the caller writes
/// its holder back.
fn walk_kids<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    k: Object,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    depth: usize,
    max_depth: usize,
    state: &mut WalkState,
) -> Result<(Object, bool)> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "structure tree depth exceeds maximum of {max_depth}"
        )));
    }
    match k {
        Object::Reference(r) => {
            walk_kid_ref(pdf, r, surviving, depth, max_depth, state)?;
            Ok((Object::Reference(r), false))
        }
        Object::Dictionary(dict) => {
            let (dict, changed) = process_elem_dict(pdf, dict, surviving, depth, max_depth, state)?;
            Ok((Object::Dictionary(dict), changed))
        }
        Object::Array(items) => {
            let mut changed = false;
            let mut new_items = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Object::Reference(r) => {
                        walk_kid_ref(pdf, r, surviving, depth, max_depth, state)?;
                        new_items.push(Object::Reference(r));
                    }
                    Object::Dictionary(d) => {
                        let (new_dict, dict_changed) =
                            process_elem_dict(pdf, d, surviving, depth, max_depth, state)?;
                        new_items.push(Object::Dictionary(new_dict));
                        changed |= dict_changed;
                    }
                    // Integer kids are marked-content identifiers (MCIDs);
                    // anything else is malformed — both are left unchanged.
                    other => new_items.push(other),
                }
            }
            Ok((Object::Array(new_items), changed))
        }
        // An integer kid is an MCID; any other type is malformed. Unchanged.
        other => Ok((other, false)),
    }
}

/// Process an indirect kid: a structure element dictionary, or an indirect
/// array of kids. Rewrites the object in place when its content changed.
///
/// `state.visited` deduplicates shared kids (an element reachable through more
/// than one parent, or a cycle in a malformed tree): a second visit would
/// re-resolve an already-remapped `/Pg` — now pointing at a *new* ref that is
/// not a key of `surviving` — and misclassify it as a removed target, dropping
/// a surviving page's entry.
fn walk_kid_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    r: ObjectRef,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    depth: usize,
    max_depth: usize,
    state: &mut WalkState,
) -> Result<()> {
    if !state.visited.insert(r) {
        return Ok(());
    }
    match pdf.resolve(r)? {
        Object::Dictionary(dict) => {
            let (dict, changed) = process_elem_dict(pdf, dict, surviving, depth, max_depth, state)?;
            if changed {
                pdf.set_object(r, Object::Dictionary(dict));
            }
        }
        Object::Array(items) => {
            let (new_k, changed) = walk_kids(
                pdf,
                Object::Array(items),
                surviving,
                depth,
                max_depth,
                state,
            )?;
            if changed {
                pdf.set_object(r, new_k);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Remap-or-drop the `/Pg` of one structure element dictionary, then recurse
/// into its `/K` kids. Returns the (possibly rewritten) dictionary and whether
/// it changed.
///
/// The `/Pg` remap-or-drop applies uniformly to structure elements,
/// marked-content references (`/Type /MCR`) and object references
/// (`/Type /OBJR`): qpdf 11.9.0 drops a dangling `/Pg` on any of them. Only
/// true structure elements carry struct-tree `/K` kids, so the `/K` recursion
/// is restricted to non-MCR/OBJR dictionaries (an MCR's `/Stm`/`/StmOwn` and
/// an OBJR's `/Obj` are not structure kids and must not be walked).
fn process_elem_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut dict: Dictionary,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    depth: usize,
    max_depth: usize,
    state: &mut WalkState,
) -> Result<(Dictionary, bool)> {
    let mut changed = false;

    // /Pg is by spec an indirect reference to a page object; any other form is
    // malformed and left unchanged. A surviving target is remapped to its new
    // ref; a removed target has the key dropped so the page is garbage-collected.
    if let Some(Object::Reference(pg)) = dict.get("Pg") {
        match surviving.get(pg) {
            Some(&new) => {
                if new != *pg {
                    dict.insert("Pg", Object::Reference(new));
                    changed = true;
                }
            }
            None => {
                dict.remove("Pg");
                changed = true;
            }
        }
    }

    // Collect an object-reference (/Type /OBJR) kid's /Obj target. The object
    // reached through /Obj (an annotation) survives the prune via this
    // reference; a separate pass (objr_obj_annot_p) drops its dangling /P
    // back-reference to a removed page. /Obj is by spec an indirect reference;
    // normalize a reference chain to its terminal ref. A non-reference /Obj is
    // malformed and ignored. Only OBJR dicts carry /Obj, so no /Type check is
    // needed.
    if let Some(Object::Reference(obj)) = dict.get("Obj") {
        let terminal = terminal_ref_of_chain(pdf, *obj)?;
        state.objr_obj_targets.push(terminal);
    }

    // Recurse only into a real structure element's /K kids. Classifying a dict
    // as MCR/OBJR resolves its /Type (possibly I/O-bound), so defer that check
    // until a /K is actually present to walk: a /K-less dictionary — which every
    // MCR/OBJR is — has nothing to recurse into regardless.
    if let Some(k) = dict.remove("K") {
        if is_mcr_or_objr(pdf, &dict)? {
            // Not a structure element: keep /K verbatim, do not walk it.
            dict.insert("K", k);
        } else {
            let (new_k, k_changed) = walk_kids(pdf, k, surviving, depth + 1, max_depth, state)?;
            dict.insert("K", new_k);
            changed |= k_changed;
        }
    }

    Ok((dict, changed))
}

/// Whether `dict` is a marked-content reference (`/Type /MCR`) or object
/// reference (`/Type /OBJR`) dictionary. `/Type` may itself be stored as an
/// indirect reference, so it is resolved before matching.
fn is_mcr_or_objr<R: Read + Seek>(pdf: &mut Pdf<R>, dict: &Dictionary) -> Result<bool> {
    match dict.get("Type") {
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(*r)? {
            Object::Name(n) => Ok(n == b"MCR" || n == b"OBJR"),
            _ => Ok(false),
        },
        Some(Object::Name(n)) => Ok(n == b"MCR" || n == b"OBJR"),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
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
        }
    }

    fn elem_dict(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> Dictionary {
        match pdf.resolve(ObjectRef::new(num, 0)).expect("resolve elem") {
            Object::Dictionary(d) => d,
            other => panic!("object {num} is not a dictionary: {other:?}"),
        }
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
        };

        drop_struct_elem_dangling_pg(&mut pdf, &result).expect("pg drop");
        let elem = elem_dict(&mut pdf, 20);
        assert!(
            matches!(elem.get("Pg"), Some(Object::Reference(r)) if r.number == 7),
            "surviving /Pg must be remapped to the new ref, got {:?}",
            elem.get("Pg")
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
        let kids = elem.get("K").and_then(|k| k.as_array()).expect("kids");
        let mcr = kids[0].as_dict().expect("inline MCR");
        let mcr_pg = mcr.get("Pg");
        assert!(
            mcr_pg.is_none(),
            "MCR dangling /Pg must be dropped, got {mcr_pg:?}"
        );
        let objr = elem_dict(&mut pdf, 21);
        let objr_pg = objr.get("Pg");
        assert!(
            objr_pg.is_none(),
            "OBJR dangling /Pg must be dropped, got {objr_pg:?}"
        );
        let objr_obj = objr.get("Obj");
        assert!(
            matches!(objr_obj, Some(Object::Reference(r)) if r.number == 5),
            "OBJR /Obj must be kept, got {objr_obj:?}"
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
        };

        drop_struct_elem_dangling_pg(&mut pdf, &result).expect("pg remap");

        let elem = elem_dict(&mut pdf, 20);
        let kids = elem.get("K").and_then(|k| k.as_array()).expect("kids");
        let mcr = kids[0].as_dict().expect("inline MCR");
        let mcr_pg = mcr.get("Pg");
        assert!(
            matches!(mcr_pg, Some(Object::Reference(r)) if r.number == 7),
            "MCR surviving /Pg must be remapped to the new ref, got {mcr_pg:?}"
        );
        let objr = elem_dict(&mut pdf, 21);
        let objr_pg = objr.get("Pg");
        assert!(
            matches!(objr_pg, Some(Object::Reference(r)) if r.number == 7),
            "OBJR surviving /Pg must be remapped to the new ref, got {objr_pg:?}"
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
        let kid = root.get("K").and_then(|k| k.as_dict()).expect("direct kid");
        assert!(
            kid.get("Pg").is_none(),
            "direct-dict kid's dangling /Pg must be dropped and written back, got {:?}",
            kid.get("Pg")
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
            elem.get("Pg").is_none(),
            "kid reached through an indirect /K array must have /Pg dropped, got {:?}",
            elem.get("Pg")
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
        assert!(
            elem.get("Pg").is_none(),
            "dangling /Pg dropped despite cycle"
        );
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
        let root = catalog
            .get("StructTreeRoot")
            .and_then(|r| r.as_dict())
            .expect("direct root");
        let kid = root.get("K").and_then(|k| k.as_dict()).expect("direct kid");
        assert!(
            kid.get("Pg").is_none(),
            "dangling /Pg under a catalog-direct /StructTreeRoot must be dropped, got {:?}",
            kid.get("Pg")
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
        let typeless_pg = typeless.get("Pg");
        assert!(
            typeless_pg.is_none(),
            "typeless StructElem must still have its dangling /Pg dropped, got {typeless_pg:?}"
        );
        let mcr = elem_dict(&mut pdf, 22);
        let mcr_pg = mcr.get("Pg");
        assert!(
            mcr_pg.is_none(),
            "MCR (indirect /Type) dangling /Pg must be dropped, got {mcr_pg:?}"
        );
        let unwalked_kid = elem_dict(&mut pdf, 23);
        let unwalked_pg = unwalked_kid.get("Pg");
        assert!(
            matches!(unwalked_pg, Some(Object::Reference(r)) if r.number == 4),
            "kid under an MCR (indirect /Type) must not be walked, so its /Pg stays, got {unwalked_pg:?}"
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

        let arr = match pdf.resolve(ObjectRef::new(25, 0)).expect("array") {
            Object::Array(items) => items,
            other => panic!("object 25 is not an array: {other:?}"),
        };
        let kid = arr[0].as_dict().expect("direct kid");
        assert!(
            kid.get("Pg").is_none(),
            "direct-dict kid in an indirect /K array must have /Pg dropped, got {:?}",
            kid.get("Pg")
        );
        assert!(
            matches!(&arr[1], Object::String(s) if s == b"noise"),
            "non-kid array entry must round-trip unchanged, got {:?}",
            arr[1]
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
