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
//! — is otherwise left unchanged.
//!
//! # Scope
//!
//! Only the `/Pg` entry of structure elements is handled. `/Pg` entries inside
//! marked-content reference (`/Type /MCR`) and object reference
//! (`/Type /OBJR`) dictionaries are left untouched, as are the page references
//! inside `/ParentTree`.

use crate::page_tree_rebuild::RebuildResult;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Maximum structure-tree nesting depth accepted by
/// [`drop_struct_elem_dangling_pg`] before the walk fails.
///
/// Bounds recursion over `/K` so a malformed or adversarial document cannot
/// overflow the stack.
pub const DEFAULT_MAX_STRUCT_TREE_DEPTH: usize = 100;

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
/// `/Pg` entries inside marked-content reference (`/Type /MCR`) and object
/// reference (`/Type /OBJR`) dictionaries are left untouched.
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`Error::Unsupported`] when the structure-tree depth limit
///   ([`DEFAULT_MAX_STRUCT_TREE_DEPTH`]) is exceeded.
pub fn drop_struct_elem_dangling_pg<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<()> {
    drop_struct_elem_dangling_pg_with_max_depth(pdf, result, DEFAULT_MAX_STRUCT_TREE_DEPTH)
}

/// Like [`drop_struct_elem_dangling_pg`] but with a caller-supplied depth limit.
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`Error::Unsupported`] when the structure-tree depth exceeds `max_depth`.
pub fn drop_struct_elem_dangling_pg_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
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
    let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
    let Some(catalog) = catalog_obj.as_dict() else {
        return Ok(());
    };

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    match catalog.get("StructTreeRoot").cloned() {
        // Usual form: /StructTreeRoot is an indirect dictionary. The root
        // itself carries no /Pg; only its /K kids are walked.
        Some(Object::Reference(root_ref)) => {
            if !visited.insert(root_ref) {
                return Ok(());
            }
            let k = {
                let root_obj = pdf.resolve_borrowed(root_ref)?;
                let Some(root) = root_obj.as_dict() else {
                    return Ok(());
                };
                root.get("K").cloned()
            };
            if let Some(k) = k {
                let (new_k, changed) = walk_kids(pdf, k, &surviving, 0, max_depth, &mut visited)?;
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
                let (new_k, changed) = walk_kids(pdf, k, &surviving, 0, max_depth, &mut visited)?;
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
    Ok(())
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
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<(Object, bool)> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "structure tree depth exceeds maximum of {max_depth}"
        )));
    }
    match k {
        Object::Reference(r) => {
            walk_kid_ref(pdf, r, surviving, depth, max_depth, visited)?;
            Ok((Object::Reference(r), false))
        }
        Object::Dictionary(dict) => {
            let (dict, changed) =
                process_elem_dict(pdf, dict, surviving, depth, max_depth, visited)?;
            Ok((Object::Dictionary(dict), changed))
        }
        Object::Array(mut items) => {
            let mut changed = false;
            for item in items.iter_mut() {
                match item {
                    Object::Reference(r) => {
                        walk_kid_ref(pdf, *r, surviving, depth, max_depth, visited)?;
                    }
                    Object::Dictionary(d) => {
                        let owned = std::mem::take(d);
                        let (new_dict, dict_changed) =
                            process_elem_dict(pdf, owned, surviving, depth, max_depth, visited)?;
                        *d = new_dict;
                        changed |= dict_changed;
                    }
                    // Integer kids are marked-content identifiers (MCIDs);
                    // anything else is malformed — both are left unchanged.
                    _ => {}
                }
            }
            Ok((Object::Array(items), changed))
        }
        // An integer kid is an MCID; any other type is malformed. Unchanged.
        other => Ok((other, false)),
    }
}

/// Process an indirect kid: a structure element dictionary, or an indirect
/// array of kids. Rewrites the object in place when its content changed.
///
/// `visited` deduplicates shared kids (an element reachable through more than
/// one parent, or a cycle in a malformed tree): a second visit would re-resolve
/// an already-remapped `/Pg` — now pointing at a *new* ref that is not a key of
/// `surviving` — and misclassify it as a removed target, dropping a surviving
/// page's entry.
fn walk_kid_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    r: ObjectRef,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    depth: usize,
    max_depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if !visited.insert(r) {
        return Ok(());
    }
    match pdf.resolve(r)? {
        Object::Dictionary(dict) => {
            let (dict, changed) =
                process_elem_dict(pdf, dict, surviving, depth, max_depth, visited)?;
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
                visited,
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
/// Marked-content reference (`/Type /MCR`) and object reference
/// (`/Type /OBJR`) dictionaries are not structure elements; they are returned
/// unchanged and their kids (they have none) are not walked.
fn process_elem_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut dict: Dictionary,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    depth: usize,
    max_depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<(Dictionary, bool)> {
    if is_mcr_or_objr(pdf, &dict)? {
        return Ok((dict, false));
    }

    let mut changed = false;

    // /Pg is by spec an indirect reference to a page object; any other form is
    // malformed and left unchanged.
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

    if let Some(k) = dict.remove("K") {
        let (new_k, k_changed) = walk_kids(pdf, k, surviving, depth + 1, max_depth, visited)?;
        dict.insert("K", new_k);
        changed |= k_changed;
    }

    Ok((dict, changed))
}

/// Whether `dict` is a marked-content reference (`/Type /MCR`) or object
/// reference (`/Type /OBJR`) dictionary. `/Type` may itself be stored as an
/// indirect reference, so it is resolved before matching.
fn is_mcr_or_objr<R: Read + Seek>(pdf: &mut Pdf<R>, dict: &Dictionary) -> Result<bool> {
    let name = match dict.get("Type") {
        Some(Object::Reference(r)) => match pdf.resolve(*r)? {
            Object::Name(n) => n,
            _ => return Ok(false),
        },
        Some(Object::Name(n)) => n.clone(),
        _ => return Ok(false),
    };
    Ok(name == b"MCR" || name == b"OBJR")
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
    fn mcr_and_objr_pg_left_untouched() {
        // StructElem 20 has an inline MCR kid and an indirect OBJR kid (21),
        // both with /Pg pointing at the removed page 4. Their /Pg is out of
        // scope and must be left untouched.
        let mut objs = base_objs();
        objs.insert(
            20,
            "<< /Type /StructElem /S /P /P 10 0 R \
             /K [ << /Type /MCR /Pg 4 0 R /MCID 0 >> 21 0 R ] >>"
                .into(),
        );
        objs.insert(21, "<< /Type /OBJR /Pg 4 0 R /Obj 5 0 R >>".into());
        let mut pdf = open(&objs);

        drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");

        let elem = elem_dict(&mut pdf, 20);
        let kids = elem.get("K").and_then(|k| k.as_array()).expect("kids");
        let mcr = kids[0].as_dict().expect("inline MCR");
        assert!(
            matches!(mcr.get("Pg"), Some(Object::Reference(r)) if r.number == 4),
            "MCR /Pg is out of scope and must be untouched, got {:?}",
            mcr.get("Pg")
        );
        let objr = elem_dict(&mut pdf, 21);
        assert!(
            matches!(objr.get("Pg"), Some(Object::Reference(r)) if r.number == 4),
            "OBJR /Pg is out of scope and must be untouched, got {:?}",
            objr.get("Pg")
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
