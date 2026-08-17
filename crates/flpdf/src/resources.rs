//! qpdf correspondence: QPDFPageObjectHelper.cc removeUnreferencedResources traversal split from the page helper.
//! Unreferenced-resource pruning (ISO 32000-1 §7.8.3 / qpdf `--remove-unreferenced-resources`).
//!
//! Scans every page's content stream(s) through qpdf-shaped parser callbacks,
//! collects every resource name that is actually referenced by a PDF operator,
//! and then removes entries from the page's `/Resources` sub-dictionaries that
//! are not referenced.
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
//!
//! The extraction walk below follows qpdf 11.9.0's Form-before-page ordering
//! (`QPDFPageObjectHelper.cc:636-649`). The separate `PageDocumentHelper`
//! facade delegates to its own page-helper route and is intentionally not
//! changed by this extraction-only responsibility.

use crate::content_stream::{parse_content_stream_data, ParseControl, ParserCallbacks};
use crate::filters::decode_stream_data;
use crate::page_object_helper::PageObjectHelper;
use crate::ref_chain::{resolve_ref_chain, terminal_ref_of_chain};
use crate::resource_finder::{ResourceFinder, ResourceNamesByType};
use crate::{Dictionary, Error, Object, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Seek};

/// Resource names referenced by a content scope, keyed by category
/// (`Font`, `XObject`, …) → set of referenced names.
type UsedNames = BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>;

/// qpdf `QPDFPageObjectHelper::removeUnreferencedResources` for one page.
///
/// qpdf first copies an inherited or indirect `/Resources` dictionary onto the
/// page, then shallow-copies the `/Font` and `/XObject` dictionaries it will
/// mutate. This intentionally differs from the CLI-oriented document-wide
/// policy below: each page gets its own mutable resource scope.
pub(crate) fn remove_unreferenced_resources_on_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<()> {
    let (unresolved, any_failures) = remove_unreferenced_resources_in_form_xobjects(pdf, page_ref)?;
    if any_failures {
        return Ok(());
    }

    // The page action itself follows qpdf's canonical PageObjectHelper route:
    // parsing stays on ObjectHandle callbacks and effective resources are
    // copied through getAttribute(copy_if_shared=true). The Form pre-pass
    // above remains separate because qpdf runs it before the page action and
    // uses its unresolved-name accumulator to protect page resources.
    let (finder, resources) = {
        let mut helper = PageObjectHelper::new(page_ref, pdf);
        let mut finder = ResourceFinder::default();
        if helper.parse_contents(&mut finder).is_err()
            || finder.had_diagnostics()
            || finder.has_pending_operands()
        {
            return Ok(());
        }
        let resources = helper.get_resources(true)?;
        (finder, resources)
    };

    if resources.is_null() {
        return Ok(());
    }

    for category in [b"/Font".as_slice(), b"/XObject".as_slice()] {
        let value = resources.get_key(category);
        if value.is_null() {
            continue;
        }
        pdf.resolve_object_handle(&value)?;
        let dictionary = if value.is_indirect() {
            let copy = value.shallow_copy()?;
            resources.replace_key(category, copy.clone())?;
            copy
        } else {
            value
        };
        if dictionary.as_dictionary().is_none() {
            continue;
        }
        let category_name = &category[1..];
        let names = finder
            .names_by_resource_type()
            .get(category_name)
            .map(|entries| entries.keys().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let remove = dictionary
            .as_dictionary()
            .into_iter()
            .flat_map(|entries| entries.into_keys())
            .filter(|name| {
                let resource_name = name.strip_prefix(b"/").unwrap_or(name.as_slice());
                !names
                    .iter()
                    .any(|used_name| used_name.as_slice() == resource_name)
                    && !unresolved
                        .iter()
                        .any(|unresolved_name| unresolved_name.as_slice() == resource_name)
            })
            .collect::<Vec<_>>();
        for name in remove {
            dictionary.remove_key(&name);
        }
        pdf.mark_object_handle_dirty(&dictionary)?;
    }

    pdf.mark_object_handle_dirty(&resources)
}

/// qpdf's Form-XObject target route for
/// `QPDFPageObjectHelper::removeUnreferencedResources`.
///
/// The page route above retains the document helper's unresolved-name
/// accumulator because page resources may be referenced by a resource-less
/// descendant Form. A Form helper has no containing page scope, so its nested
/// Forms are pruned first through the same canonical ObjectHandle parser, then
/// the requested Form is pruned.
pub(crate) fn remove_unreferenced_resources_on_form<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    form: ObjectHandle,
) -> Result<()> {
    let mut nested_forms = Vec::new();
    {
        let mut helper = PageObjectHelper::from_object_handle(form.clone(), pdf);
        helper.for_each_form_xobject(true, |nested, _, _| {
            nested_forms.push(nested);
            Ok(())
        })?;
    }

    for nested in nested_forms {
        prune_canonical_resource_target(pdf, nested)?;
    }
    prune_canonical_resource_target(pdf, form)
}

/// Prune a single canonical page/Form target after its content has been
/// parsed by [`ResourceFinder`]. This is the ObjectHandle counterpart of
/// qpdf's `removeUnreferencedResourcesHelper` resource-dictionary mutation.
fn prune_canonical_resource_target<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    target: ObjectHandle,
) -> Result<()> {
    let (finder, resources) = {
        let mut helper = PageObjectHelper::from_object_handle(target, pdf);
        let mut finder = ResourceFinder::default();
        if helper.parse_contents(&mut finder).is_err()
            || finder.had_diagnostics()
            || finder.has_pending_operands()
        {
            return Ok(());
        }
        let resources = helper.get_resources(true)?;
        (finder, resources)
    };

    if resources.is_null() {
        return Ok(());
    }

    let categories = [b"/Font".as_slice(), b"/XObject".as_slice()];
    let mut dictionaries = Vec::new();
    let mut known_names = BTreeSet::new();
    for category in categories {
        let value = resources.get_key(category);
        if value.is_null() {
            continue;
        }
        pdf.resolve_object_handle(&value)?;
        let dictionary = if value.is_indirect() {
            let copy = value.shallow_copy()?;
            resources.replace_key(category, copy.clone())?;
            copy
        } else {
            value
        };
        let Some(entries) = dictionary.as_dictionary() else {
            continue;
        };
        known_names.extend(
            entries
                .keys()
                .map(|key| key.strip_prefix(b"/").unwrap_or(key.as_slice()).to_vec()),
        );
        dictionaries.push((category, dictionary));
    }

    // qpdf treats an unresolved Font/XObject name as a veto when this target
    // has a Resources dictionary. The known-name set is intentionally shared
    // across both categories, matching ResourceFinder::getNames() in qpdf.
    let local_unresolved = categories
        .iter()
        .filter_map(|category| finder.names_by_resource_type().get(&category[1..]))
        .flat_map(|entries| entries.keys())
        .filter(|name| !known_names.contains(*name))
        .cloned()
        .collect::<BTreeSet<_>>();
    if !local_unresolved.is_empty() && resources.as_dictionary().is_some() {
        return Ok(());
    }

    for (category, dictionary) in dictionaries {
        let Some(entries) = dictionary.as_dictionary() else {
            continue;
        };
        let category_name = &category[1..];
        let names = finder
            .names_by_resource_type()
            .get(category_name)
            .map(|entries| entries.keys().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let remove = entries
            .keys()
            .filter(|key| {
                let name = key.strip_prefix(b"/").unwrap_or(key.as_slice());
                !names.iter().any(|used| used.as_slice() == name)
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in remove {
            dictionary.remove_key(&key);
        }
        pdf.mark_object_handle_dirty(&dictionary)?;
    }

    pdf.mark_object_handle_dirty(&resources)
}

/// Mirror qpdf's `forEachFormXObject(true, ...)` pre-pass for a page-resource
/// scope. Each indirect Form XObject gets its own `/Font` and `/XObject`
/// dictionaries shallow-copied and pruned before the containing page is
/// processed. The traversal follows declared `/XObject` resources rather than
/// only `Do` operators, exactly as qpdf's helper traversal does.
fn remove_unreferenced_resources_in_form_xobjects<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<(BTreeSet<Vec<u8>>, bool)> {
    let Some(page_resources) = crate::pages::resolve_inherited_resources(pdf, page_ref)? else {
        return Ok((BTreeSet::new(), false));
    };
    let mut pending = VecDeque::from(form_xobjects_in_resources(pdf, &page_resources)?);
    let mut visited = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    let mut any_failures = false;

    while let Some(form_ref) = pending.pop_front() {
        if !visited.insert(form_ref) {
            continue;
        }
        let Object::Stream(mut form) = pdf.resolve(form_ref)? else {
            continue; // cov:ignore: form_xobjects_in_resources queues only terminal Stream objects
        };
        if !is_form_xobject(&form.dict) {
            continue; // cov:ignore: form_xobjects_in_resources queues only /Subtype /Form streams
        }
        let mut resources = match form.dict.get("Resources") {
            Some(Object::Dictionary(resources)) => Some(resources.clone()),
            Some(reference @ Object::Reference(_)) => {
                resolve_ref_chain(pdf, reference)?.0.into_dict()
            }
            _ => None,
        };
        let Ok(bytes) = decode_stream_data(&form.dict, &form.data) else {
            any_failures = true;
            if let Some(resources) = resources.as_ref() {
                pending.extend(form_xobjects_in_resources(pdf, resources)?);
            } // cov:ignore: llvm-cov maps the covered child-Form continuation to this closing brace
            continue;
        };
        let Some(used) = collect_used_names_for_form(&bytes) else {
            any_failures = true;
            if let Some(resources) = resources.as_ref() {
                pending.extend(form_xobjects_in_resources(pdf, resources)?);
            } // cov:ignore: llvm-cov maps the covered child-Form continuation to this closing brace
            continue; // cov:ignore: malformed Form regression exercises this path; llvm maps the parser failure to collect_used_names_for_form
        };
        let local_unresolved = unresolved_resource_names(pdf, resources.as_ref(), &used)?;
        unresolved.extend(local_unresolved.iter().cloned());
        // qpdf's forEachFormXObject retains the original child object handle
        // while its action prunes the parent. Capture those children before
        // pruning can remove their names from this Form's /XObject dictionary.
        let child_forms = match resources.as_ref() {
            Some(resources) => form_xobjects_in_resources(pdf, resources)?,
            None => Vec::new(),
        };

        if !local_unresolved.is_empty() && resources.is_some() {
            any_failures = true;
        } else if let Some(resources) = &mut resources {
            prune_font_and_xobject_dictionaries(pdf, resources, &used)?;
            form.dict
                .insert("Resources", Object::Dictionary(resources.clone()));
            pdf.set_object(form_ref, Object::Stream(form));
        }

        pending.extend(child_forms);
    }
    Ok((unresolved, any_failures))
}

/// Return `/Font` and `/XObject` names used by `used` but absent from the
/// current Form resource scope. qpdf deliberately compares the categories as
/// one name set before deciding whether a Form may be pruned.
fn unresolved_resource_names<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: Option<&Dictionary>,
    used: &UsedNames,
) -> Result<BTreeSet<Vec<u8>>> {
    let mut known_names: BTreeSet<Vec<u8>> = BTreeSet::new();
    if let Some(resources) = resources {
        for category in [b"Font".as_slice(), b"XObject".as_slice()] {
            let Some(value) = resources.get(category).cloned() else {
                continue;
            };
            let Some(dictionary) = resolve_ref_chain(pdf, &value)?.0.into_dict() else {
                continue;
            };
            known_names.extend(dictionary.iter().map(|(name, _)| name.to_vec()));
        }
    }

    let mut unresolved = BTreeSet::new();
    for category in [b"Font".as_slice(), b"XObject".as_slice()] {
        for name in used
            .get(category)
            .into_iter()
            .flat_map(|names| names.iter())
        {
            if !known_names.contains(&name.to_vec()) {
                unresolved.insert(name.clone());
            }
        }
    }
    Ok(unresolved)
}

/// Return direct indirect Form XObjects listed in a resource dictionary.
fn form_xobjects_in_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: &Dictionary,
) -> Result<Vec<ObjectRef>> {
    let xobjects = match resources.get("XObject") {
        Some(Object::Dictionary(dict)) => Some(dict.clone()),
        Some(reference @ Object::Reference(_)) => resolve_ref_chain(pdf, reference)?.0.into_dict(),
        _ => None,
    };
    let Some(xobjects) = xobjects else {
        return Ok(Vec::new());
    };
    let mut forms = Vec::new();
    for (_, value) in xobjects.iter() {
        let Object::Reference(reference) = value else {
            continue;
        };
        let (resolved, terminal) = resolve_ref_chain(pdf, &Object::Reference(*reference))?;
        let Object::Stream(stream) = resolved else {
            continue;
        };
        if is_form_xobject(&stream.dict) {
            forms.push(terminal.unwrap_or(*reference));
        }
    }
    Ok(forms)
}

/// Collect only the names used directly by a Form's content stream.
///
/// qpdf's `removeUnreferencedResourcesHelper` parses each Form independently.
/// Its resource-less descendants contribute unresolved names only to the
/// containing page's protection set; they do not keep entries in this Form's
/// own `/Font` or `/XObject` dictionaries.
fn collect_used_names_for_form(stream_bytes: &[u8]) -> Option<UsedNames> {
    let mut used = BTreeMap::new();
    let mut callbacks = ResourceCallbacks {
        finder: ResourceFinder::default(),
        inline_header: None,
        valid_xobjects: BTreeMap::new(),
        complete: true,
    };
    let complete = parse_content_stream_data(stream_bytes, &mut callbacks).is_ok()
        && !callbacks.finder.had_diagnostics()
        && callbacks.complete;
    if complete {
        record_direct_names(&mut used, callbacks.finder.names_by_resource_type(), true);
        Some(used)
    } else {
        None // cov:ignore: unit regression asserts malformed Form streams return None
    }
}

/// Shallow-copy qpdf's mutable resource categories then remove names not used
/// by the directly parsed content stream. Empty category dictionaries remain
/// present, matching qpdf's `removeKey` loop on the category contents.
fn prune_font_and_xobject_dictionaries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: &mut Dictionary,
    used: &UsedNames,
) -> Result<()> {
    for category in [b"Font".as_slice(), b"XObject".as_slice()] {
        let Some(value) = resources.get(category).cloned() else {
            continue;
        };
        let Some(mut dictionary) = resolve_ref_chain(pdf, &value)?.0.into_dict() else {
            continue;
        };
        let names = used.get(category).cloned().unwrap_or_default();
        let remove = dictionary
            .iter()
            .filter(|(name, _)| !names.contains(*name))
            .map(|(name, _)| name.to_vec())
            .collect::<Vec<_>>();
        for name in remove {
            dictionary.remove(&name);
        }
        resources.insert(category, Object::Dictionary(dictionary));
    }
    Ok(())
}

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
            // Auto: skip shared resources — we must not prune the shared
            // page-level dictionary itself, and its own content would only be
            // decoded/tokenised to compute used-names that get discarded
            // regardless, so skip that (potentially expensive, and — with N
            // pages sharing one /Resources — potentially N-times-repeated)
            // work entirely. Still discover and prune every Form XObject
            // declared in the page's /Resources/XObject: qpdf's structural
            // Form discovery (`forEachXObject`, `QPDFPageObjectHelper.cc:
            // 313-345`) is independent of page-level pruning decisions,
            // matching the separate `remove_unreferenced_resources_on_page`
            // route's identical ordering, so it must still run here even
            // though the page's own resources are left untouched.
            prune_declared_forms_for_page(pdf, page_ref)?;
            continue;
        }

        // Collect the used names for this page. When the page's /Contents cannot
        // be fully understood — it failed to decode (corrupt FlateDecode) or to
        // tokenise part-way (malformed content syntax) — collection returns
        // `None` rather than aborting, degrading like the Form XObject path (see
        // `recurse_form_xobject`). Genuine structural errors (resolving the
        // inherited /Resources or a Form object reference) still propagate via
        // `?`. This call also discovers and prunes every Form XObject declared
        // in the page's /Resources/XObject, matching the Auto-shared branch
        // above.
        //
        // Mark the page's resources group as protected so its resources are
        // conservatively retained rather than pruned against an incomplete
        // used-name set.
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

        // Classify `/Resources` and capture the parent ref *by reference*. We
        // only ever need a small `ObjectRef` (or a "this is an inline dict" flag)
        // out of this borrow, so an inline `/Resources` dictionary must not be
        // deep-cloned on this per-page hot path. The `node_obj` borrow ends once
        // these two cheap values are extracted, freeing `&mut pdf` for the
        // holder-chain follow below.
        enum ResKind {
            /// `/Resources` is an indirect reference (the holder-chain head).
            Ref(ObjectRef),
            /// `/Resources` is an inline (direct) dictionary.
            Inline,
            /// `/Resources` is absent or null — inherit from the parent.
            Absent,
            /// `/Resources` is present but neither a reference, dict, nor null —
            /// nothing prunable.
            Other,
        }
        let res_kind = match dict.get("Resources") {
            Some(Object::Reference(r)) => ResKind::Ref(*r),
            Some(Object::Dictionary(_)) => ResKind::Inline,
            Some(Object::Null) | None => ResKind::Absent,
            _ => ResKind::Other,
        };
        let parent_ref: Option<ObjectRef> = match dict.get("Parent") {
            Some(Object::Reference(p)) => Some(*p),
            _ => None,
        };

        match res_kind {
            ResKind::Ref(r) => {
                // Collapse the holder chain (`a 0 R → b 0 R → <<dict>>`) to its
                // terminal so every `ResourcesLoc::Indirect` consumer (grouping,
                // ref-counting, pruning) keys on the terminal — the same dict the
                // read side (`pages::resolve_inherited_resources`) resolves to.
                // Keying on the first hop instead made `prune_resources_object`
                // resolve a ref-to-a-ref, see a non-dict, and silently skip
                // pruning. A single-hop ref's terminal is itself, so this
                // preserves the prior result for that case.
                //
                // `terminal_ref_of_chain` walks by borrow and returns only the
                // ref, so the terminal `/Resources` dictionary — re-inspected by
                // borrow just below — is never cloned on this per-page hot path.
                let terminal_ref = terminal_ref_of_chain(pdf, r)?;
                match pdf.resolve_borrowed(terminal_ref)? {
                    // A chain resolving to null is "absent" (PDF §7.3.9): fall
                    // through to the parent, matching the `Absent` arm and the
                    // read side. Returning `Indirect(terminal)` would key this
                    // page's used names on the null ref while it renders from the
                    // inherited parent dict, so the parent could be pruned against
                    // only the other pages' names and lose this page's resources.
                    Object::Null => match parent_ref {
                        Some(parent) => current = parent,
                        None => return Ok(ResourcesLoc::None),
                    },
                    Object::Dictionary(_) => return Ok(ResourcesLoc::Indirect(terminal_ref)),
                    // A non-dict, non-null terminal has nothing prunable; the
                    // read side rejects it, but here we simply skip (no-op).
                    _ => return Ok(ResourcesLoc::None),
                }
            }
            ResKind::Inline => {
                // Inline dict: distinguish page-level vs ancestor.
                if current == page_ref {
                    return Ok(ResourcesLoc::PageInline);
                } else {
                    return Ok(ResourcesLoc::AncestorInline(current));
                }
            }
            ResKind::Absent => match parent_ref {
                // Fall through to parent.
                Some(parent) => current = parent,
                None => return Ok(ResourcesLoc::None),
            },
            ResKind::Other => return Ok(ResourcesLoc::None),
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
    // Resolving the inherited /Resources is a structural operation, not content
    // comprehension: propagate its errors rather than silently degrading.
    let page_resources = crate::pages::resolve_inherited_resources(pdf, page_ref)?;

    let mut used: UsedNames = BTreeMap::new();
    let mut form_used: BTreeMap<ObjectRef, UsedNames> = BTreeMap::new();
    let mut visited_forms: BTreeSet<(ObjectRef, ObjectRef)> = BTreeSet::new();

    // `collect_from_stream` returns `false` when tokenisation stopped early on a
    // malformed token: the collected names are then incomplete → retain. The
    // page records its own directly-referenced names and owns the initial scope
    // (`owner = page_ref`). Form-owned names are kept separately so the Form's
    // own `/Font` and `/XObject` dictionaries can be pruned after its complete
    // content has been collected.
    let complete = {
        let mut ctx = CollectCtx {
            pdf,
            used: &mut used,
            form_used: &mut form_used,
            visited: &mut visited_forms,
        };
        let page_scope = Scope {
            resources: page_resources.as_ref(),
            target: UsedTarget::Page,
            owner: page_ref,
        };
        // A failure to *decode* the page's /Contents (corrupt filter) is a
        // content-comprehension failure -- the page's own resource usage is
        // unknown, so its /Font,/XObject dictionaries must be conservatively
        // retained. But qpdf's structural Form discovery (`forEachXObject`,
        // `QPDFPageObjectHelper.cc:313-345`) is independent of content-stream
        // decoding, so the page's declared Forms must still be discovered and
        // recursed into so their own resources are pruned -- mirroring
        // `recurse_form_xobject`'s identical decode-failure handling above
        // (`Err(_) => complete = false` followed by unconditional declared-child
        // discovery).
        match crate::pages::page_content_bytes(ctx.pdf, page_ref) {
            Ok(content_bytes) => collect_from_stream(&mut ctx, &content_bytes, page_scope, 0)?,
            Err(_) => {
                for (_, val) in declared_xobjects(ctx.pdf, page_scope.resources)? {
                    recurse_form_xobject(&mut ctx, val, page_scope, 0)?;
                }
                false
            }
        }
    };

    Ok(complete.then_some(used))
}

/// Discover and prune every Form XObject declared in a page's own
/// `/Resources/XObject`, without decoding or tokenising the page's own
/// content stream.
///
/// This is the cheap half of [`collect_used_names_for_page`], split out for
/// callers (Auto mode's shared-page skip) that need the page-content-independent
/// structural Form-pruning side effect (qpdf's `forEachXObject`,
/// `QPDFPageObjectHelper.cc:313-345`) but will discard the page's own
/// used-names regardless, so decoding/tokenising its (potentially large)
/// content stream would be pure waste.
fn prune_declared_forms_for_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<()> {
    let page_resources = crate::pages::resolve_inherited_resources(pdf, page_ref)?;

    let mut used: UsedNames = BTreeMap::new();
    let mut form_used: BTreeMap<ObjectRef, UsedNames> = BTreeMap::new();
    let mut visited_forms: BTreeSet<(ObjectRef, ObjectRef)> = BTreeSet::new();
    let mut ctx = CollectCtx {
        pdf,
        used: &mut used,
        form_used: &mut form_used,
        visited: &mut visited_forms,
    };
    let page_scope = Scope {
        resources: page_resources.as_ref(),
        target: UsedTarget::Page,
        owner: page_ref,
    };
    for (_, val) in declared_xobjects(ctx.pdf, page_scope.resources)? {
        recurse_form_xobject(&mut ctx, val, page_scope, 0)?;
    }
    Ok(())
}

/// Maximum recursion depth for Form XObject traversal.
///
/// Each `(Form, scope)` pair is expanded at most once (`visited` is an ever-seen
/// set), so cycles and shared/DAG references cannot loop. This cap is a
/// stack-overflow safeguard for content that nests a very long chain of
/// *distinct* Form XObjects.
const MAX_FORM_DEPTH: usize = 64;

/// State threaded unchanged through every level of the (mutually recursive)
/// resource-name collection walk: the reader, the used-name accumulator, and the
/// dedup set. Only the per-call [`Scope`] and `depth` vary, so they are passed
/// separately. (Keeping the invariant borrows in one parameter also keeps the
/// recursive call sites on a single line, which stops rustfmt from reflowing a
/// `)?` onto its own line and fragmenting its coverage region.)
struct CollectCtx<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    /// The page's real used-name accumulator — the bubble target for the names
    /// of resource-less Form XObjects, wherever they sit in the Form tree.
    used: &'a mut UsedNames,
    /// Ever-seen set of expanded `(Form ref, scope owner)` pairs — the traversal
    /// dedup guard. Keying on the pair (not the Form alone) lets a resource-less
    /// Form reached under diverging scopes be explored once per scope. See the
    /// `recurse_form_xobject` "Traversal bound" doc for the full rationale.
    visited: &'a mut BTreeSet<(ObjectRef, ObjectRef)>,
    /// Directly referenced names for each Form with its own `/Resources`.
    /// qpdf 11.9.0's `removeUnreferencedResourcesHelper` shallow-copies and
    /// prunes only `/Font` and `/XObject` in that Form's own scope
    /// (`QPDFPageObjectHelper.cc:539-633`), so these names must not be merged
    /// into the page accumulator.
    form_used: &'a mut BTreeMap<ObjectRef, UsedNames>,
}

/// Where direct resource names discovered by the content walker belong.
///
/// qpdf distinguishes the page scope from a Form's own `/Resources` scope. A
/// resource-less Form uses [`UsedTarget::Page`] and bubbles its names outward;
/// an own-resources Form is pruned from its own accumulator.
#[derive(Clone, Copy)]
enum UsedTarget {
    Page,
    Form(ObjectRef),
}

/// The resource scope in force while walking a content stream.
#[derive(Clone, Copy)]
struct Scope<'a> {
    /// The `/Resources` dict that names in this stream resolve against.
    resources: Option<&'a Dictionary>,
    /// The accumulator receiving this stream's direct names. Resource-less
    /// Forms target the page; own-resources Forms target their own object.
    target: UsedTarget,
    /// Identity of the scope for `visited` keying: the page, or the `ObjectRef`
    /// of the nearest Form with its own `/Resources`. A resource-less Form
    /// inherits its caller's `owner`; an own-`/Resources` Form becomes its own.
    owner: ObjectRef,
}

/// Parser callback that delegates ordinary resource classification to
/// [`ResourceFinder`] and tracks the special `BI`...`ID` inline-image header
/// scope.
///
/// Inline-image payload objects are deliberately ignored. Header names are
/// ordinary parser object events, so `/CS` and `/ColorSpace` are interpreted
/// here without recreating byte or token boundaries.
struct ResourceCallbacks {
    finder: ResourceFinder,
    inline_header: Option<Vec<Object>>,
    valid_xobjects: BTreeMap<Vec<u8>, usize>,
    complete: bool,
}

impl ResourceCallbacks {
    fn finish_inline_header(&mut self, header: Vec<Object>, offset: usize) -> bool {
        let mut chunks = header.chunks_exact(2);
        let mut color_space = None;
        for pair in &mut chunks {
            let Some(key) = pair[0].as_name() else {
                return false;
            };
            if matches!(key, b"CS" | b"ColorSpace") {
                color_space = pair[1].as_name().map(<[u8]>::to_vec);
            }
        }
        if !chunks.remainder().is_empty() {
            return false;
        }

        if let Some(name) = color_space.filter(|name| !is_builtin_inline_image_cs(name)) {
            self.finder
                .record_resource_name(b"ColorSpace", &name, offset);
        } // cov:ignore: llvm-cov gap region after the covered ColorSpace insertion
        true
    }

    fn stop_incomplete(&mut self) -> Result<ParseControl> {
        self.complete = false;
        Ok(ParseControl::Stop)
    }
}

impl ParserCallbacks for ResourceCallbacks {
    fn handle_diagnostic(&mut self, offset: usize, message: &str) -> Result<()> {
        self.finder.handle_diagnostic(offset, message)?;
        self.complete = false;
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl> {
        self.finder
            .handle_object_borrowed(&object, offset, length)?;
        match object {
            Object::Operator(operator) if self.inline_header.is_some() => {
                let header = self
                    .inline_header
                    .take()
                    .expect("inline_header guard guarantees a header");
                if operator != b"ID" || !self.finish_inline_header(header, offset) {
                    return self.stop_incomplete();
                }
                Ok(ParseControl::Continue)
            }
            Object::Operator(operator) if operator == b"BI" => {
                if !self.finder.last_operator_started_at_boundary() {
                    return self.stop_incomplete();
                }
                self.inline_header = Some(Vec::new());
                Ok(ParseControl::Continue)
            }
            Object::Operator(operator) if operator == b"ID" => self.stop_incomplete(),
            Object::Operator(operator) => {
                if operator == b"Do" && self.complete {
                    if let Some(name) = self.finder.last_name() {
                        if !self.valid_xobjects.contains_key(name) {
                            self.valid_xobjects.insert(name.to_vec(), offset);
                        }
                    }
                }
                Ok(ParseControl::Continue)
            }
            Object::InlineImage(_) => Ok(ParseControl::Continue),
            operand => {
                if let Some(header) = &mut self.inline_header {
                    header.push(operand);
                }
                Ok(ParseControl::Continue)
            }
        }
    }

    fn handle_eof(&mut self) -> Result<()> {
        self.finder.handle_eof()?;
        if self.inline_header.is_some() || self.finder.has_pending_operands() {
            self.complete = false;
        }
        Ok(())
    }
}

/// Result of parsing a content stream before walking its referenced Forms.
///
/// `ResourceCallbacks::valid_xobjects` (the `Do`-invoked names) is
/// deliberately not surfaced here: the recursion-triggering walk uses the
/// declared `/XObject` resource-dictionary keys instead (see
/// [`declared_xobjects`]), matching qpdf's `forEachXObject`
/// (`QPDFPageObjectHelper.cc:313-345`), which is a static structural walk over
/// `xobj_dict.getKeys()` independent of content-stream parsing. Every name a
/// `Do` operator could legally invoke is necessarily a declared key, so the
/// declared set is a superset and `Do`-invoked names add nothing here.
struct ParsedContent {
    complete: bool,
    names: ResourceNamesByType,
}

/// Tokenise `stream_bytes` and collect its direct resource events without
/// resolving any child object yet.
fn parse_resource_content(stream_bytes: &[u8]) -> ParsedContent {
    let mut callbacks = ResourceCallbacks {
        finder: ResourceFinder::default(),
        inline_header: None,
        valid_xobjects: BTreeMap::new(),
        complete: true,
    };
    let parse_result = parse_content_stream_data(stream_bytes, &mut callbacks);
    let complete =
        parse_result.is_ok() && !callbacks.finder.had_diagnostics() && callbacks.complete;
    let names = callbacks.finder.names_by_resource_type().clone();
    ParsedContent { complete, names }
}

/// Return the declared `/XObject` resource-dictionary entries in `resources`,
/// each already resolved to its `(name, value)` pair.
///
/// Matches qpdf's `QPDFPageObjectHelper::forEachXObject`
/// (`QPDFPageObjectHelper.cc:313-345`): `xobj_dict` is fetched once —
/// `getAttribute("/Resources", false).getKeyIfDict("/XObject")` — and then
/// `for (auto const& key: xobj_dict.getKeys()) { QPDFObjectHandle obj =
/// xobj_dict.getKey(key); ... }` reads both the key and its value from that
/// single resolved dict. Returning the value alongside the name here (rather
/// than making the caller re-resolve the `/XObject` category per name) is the
/// same "one held dict, per-key value from it" shape — resolving the category
/// separately for each of its own N entries would make an O(N)-key dict cost
/// O(N) full-dict resolves.
///
/// Every key declared in the current scope's `/XObject` category is a
/// recursion candidate, independent of whether the content stream ever
/// invokes it with a `Do` operator. A Form declared but never invoked is
/// still queued and walked by qpdf's traversal, so it must be here too.
///
/// The `/XObject` category itself may be a direct dictionary or an indirect
/// reference (resolved the same way `form_xobjects_in_resources` resolves the
/// category for the sibling "Form-owned resources" pruning pass).
fn declared_xobjects<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: Option<&Dictionary>,
) -> Result<Vec<(Vec<u8>, Object)>> {
    let xobj_dict = match resources.and_then(|res| res.get("XObject")) {
        Some(Object::Dictionary(xobj_dict)) => Some(xobj_dict.clone()),
        Some(cat_ref @ Object::Reference(_)) => resolve_ref_chain(pdf, cat_ref)?.0.into_dict(),
        _ => None,
    };
    Ok(xobj_dict
        .map(|dict| {
            dict.iter()
                .map(|(name, val)| (name.to_vec(), val.clone()))
                .collect()
        })
        .unwrap_or_default())
}

/// Record direct resource names in the page or owning Form accumulator.
fn record_names_for_target(
    ctx: &mut CollectCtx<'_, impl Read + Seek + 'static>,
    target: UsedTarget,
    names: &ResourceNamesByType,
) {
    match target {
        UsedTarget::Page => record_direct_names(ctx.used, names, true),
        UsedTarget::Form(form_ref) => {
            let used = ctx.form_used.entry(form_ref).or_default();
            record_direct_names(used, names, true);
        }
    }
}

/// Core recursive walker: tokenises `stream_bytes` and records every resource
/// reference into the accumulator selected by `scope` (see [`Scope`]). qpdf
/// 11.9.0 parses a Form with its own `/Resources` in that local scope and
/// shallow-copies `/Font` and `/XObject` before pruning them
/// (`QPDFPageObjectHelper.cc:539-633`); resource-less descendants instead
/// contribute unresolved names to the containing page's protection set. Nested
/// Form XObjects are still traversed regardless, so a resource-less Form nested
/// inside an own-resources Form still contributes names to the page.
///
/// Recursion targets are `scope`'s *declared* `/XObject` resource-dictionary
/// keys (see [`declared_xobjects`]), not the `Do`-invoked names found
/// while tokenising this stream — qpdf's `forEachXObject` walk
/// (`QPDFPageObjectHelper.cc:313-345`) is independent of content-stream
/// parsing. Every declared child is visited even after an earlier child comes
/// back incomplete: qpdf's `forEachFormXObject` visits every queued Form
/// unconditionally, and only the accumulated failure gates the final
/// page-level prune (`QPDFPageObjectHelper.cc:636-649`), so this walk must not
/// short-circuit either.
///
/// Returns `Ok(true)` when the stream was tokenised to the end, `Ok(false)` when
/// tokenisation stopped early on a malformed token (so `ctx.used` is incomplete
/// and the page must be conservatively retained). Structural errors from
/// nested Form XObject resolution propagate as `Err`.
fn collect_from_stream<R: Read + Seek>(
    ctx: &mut CollectCtx<'_, R>,
    stream_bytes: &[u8],
    scope: Scope<'_>,
    depth: usize,
) -> Result<bool> {
    let parsed = parse_resource_content(stream_bytes);
    let mut complete = parsed.complete;

    if parsed.complete {
        record_names_for_target(ctx, scope.target, &parsed.names);
    }

    for (_, val) in declared_xobjects(ctx.pdf, scope.resources)? {
        if !recurse_form_xobject(ctx, val, scope, depth)? {
            complete = false;
        }
    }
    Ok(complete)
}

fn record_direct_names(used: &mut UsedNames, names: &ResourceNamesByType, record_direct: bool) {
    if !record_direct {
        return;
    }
    for &category in RESOURCE_CATEGORIES {
        let category = category.as_bytes();
        for name in names
            .get(category)
            .into_iter()
            .flat_map(|by_name| by_name.keys())
        {
            if category == b"ColorSpace" && is_builtin_color_space_cs_op(name) {
                continue;
            }
            used.entry(category.to_vec())
                .or_default()
                .insert(name.clone());
        }
    }
}

/// Whether an (already resource-resolved) XObject stream dict is a Form XObject
/// (`/Subtype /Form`).
fn is_form_xobject(dict: &Dictionary) -> bool {
    matches!(dict.get("Subtype"), Some(Object::Name(n)) if n.as_slice() == b"Form")
}

/// If `xobj_val` (the value of a declared `/XObject` entry — see
/// [`declared_xobjects`]) is a Form XObject, decode and recurse into its
/// content stream.
///
/// Scoping rule (PDF spec §8.10.4):
/// - If the Form XObject has its own `/Resources` entry, resource names used
///   inside the Form resolve against the Form's own resources dict. Those names
///   must NOT be added to the calling page's `used` set — doing so would cause
///   page-level resources with the same name (but unused by the page itself) to
///   be incorrectly retained when pruning. The Form's content is still walked to
///   reach nested resource-less Form XObjects, whose names ARE page-relevant.
/// - If the Form XObject has no `/Resources` entry, it inherits the calling
///   scope's resources; names referenced inside the Form are attributed to the
///   page's `used` set.
///
/// # Traversal bound
///
/// `visited` is an *ever-seen* set of `(Form ref, scope owner)` pairs. A Form
/// with its own `/Resources` resolves its names against its own scope, so it is
/// keyed as `(ref, ref)` and expanded once regardless of how it is reached; a
/// resource-less Form resolves nested `Do` names against the inherited scope, so
/// it is keyed as `(ref, inherited scope owner)` and expanded once per distinct
/// scope it is reached under. This bounds shared / DAG-shaped Form trees (e.g. a
/// `/Fm(i) Do /Fm(i) Do` chain, which a "currently-on-stack" set would
/// re-traverse `2^depth` times) without collapsing scope-divergent resolution:
/// a resource-less Form reached under two scopes that resolve its nested names
/// differently contributes each scope's names, so no page-rendered resource is
/// dropped. Bounded by O(V²) pairs (no exponential blow-up); realistic documents
/// have few distinct scopes, so it is ~O(V).
///
/// # Direct-stream Form XObjects
///
/// The `/Resources/XObject` entry may also be a direct (inline) `Object::Stream`
/// rather than an `Object::Reference`. Inline streams are malformed per ISO
/// 32000-2 §7.3.8 (a stream shall be an indirect object); qpdf does not even
/// parse them as Form XObjects. Such a stream carries no `ObjectRef` identity,
/// so the `visited` guard cannot dedup a self-reference reached through
/// inherited resources — recursing would blow up exponentially. We therefore do
/// not recurse into direct-stream Forms and instead report the page as
/// incomplete (`Ok(false)`), conservatively retaining its resources. This
/// avoids both the DoS and dropping fonts the Form uses.
///
/// # Discovery vs. pruning order for nested Forms
///
/// qpdf's `removeUnreferencedResourcesHelper` (`QPDFPageObjectHelper.cc:539-633`)
/// parses a Form's own content and, only if that parse succeeds *and* leaves no
/// unresolved `/Font`/`/XObject` name, shallow-copies and filters that Form's
/// own `/Font`/`/XObject` dictionaries down to the names its content actually
/// uses (`QPDFPageObjectHelper.cc:565-633`). Three cases return `false` *before*
/// that filtering step runs — the content parse throws, the content parse
/// records a new warning, or an unresolved name is present — and in every one
/// of them the Form's own `/Resources` sub-dictionaries are left untouched.
///
/// `forEachXObject`'s BFS (`QPDFPageObjectHelper.cc:313-345`) discovers a
/// Form's *own* declared children by reading its *current* `/XObject` dict when
/// that Form reaches the front of the traversal queue. That Form's own
/// filtering step (above) runs earlier and synchronously, the moment it is
/// *discovered* as some ancestor's declared child (as the `action` callback
/// `forEachXObject` invokes) — strictly before it is later popped from the
/// queue to have its own children discovered. So whether a Form's children are
/// discovered from its post-filter or pre-filter `/XObject` dict depends on
/// whether that Form's own filtering step ran:
/// - Ran (content parsed, no unresolved names): children are discovered from
///   the **post-filter** dict — a declared-but-never-`Do`-invoked child's key
///   is already gone, so it is never discovered or visited, and its own
///   resources are never pruned. Verified empirically against qpdf 11.9.0
///   (`--remove-unreferenced-resources=yes --pages ... --preserve-unreferenced`):
///   a declared-but-uninvoked child Form nested inside a `Do`-invoked parent is
///   completely absent from the live-reachable resource graph, orphaned
///   alongside the parent's original, now-superseded `/Resources` object.
/// - Did not run (this function's own `decode_stream_data`/
///   `parse_resource_content` failed, or an unresolved name vetoed pruning):
///   children are discovered from the **pre-filter** dict — every declared
///   child, `Do`-invoked or not. Verified the same way with the parent's own
///   content made unparseable: the child Form still has its own unused Font
///   pruned, despite the parent's decode failure.
///
/// The page itself is the sole exception to "a Form's own filtering-ran state
/// gates its children's discovery snapshot": the page is not reached via this
/// function, and its own filtering is deferred to *after* the entire
/// `forEachFormXObject` traversal completes (`QPDFPageObjectHelper.cc:636-649`),
/// so the page's declared children (`collect_from_stream`'s top-level call)
/// are always discovered from its pristine, unfiltered `/XObject` dict.
///
/// # Return value
///
/// Returns `Ok(true)` when the referenced Form (if any) was tokenised completely
/// or nothing page-relevant could be lost (non-Form / already-visited /
/// depth-limit). Returns `Ok(false)` when the page's `used` set may be
/// incomplete — a decode/tokenise failure whose names feed the page, or a
/// direct-stream Form — signalling the page must be conservatively retained.
/// Structural resolution errors propagate as `Err`.
fn recurse_form_xobject<R: Read + Seek>(
    ctx: &mut CollectCtx<'_, R>,
    xobj_val: Object,
    caller: Scope<'_>,
    depth: usize,
) -> Result<bool> {
    // Stack-overflow safeguard for a very long chain of *distinct* Forms.
    // Cycles / shared references are already bounded by the ever-seen
    // `ctx.visited` set below, so this is not the DoS guard — it caps recursion
    // depth only. Report the page as incomplete rather than complete: cutting the
    // walk off here means a resource-less Form beyond the limit could reference a
    // page resource we never recorded, so the page must be conservatively
    // retained rather than pruned against a partial used-name set.
    if depth >= MAX_FORM_DEPTH {
        return Ok(false);
    }

    // Resolve to a Stream. An indirect XObject may be reached through more than
    // one indirect hop (ref -> ref -> stream); follow the chain to its terminal.
    // A direct (inline) stream is handled separately: it has no `ObjectRef`
    // identity, so the `visited` guard cannot protect against a self-reference
    // reached through inherited resources. Rather than recurse (unbounded) or
    // prune (dropping fonts the Form uses), we report the page as incomplete so
    // its resources are conservatively retained. qpdf never parses an inline
    // stream as a Form, so there is no byte-identical target for this case.
    let (mut stream, xobj_ref) = match xobj_val {
        Object::Reference(r) => {
            // Key the `visited` set on the *terminal* ref, not the first hop, so a
            // Form reached via distinct reference chains (`r_a -> T`, `r_b -> T`)
            // is expanded once — matching qpdf, which keys on the terminal
            // object's `QPDFObjGen`. First-hop keying would re-expand the shared
            // terminal once per aliasing name (redundant work, and the only lever
            // that could re-walk a subtree once inherited scopes diverge);
            // terminal keying makes each Form's expansion path-independent.
            let (resolved, terminal) = resolve_ref_chain(ctx.pdf, &Object::Reference(r))?;
            match resolved.into_stream() {
                Some(s) => (s, terminal.unwrap_or(r)),
                None => return Ok(true), // non-stream terminal → nothing to recurse into
            }
        }
        Object::Stream(s) => {
            // A direct-stream Form → retain the page (`Ok(false)`); a direct-stream
            // non-Form (e.g. an inline image) → nothing to recurse into (`Ok(true)`).
            return Ok(!is_form_xobject(&s.dict));
        }
        _ => return Ok(true), // not a reference or stream → nothing to recurse into
    };

    // Only recurse into Form XObjects.
    if !is_form_xobject(&stream.dict) {
        return Ok(true);
    }

    // Determine whether the Form has its own /Resources entry (checked before the
    // visited key and before decode so both use the right scope / completeness).
    // Resolve the Form's own `/Resources` to a dictionary, if it has one. The
    // entry may be a direct dict or a holder chain (ref -> ref -> dict), which we
    // follow to its terminal — the same way the `/XObject` category is resolved
    // above; a single-hop read would drop a chained /Resources.
    //
    // Whether the Form *owns* a resource scope is then `is_some()`, NOT bare key
    // presence: per ISO 32000-2 §7.3.9 a `null` value — direct (`/Resources null`)
    // or at the end of the chain (`/Resources 7 0 R` → `null`) — is equivalent to
    // an absent entry, and so is any non-dictionary value. Treating those as owned
    // would walk the Form with `record_direct = false` and an empty scope, so a
    // page-inherited name it uses would never be recorded → over-prune. (Resolved
    // objects are cached, so re-resolving on a later visit is O(1).)
    let mut form_resources: Option<Dictionary> = match stream.dict.get("Resources") {
        Some(Object::Dictionary(d)) => Some(d.clone()),
        Some(res_ref @ Object::Reference(_)) => resolve_ref_chain(ctx.pdf, res_ref)?.0.into_dict(),
        _ => None,
    };
    let form_has_own_resources = form_resources.is_some();

    // Snapshot this Form's own /Resources before its own filtering step
    // (below) can prune it. Whichever of this snapshot or the (possibly
    // filtered) `form_resources` drives discovery of this Form's own children
    // depends on whether that filtering step actually ran — see "Discovery vs.
    // pruning order for nested Forms" on this function's doc.
    let declared_child_resources = form_resources.clone();

    // A Form with its own /Resources is scope-independent (owner = itself); a
    // resource-less Form inherits the caller's scope owner. (See "Traversal
    // bound" for why the owner keys the dedup below.)
    let scope_owner = if form_has_own_resources {
        xobj_ref
    } else {
        caller.owner
    };

    // Ever-seen dedup: insert the (Form, scope owner) pair and NEVER remove, so
    // each pair is expanded at most once. Keyed *before* decode so a decode
    // failure also marks the pair seen — decoding depends only on the bytes, not
    // the path.
    if !ctx.visited.insert((xobj_ref, scope_owner)) {
        return Ok(true); // already expanded under this scope
    }

    let child_target = if form_has_own_resources {
        // The Form owns its scope (`owner = scope_owner = xobj_ref`); its direct
        // names are accumulated for local pruning, while nested resource-less
        // Forms still bubble to the page.
        UsedTarget::Form(xobj_ref)
    } else {
        // No own /Resources → the Form inherits the caller's scope and owner and
        // attributes its names to the page's used set.
        UsedTarget::Page
    };

    // Decode this Form's content stream and, on success, parse it and (for a
    // Form with its own /Resources) filter its own /Font,/XObject dictionaries
    // down to the names it actually uses — mirroring qpdf's three
    // early-return-`false` sites in `removeUnreferencedResourcesHelper` that
    // all precede its filtering step (`QPDFPageObjectHelper.cc:539-633`): a
    // decode failure here corresponds to qpdf's content-parse throwing; an
    // incomplete tokenisation corresponds to qpdf's content-parse recording a
    // new warning; an unresolved name is the same check qpdf makes.
    // `pruning_ran` records which case this was: it decides whether `child`
    // below reads the post-filter or pre-filter /XObject snapshot (see
    // "Discovery vs. pruning order for nested Forms"). qpdf's structural
    // `forEachXObject` walk never decodes content at all
    // (`QPDFPageObjectHelper.cc:313-345`), so a decode/parse failure here still
    // yields `complete = false` (the page must be conservatively retained) but
    // must not skip discovering and recursing into this Form's declared
    // children below.
    let mut complete;
    let mut pruning_ran = false;
    match decode_stream_data(&stream.dict, &stream.data) {
        Ok(form_bytes) => {
            // Parse the current Form before visiting children. qpdf's
            // `forEachFormXObject(true, ...)` invokes the parent Form action
            // before its queued children (`QPDFPageObjectHelper.cc:318-343`),
            // and a later child failure does not roll back a successful parent
            // prune (`QPDFPageObjectHelper.cc:636-649`).
            let parsed = parse_resource_content(&form_bytes);
            complete = parsed.complete;
            if parsed.complete {
                record_names_for_target(ctx, child_target, &parsed.names);

                // qpdf also refuses to prune a Form with an unresolved
                // Font/XObject name in its own dictionary
                // (`QPDFPageObjectHelper.cc:575-633`); mark the page walk
                // incomplete so its resources remain conservative in that case.
                if form_has_own_resources {
                    let mut resources = form_resources
                        .take()
                        .expect("own Form resources were resolved");
                    let local_used = ctx.form_used.remove(&xobj_ref).unwrap_or_default();
                    if unresolved_resource_names(ctx.pdf, Some(&resources), &local_used)?.is_empty()
                    {
                        prune_font_and_xobject_dictionaries(ctx.pdf, &mut resources, &local_used)?;
                        stream
                            .dict
                            .insert("Resources", Object::Dictionary(resources.clone()));
                        ctx.pdf.set_object(xobj_ref, Object::Stream(stream));
                        form_resources = Some(resources);
                        pruning_ran = true;
                    } else {
                        complete = false;
                    }
                }
            }
        }
        Err(_) => {
            // The Form's own content is unreadable, so its resource usage is
            // unknown and the page (or Form) it feeds must be conservatively
            // retained. `pruning_ran` stays `false`: this Form's own
            // /Font,/XObject dictionaries were never touched, so its declared
            // children are still discovered below from the pre-filter snapshot.
            complete = false;
        }
    }

    // `child.resources` drives discovery of this Form's own declared children
    // below: the post-filter `form_resources` if this Form's own filtering
    // step ran, the pre-filter `declared_child_resources` snapshot if it did
    // not — see "Discovery vs. pruning order for nested Forms" above. This is
    // unrelated to what gets *written* to `ctx.pdf`: any write-back above
    // already used the filtered `resources`/`form_resources`; only the
    // discovery scope changes here.
    let child = Scope {
        resources: if !form_has_own_resources {
            caller.resources
        } else if pruning_ran {
            form_resources.as_ref()
        } else {
            declared_child_resources.as_ref()
        },
        target: child_target,
        owner: scope_owner,
    };

    // The child walk's completeness is the page's completeness: an incomplete
    // tokenisation or child resolution may have missed a page-relevant name, so
    // it propagates up and the page is conservatively retained. Child traversal
    // still runs after a malformed parent, matching qpdf's independent Form
    // queue and allowing a valid child to be processed.
    //
    // Children are this Form's *declared* `/XObject` entries (see
    // `declared_xobjects`), not just its `Do`-invoked names — qpdf's
    // `forEachFormXObject` queues every declared Form regardless of `Do` usage
    // (`QPDFPageObjectHelper.cc:313-345`). Every declared child is visited even
    // after an earlier sibling comes back incomplete: qpdf's traversal never
    // short-circuits on one Form's failure (`QPDFPageObjectHelper.cc:636-649`),
    // it only accumulates `any_failures` for the final page-level prune.
    for (_, val) in declared_xobjects(ctx.pdf, child.resources)? {
        if !recurse_form_xobject(ctx, val, child, depth + 1)? {
            complete = false;
        }
    }

    Ok(complete)
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
        carriers: &[(u32, u32)],
    ) -> Vec<String> {
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        for &(from, to) in carriers {
            pdf.set_object(
                ObjectRef::new(from, 0),
                Object::Reference(ObjectRef::new(to, 0)),
            );
        }
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
            &[(4, 5)],
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
            &[],
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
            let keys = font_keys_after_prune(
                build_shared_terminal_distinct_carriers_pdf(),
                mode,
                8,
                &[(7, 8), (9, 8)],
            );
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
            let keys = font_keys_after_prune(build_null_chain_resources_pdf(), mode, 8, &[]);
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

    // Edge: /Resources is a *direct* non-dict, non-reference value (here an
    // integer) — the `ResKind::Other` arm; nothing prunable, location is None.
    #[test]
    fn resources_location_direct_non_dict_is_none() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 42 >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let loc = resources_location(&mut pdf, ObjectRef::new(3, 0)).expect("ok");
        assert_eq!(loc, ResourcesLoc::None);
    }

    // Edge: no /Resources anywhere and no /Parent to walk to — the `ResKind::Absent`
    // arm with no parent ref; location is None.
    #[test]
    fn resources_location_absent_without_parent_is_none() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let loc = resources_location(&mut pdf, ObjectRef::new(3, 0)).expect("ok");
        assert_eq!(loc, ResourcesLoc::None);
    }

    fn collect_test_content(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        content: &[u8],
        resources: Option<&Dictionary>,
    ) -> Result<(bool, UsedNames)> {
        let mut used = UsedNames::new();
        let mut form_used = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let complete = {
            let mut ctx = CollectCtx {
                pdf,
                used: &mut used,
                form_used: &mut form_used,
                visited: &mut visited,
            };
            let scope = Scope {
                resources,
                target: UsedTarget::Page,
                owner: ObjectRef::new(3, 0),
            };
            collect_from_stream(&mut ctx, content, scope, 0)?
        };
        Ok((complete, used))
    }

    fn malformed_form_pdf_and_resources() -> (Pdf<Cursor<Vec<u8>>>, Dictionary) {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "<0g>",
        );
        let pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let mut xobjects = Dictionary::new();
        xobjects.insert("Fm0", Object::Reference(ObjectRef::new(4, 0)));
        let mut resources = Dictionary::new();
        resources.insert("XObject", Object::Dictionary(xobjects));
        (pdf, resources)
    }

    #[test]
    fn resource_callbacks_reject_malformed_inline_image_protocol() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "null",
        );
        let malformed: &[&[u8]] = &[
            // Header keys must be names.
            b"BI 1 /Foo ID payload EI",
            // Header objects must form key/value pairs.
            b"BI /CS ID payload EI",
            // BI starts only at an operation boundary.
            b"1 BI /CS /Foo ID payload EI",
        ];

        for content in malformed {
            let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("PDF should parse");
            let (complete, _) =
                collect_test_content(&mut pdf, content, None).expect("content walk should stop");
            assert!(!complete, "malformed content was accepted: {content:?}");
        }
    }

    #[test]
    fn resource_callbacks_retain_on_object_parse_error() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let (complete, _) = collect_test_content(&mut pdf, b"<0g>", None)
            .expect("content syntax errors are conservative, not structural");
        assert!(!complete);
    }

    #[test]
    fn form_resource_collection_rejects_malformed_content() {
        assert!(collect_used_names_for_form(b"<0g>").is_none());
    }

    #[test]
    fn resource_callbacks_deduplicate_repeated_xobject_names_before_traversal() {
        let mut callbacks = ResourceCallbacks {
            finder: ResourceFinder::default(),
            inline_header: None,
            valid_xobjects: Default::default(),
            complete: true,
        };

        parse_content_stream_data(b"/VeryLongFormName Do Do Do", &mut callbacks).unwrap();

        assert_eq!(callbacks.valid_xobjects.len(), 1);
        assert_eq!(
            callbacks.valid_xobjects.keys().next().unwrap(),
            b"VeryLongFormName"
        );
    }

    #[test]
    fn resource_callbacks_propagate_form_resolution_errors() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "<0g>",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let mut xobjects = Dictionary::new();
        xobjects.insert("Fm0", Object::Reference(ObjectRef::new(4, 0)));
        let mut resources = Dictionary::new();
        resources.insert("XObject", Object::Dictionary(xobjects));

        let error = collect_test_content(&mut pdf, b"/Fm0 Do", Some(&resources))
            .expect_err("malformed Form object is a structural error");
        assert!(matches!(error, Error::Parse { .. }));
    }

    #[test]
    fn form_error_before_later_parser_diagnostic_is_propagated() {
        let (mut pdf, resources) = malformed_form_pdf_and_resources();

        let error = collect_test_content(&mut pdf, b"/Fm0 Do <0g>", Some(&resources))
            .expect_err("the valid earlier Do must surface its structural Form error");
        assert!(matches!(error, Error::Parse { .. }));
    }

    // The next two tests used to assert that a `Do` obscured by an earlier
    // parser diagnostic, or absorbed into inline-image header parsing, never
    // got recorded as a content-stream invocation and so never triggered
    // recursion into the malformed `Fm0` Form. That distinction no longer
    // matters: recursion is driven by the *declared* `/XObject` keys (see
    // `declared_xobjects`), matching qpdf's `forEachXObject`
    // (`QPDFPageObjectHelper.cc:313-345`), which walks every declared Form
    // regardless of what (if anything) the content stream does with `Do`.
    // `Fm0` is declared in both fixtures, so it is visited either way and its
    // malformed target surfaces the same structural `Error::Parse` as a
    // directly Do-invoked malformed Form (see
    // `resource_callbacks_propagate_form_resolution_errors`).
    #[test]
    fn parser_diagnostic_does_not_prevent_declared_form_traversal() {
        let (mut pdf, resources) = malformed_form_pdf_and_resources();

        let error = collect_test_content(&mut pdf, b"<0g> /Fm0 Do", Some(&resources))
            .expect_err("a declared Form is still visited even when an earlier token is malformed");
        assert!(matches!(error, Error::Parse { .. }));
    }

    #[test]
    fn invalid_inline_header_does_not_prevent_declared_form_traversal() {
        let (mut pdf, resources) = malformed_form_pdf_and_resources();

        let error = collect_test_content(&mut pdf, b"BI /Fm0 Do", Some(&resources)).expect_err(
            "a declared Form is still visited even though this Do is absorbed by inline-header parsing",
        );
        assert!(matches!(error, Error::Parse { .. }));
    }

    // Inline-image payload bytes are opaque to the parser (`/Fm0 Do` inside
    // `ID`...`EI` is never tokenised as a content operator), so it must not be
    // recorded as a used name. This is a `parsed.names` bookkeeping claim,
    // independent of declared-name-driven recursion, so `resources` omits
    // `/XObject` entirely here to isolate it from the (unrelated, and now
    // unconditional) declared-Form traversal covered by the two tests above.
    #[test]
    fn inline_image_payload_do_is_opaque_to_resource_name_detection() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let (complete, used) = collect_test_content(&mut pdf, b"BI /W 1 ID /Fm0 Do EI Q", None)
            .expect("inline-image payload bytes are opaque");
        assert!(complete);
        assert!(!used.contains_key(b"XObject".as_slice()));
    }

    #[test]
    fn pruning_uses_shared_resource_finder_last_name_semantics() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let (complete, used) =
            collect_test_content(&mut pdf, b"/Shared 12 Tf 99 gs", None).unwrap();
        assert!(complete);
        assert!(used[b"Font".as_slice()].contains(b"Shared".as_slice()));
        assert!(used[b"ExtGState".as_slice()].contains(b"Shared".as_slice()));
    }

    // Previously this asserted that the shared `last_name` correctly routed
    // `Do` to a malformed declared Form, proving the *old* Do-invoked
    // recursion trigger picked it up. Recursion is no longer Do-invoked (see
    // `declared_xobjects`), so that no longer discriminates: `Fm0` would
    // be visited regardless of whether `Do` ever routed it. Re-pointed at what
    // the shared `last_name` claim actually is — `parsed.names` bookkeeping —
    // by checking the *same* name is recorded under both `Font` (from `Tf`)
    // and `XObject` (from `Do`) from one shared preceding name token.
    #[test]
    fn shared_finder_last_name_recorded_for_font_and_xobject() {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 612 792] >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let (complete, used) = collect_test_content(&mut pdf, b"/Fm0 12 Tf 99 Do", None)
            .expect("no /XObject is declared, so there is nothing to recurse into");
        assert!(complete);
        assert!(used[b"Font".as_slice()].contains(b"Fm0".as_slice()));
        assert!(used[b"XObject".as_slice()].contains(b"Fm0".as_slice()));
    }
}
