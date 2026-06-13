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
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Resource names referenced by a content scope, keyed by category
/// (`Font`, `XObject`, …) → set of referenced names.
type UsedNames = BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>;

/// Tracking information for indirect category sub-dictionary objects
/// (e.g. `/Font 6 0 R`).  Built in a global pre-pass over all pages so that
/// the sharing count includes pages Auto-mode would otherwise skip.
///
/// Key: `(category_bytes, cat_sub_dict_ref)`.
type CatRefKey = (Vec<u8>, ObjectRef);

/// Per-category-ref metadata accumulated across all top-level `/Resources`.
struct CatRefInfo {
    /// Number of distinct top-level `/Resources` groups that point to this
    /// indirect category sub-dict.  Used by Auto mode: if > 1, skip pruning.
    group_count: usize,
    /// Union of used names contributed by groups whose used-names are known.
    /// Groups skipped by Auto (shared top-level) do not contribute used-names,
    /// but they still increment `group_count`, so the ref ends up protected.
    used_union: BTreeSet<Vec<u8>>,
    /// True if any top-level `/Resources` group pointing to this sub-dict belongs
    /// to a page whose content stream could not be decoded. Such a
    /// sub-dict must never be pruned in either mode: the corrupt page's true
    /// usage is unknown, so we conservatively retain every entry.
    protected: bool,
}

/// Canonical identifier for a top-level `/Resources` group.
///
/// Used as the key in `cat_ref_seen_groups` so that distinct group types never
/// collide even when they share the same `ObjectRef` number.
///
/// `PageInline` stores the page's own `ObjectRef` (generation 0) without
/// inventing a synthetic generation-1 value, eliminating a collision where a
/// real indirect `(N, 1)` object and a page-inline group for page object
/// `(N, 0)` could previously share the same synthetic key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ResGroupKey {
    /// `/Resources` is an indirect object at the given ref.
    Indirect(ObjectRef),
    /// `/Resources` is an inline dict on the ancestor `/Pages` node at the given ref.
    AncestorInline(ObjectRef),
    /// `/Resources` is an inline dict on the page dict itself; key is the page's ObjectRef.
    PageInline(ObjectRef),
}

/// Canonical [`ResGroupKey`] for a page's resources location, or `None` when the
/// page has no `/Resources` anywhere in its chain (nothing to key on / prune).
fn res_group_key(loc: &ResourcesLoc, page_ref: ObjectRef) -> Option<ResGroupKey> {
    match loc {
        ResourcesLoc::Indirect(r) => Some(ResGroupKey::Indirect(*r)),
        ResourcesLoc::AncestorInline(a) => Some(ResGroupKey::AncestorInline(*a)),
        ResourcesLoc::PageInline => Some(ResGroupKey::PageInline(page_ref)),
        ResourcesLoc::None => None,
    }
}

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
    /// `/Resources` is an indirect object reference, collapsed to the terminal
    /// ref of any holder chain (`a 0 R → b 0 R → <<dict>>` stores `b`).
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

/// Names that appear as operands to the page-content `cs`/`CS` operators but
/// are **built-in** device colour spaces, not entries in the page's
/// `/ColorSpace` dictionary.
///
/// ISO 32000-1 §8.6.8: only `/DeviceGray`, `/DeviceRGB`, `/DeviceCMYK`, and
/// `/Pattern` may be selected by name directly in page content.  All other
/// colour spaces (`/CalGray`, `/CalRGB`, `/Lab`, `/ICCBased`, `/Indexed`, …)
/// are array-based and **must** be named via an entry in `/Resources/ColorSpace`.
fn is_builtin_color_space_cs_op(name: &[u8]) -> bool {
    matches!(
        name,
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Pattern"
    )
}

/// Names that are valid **inline-image** colour-space specifiers (ISO 32000-1
/// Table 93) and do **not** correspond to entries in `/Resources/ColorSpace`.
///
/// This covers both the full Device names and the one-letter abbreviations
/// permitted inside inline-image dictionaries (`BI … ID … EI`).
fn is_builtin_inline_image_cs(name: &[u8]) -> bool {
    matches!(
        name,
        // Full Device names are also valid in inline images.
        b"DeviceGray"
            | b"DeviceRGB"
            | b"DeviceCMYK"
            | b"Pattern"
            // Abbreviated names (Table 93).
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
/// Propagates *structural* errors: page-tree traversal
/// ([`crate::pages::page_refs`]), resources-location resolution
/// (`resources_location`), inherited-`/Resources` resolution
/// ([`crate::pages::resolve_inherited_resources`]), and Form XObject object
/// resolution. These are not content-comprehension failures.
///
/// A failure to **decode** (corrupt filter) or **tokenise** (malformed content
/// syntax, even part-way through a stream that decoded fine) an individual page's
/// content stream does NOT abort the operation: that page is skipped and its
/// resources are conservatively retained (never pruned), matching qpdf's liberal
/// behaviour and the graceful degradation already applied to Form XObjects
/// (see `recurse_form_xobject`).
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
    let mut ref_used: BTreeMap<ObjectRef, UsedNames> = BTreeMap::new();
    // Map from ancestor /Pages ObjectRef → union of used names (AncestorInline case).
    let mut ancestor_inline_used: BTreeMap<ObjectRef, UsedNames> = BTreeMap::new();
    // For pages with PageInline /Resources, store per-page used names keyed by index.
    let mut inline_used: Vec<Option<UsedNames>> = vec![None; page_refs.len()];

    // Resources groups belonging to pages whose content stream could not be
    // decoded/parsed (flpdf-s9s). Their resources are conservatively retained:
    // the prune loops skip these groups, and the cat-ref sharing table (step 3b)
    // marks their indirect category sub-dicts `protected` so cross-group pruning
    // also leaves them untouched.
    let mut protected_groups: BTreeSet<ResGroupKey> = BTreeSet::new();

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

        // Collect the used names for this page. When the page's /Contents cannot
        // be fully understood — it failed to decode (corrupt FlateDecode) or to
        // tokenise part-way (malformed content syntax) — collection returns
        // `None` rather than aborting, degrading like the Form XObject path (see
        // `recurse_form_xobject`). Mark the page's resources group as protected so
        // its resources are conservatively retained rather than pruned against an
        // incomplete used-name set. Genuine structural errors (resolving the
        // inherited /Resources or a Form object reference) still propagate via `?`.
        let used = match collect_used_names_for_page(pdf, page_ref)? {
            Some(used) => used,
            None => {
                if let Some(key) = res_group_key(&page_res_loc[i], page_ref) {
                    protected_groups.insert(key);
                }
                continue;
            }
        };

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

    // ── 3b. Build global cat-ref sharing table (指摘1 fix) ───────────────────
    // An indirect category sub-dict (e.g. `/Font 6 0 R`) may be shared between
    // several *different* top-level /Resources objects that each belong to
    // different page groups.  Auto mode's top-level sharing detection only
    // protects pages that share the same top-level /Resources, so it cannot
    // prevent cross-group damage.
    //
    // We do a **global** pre-pass over every page's top-level /Resources
    // (regardless of Auto skip decisions) to count how many distinct top-level
    // /Resources groups contain each indirect category ref, and to accumulate
    // the union of used names from groups whose used-names are already known.
    //
    // Key insight: groups that Auto *skipped* (shared top-level) do NOT
    // contribute used-names (we have none), but they DO increment `group_count`,
    // so any cat-ref they contain ends up with group_count > 1 and is protected
    // even in Yes mode.
    let mut cat_ref_map: BTreeMap<CatRefKey, CatRefInfo> = BTreeMap::new();

    // Helper closure-equivalent: collect cat-refs from a /Resources dict.
    // `top_res_ref` is the ObjectRef of the owning top-level resources group
    // (used as a set key so the same group is only counted once per cat-ref).
    // `used_for_group` is the already-computed used-names for this group,
    // or None if this group was Auto-skipped.
    //
    // We track (cat_ref, seen_top_level_groups) to avoid double-counting when
    // multiple pages share the same top-level Indirect /Resources.
    let mut cat_ref_seen_groups: BTreeMap<CatRefKey, BTreeSet<ResGroupKey>> = BTreeMap::new();

    for (i, loc) in page_res_loc.iter().enumerate() {
        // Resolve this page's top-level /Resources dict (if any).
        let res_dict_opt: Option<Dictionary> = match loc {
            ResourcesLoc::Indirect(r) => match pdf.resolve_borrowed(*r) {
                Ok(Object::Dictionary(d)) => Some(d.clone()),
                _ => None,
            },
            ResourcesLoc::PageInline => {
                // Read the inline dict from the page object.
                match pdf.resolve_borrowed(page_refs[i]) {
                    Ok(Object::Dictionary(page_dict)) => {
                        match page_dict.get("Resources").cloned() {
                            Some(Object::Dictionary(d)) => Some(d),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            ResourcesLoc::AncestorInline(a) => match pdf.resolve_borrowed(*a) {
                Ok(Object::Dictionary(anc_dict)) => match anc_dict.get("Resources").cloned() {
                    Some(Object::Dictionary(d)) => Some(d),
                    _ => None,
                },
                _ => None,
            },
            ResourcesLoc::None => None,
        };

        let Some(res_dict) = res_dict_opt else {
            continue;
        };

        // The canonical key for this top-level resources group.
        // Use the typed ResGroupKey enum so that PageInline(page_ref) never
        // collides with a real Indirect(ObjectRef) even when the object numbers
        // happen to be identical (roborev low 指摘2).
        let group_key: ResGroupKey = res_group_key(loc, page_refs[i])
            .unwrap_or_else(|| unreachable!("None loc filtered above by res_dict_opt"));

        // Pages whose content failed to decode poison their whole resources group
        // (flpdf-s9s): every indirect category sub-dict they reference must be
        // retained, even if some *other* group would otherwise prune it.
        let group_protected = protected_groups.contains(&group_key);

        // What used-names does this group contribute?
        // None if the group was Auto-skipped (shared top-level).
        let group_used: Option<&UsedNames> = match loc {
            ResourcesLoc::Indirect(r) => ref_used.get(r),
            ResourcesLoc::AncestorInline(a) => ancestor_inline_used.get(a),
            ResourcesLoc::PageInline => inline_used[i].as_ref(),
            ResourcesLoc::None => None,
        };

        for &category in RESOURCE_CATEGORIES {
            let cat_key = category.as_bytes();
            if let Some(cat_ref) = res_dict.get(category).and_then(Object::as_ref_id) {
                let key: CatRefKey = (cat_key.to_vec(), cat_ref);

                let seen = cat_ref_seen_groups.entry(key.clone()).or_default();
                if seen.insert(group_key) {
                    // First time we see this cat_ref from this group.
                    let info = cat_ref_map
                        .entry(key.clone())
                        .or_insert_with(|| CatRefInfo {
                            group_count: 0,
                            used_union: BTreeSet::new(),
                            protected: false,
                        });
                    info.group_count += 1;
                    info.protected |= group_protected;

                    // Merge this group's known used-names for this category.
                    if let Some(used_map) = group_used {
                        if let Some(names) = used_map.get(cat_key) {
                            info.used_union.extend(names.iter().cloned());
                        }
                    }
                    // If group_used is None (Auto-skipped group), we have no
                    // used-names to contribute — group_count alone protects
                    // the ref in Auto mode.
                }
            }
        }
    }

    // ── 4. Prune: indirect resources objects ──────────────────────────────────
    // Skip groups poisoned by an undecodable page (flpdf-s9s): in Yes mode a
    // failed page sharing this dict with a healthy sibling would otherwise see
    // its resources pruned against the sibling's incomplete used-name union.
    for (res_ref, used) in &ref_used {
        if protected_groups.contains(&ResGroupKey::Indirect(*res_ref)) {
            continue;
        }
        prune_resources_object(pdf, *res_ref, used, &cat_ref_map, mode)?;
    }

    // ── 5. Prune: inline (direct) resources embedded in ancestor /Pages nodes ──
    for (ancestor_ref, used) in &ancestor_inline_used {
        if protected_groups.contains(&ResGroupKey::AncestorInline(*ancestor_ref)) {
            continue;
        }
        prune_ancestor_inline_resources(pdf, *ancestor_ref, used, &cat_ref_map, mode)?;
    }

    // ── 6. Prune: inline (direct) resources embedded in page dicts ────────────
    // A poisoned PageInline group never populates `inline_used[i]` (collection
    // `continue`s on failure), so it is already skipped by the `else { continue }`
    // below; no explicit `protected_groups` check is needed here.
    for (i, used_opt) in inline_used.iter().enumerate() {
        let Some(used) = used_opt else { continue };
        let page_ref = page_refs[i];
        prune_inline_resources(pdf, page_ref, used, &cat_ref_map, mode)?;
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Determine where a page's `/Resources` dictionary physically lives.
///
/// Walks the parent chain looking for the first `/Resources` entry and
/// returns a [`ResourcesLoc`] discriminating among:
/// - [`ResourcesLoc::Indirect`] – indirect object reference, collapsed to the
///   terminal ref of any holder chain
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

        let node_obj = pdf.resolve_borrowed(current)?;
        let Some(dict) = node_obj.as_dict() else {
            return Ok(ResourcesLoc::None);
        };

        // Clone the values we need, then drop the `node_obj` borrow so the
        // holder-chain follow below can take `&mut pdf`.
        let resources_val = dict.get("Resources").cloned();
        let parent_val = dict.get("Parent").cloned();

        match resources_val {
            Some(rv @ Object::Reference(_)) => {
                // Collapse the holder chain (`a 0 R → b 0 R → <<dict>>`) to its
                // terminal so every `ResourcesLoc::Indirect` consumer (grouping,
                // ref-counting, pruning) keys on the terminal — the same dict the
                // read side (`pages::resolve_inherited_resources`) resolves to.
                // Keying on the first hop instead made `prune_resources_object`
                // resolve a ref-to-a-ref, see a non-dict, and silently skip
                // pruning. A single-hop ref's terminal is itself, so this
                // preserves the prior result for that case.
                let (terminal_obj, terminal_ref) = resolve_ref_chain(pdf, &rv)?;
                match terminal_obj {
                    // A chain resolving to null is "absent" (PDF §7.3.9): fall
                    // through to the parent, matching the direct-null arm and the
                    // read side. Returning `Indirect(terminal)` would key this
                    // page's used names on the null ref while it renders from the
                    // inherited parent dict, so the parent could be pruned against
                    // only the other pages' names and lose this page's resources.
                    Object::Null => match parent_val {
                        Some(Object::Reference(parent)) => current = parent,
                        _ => return Ok(ResourcesLoc::None),
                    },
                    Object::Dictionary(_) => {
                        return Ok(terminal_ref.map_or(ResourcesLoc::None, ResourcesLoc::Indirect));
                    }
                    // A non-dict, non-null terminal has nothing prunable; the
                    // read side rejects it, but here we simply skip (no-op).
                    _ => return Ok(ResourcesLoc::None),
                }
            }
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
                match parent_val {
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
///
/// # Return value
///
/// - `Ok(Some(used))` — the content was fully decoded and tokenised; `used` is a
///   reliable, complete picture of the page's resource usage and is safe to
///   prune against.
/// - `Ok(None)` — the page's content could not be fully understood: its
///   `/Contents` failed to **decode** (corrupt filter) or failed to **tokenise**
///   part-way (malformed content syntax). The collected names would be
///   incomplete, so the caller must conservatively retain the page's resources
///   rather than prune against a partial set. This mirrors the Form XObject
///   decode-failure path (`recurse_form_xobject`).
/// - `Err(_)` — a genuine structural error (e.g. resolving the inherited
///   `/Resources`, or a Form XObject object reference). These are **not**
///   content-comprehension failures and propagate as before.
fn collect_used_names_for_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Option<UsedNames>> {
    // A failure to *decode* the page's /Contents (corrupt filter) is a
    // content-comprehension failure → conservatively retain (None), do not abort.
    let content_bytes = match crate::pages::page_content_bytes(pdf, page_ref) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };

    // Resolving the inherited /Resources is a structural operation, not content
    // comprehension: propagate its errors rather than silently degrading.
    let page_resources = crate::pages::resolve_inherited_resources(pdf, page_ref)?;

    let mut used: UsedNames = BTreeMap::new();
    let mut visited_xobjects: BTreeSet<ObjectRef> = BTreeSet::new();

    // `collect_from_stream` returns `false` when tokenisation stopped early on a
    // malformed token: the collected names are then incomplete → retain.
    let complete = collect_from_stream(
        pdf,
        &content_bytes,
        page_resources.as_ref(),
        &mut used,
        &mut visited_xobjects,
        0,
    )?;

    Ok(complete.then_some(used))
}

/// Maximum recursion depth for Form XObject traversal.
///
/// Indirect Form XObjects are guarded by `visited` (ObjectRef cycle detection).
/// Direct-stream Form XObjects are owned by their containing dict and cannot
/// form cycles in well-formed PDFs, but we cap depth as an extra safeguard.
const MAX_FORM_DEPTH: usize = 64;

/// Core recursive walker: tokenises `stream_bytes` and records every resource
/// reference into `used`. `resources` is the `/Resources` dict in scope for
/// this stream (the page dict's or a Form XObject's). `visited` prevents
/// infinite recursion through cyclic Form XObjects.
///
/// Returns `Ok(true)` when the stream was tokenised to the end, `Ok(false)` when
/// tokenisation stopped early on a malformed token (so `used` is incomplete and
/// the page must be conservatively retained). Structural errors from
/// nested Form XObject resolution propagate as `Err`.
fn collect_from_stream<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream_bytes: &[u8],
    resources: Option<&Dictionary>,
    used: &mut UsedNames,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<bool> {
    let parser = ContentStreamParser::new(stream_bytes);
    for token_result in parser {
        // A malformed token means the rest of this stream is unreliable. Signal
        // an incomplete collection so the page's resources are retained rather
        // than pruned against a partial used-name set.
        let Ok(token) = token_result else {
            return Ok(false);
        };

        match token {
            ContentToken::Op { operands, operator } => {
                if !process_operator(pdf, &operator, &operands, resources, used, visited, depth)? {
                    return Ok(false);
                }
            }
            ContentToken::InlineImage { dict, .. } => {
                // /CS operand in an inline image may reference /ColorSpace.
                // Abbreviated key is /CS (ISO 32000-1 Table 93).
                let cs_val = dict.get("CS").or_else(|| dict.get("ColorSpace")).cloned();
                if let Some(name) = cs_val.and_then(Object::into_name) {
                    if !is_builtin_inline_image_cs(&name) {
                        used.entry(b"ColorSpace".to_vec()).or_default().insert(name);
                    }
                }
            }
            ContentToken::Comment(_) => {}
        }
    }
    Ok(true)
}

/// Process a single content-stream operator and record any resource references
/// into `used`.
///
/// Returns the completeness flag of any nested Form XObject recursion (`true`
/// for non-recursing operators): `false` propagates an incomplete collection up
/// so the page is conservatively retained.
fn process_operator<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    operator: &[u8],
    operands: &[Object],
    resources: Option<&Dictionary>,
    used: &mut UsedNames,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<bool> {
    match operator {
        // /XObject — `name Do`
        b"Do" => {
            if let Some(name) = operands.first().and_then(Object::as_name) {
                used.entry(b"XObject".to_vec())
                    .or_default()
                    .insert(name.to_vec());

                // Recurse into Form XObjects, propagating their completeness.
                return recurse_form_xobject(pdf, name, resources, used, visited, depth);
            }
        }

        // /Font — `name size Tf`
        b"Tf" => {
            if let Some(name) = operands.first().and_then(Object::as_name) {
                used.entry(b"Font".to_vec())
                    .or_default()
                    .insert(name.to_vec());
            }
        }

        // /ExtGState — `name gs`
        b"gs" => {
            if let Some(name) = operands.first().and_then(Object::as_name) {
                used.entry(b"ExtGState".to_vec())
                    .or_default()
                    .insert(name.to_vec());
            }
        }

        // /ColorSpace — `name cs` (non-stroking) / `name CS` (stroking)
        b"cs" | b"CS" => {
            if let Some(name) = operands.first().and_then(Object::as_name) {
                if !is_builtin_color_space_cs_op(name) {
                    used.entry(b"ColorSpace".to_vec())
                        .or_default()
                        .insert(name.to_vec());
                }
            }
        }

        // /Pattern — `scn` / `SCN`: last operand may be a Name (pattern name).
        b"scn" | b"SCN" => {
            if let Some(name) = operands.last().and_then(Object::as_name) {
                used.entry(b"Pattern".to_vec())
                    .or_default()
                    .insert(name.to_vec());
            }
        }

        // /Shading — `name sh`
        b"sh" => {
            if let Some(name) = operands.first().and_then(Object::as_name) {
                used.entry(b"Shading".to_vec())
                    .or_default()
                    .insert(name.to_vec());
            }
        }

        // /Properties — `tag props BDC` / `tag props DP`
        // Operand index 1 is the property list: a Name → /Properties ref, or
        // a direct dict << … >> (not a /Properties ref; skip).
        b"BDC" | b"DP" => {
            if let Some(name) = operands.get(1).and_then(Object::as_name) {
                used.entry(b"Properties".to_vec())
                    .or_default()
                    .insert(name.to_vec());
            }
        }

        _ => {}
    }
    Ok(true)
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
///
/// The XObject entry in `/Resources/XObject` may be either an indirect
/// `Object::Reference` (the common case) or a direct `Object::Stream` (allowed
/// by the PDF spec for inline stream objects).  Both are handled here.
///
/// Indirect references use `visited` (an `ObjectRef` set) for cycle detection.
/// Direct streams are owned by their containing dictionary and therefore cannot
/// be reached through a cycle in well-formed PDFs; `depth` provides an extra
/// guard against pathological documents.
///
/// Returns `Ok(true)` when the Form (if any) was tokenised completely, `Ok(false)`
/// when the Form's content could not be fully decoded/tokenised AND its names
/// feed the calling page's scope — signalling the page must be conservatively
/// retained. A Form with its **own** `/Resources` cannot make the
/// page incomplete (its names are scoped to itself and discarded here), so it
/// always reports `Ok(true)`. Non-Form / absent / cycle / depth-limit cases also
/// report `Ok(true)` (nothing page-relevant was lost). Structural resolution
/// errors propagate as `Err`.
fn recurse_form_xobject<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobject_name: &[u8],
    page_resources: Option<&Dictionary>,
    used: &mut UsedNames,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<bool> {
    // Guard against excessively deep recursion (applies to direct-stream forms
    // in particular, since indirect refs are additionally guarded by `visited`).
    if depth >= MAX_FORM_DEPTH {
        return Ok(true);
    }

    // Locate the XObject entry in the current /Resources scope. The
    // `/XObject` resource category itself may be a direct dictionary *or*
    // an indirect reference (`/XObject 6 0 R`) — resolve the latter, the
    // same way `apply_pruning` already treats indirect category dicts. The
    // reference may itself be reached through more than one indirect hop
    // (ref -> ref -> dict); follow the chain to its terminal dictionary.
    let xobj_val: Option<Object> = match page_resources.and_then(|res| res.get("XObject")) {
        Some(Object::Dictionary(xobj_dict)) => xobj_dict.get(xobject_name).cloned(),
        Some(cat_ref @ Object::Reference(_)) => resolve_ref_chain(pdf, cat_ref)?
            .0
            .into_dict()
            .and_then(|xobj_dict| xobj_dict.get(xobject_name).cloned()),
        _ => None,
    };

    let Some(xobj_val) = xobj_val else {
        return Ok(true);
    };

    // For indirect XObject references, record the ObjectRef so we can do
    // stack-pop cycle detection (see below).  Direct streams cannot form cycles
    // in well-formed PDFs, so no entry is needed for them.
    let indirect_ref: Option<ObjectRef> = match &xobj_val {
        Object::Reference(r) => Some(*r),
        _ => None,
    };

    // Resolve to a Stream, handling both indirect references and direct streams.
    // An indirect XObject may be reached through more than one indirect hop
    // (ref -> ref -> stream); follow the chain to its terminal and move the
    // owned Stream out rather than cloning it again. (`indirect_ref` above keeps
    // the *first* hop for the `visited` stack-pop cycle guard — unaffected here.)
    // A non-Stream terminal, a non-reference/non-stream value, and (above) an
    // absent entry all mean "nothing to recurse into" → the page stays complete.
    let stream: Option<crate::object::Stream> = match xobj_val {
        Object::Reference(xobj_ref) => resolve_ref_chain(pdf, &Object::Reference(xobj_ref))?
            .0
            .into_stream(),
        // Direct stream: owned by its parent dict, so cycles are impossible in
        // well-formed PDFs.  Depth guard above is sufficient.
        Object::Stream(s) => Some(s),
        _ => None,
    };
    let Some(stream) = stream else {
        return Ok(true);
    };

    // Only recurse into Form XObjects.
    let is_form = stream
        .dict
        .get("Subtype")
        .is_some_and(|v| matches!(v, Object::Name(n) if n.as_slice() == b"Form"));
    if !is_form {
        return Ok(true);
    }

    // Determine whether the Form has its own /Resources entry (checked before
    // decode so a decode failure can report the right completeness). We check for
    // key *presence* (not the resolved value) because an empty or indirect
    // /Resources still means the Form owns its resource scope.
    let form_has_own_resources = stream.dict.get("Resources").is_some();

    // Decode the Form's content stream. On failure, the Form's resource usage is
    // unknown: that only makes the *page* incomplete when the Form inherits the
    // page scope (its names feed `used`). A Form with its own /Resources scopes
    // its names to itself (discarded here), so the page stays complete.
    let form_bytes = match decode_stream_data(&stream.dict, &stream.data) {
        Ok(b) => b,
        Err(_) => return Ok(form_has_own_resources),
    };

    // ── Cycle guard (stack-pop style, roborev medium 指摘1) ──────────────────
    //
    // The cycle check is deferred until just before recursion so that early
    // returns above (non-Stream, non-Form, decode failure) never leave the
    // ObjectRef stranded in `visited`.
    //
    // By inserting here and removing *after* the recursive call returns, `visited`
    // acts as a "currently on the call stack" set rather than a "ever visited"
    // set.  This means that the same XObject can be legitimately visited via
    // multiple independent paths (e.g. first inside a Form with its own
    // /Resources, then directly from the page scope) without the first path
    // blocking the second.
    //
    // True cycles (A → B → A) are still caught: while recursing into A, `visited`
    // contains A; when B tries to recurse into A again, `insert` returns false
    // and we return immediately, breaking the loop.  Once B returns and we remove
    // A from `visited`, no infinite loop is possible.
    if let Some(r) = indirect_ref {
        if !visited.insert(r) {
            return Ok(true); // cycle detected — already on the current call stack
        }
    }

    let page_complete = if form_has_own_resources {
        // Resolve the Form's own /Resources dict (may be direct or indirect).
        let form_resources: Option<Dictionary> = match stream.dict.get("Resources").cloned() {
            Some(Object::Dictionary(d)) => Some(d),
            Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
                Object::Dictionary(d) => Some(d.clone()),
                _ => None, // broken ref → treat as empty own scope
            },
            _ => None,
        };

        // Use a throwaway accumulator so that resource names referenced inside
        // the Form do NOT pollute the calling page's used set. Because those
        // names are scoped to the Form and discarded, an incomplete tokenisation
        // here cannot make the *page* unreliable — the page stays complete.
        let mut form_used: UsedNames = BTreeMap::new();
        collect_from_stream(
            pdf,
            &form_bytes,
            form_resources.as_ref(),
            &mut form_used,
            visited,
            depth + 1,
        )?;
        // form_used is intentionally discarded; Form's own /Resources pruning
        // is out of scope for this minimum fix (flpdf-9hc.12.4).
        true
    } else {
        // No /Resources key → Form inherits the calling scope's resources.
        // Attribute all references to the page's used set (original behaviour).
        // The Form's completeness IS the page's completeness here: an incomplete
        // tokenisation means page-scoped names may be missing → retain.
        collect_from_stream(pdf, &form_bytes, page_resources, used, visited, depth + 1)?
    };

    // ── Stack pop: remove from visited so sibling/later paths can visit this ref ─
    if let Some(r) = indirect_ref {
        visited.remove(&r);
    }

    Ok(page_complete)
}

/// Prune `res_ref` (an indirect /Resources object) in-place: remove every
/// entry from each category sub-dict that is NOT in `used`.
fn prune_resources_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    res_ref: ObjectRef,
    used: &UsedNames,
    cat_ref_map: &BTreeMap<CatRefKey, CatRefInfo>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    let obj = pdf.resolve_borrowed(res_ref)?;
    let Some(mut res_dict) = obj.as_dict().cloned() else {
        return Ok(()); // not a dict — nothing to prune
    };

    apply_pruning(pdf, &mut res_dict, used, cat_ref_map, mode)?;
    pdf.set_object(res_ref, Object::Dictionary(res_dict));
    Ok(())
}

/// Prune the /Resources that is embedded directly in a page dictionary (not an
/// indirect object). We must re-resolve and re-write the whole page dict.
fn prune_inline_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    used: &UsedNames,
    cat_ref_map: &BTreeMap<CatRefKey, CatRefInfo>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Some(mut page_dict) = page_obj.as_dict().cloned() else {
        return Err(Error::Unsupported(format!(
            "page {page_ref} resolved to non-dictionary"
        )));
    };

    // Pull out the inline /Resources dict, prune it, put it back.
    let resources_val = page_dict.get("Resources").cloned();
    match resources_val {
        Some(Object::Dictionary(mut res_dict)) => {
            apply_pruning(pdf, &mut res_dict, used, cat_ref_map, mode)?;
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
    used: &UsedNames,
    cat_ref_map: &BTreeMap<CatRefKey, CatRefInfo>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    let ancestor_obj = pdf.resolve_borrowed(ancestor_ref)?;
    let Some(mut ancestor_dict) = ancestor_obj.as_dict().cloned() else {
        return Ok(()); // not a dict — nothing to prune
    };

    let resources_val = ancestor_dict.get("Resources").cloned();
    match resources_val {
        Some(Object::Dictionary(mut res_dict)) => {
            apply_pruning(pdf, &mut res_dict, used, cat_ref_map, mode)?;
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
///
/// # Indirect category sub-dict sharing
///
/// An indirect category sub-dict (e.g. `/Font 6 0 R`) may be referenced by
/// several different top-level `/Resources` groups.  `cat_ref_map` carries
/// the global sharing count and the union of used names across all groups.
///
/// - **Auto mode**: if `group_count > 1`, leave the sub-dict untouched
///   (same philosophy as top-level sharing protection).
/// - **Yes mode**: always use the global `used_union` rather than the
///   per-group `used` argument, so no cross-group used name is lost.
///
/// Because the same `cat_ref` may appear in multiple calls to `apply_pruning`
/// (once per top-level resources group that contains it), we do the actual
/// `set_object` write on the first call only.  A written `cat_ref` is detected
/// on subsequent calls by the fact that its resolved dict will have already
/// had entries removed; since we only write (never expand), idempotency is
/// guaranteed — but to avoid redundant I/O we rely on `cat_ref_map` tracking.
///
/// Note: when all entries are pruned from an indirect cat sub-dict, the empty
/// indirect object is left in place (as an orphan).  Removing the containing
/// category key from *all* top-level `/Resources` dicts that reference it would
/// require tracking every such res_dict across calls, which is out of scope;
/// xref-level GC handles orphans separately.
fn apply_pruning<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    res_dict: &mut Dictionary,
    used: &UsedNames,
    cat_ref_map: &BTreeMap<CatRefKey, CatRefInfo>,
    mode: RemoveUnreferencedResources,
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
                //
                // Check the global cat-ref sharing table first (指摘1):
                let map_key: CatRefKey = (cat_key.to_vec(), cat_ref);

                if let Some(info) = cat_ref_map.get(&map_key) {
                    if info.protected {
                        // A page referencing this sub-dict has an undecodable
                        // content stream (flpdf-s9s): retain every entry in both
                        // modes, since that page's true usage is unknown.
                        continue;
                    }
                    if mode == RemoveUnreferencedResources::Auto && info.group_count > 1 {
                        // Auto: multiple top-level /Resources groups share this
                        // indirect cat sub-dict → protect it, same as top-level
                        // sharing protection.
                        continue;
                    }

                    // Yes (or Auto with group_count == 1): prune using the
                    // global union, not just the per-group `used`.
                    let resolved = pdf.resolve_borrowed(cat_ref)?;
                    let Some(mut cat_dict) = resolved.as_dict().cloned() else {
                        continue; // not a dict — skip safely
                    };

                    let to_remove: Vec<Vec<u8>> = cat_dict
                        .iter()
                        .filter(|(k, _)| !info.used_union.contains(*k))
                        .map(|(k, _)| k.to_vec())
                        .collect();
                    for key in to_remove {
                        cat_dict.remove(&key);
                    }

                    if cat_dict.iter().next().is_none() {
                        // All entries pruned — leave the empty indirect object as
                        // an orphan (xref GC is out of scope for this module).
                        // Do not remove the category from res_dict here because
                        // the same cat_ref may still be referenced by other
                        // top-level /Resources dicts; their apply_pruning calls
                        // will each see an already-empty dict and remove their
                        // own res_dict entry.
                        res_dict.remove(category);
                    } else {
                        pdf.set_object(cat_ref, Object::Dictionary(cat_dict));
                    }
                } else {
                    // No entry in cat_ref_map (e.g. this page was fully
                    // Auto-skipped and no used-names were collected). Fall back
                    // to the legacy per-group path which leaves the dict untouched
                    // when used_names is empty — a safe conservative choice.
                    let resolved = pdf.resolve_borrowed(cat_ref)?;
                    let Some(mut cat_dict) = resolved.as_dict().cloned() else {
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
                        res_dict.remove(category);
                    } else {
                        pdf.set_object(cat_ref, Object::Dictionary(cat_dict));
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a 1-page PDF whose inherited `/Resources` is an indirect reference.
    /// The terminal `/Font` dict (object 5) carries a used `/F1` and an unused
    /// `/F2`; the single page's content references only `/F1`.
    ///
    /// `resources_ref` is the object the `/Pages` node points its `/Resources`
    /// at, and `obj4_body` is object 4's body — together they select the shape:
    /// - holder chain: `resources_ref = 4`, `obj4_body = "5 0 R"`
    ///   (`/Resources 4 0 R → 5 0 R → <<dict>>`)
    /// - single hop:   `resources_ref = 5`, `obj4_body = "<< >>"`
    ///   (`/Resources 5 0 R → <<dict>>`; object 4 is an inert orphan)
    fn build_inherited_indirect_resources_pdf(resources_ref: u32, obj4_body: &str) -> Vec<u8> {
        let content = b"BT /F1 12 Tf 10 10 Td (hi) Tj ET";
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

        let dicts: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources {resources_ref} 0 R >>"),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>".into(),
            ),
            (4, obj4_body.into()),
            (
                5,
                "<< /Font << \
                 /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
                 /F2 << /Type /Font /Subtype /Type1 /BaseFont /Courier >> \
                 >> >>"
                    .into(),
            ),
        ];
        for (n, s) in &dicts {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        offs.insert(6, out.len() as u64);
        out.extend_from_slice(
            format!("6 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        out.extend_from_slice(content);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_start = out.len() as u64;
        let total = 7u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// Run a prune in `mode` and return the `/Font` sub-dictionary keys of the
    /// terminal `/Resources` object `terminal_obj`.
    fn font_keys_after_prune(
        bytes: Vec<u8>,
        mode: RemoveUnreferencedResources,
        terminal_obj: u32,
    ) -> Vec<String> {
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        remove_unreferenced_resources(&mut pdf, mode).expect("prune should succeed");
        let obj = pdf
            .resolve_borrowed(ObjectRef::new(terminal_obj, 0))
            .expect("terminal obj resolves");
        let res_dict = obj.as_dict().expect("terminal obj is the /Resources dict");
        let font = res_dict
            .get("Font")
            .and_then(Object::as_dict)
            .expect("/Font present");
        font.iter()
            .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
            .collect()
    }

    // Regression (flpdf-12jh): when a page's inherited /Resources lives behind a
    // holder chain, pruning must follow the chain to the terminal dict and remove
    // the unused /F2. Before the resources_location terminal-collapse fix this
    // asserts FAIL — the first-hop ref resolved to another reference, as_dict()
    // returned None, and the prune was a silent no-op that left /F2 in place.
    #[test]
    fn prune_follows_holder_chain_to_terminal_inherited_resources() {
        let keys = font_keys_after_prune(
            build_inherited_indirect_resources_pdf(4, "5 0 R"),
            RemoveUnreferencedResources::Auto,
            5,
        );
        assert!(
            keys.contains(&"F1".to_string()),
            "F1 (used) must remain: {keys:?}"
        );
        assert!(
            !keys.contains(&"F2".to_string()),
            "F2 (unused) must be pruned from the terminal dict: {keys:?}"
        );
    }

    // No-regression guard: a single-hop indirect /Resources (5 0 R -> <<dict>>)
    // must still prune exactly as before the terminal-collapse fix — its terminal
    // ref is the ref itself, so ResourcesLoc::Indirect is unchanged.
    #[test]
    fn prune_single_hop_indirect_inherited_resources_unaffected() {
        let keys = font_keys_after_prune(
            build_inherited_indirect_resources_pdf(5, "<< >>"),
            RemoveUnreferencedResources::Auto,
            5,
        );
        assert!(
            keys.contains(&"F1".to_string()),
            "F1 (used) must remain: {keys:?}"
        );
        assert!(
            !keys.contains(&"F2".to_string()),
            "F2 (unused) must be pruned: {keys:?}"
        );
    }

    /// Build a 2-page PDF where each page reaches one *shared* terminal
    /// `/Resources` dict (object 8 = `<< /Font << /F1 /F2 >> >>`) through its own
    /// distinct first-hop carrier: page A `/Resources 7 0 R → 8 0 R`, page B
    /// `/Resources 9 0 R → 8 0 R`. Page A's content uses only `/F1`, page B's
    /// only `/F2`.
    fn build_shared_terminal_distinct_carriers_pdf() -> Vec<u8> {
        let content_a = b"BT /F1 12 Tf 10 10 Td (A) Tj ET";
        let content_b = b"BT /F2 12 Tf 10 10 Td (B) Tj ET";
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

        let dicts: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
                 /Resources 7 0 R >>"
                    .into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R \
                 /Resources 9 0 R >>"
                    .into(),
            ),
            (7, "8 0 R".into()), // carrier A → shared terminal
            (
                8,
                "<< /Font << \
                 /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
                 /F2 << /Type /Font /Subtype /Type1 /BaseFont /Courier >> \
                 >> >>"
                    .into(),
            ),
            (9, "8 0 R".into()), // carrier B → same shared terminal
        ];
        for (n, s) in &dicts {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        for (n, content) in [(5u32, &content_a[..]), (6u32, &content_b[..])] {
            offs.insert(n, out.len() as u64);
            out.extend_from_slice(
                format!("{n} 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            );
            out.extend_from_slice(content);
            out.extend_from_slice(b"\nendstream\nendobj\n");
        }

        let xref_start = out.len() as u64;
        let total = 10u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    // Over-prune guard (the scenario this issue was deferred over): two retained
    // pages reach ONE shared terminal /Resources via distinct first-hop carriers.
    // Collapsing to the terminal at the source keeps grouping/ref-count terminal-
    // consistent, so the shared dict is detected as shared. Both /F1 (used by A)
    // and /F2 (used by B) must survive — under Auto (shared → skipped) and Yes
    // (union of both pages' used names). A prune-only collapse (grouping left at
    // the first hop) would prune each page against its own single used name and
    // strip the other's font — this test fails in that broken variant.
    #[test]
    fn shared_terminal_via_distinct_carriers_not_over_pruned() {
        for mode in [
            RemoveUnreferencedResources::Auto,
            RemoveUnreferencedResources::Yes,
        ] {
            let keys =
                font_keys_after_prune(build_shared_terminal_distinct_carriers_pdf(), mode, 8);
            assert!(
                keys.contains(&"F1".to_string()) && keys.contains(&"F2".to_string()),
                "shared terminal must keep both fonts in {mode:?} mode: {keys:?}"
            );
        }
    }

    /// Build a 2-page PDF where page B's `/Resources` is an indirect chain that
    /// resolves to `null` (`7 0 R → null`), so both pages inherit the shared
    /// `/Pages` dict (object 8 = `<< /Font << /F1 /F2 >> >>`). Page A uses only
    /// `/F1`, page B only `/F2`.
    fn build_null_chain_resources_pdf() -> Vec<u8> {
        let content_a = b"BT /F1 12 Tf 10 10 Td (A) Tj ET";
        let content_b = b"BT /F2 12 Tf 10 10 Td (B) Tj ET";
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

        let dicts: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Resources 8 0 R >>".into(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>".into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R \
                 /Resources 7 0 R >>"
                    .into(),
            ),
            (7, "null".into()), // page B's /Resources chain resolves to null → absent
            (
                8,
                "<< /Font << \
                 /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
                 /F2 << /Type /Font /Subtype /Type1 /BaseFont /Courier >> \
                 >> >>"
                    .into(),
            ),
        ];
        for (n, s) in &dicts {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        for (n, content) in [(5u32, &content_a[..]), (6u32, &content_b[..])] {
            offs.insert(n, out.len() as u64);
            out.extend_from_slice(
                format!("{n} 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            );
            out.extend_from_slice(content);
            out.extend_from_slice(b"\nendstream\nendobj\n");
        }

        let xref_start = out.len() as u64;
        let total = 9u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    // Correctness guard: a page whose /Resources is an indirect chain resolving
    // to null must fall through to the parent (PDF §7.3.9), exactly as the read
    // side (pages::resolve_inherited_resources) does. Otherwise its used names
    // are keyed on the null ref while it renders from the inherited parent dict,
    // and the parent gets pruned against the *other* page's names only — silently
    // deleting this page's font. Both /F1 (page A) and /F2 (page B) must survive
    // under Auto (shared parent → skipped) and Yes (union). FAILS before the
    // null-terminal fall-through: page B keys on ref 7, the parent (obj 8) is seen
    // as unshared, and /F2 is stripped.
    #[test]
    fn null_chain_resources_falls_through_to_parent_no_corruption() {
        for mode in [
            RemoveUnreferencedResources::Auto,
            RemoveUnreferencedResources::Yes,
        ] {
            let keys = font_keys_after_prune(build_null_chain_resources_pdf(), mode, 8);
            assert!(
                keys.contains(&"F1".to_string()) && keys.contains(&"F2".to_string()),
                "inherited parent dict must keep both fonts in {mode:?} mode: {keys:?}"
            );
        }
    }

    /// Build a minimal doc with a single leaf at object 3 whose body is
    /// `page_body` and whose `/Resources 4 0 R` carrier (object 4) has body
    /// `obj4_body`. Used to drive `resources_location` edge cases directly.
    fn build_page_with_resources_carrier_pdf(page_body: &str, obj4_body: &str) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();
        let dicts: [(u32, &str); 4] = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page_body),
            (4, obj4_body),
        ];
        for (n, s) in dicts {
            offs.insert(n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        let xref_start = out.len() as u64;
        let total = 5u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    // Edge: /Resources chain resolves to null and the node has no /Parent to
    // inherit from — there is nothing to prune, so the location is None.
    #[test]
    fn resources_location_null_chain_without_parent_is_none() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] /Resources 4 0 R >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let loc = resources_location(&mut pdf, ObjectRef::new(3, 0)).expect("ok");
        assert_eq!(loc, ResourcesLoc::None);
    }

    // Edge: /Resources chain resolves to a non-dict, non-null terminal (here an
    // integer) — nothing prunable, so the location is None (no-op).
    #[test]
    fn resources_location_chain_to_non_dict_is_none() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 4 0 R >>",
            "42",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let loc = resources_location(&mut pdf, ObjectRef::new(3, 0)).expect("ok");
        assert_eq!(loc, ResourcesLoc::None);
    }
}
