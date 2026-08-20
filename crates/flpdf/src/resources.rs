//! qpdf correspondence: `QPDFPageObjectHelper::removeUnreferencedResources`.
//! (`QPDFPageObjectHelper.cc:539-649`) split into page/Form traversal helpers.
//!
//! The canonical route parses one page or Form at a time, then shallow-copies
//! and prunes only its `/Font` and `/XObject` dictionaries. Document-level
//! callers own the page iteration; the `Auto` decision is the separate qpdf
//! job-level `shouldRemoveUnreferencedResources` heuristic below.

use crate::content_stream::{parse_content_stream_data, ParseControl, ParserCallbacks};
use crate::filters::{decode_stream_data_from_handle, DecodeLimits};
use crate::object_handle::ObjectHandleIdentity;
use crate::page_object_helper::PageObjectHelper;
use crate::ref_chain::resolve_ref_chain;
use crate::resource_finder::{ResourceFinder, ResourceNamesByType};
use crate::{Dictionary, Error, Object, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
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
        pdf.resolve_object_handle(&value)?;
        let dictionary = if value.is_indirect() {
            let copy = value.shallow_copy()?;
            resources.replace_key(category, copy.clone())?;
            copy
        } else {
            value
        };
        let Some(_) = dictionary.as_dictionary() else {
            continue;
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
        let form_handle = pdf.get_object_handle(form_ref);
        let decoded = (|| -> Result<Vec<u8>> {
            pdf.resolve_object_handle(&form_handle)?;
            let stream_dict = form_stream_dict(&form_handle)?;
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

/// Mode passed by qpdf job-level page operations to resource pruning.
///
/// Mirrors qpdf's `--remove-unreferenced-resources=auto|yes|no`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoveUnreferencedResources {
    /// Let the qpdf job-level caller decide whether the source document warrants
    /// the per-page pruning pass.
    #[default]
    Auto,
    /// Run the canonical per-page pruning pass without the Auto heuristic.
    Yes,
    /// No-op: leave all `/Resources` entries untouched.
    No,
}

/// Decide whether qpdf's `--pages` Auto mode should run page-level resource
/// pruning for this source document.
///
/// This is qpdf 11.9.0's `QPDFJob::shouldRemoveUnreferencedResources`
/// heuristic (`libqpdf/QPDFJob.cc:2251-2337`). qpdf only pays the cost of
/// `QPDFPageObjectHelper::removeUnreferencedResources` when the source page
/// tree contains an inherited/non-leaf `/Resources`, a shared indirect
/// `/Resources` object, or a shared indirect `/XObject` dictionary. A
/// page-local indirect `/Resources` that appears once therefore returns false.
///
/// Form XObjects reachable from page `/XObject` dictionaries are traversed as
/// qpdf does, so sharing discovered in a nested Form resource scope also
/// enables the page-job pruning route.
pub fn should_remove_unreferenced_resources<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let catalog = pdf.get_object_handle(root_ref);
    let pages = pdf.resolve_object_handle_to_terminal(&catalog.try_get_key(b"/Pages")?)?;
    if pages.is_null() {
        return Ok(false);
    }

    let mut queue = VecDeque::from([pages]);
    #[allow(
        clippy::mutable_key_type,
        reason = "qpdf page-job traversal intentionally keys on canonical handle identity"
    )]
    let mut nodes_seen: HashSet<ObjectHandleIdentity> = HashSet::new();
    let mut indirect_resources_seen: BTreeSet<ObjectRef> = BTreeSet::new();

    while let Some(node) = queue.pop_front() {
        let node = pdf.resolve_object_handle_to_terminal(&node)?;
        if !nodes_seen.insert(node.identity_key()) {
            continue;
        }

        let dict = node.as_stream_dict().unwrap_or_else(|| node.clone());
        let kids = pdf.resolve_object_handle_to_terminal(&dict.try_get_key(b"/Kids")?)?;
        if let Some(kids) = kids.try_as_array()? {
            // qpdf returns true for any non-leaf page node that owns a
            // /Resources key, even if only one descendant page is selected.
            if dict.try_has_key(b"/Resources")? {
                return Ok(true);
            }
            queue.extend(kids);
            continue;
        }

        let resources = dict.try_get_key(b"/Resources")?;
        if let Some(resources_ref) = resources.object_ref() {
            if !indirect_resources_seen.insert(resources_ref) {
                return Ok(true);
            }
        }

        let resources = pdf.resolve_object_handle_to_terminal(&resources)?;
        let Some(resources_dict) = resources.as_dictionary() else {
            continue;
        };
        let xobject = resources_dict
            .get(b"/XObject".as_slice())
            .cloned()
            .unwrap_or_else(ObjectHandle::null);
        if let Some(xobject_ref) = xobject.object_ref() {
            if !indirect_resources_seen.insert(xobject_ref) {
                return Ok(true);
            }
        }

        let xobject = pdf.resolve_object_handle_to_terminal(&xobject)?;
        let Some(entries) = xobject.as_dictionary() else {
            continue;
        };
        for object in entries.into_values() {
            let object = pdf.resolve_object_handle_to_terminal(&object)?;
            if object.is_form_xobject()? {
                queue.push_back(object);
            }
        }
    }

    Ok(false)
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

fn form_stream_dict(handle: &ObjectHandle) -> Result<ObjectHandle> {
    handle.as_stream_dict().ok_or_else(|| {
        Error::Internal("Form XObject handle did not resolve to a stream".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stream;
    use std::io::Cursor;

    /// Build a minimal one-page PDF with a caller-supplied page dictionary and
    /// object 4 body. The Auto heuristic tests use object 4 as either a
    /// resource dictionary, a holder, or an inert value.
    fn build_page_with_resources_carrier_pdf(page_body: &str, obj4_body: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
        let objects: [(u32, &str); 4] = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page_body),
            (4, obj4_body),
        ];
        for (number, body) in objects {
            offsets.insert(number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = out.len() as u64;
        let total = 5u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for number in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

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

    /// Build a reachable Form XObject whose `/Font` category is indirect and
    /// whose content references an undeclared font name. The canonical Form
    /// pruning path must shallow-copy `/Font` and dirty the Form owner before
    /// the unresolved-name veto returns.
    fn build_form_with_indirect_font_pdf() -> Vec<u8> {
        let content = b"BT /Fmissing 12 Tf (text) Tj ET";
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

        let objects = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                   /Resources << /XObject << /Fm 5 0 R >> >> >>"
                    .to_vec(),
            ),
            (7, b"<< /F1 << >> /F2 << >> >>".to_vec()),
        ];
        for (number, body) in objects {
            offsets.insert(number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\nendobj\n");
        }

        offsets.insert(5, out.len() as u64);
        out.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                 /Resources << /Font 7 0 R >> /Length {} >>\nstream\n",
                content.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(content);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_start = out.len() as u64;
        let total = 8u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for number in 1..total {
            if let Some(offset) = offsets.get(&number) {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            } else {
                out.extend_from_slice(b"0000000000 65535 f \n");
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// Build two leaf pages whose resource dictionaries are distinct while
    /// their indirect `/XObject` category dictionary is shared.
    fn build_shared_xobject_heuristic_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        let objects = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 5 0 R >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 6 0 R >>",
            ),
            (5, "<< /XObject 7 0 R >>"),
            (6, "<< /XObject 7 0 R >>"),
            (7, "<< /Fm 8 0 R >>"),
        ];
        for (number, body) in objects {
            offsets.insert(number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        offsets.insert(8, out.len() as u64);
        out.extend_from_slice(
            b"8 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );

        let xref_start = out.len() as u64;
        let total = 9u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for number in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// Build two leaves that point at the same indirect `/Resources` object.
    fn build_shared_resources_heuristic_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        let objects = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 5 0 R >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 5 0 R >>",
            ),
            (5, "<< >>"),
        ];
        for (number, body) in objects {
            offsets.insert(number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }

        let xref_start = out.len() as u64;
        let total = 6u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for number in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// Build a page tree that repeats one canonical leaf handle in `/Kids`.
    fn build_duplicate_page_heuristic_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        let objects = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
        ];
        for (number, body) in objects {
            offsets.insert(number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }

        let xref_start = out.len() as u64;
        let total = 4u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for number in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// Remove the trailer's `/Root` from a valid fixture.
    fn build_rootless_heuristic_pdf() -> Vec<u8> {
        let mut out = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 100 100] >>",
            "<< >>",
        );
        let marker = b"/Root 1 0 R";
        let start = out
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("fixture trailer should contain /Root");
        out[start..start + marker.len()].fill(b' ');
        out
    }

    /// Build a valid catalog whose `/Pages` key is explicitly null.
    fn build_null_pages_heuristic_pdf() -> Vec<u8> {
        let mut out = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /MediaBox [0 0 100 100] >>",
            "<< >>",
        );
        let marker = b"/Pages 2 0 R";
        let replacement = b"/Pages null ";
        let start = out
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("fixture catalog should contain /Pages");
        out[start..start + marker.len()].copy_from_slice(replacement);
        out
    }

    #[test]
    fn pages_auto_resource_heuristic_matches_qpdf_trigger_shapes() {
        let mut rootless = Pdf::open(Cursor::new(build_rootless_heuristic_pdf())).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut rootless).unwrap());

        let mut pages_null = Pdf::open(Cursor::new(build_null_pages_heuristic_pdf())).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut pages_null).unwrap());

        let mut duplicate_nodes =
            Pdf::open(Cursor::new(build_duplicate_page_heuristic_pdf())).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut duplicate_nodes).unwrap());

        let mut page_local = Pdf::open(Cursor::new(build_page_with_resources_carrier_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 4 0 R >>",
            "<< /Font << >> >>",
        )))
        .unwrap();
        assert!(!should_remove_unreferenced_resources(&mut page_local).unwrap());

        let mut inherited = Pdf::open(Cursor::new(build_inherited_indirect_resources_pdf(
            4, "5 0 R",
        )))
        .unwrap();
        assert!(should_remove_unreferenced_resources(&mut inherited).unwrap());

        let mut form = Pdf::open(Cursor::new(build_form_with_indirect_font_pdf())).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut form).unwrap());

        let mut shared_xobject =
            Pdf::open(Cursor::new(build_shared_xobject_heuristic_pdf())).unwrap();
        assert!(should_remove_unreferenced_resources(&mut shared_xobject).unwrap());

        let mut shared_resources =
            Pdf::open(Cursor::new(build_shared_resources_heuristic_pdf())).unwrap();
        assert!(should_remove_unreferenced_resources(&mut shared_resources).unwrap());
    }

    #[test]
    fn canonical_form_resource_veto_marks_indirect_category_copy_dirty() {
        let mut pdf =
            Pdf::open(Cursor::new(build_form_with_indirect_font_pdf())).expect("PDF should parse");
        let form_ref = ObjectRef::new(5, 0);
        let form = pdf.get_object_handle(form_ref);
        assert!(!pdf.is_dirty(form_ref));

        prune_canonical_resource_target(&mut pdf, form).expect("prune should succeed");

        assert!(
            pdf.is_dirty(form_ref),
            "privatizing an indirect category before the veto must dirty its Form owner"
        );
    }

    fn build_page_with_indirect_form_filter_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = build_page_with_resources_carrier_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 5 0 R /Resources 6 0 R >>",
            "null",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let mut page_content = Dictionary::new();
        page_content.insert("Length", Object::Integer(7));
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Stream(Stream::new(page_content, b"/Fm Do".to_vec())),
        );

        let mut page_xobjects = Dictionary::new();
        page_xobjects.insert("Fm", Object::Reference(ObjectRef::new(4, 0)));
        let mut page_resources = Dictionary::new();
        page_resources.insert("XObject", Object::Dictionary(page_xobjects));
        pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(page_resources));

        let form_body = b"BT /F1 12 Tf ET";
        let mut filter_dict = Dictionary::new();
        filter_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = crate::filters::encode_stream_data(&filter_dict, form_body).unwrap();
        let mut form_dict = Dictionary::new();
        form_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        form_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        form_dict.insert("Resources", Object::Reference(ObjectRef::new(9, 0)));
        form_dict.insert("Filter", Object::Reference(ObjectRef::new(7, 0)));
        form_dict.insert("DecodeParms", Object::Reference(ObjectRef::new(8, 0)));
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Stream(Stream::new(form_dict, encoded)),
        );
        pdf.set_object(ObjectRef::new(7, 0), Object::Name(b"FlateDecode".to_vec()));
        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(1));
        pdf.set_object(ObjectRef::new(8, 0), Object::Dictionary(decode_params));

        let mut fonts = Dictionary::new();
        fonts.insert("F1", Object::Dictionary(Dictionary::new()));
        fonts.insert("F2", Object::Dictionary(Dictionary::new()));
        let mut form_resources = Dictionary::new();
        form_resources.insert("Font", Object::Dictionary(fonts));
        pdf.set_object(ObjectRef::new(9, 0), Object::Dictionary(form_resources));

        pdf
    }

    fn assert_form_fonts_pruned(pdf: &mut Pdf<Cursor<Vec<u8>>>) {
        let form = pdf
            .resolve(ObjectRef::new(4, 0))
            .expect("Form should resolve")
            .into_stream()
            .expect("Form target should remain a stream");
        let resources = form
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("pruned Form resources should be inline");
        let fonts = resources
            .get("Font")
            .and_then(Object::as_dict)
            .expect("Form resources should retain /Font");
        assert!(fonts.get("F1").is_some(), "used /F1 must remain");
        assert!(fonts.get("F2").is_none(), "unused /F2 must be pruned");
    }

    #[test]
    fn remove_unreferenced_resources_resolves_indirect_form_filter() {
        let mut pdf = build_page_with_indirect_form_filter_pdf();

        remove_unreferenced_resources_on_page(&mut pdf, ObjectRef::new(3, 0))
            .expect("resource pruning should succeed");

        assert_form_fonts_pruned(&mut pdf);
    }

    #[test]
    fn form_stream_dict_rejects_non_stream_handle() {
        assert!(form_stream_dict(&ObjectHandle::integer(1)).is_err());
    }
}
