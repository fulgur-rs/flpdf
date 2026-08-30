//! qpdf correspondence: `QPDFPageObjectHelper::removeUnreferencedResources`.
//! (`QPDFPageObjectHelper.cc:539-649`) split into page/Form traversal helpers.
//!
//! The canonical route parses one page or Form at a time, then shallow-copies
//! and prunes only its `/Font` and `/XObject` dictionaries. Document-level
//! callers own the page iteration; the `Auto` decision is the separate qpdf
//! job-level `shouldRemoveUnreferencedResources` heuristic in
//! [`crate::job::should_remove_unreferenced_resources`]. Both the Form pre-pass
//! and the ResourceReplacer name scan use the canonical
//! `ObjectHandleParserCallbacks` content route.
//!
//! Form XObject lookup resolves each canonical handle before inspecting its
//! stream dictionary, so lazy indirect resource values remain live through
//! the complete pruning walk.

use crate::content_stream::parse_content_stream_handles;
use crate::filters::{decode_stream_data_from_handle, DecodeLimits};
use crate::page_object_helper::PageObjectHelper;
use crate::resource_finder::{ResourceFinder, ResourceNamesByType};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Seek};

/// Resource names referenced by a content scope, keyed by category
/// (`Font`, `XObject`, …) → set of referenced names.
type UsedNames = BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>;

/// qpdf `QPDFPageObjectHelper::removeUnreferencedResources` for one page.
///
/// qpdf first copies an inherited or indirect `/Resources` dictionary onto the
/// page, then shallow-copies the `/Font` and `/XObject` dictionaries it will
/// mutate. Each page therefore gets its own mutable resource scope; the caller
/// is responsible for iterating the selected pages and applying the job mode.
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
        pdf.resolve(&value)?;
        if value.as_dictionary().is_none() {
            // qpdf only shallow-copies and mutates a category when
            // `dict.isDictionary()` (`QPDFPageObjectHelper.cc:576-585`); a
            // malformed category is left as its original (possibly indirect)
            // value, never replaced or removed.
            continue;
        }
        let dictionary = if value.is_indirect() {
            let copy = value.shallow_copy()?;
            resources.replace_key(category, copy.clone())?;
            copy
        } else {
            value
        };
        let names = finder.names();
        let remove = dictionary
            .as_dictionary()
            .into_iter()
            .flat_map(|entries| entries.into_keys())
            .filter(|name| {
                let resource_name = name.strip_prefix(b"/").unwrap_or(name.as_slice());
                !names.contains(resource_name)
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
        pdf.resolve(&value)?;
        if value.as_dictionary().is_none() {
            // qpdf leaves a malformed /Font or /XObject category untouched;
            // see the matching comment in remove_unreferenced_resources_on_page.
            continue;
        }
        let dictionary = if value.is_indirect() {
            let copy = value.shallow_copy()?;
            resources.replace_key(category, copy.clone())?;
            copy
        } else {
            value
        };
        let live_keys = dictionary.try_get_keys()?;
        known_names.extend(
            live_keys
                .iter()
                .map(|key| key.strip_prefix(b"/").unwrap_or(key.as_slice()).to_vec()),
        );
        dictionaries.push((category, dictionary, live_keys));
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
        pdf.mark_object_handle_dirty(&resources)?;
        return Ok(());
    }

    for (_category, dictionary, live_keys) in dictionaries {
        let Some(entries) = dictionary.as_dictionary() else {
            continue;
        };
        let names = finder.names();
        let remove = entries
            .keys()
            .filter(|key| live_keys.contains(*key))
            .filter(|key| {
                let name = key.strip_prefix(b"/").unwrap_or(key.as_slice());
                !names.contains(name)
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
    let page_resources = {
        let mut helper = PageObjectHelper::new(page_ref, pdf);
        // qpdf's forEachFormXObject uses getAttribute("/Resources", false)
        // while discovering nested Forms; the later per-Form pruning helper
        // is the first boundary that copies an indirect Resources dictionary.
        helper.get_resources(false)?
    };
    if page_resources.is_null() {
        return Ok((BTreeSet::new(), false));
    }
    let mut pending = VecDeque::from(form_xobjects_in_resources(pdf, &page_resources)?);
    let mut visited = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    let mut any_failures = false;

    while let Some(form_ref) = pending.pop_front() {
        if !visited.insert(form_ref) {
            continue;
        }
        let holder_handle = pdf.get_object_handle(form_ref);
        // Resolve the canonical form handle before inspecting its stream type.
        let form_handle = pdf.resolve_handle(&holder_handle)?;
        if !form_handle.is_form_xobject()? {
            continue; // cov:ignore: form_xobjects_in_resources already terminal-chase-filters to Form XObjects
        }
        let stream_dict = form_stream_dict(&form_handle)?;
        // Resolve the live resource dictionary before reading its children.
        let resources = pdf.resolve_handle(&stream_dict.try_get_key(b"/Resources")?)?;
        let resources = resources
            .try_as_dictionary()?
            .map(|_| resources)
            .filter(|handle| !handle.is_null());
        let decoded = (|| -> Result<Vec<u8>> {
            let stream_data = form_handle.get_raw_stream_data()?;
            decode_stream_data_from_handle(
                &stream_dict,
                stream_data.as_ref(),
                DecodeLimits::default(),
            )
        })();
        let Ok(bytes) = decoded else {
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
        let local_unresolved = unresolved_resource_names(resources.as_ref(), &used)?;
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
        } else if let Some(resources) = &resources {
            // Only shallow-copy the shared indirect Resources dictionary once
            // pruning is actually going to happen: qpdf's contract (mirrored
            // by prune_canonical_resource_target's own parse-then-copy order)
            // leaves a Form whose content failed to parse untouched.
            let resources = if resources.is_indirect() {
                let copy = resources.shallow_copy()?;
                stream_dict.replace_key(b"/Resources", copy.clone())?;
                pdf.mark_object_handle_dirty(&stream_dict)?;
                copy
            } else {
                resources.clone()
            };
            prune_font_and_xobject_dictionaries(pdf, &resources, &used)?;
        }

        pending.extend(child_forms);
    }
    Ok((unresolved, any_failures))
}

/// Return `/Font` and `/XObject` names used by `used` but absent from the
/// current Form resource scope. qpdf deliberately compares the categories as
/// one name set before deciding whether a Form may be pruned.
fn unresolved_resource_names(
    resources: Option<&ObjectHandle>,
    used: &UsedNames,
) -> Result<BTreeSet<Vec<u8>>> {
    let mut known_names: BTreeSet<Vec<u8>> = BTreeSet::new();
    if let Some(resources) = resources {
        for category in [b"Font".as_slice(), b"XObject".as_slice()] {
            let mut key = Vec::with_capacity(category.len() + 1);
            key.push(b'/');
            key.extend_from_slice(category);
            let value = resources.try_get_key(&key)?;
            value.try_dereference()?;
            let Some(dictionary) = value.try_as_dictionary()? else {
                continue;
            };
            known_names.extend(
                dictionary
                    .keys()
                    .map(|name| name.strip_prefix(b"/").unwrap_or(name.as_slice()).to_vec()),
            );
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
    resources: &ObjectHandle,
) -> Result<Vec<ObjectRef>> {
    let xobjects = resources.try_get_key(b"/XObject")?;
    xobjects.try_dereference()?;
    let Some(xobjects) = xobjects.try_as_dictionary()? else {
        return Ok(Vec::new());
    };
    let mut forms = Vec::new();
    for value in xobjects.values() {
        if !value.is_indirect() {
            continue;
        }
        // Resolve the live XObject value before applying the form predicate.
        if !pdf.resolve_handle(value)?.is_form_xobject()? {
            continue;
        }
        if let Some(reference) = value.object_ref() {
            forms.push(reference);
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
    let mut finder = ResourceFinder::default();
    let complete = parse_content_stream_handles(stream_bytes, None, &mut finder).is_ok()
        && !finder.had_diagnostics()
        && !finder.has_pending_operands();
    if complete {
        record_direct_names(&mut used, finder.names_by_resource_type(), true);
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
    resources: &ObjectHandle,
    used: &UsedNames,
) -> Result<()> {
    for category in [b"Font".as_slice(), b"XObject".as_slice()] {
        let mut key = Vec::with_capacity(category.len() + 1);
        key.push(b'/');
        key.extend_from_slice(category);
        let value = resources.try_get_key(&key)?;
        value.try_dereference()?;
        if value.try_as_dictionary()?.is_none() {
            // qpdf leaves a malformed /Font or /XObject category untouched;
            // see the matching comment in remove_unreferenced_resources_on_page.
            continue;
        }
        let dictionary = if value.is_indirect() {
            let copy = value.shallow_copy()?;
            resources.replace_key(&key, copy.clone())?;
            copy
        } else {
            value
        };
        let names = used.get(category).cloned().unwrap_or_default();
        let remove = dictionary
            .try_get_keys()?
            .into_iter()
            .filter(|name| !names.contains(name.strip_prefix(b"/").unwrap_or(name.as_slice())))
            .collect::<Vec<_>>();
        for name in remove {
            dictionary.remove_key(&name);
        }
        pdf.mark_object_handle_dirty(&dictionary)?;
    }
    Ok(())
}

/// Resource dictionary categories reported by qpdf's `ResourceFinder`.
///
/// The canonical pruning mutation uses only `/Font` and `/XObject`, but the
/// content walk records all categories so Form scope and unresolved-name
/// handling remain faithful to qpdf's parser callbacks.
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

fn form_stream_dict(handle: &ObjectHandle) -> Result<ObjectHandle> {
    handle.as_stream_dict().ok_or_else(|| {
        Error::Internal("Form XObject handle did not resolve to a stream".to_owned())
    })
}

#[cfg(test)]
mod final_handle_tests {
    use super::remove_unreferenced_resources_in_form_xobjects;
    use crate::{ObjectHandle, Pdf};
    use std::rc::Rc;

    #[test]
    fn form_resource_prepass_resolves_form_and_resource_handles() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/direct-root-one-page.pdf");
        let mut pdf = Pdf::open_mem_owned(std::fs::read(path).expect("fixture exists"))
            .expect("fixture opens");
        let page_ref = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page_ref);
        pdf.resolve(&page_handle).expect("page resolves");
        let replacement = page_handle.shallow_copy().expect("page is copyable");

        let form = pdf
            .new_stream_with_data(Rc::new(b"q Q".to_vec()))
            .expect("form stream");
        let form_dict = form.as_stream_dict().expect("form dictionary");
        form_dict
            .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
            .expect("form type");
        form_dict
            .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
            .expect("form subtype");
        form_dict
            .replace_key(
                b"/Resources",
                ObjectHandle::dictionary(vec![(
                    b"/Font".to_vec(),
                    ObjectHandle::dictionary(vec![(b"/Unused".to_vec(), ObjectHandle::integer(1))]),
                )]),
            )
            .expect("form resources");
        let resources = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Fm0".to_vec(), form)]),
        )]);
        replacement
            .replace_key(b"/Resources", resources)
            .expect("page resources");
        pdf.replace_object(page_ref, replacement)
            .expect("replace page");

        let (unresolved, failures) =
            remove_unreferenced_resources_in_form_xobjects(&mut pdf, page_ref)
                .expect("form resource prepass");
        assert!(unresolved.is_empty());
        assert!(!failures);
    }
}
