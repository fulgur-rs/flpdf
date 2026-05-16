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

    // ── 2. For each page, determine which /Resources object ref it uses ───────
    // This drives both the "shared" detection for Auto mode and the grouping
    // for Yes mode (union-over-sharing-pages).
    //
    // `page_res_ref[i]` = Some(ObjectRef) when the page or an ancestor has an
    // indirect /Resources reference, None when /Resources is embedded directly
    // in the page dict (inline dict → always unshared).
    let mut page_res_ref: Vec<Option<ObjectRef>> = Vec::with_capacity(page_refs.len());
    for &pr in &page_refs {
        page_res_ref.push(resources_indirect_ref(pdf, pr)?);
    }

    // Count how many pages reference each resources ObjectRef.
    // ref_count[r] > 1 means the resources dict is shared.
    let mut ref_count: BTreeMap<ObjectRef, usize> = BTreeMap::new();
    for opt in &page_res_ref {
        if let Some(r) = opt {
            *ref_count.entry(*r).or_insert(0) += 1;
        }
    }

    // ── 3. Collect used names per resources-ref (for Yes-mode union) ──────────
    // For Auto mode we only need per-page used-names for unshared pages.
    // For Yes mode we need the union grouped by resources-ref.

    // Map from resources ObjectRef → union of used names per category.
    let mut ref_used: BTreeMap<ObjectRef, BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>> = BTreeMap::new();
    // For pages with inline /Resources (no indirect ref), store inline
    // per-page used names keyed by page index.
    let mut inline_used: Vec<Option<BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>>> =
        vec![None; page_refs.len()];

    for (i, &page_ref) in page_refs.iter().enumerate() {
        // Determine if this page's resources are shared (for Auto mode skip).
        let is_shared = match &page_res_ref[i] {
            Some(r) => ref_count.get(r).copied().unwrap_or(0) > 1,
            None => false, // inline dict: always unshared
        };

        if mode == RemoveUnreferencedResources::Auto && is_shared {
            // Auto: skip shared resources — we must not prune them.
            continue;
        }

        // Collect the used names for this page.
        let used = collect_used_names_for_page(pdf, page_ref)?;

        match &page_res_ref[i] {
            Some(r) => {
                // Merge into the union for this resources ref.
                let entry = ref_used.entry(*r).or_default();
                for (cat, names) in used {
                    entry.entry(cat).or_default().extend(names);
                }
            }
            None => {
                // Inline resources: store per-page.
                inline_used[i] = Some(used);
            }
        }
    }

    // ── 4. Prune: indirect resources objects ──────────────────────────────────
    for (res_ref, used) in &ref_used {
        prune_resources_object(pdf, *res_ref, used)?;
    }

    // ── 5. Prune: inline (direct) resources embedded in page dicts ────────────
    for (i, used_opt) in inline_used.iter().enumerate() {
        let Some(used) = used_opt else { continue };
        let page_ref = page_refs[i];
        prune_inline_resources(pdf, page_ref, used)?;
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the indirect `ObjectRef` that a page's `/Resources` resolves to, if
/// any. Returns `None` when:
/// - the page has an inline (direct dictionary) `/Resources`, or
/// - no `/Resources` is found at all.
///
/// When a page inherits `/Resources` from a parent `/Pages` node, the ref is
/// the one stored on that parent node (not the page's own ref).
fn resources_indirect_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // Walk the parent chain looking for the first /Resources entry.
    let mut current = page_ref;
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut depth = 0usize;
    const MAX_DEPTH: usize = 100;

    loop {
        if depth >= MAX_DEPTH {
            return Ok(None);
        }
        if !seen.insert(current) {
            return Ok(None); // cycle
        }
        depth += 1;

        let node_obj = pdf.resolve(current)?;
        let Object::Dictionary(dict) = node_obj else {
            return Ok(None);
        };

        match dict.get("Resources").cloned() {
            Some(Object::Reference(r)) => return Ok(Some(r)),
            Some(Object::Dictionary(_)) => return Ok(None), // inline
            Some(Object::Null) | None => {
                // Fall through to parent.
                match dict.get("Parent").cloned() {
                    Some(Object::Reference(parent)) => {
                        current = parent;
                    }
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
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
/// content stream. Uses the Form's own `/Resources` (falling back to `page_resources`)
/// as the scope for the recursive scan.
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

    // Cycle guard.
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

    // The Form's /Resources (may be absent → falls back to page resources).
    let form_resources: Option<Dictionary> = match stream.dict.get("Resources").cloned() {
        Some(Object::Dictionary(d)) => Some(d),
        Some(Object::Reference(r)) => {
            match pdf.resolve(r)? {
                Object::Dictionary(d) => Some(d),
                _ => page_resources.cloned(),
            }
        }
        _ => page_resources.cloned(),
    };

    collect_from_stream(pdf, &form_bytes, form_resources.as_ref(), used, visited)?;

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

    apply_pruning(&mut res_dict, used);
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
            apply_pruning(&mut res_dict, used);
            page_dict.insert("Resources", Object::Dictionary(res_dict));
        }
        _ => return Ok(()), // nothing inline to prune
    }

    pdf.set_object(page_ref, Object::Dictionary(page_dict));
    Ok(())
}

/// Mutate `res_dict` by removing every entry from each resource sub-category
/// that is not listed in `used`. Empty sub-category dicts are removed entirely.
fn apply_pruning(
    res_dict: &mut Dictionary,
    used: &BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>,
) {
    for &category in RESOURCE_CATEGORIES {
        let cat_key = category.as_bytes();
        let cat_val = res_dict.get(category).cloned();

        let Some(Object::Dictionary(mut cat_dict)) = cat_val else {
            continue;
        };

        let empty_set = BTreeSet::new();
        let used_names = used.get(cat_key).unwrap_or(&empty_set);

        // Collect keys to remove (can't mutate while iterating).
        let to_remove: Vec<Vec<u8>> = cat_dict
            .iter()
            .filter(|(k, _)| !used_names.contains(*k))
            .map(|(k, _)| k.to_vec())
            .collect();
        for key in to_remove {
            cat_dict.remove(&key);
        }

        if cat_dict.iter().next().is_none() {
            // Remove the whole sub-dictionary when nothing remains.
            res_dict.remove(category);
        } else {
            res_dict.insert(category, Object::Dictionary(cat_dict));
        }
    }
}
