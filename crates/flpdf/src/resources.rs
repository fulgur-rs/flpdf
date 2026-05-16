//! Unreferenced-resource pruning (ISO 32000-1 §7.8.3 / qpdf `--remove-unreferenced-resources`).
//!
//! Scans every page's content stream(s) via [`crate::content_stream::ContentStreamParser`],
//! collects every resource name that is actually referenced by a PDF operator, and then
//! removes entries from the page's `/Resources` sub-dictionaries that are not referenced.
//!
//! # Modes
//!
//! | Mode | Behaviour |
//! |------|-----------|
//! | [`RemoveUnreferencedResources::No`]  | No-op. |
//! | [`RemoveUnreferencedResources::Auto`] | Only prunes a page's `/Resources` when it is not shared with (or inherited by) another page. Safe heuristic, qpdf-compatible. |
//! | [`RemoveUnreferencedResources::Yes`] | Prunes on a per-page basis regardless of sharing. When multiple pages reference the same `/Resources` object, the kept set is the *union* of all referencing pages, so no rendering breakage occurs. |
//!
//! # Scope
//!
//! This module only removes entries from the `/Resources` sub-dictionaries
//! (`/Font`, `/XObject`, `/ColorSpace`, `/Pattern`, `/Shading`, `/ExtGState`,
//! `/Properties`). It does **not** garbage-collect unreachable PDF objects at
//! the xref level — that is a separate concern.

use crate::content_stream::{ContentStreamParser, ContentToken};
use crate::filters::decode_stream_data;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

// ── Resource location ─────────────────────────────────────────────────────────

/// Where a page's `/Resources` dictionary physically lives.
///
/// Used to distinguish the three cases that need different pruning strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ResourcesLoc {
    /// The page has no `/Resources` entry anywhere in its parent chain.
    None,
    /// `/Resources` is an inline (direct) dictionary on the page dict itself.
    PageInline,
    /// `/Resources` is an inline (direct) dictionary on an ancestor `/Pages` node.
    AncestorInline(ObjectRef),
    /// `/Resources` is an indirect object reference (anywhere in the chain).
    Indirect(ObjectRef),
}

// ── Resource category names we prune ─────────────────────────────────────────

/// The seven resource sub-dictionary keys we inspect and prune.
const RESOURCE_CATEGORIES: &[&str] = &[
    "Font",
    "XObject",
    "ColorSpace",
    "Pattern",
    "Shading",
    "ExtGState",
    "Properties",
];

// ── Device-colorspace names that are never looked up in /ColorSpace ───────────

/// Names that appear as operands to `cs`/`CS`/`sc`/`SC` etc. but are **built-in**
/// device colour spaces, not entries in the page's `/ColorSpace` dictionary.
fn is_builtin_color_space(name: &[u8]) -> bool {
    matches!(
        name,
        b"DeviceGray"
            | b"DeviceRGB"
            | b"DeviceCMYK"
            | b"Pattern"
            | b"Indexed"
            | b"CalGray"
            | b"CalRGB"
            | b"Lab"
            | b"ICCBased"
            // Inline-image abbreviations (ISO 32000-1 Table 93)
            | b"G"
            | b"RGB"
            | b"CMYK"
            | b"I"
    )
}

// ── Mode enum ────────────────────────────────────────────────────────────────

/// How [`remove_unreferenced_resources`] handles the `/Resources` sub-dictionaries.
///
/// Mirrors qpdf's `--remove-unreferenced-resources=auto|yes|no`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoveUnreferencedResources {
    /// Safe heuristic: only prune `/Resources` that belong exclusively to one page
    /// (not shared via inheritance or the same indirect object reference).
    /// This is the default and matches qpdf's `auto` behaviour.
    #[default]
    Auto,
    /// Prune unreferenced entries on a per-page basis regardless of sharing.
    /// When the same `/Resources` object is referenced by several pages, the
    /// union of all referencing pages' used names is computed so that no page
    /// loses a resource it genuinely needs.
    Yes,
    /// No-op: leave all `/Resources` entries untouched.
    No,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Remove unreferenced `/Resources` entries from every page in `pdf`.
///
/// See [`RemoveUnreferencedResources`] for mode semantics.
///
/// # Errors
///
/// Propagates errors from [`Pdf::resolve`], content-stream decoding, or
/// content-stream tokenisation. Malformed-but-recoverable content is silently
/// skipped (an error in one page's content stream does not abort the whole
/// operation, matching qpdf's liberal behaviour).
pub fn remove_unreferenced_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    if mode == RemoveUnreferencedResources::No {
        return Ok(());
    }

    // ── 1. Collect all page refs ──────────────────────────────────────────────
    let page_refs = crate::pages::page_refs(pdf)?;

    // ── 2. For each page, determine where its /Resources physically lives ────────
    // This drives both the "shared" detection for Auto mode and the grouping
    // for Yes mode (union-over-sharing-pages).
    let mut page_res_loc: Vec<ResourcesLoc> = Vec::with_capacity(page_refs.len());
    for &pr in &page_refs {
        page_res_loc.push(resources_location(pdf, pr)?);
    }

    // Count how many pages reference each resources ObjectRef (Indirect or
    // AncestorInline). ref_count[r] > 1 means the resources dict is shared.
    let mut ref_count: BTreeMap<ObjectRef, usize> = BTreeMap::new();
    let mut ancestor_count: BTreeMap<ObjectRef, usize> = BTreeMap::new();
    for loc in &page_res_loc {
        match loc {
            ResourcesLoc::Indirect(r) => {
                *ref_count.entry(*r).or_insert(0) += 1;
            }
            ResourcesLoc::AncestorInline(a) => {
                *ancestor_count.entry(*a).or_insert(0) += 1;
            }
            _ => {}
        }
    }

    // ── 3. Collect used names per resources-ref (for Yes-mode union) ──────────
    // For Auto mode we only need per-page used-names for unshared pages.
    // For Yes mode we need the union grouped by resources-ref.

    // Map from indirect resources ObjectRef → union of used names per category.
    let mut ref_used: BTreeMap<ObjectRef, BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>> = BTreeMap::new();
    // Map from ancestor /Pages ObjectRef → union of used names (AncestorInline case).
    let mut ancestor_inline_used: BTreeMap<ObjectRef, BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>> =
        BTreeMap::new();
    // For pages with PageInline /Resources, store per-page used names keyed by index.
    let mut inline_used: Vec<Option<BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>>> =
        vec![None; page_refs.len()];

    for (i, &page_ref) in page_refs.iter().enumerate() {
        // Determine if this page's resources are shared (for Auto mode skip).
        let is_shared = match &page_res_loc[i] {
            ResourcesLoc::Indirect(r) => ref_count.get(r).copied().unwrap_or(0) > 1,
            ResourcesLoc::AncestorInline(a) => ancestor_count.get(a).copied().unwrap_or(0) > 1,
            _ => false, // PageInline and None: always unshared
        };

        if mode == RemoveUnreferencedResources::Auto && is_shared {
            // Auto: skip shared resources — we must not prune them.
            continue;
        }

        // Collect the used names for this page.
        let used = collect_used_names_for_page(pdf, page_ref)?;

        match &page_res_loc[i] {
            ResourcesLoc::Indirect(r) => {
                // Merge into the union for this resources ref.
                let entry = ref_used.entry(*r).or_default();
                for (cat, names) in used {
                    entry.entry(cat).or_default().extend(names);
                }
            }
            ResourcesLoc::AncestorInline(a) => {
                // Merge into the union for this ancestor /Pages node.
                let entry = ancestor_inline_used.entry(*a).or_default();
                for (cat, names) in used {
                    entry.entry(cat).or_default().extend(names);
                }
            }
            ResourcesLoc::PageInline => {
                // Inline resources on page itself: store per-page.
                inline_used[i] = Some(used);
            }
            ResourcesLoc::None => {}
        }
    }

    // ── 4. Prune: indirect resources objects ──────────────────────────────────
    for (res_ref, used) in &ref_used {
        prune_resources_object(pdf, *res_ref, used)?;
    }

    // ── 5. Prune: inline (direct) resources embedded in ancestor /Pages nodes ──
    for (ancestor_ref, used) in &ancestor_inline_used {
        prune_ancestor_inline_resources(pdf, *ancestor_ref, used)?;
    }

    // ── 6. Prune: inline (direct) resources embedded in page dicts ────────────
    for (i, used_opt) in inline_used.iter().enumerate() {
        let Some(used) = used_opt else { continue };
        let page_ref = page_refs[i];
        prune_inline_resources(pdf, page_ref, used)?;
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Determine where a page's `/Resources` dictionary physically lives.
///
/// Walks the parent chain looking for the first `/Resources` entry and
/// returns a [`ResourcesLoc`] discriminating among:
/// - [`ResourcesLoc::Indirect`] – indirect object reference (anywhere in chain)
/// - [`ResourcesLoc::PageInline`] – inline dict on the page dict itself
/// - [`ResourcesLoc::AncestorInline`] – inline dict on a `/Pages` ancestor
/// - [`ResourcesLoc::None`] – no `/Resources` found at all
fn resources_location<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<ResourcesLoc> {
    // Walk the parent chain looking for the first /Resources entry.
    let mut current = page_ref;
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut depth = 0usize;
    const MAX_DEPTH: usize = 100;

    loop {
        if depth >= MAX_DEPTH {
            return Ok(ResourcesLoc::None);
        }
        if !seen.insert(current) {
            return Ok(ResourcesLoc::None); // cycle
        }
        depth += 1;

        let node_obj = pdf.resolve(current)?;
        let Object::Dictionary(dict) = node_obj else {
            return Ok(ResourcesLoc::None);
        };

        match dict.get("Resources").cloned() {
            Some(Object::Reference(r)) => return Ok(ResourcesLoc::Indirect(r)),
            Some(Object::Dictionary(_)) => {
                // Inline dict: distinguish page-level vs ancestor.
                if current == page_ref {
                    return Ok(ResourcesLoc::PageInline);
                } else {
                    return Ok(ResourcesLoc::AncestorInline(current));
                }
            }
            Some(Object::Null) | None => {
                // Fall through to parent.
                match dict.get("Parent").cloned() {
                    Some(Object::Reference(parent)) => {
                        current = parent;
                    }
                    _ => return Ok(ResourcesLoc::None),
                }
            }
            _ => return Ok(ResourcesLoc::None),
        }
    }
}

/// Collect every resource name actually referenced by a page's content stream.
///
/// Returns a `BTreeMap<category_name, BTreeSet<resource_name>>` covering the
/// seven resource categories.
///
/// Form XObjects are recursed into (using their own `/Resources` scope), and
/// any name that falls outside a Form's own resources sub-category is
/// attributed to the calling page's resources.
fn collect_used_names_for_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>> {
    let content_bytes = crate::pages::page_content_bytes(pdf, page_ref)?;

    // Resolve the page's own /Resources for Form recursion scoping.
    let page_resources = crate::pages::resolve_inherited_resources(pdf, page_ref)?;

    let mut used: BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>> = BTreeMap::new();
    let mut visited_xobjects: BTreeSet<ObjectRef> = BTreeSet::new();

    collect_from_stream(
        pdf,
        &content_bytes,
        page_resources.as_ref(),
        &mut used,
        &mut visited_xobjects,
    )?;

    Ok(used)
}

/// Core recursive walker: tokenises `stream_bytes` and records every resource
/// reference into `used`. `resources` is the `/Resources` dict in scope for
/// this stream (the page dict's or a Form XObject's). `visited` prevents
/// infinite recursion through cyclic Form XObjects.
fn collect_from_stream<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream_bytes: &[u8],
    resources: Option<&Dictionary>,
    used: &mut BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let parser = ContentStreamParser::new(stream_bytes);
    for token_result in parser {
        // On parse error: skip the rest of this stream gracefully.
        let Ok(token) = token_result else { break };

        match token {
            ContentToken::Op { operands, operator } => {
                process_operator(pdf, &operator, &operands, resources, used, visited)?;
            }
            ContentToken::InlineImage { dict, .. } => {
                // /CS operand in an inline image may reference /ColorSpace.
                // Abbreviated key is /CS (ISO 32000-1 Table 93).
                let cs_val = dict.get("CS").or_else(|| dict.get("ColorSpace")).cloned();
                if let Some(Object::Name(name)) = cs_val {
                    if !is_builtin_color_space(&name) {
                        used.entry(b"ColorSpace".to_vec())
                            .or_default()
                            .insert(name);
                    }
                }
            }
            ContentToken::Comment(_) => {}
        }
    }
    Ok(())
}

/// Process a single content-stream operator and record any resource references
/// into `used`.
fn process_operator<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    operator: &[u8],
    operands: &[Object],
    resources: Option<&Dictionary>,
    used: &mut BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    match operator {
        // /XObject — `name Do`
        b"Do" => {
            if let Some(Object::Name(name)) = operands.first() {
                used.entry(b"XObject".to_vec())
                    .or_default()
                    .insert(name.clone());

                // Recurse into Form XObjects.
                recurse_form_xobject(pdf, name, resources, used, visited)?;
            }
        }

        // /Font — `name size Tf`
        b"Tf" => {
            if let Some(Object::Name(name)) = operands.first() {
                used.entry(b"Font".to_vec())
                    .or_default()
                    .insert(name.clone());
            }
        }

        // /ExtGState — `name gs`
        b"gs" => {
            if let Some(Object::Name(name)) = operands.first() {
                used.entry(b"ExtGState".to_vec())
                    .or_default()
                    .insert(name.clone());
            }
        }

        // /ColorSpace — `name cs` (non-stroking) / `name CS` (stroking)
        b"cs" | b"CS" => {
            if let Some(Object::Name(name)) = operands.first() {
                if !is_builtin_color_space(name) {
                    used.entry(b"ColorSpace".to_vec())
                        .or_default()
                        .insert(name.clone());
                }
            }
        }

        // /Pattern — `scn` / `SCN`: last operand may be a Name (pattern name).
        b"scn" | b"SCN" => {
            if let Some(Object::Name(name)) = operands.last() {
                used.entry(b"Pattern".to_vec())
                    .or_default()
                    .insert(name.clone());
            }
        }

        // /Shading — `name sh`
        b"sh" => {
            if let Some(Object::Name(name)) = operands.first() {
                used.entry(b"Shading".to_vec())
                    .or_default()
                    .insert(name.clone());
            }
        }

        // /Properties — `tag props BDC` / `tag props DP`
        // Operand index 1 is the property list: a Name → /Properties ref, or
        // a direct dict << … >> (not a /Properties ref; skip).
        b"BDC" | b"DP" => {
            if let Some(Object::Name(name)) = operands.get(1) {
                used.entry(b"Properties".to_vec())
                    .or_default()
                    .insert(name.clone());
            }
        }

        _ => {}
    }
    Ok(())
}

/// If `xobject_name` resolves to a Form XObject, decode and recurse into its
/// content stream.
///
/// Scoping rule (PDF spec §8.10.4):
/// - If the Form XObject has its own `/Resources` entry, resource names used
///   inside the Form resolve against the Form's own resources dict. Those names
///   must NOT be added to the calling page's `used` set — doing so would cause
///   page-level resources with the same name (but unused by the page itself) to
///   be incorrectly retained when pruning.
/// - If the Form XObject has no `/Resources` entry, it inherits the calling
///   scope's resources; names referenced inside the Form are attributed to the
///   page's `used` set as before.
fn recurse_form_xobject<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobject_name: &[u8],
    page_resources: Option<&Dictionary>,
    used: &mut BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    // Locate the XObject sub-dictionary in the current /Resources scope.
    let xobj_ref = match page_resources {
        Some(res) => match res.get("XObject") {
            Some(Object::Dictionary(xobj_dict)) => xobj_dict.get_ref(
                std::str::from_utf8(xobject_name).unwrap_or(""),
            ),
            _ => None,
        },
        None => None,
    };

    let Some(xobj_ref) = xobj_ref else {
        return Ok(());
    };

    // Cycle guard.  Shared across all recursion levels to prevent infinite loops.
    if !visited.insert(xobj_ref) {
        return Ok(());
    }

    let obj = pdf.resolve(xobj_ref)?;
    let Object::Stream(stream) = obj else {
        return Ok(());
    };

    // Only recurse into Form XObjects.
    let is_form = stream
        .dict
        .get("Subtype")
        .is_some_and(|v| matches!(v, Object::Name(n) if n.as_slice() == b"Form"));
    if !is_form {
        return Ok(());
    }

    // Decode the Form's content stream.
    let form_bytes = match decode_stream_data(&stream.dict, &stream.data) {
        Ok(b) => b,
        Err(_) => return Ok(()), // graceful degradation
    };

    // Determine whether the Form has its own /Resources entry.
    // We check for key *presence* (not the resolved value) because an empty or
    // indirect /Resources still means the Form owns its resource scope.
    let form_has_own_resources = stream.dict.get("Resources").is_some();

    if form_has_own_resources {
        // Resolve the Form's own /Resources dict (may be direct or indirect).
        let form_resources: Option<Dictionary> =
            match stream.dict.get("Resources").cloned() {
                Some(Object::Dictionary(d)) => Some(d),
                Some(Object::Reference(r)) => match pdf.resolve(r)? {
                    Object::Dictionary(d) => Some(d),
                    _ => None, // broken ref → treat as empty own scope
                },
                _ => None,
            };

        // Use a throwaway accumulator so that resource names referenced inside
        // the Form do NOT pollute the calling page's used set.
        let mut form_used: BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>> = BTreeMap::new();
        collect_from_stream(
            pdf,
            &form_bytes,
            form_resources.as_ref(),
            &mut form_used,
            visited,
        )?;
        // form_used is intentionally discarded; Form's own /Resources pruning
        // is out of scope for this minimum fix (flpdf-9hc.12.4).
    } else {
        // No /Resources key → Form inherits the calling scope's resources.
        // Attribute all references to the page's used set (original behaviour).
        collect_from_stream(pdf, &form_bytes, page_resources, used, visited)?;
    }

    Ok(())
}

/// Prune `res_ref` (an indirect /Resources object) in-place: remove every
/// entry from each category sub-dict that is NOT in `used`.
fn prune_resources_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    res_ref: ObjectRef,
    used: &BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
) -> Result<()> {
    let obj = pdf.resolve(res_ref)?;
    let Object::Dictionary(mut res_dict) = obj else {
        return Ok(()); // not a dict — nothing to prune
    };

    apply_pruning(pdf, &mut res_dict, used)?;
    pdf.set_object(res_ref, Object::Dictionary(res_dict));
    Ok(())
}

/// Prune the /Resources that is embedded directly in a page dictionary (not an
/// indirect object). We must re-resolve and re-write the whole page dict.
fn prune_inline_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    used: &BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
) -> Result<()> {
    let page_obj = pdf.resolve(page_ref)?;
    let Object::Dictionary(mut page_dict) = page_obj else {
        return Err(Error::Unsupported(format!(
            "page {page_ref} resolved to non-dictionary"
        )));
    };

    // Pull out the inline /Resources dict, prune it, put it back.
    let resources_val = page_dict.get("Resources").cloned();
    match resources_val {
        Some(Object::Dictionary(mut res_dict)) => {
            apply_pruning(pdf, &mut res_dict, used)?;
            page_dict.insert("Resources", Object::Dictionary(res_dict));
        }
        _ => return Ok(()), // nothing inline to prune
    }

    pdf.set_object(page_ref, Object::Dictionary(page_dict));
    Ok(())
}

/// Prune the /Resources that is embedded directly in an ancestor /Pages node.
/// The union of used names across all pages that inherit from this ancestor is
/// given by `used`. We resolve the ancestor node, mutate its inline /Resources,
/// and write it back via `set_object`.
fn prune_ancestor_inline_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    ancestor_ref: ObjectRef,
    used: &BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
) -> Result<()> {
    let ancestor_obj = pdf.resolve(ancestor_ref)?;
    let Object::Dictionary(mut ancestor_dict) = ancestor_obj else {
        return Ok(()); // not a dict — nothing to prune
    };

    let resources_val = ancestor_dict.get("Resources").cloned();
    match resources_val {
        Some(Object::Dictionary(mut res_dict)) => {
            apply_pruning(pdf, &mut res_dict, used)?;
            ancestor_dict.insert("Resources", Object::Dictionary(res_dict));
        }
        _ => return Ok(()), // not an inline dict — nothing to prune here
    }

    pdf.set_object(ancestor_ref, Object::Dictionary(ancestor_dict));
    Ok(())
}

/// Mutate `res_dict` by removing every entry from each resource sub-category
/// that is not listed in `used`. Empty sub-category dicts are removed entirely.
///
/// Handles both direct Dictionary values and indirect Reference values for
/// category sub-dictionaries (e.g., `/Font 10 0 R`). When the category value
/// is a Reference, the referenced object is pruned in-place via `pdf.set_object`.
fn apply_pruning<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    res_dict: &mut Dictionary,
    used: &BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
) -> Result<()> {
    for &category in RESOURCE_CATEGORIES {
        let cat_key = category.as_bytes();
        let cat_val = res_dict.get(category).cloned();

        match cat_val {
            Some(Object::Dictionary(mut cat_dict)) => {
                // Direct dictionary — prune in place and write back.
                let empty_set = BTreeSet::new();
                let used_names = used.get(cat_key).unwrap_or(&empty_set);

                let to_remove: Vec<Vec<u8>> = cat_dict
                    .iter()
                    .filter(|(k, _)| !used_names.contains(*k))
                    .map(|(k, _)| k.to_vec())
                    .collect();
                for key in to_remove {
                    cat_dict.remove(&key);
                }

                if cat_dict.iter().next().is_none() {
                    res_dict.remove(category);
                } else {
                    res_dict.insert(category, Object::Dictionary(cat_dict));
                }
            }
            Some(Object::Reference(cat_ref)) => {
                // Indirect reference to category sub-dictionary.
                // Resolve, prune, and write back via set_object.
                let resolved = pdf.resolve(cat_ref)?;
                let Object::Dictionary(mut cat_dict) = resolved else {
                    // Not a dictionary (e.g. Stream) — skip safely.
                    continue;
                };

                let empty_set = BTreeSet::new();
                let used_names = used.get(cat_key).unwrap_or(&empty_set);

                let to_remove: Vec<Vec<u8>> = cat_dict
                    .iter()
                    .filter(|(k, _)| !used_names.contains(*k))
                    .map(|(k, _)| k.to_vec())
                    .collect();
                for key in to_remove {
                    cat_dict.remove(&key);
                }

                if cat_dict.iter().next().is_none() {
                    // All entries pruned — remove the category reference from res_dict.
                    // (The now-empty indirect object is left as an orphan; xref GC
                    // is out of scope for this module.)
                    res_dict.remove(category);
                } else {
                    // Update the referenced object in-place; keep the reference in res_dict.
                    pdf.set_object(cat_ref, Object::Dictionary(cat_dict));
                }
            }
            _ => {
                // Absent or non-dictionary/non-reference value — skip.
                continue;
            }
        }
    }
    Ok(())
}
