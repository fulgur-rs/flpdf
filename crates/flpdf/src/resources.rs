//! Remove unreferenced resources from page, Form, and direct object content.
//!
//! qpdf correspondence: `QPDFPageObjectHelper::removeUnreferencedResources`.
//!
//! (`QPDFPageObjectHelper.cc:539-650`) uses one raw-handle scope helper for the
//! root and each nested Form. The canonical route parses the Form pre-pass in
//! qpdf's action-then-enqueue order, shares unresolved resource names, and then
//! prunes the root according to its Form/Page classification. Document-level
//! callers own page iteration; the `Auto` decision is the separate qpdf
//! job-level `shouldRemoveUnreferencedResources` heuristic in
//! `should_remove_unreferenced_resources`.
//!
//! Form XObject lookup resolves each canonical handle before inspecting its
//! stream dictionary, so lazy indirect resource values remain live through
//! the complete pruning walk.

use crate::page_object_helper::PageObjectHelper;
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::resource_finder::ResourceFinder;
use crate::{ObjectHandle, Pdf, Result};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek};

/// Snapshot qpdf's `QPDF::numWarnings` around a document-owned content parse
/// (`QPDFPageObjectHelper.cc:547-557`).
fn diagnostic_count<R: Read + Seek>(pdf: &Pdf<R>) -> usize {
    pdf.num_warnings()
}

const BAD_TOKEN_WARNING: &str =
    "Bad token found while scanning content stream; not attempting to remove unreferenced objects from this object";

/// Report the same warning that qpdf's `removeUnreferencedResourcesHelper`
/// emits for a parse exception or a parser warning. The distinction matters:
/// qpdf uses the exception text for the former and the fixed bad-token text
/// for the latter (`QPDFPageObjectHelper.cc:539-564`).
fn warn_resource_parse_failure(handle: &ObjectHandle, parse_error: Option<&str>) -> Result<()> {
    if let Some(parse_error) = parse_error {
        let warning = format!(
            "Unable to parse content stream: {parse_error}; not attempting to remove unreferenced objects from this object"
        );
        handle.warn_if_possible(&warning)
    } else {
        handle.warn_if_possible(BAD_TOKEN_WARNING)
    }
}

/// Match qpdf's `QPDFPageObjectHelper::removeUnreferencedResources` for the
/// receiver's raw ObjectHandle (`libqpdf/QPDFPageObjectHelper.cc:539-650`).
///
/// qpdf visits nested Forms first, accumulating unresolved names and failures,
/// then prunes the receiver if it is a Form or no nested Form failed. No page
/// type or ObjectRef projection is part of this helper boundary.
pub(crate) fn remove_unreferenced_resources_on_target<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    target: ObjectHandle,
) -> Result<()> {
    let (mut unresolved, any_failures) =
        remove_unreferenced_resources_in_form_xobjects(pdf, target.clone())?;
    let is_form = target.is_form_xobject()?;
    if is_form || !any_failures {
        let _ = remove_unreferenced_resources_helper(pdf, target, !is_form, &mut unresolved)?;
    }
    Ok(())
}

/// qpdf's `removeUnreferencedResourcesHelper` over one raw page/Form target.
///
/// Return false when qpdf aborts pruning after a parse warning, parse exception,
/// or unresolved resource name with a Resources dictionary. qpdf shallow-copies
/// indirect resource-category dictionaries before the unresolved-name veto.
fn remove_unreferenced_resources_helper<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    target: ObjectHandle,
    is_page: bool,
    unresolved: &mut BTreeSet<Vec<u8>>,
) -> Result<bool> {
    let diagnostics_before = diagnostic_count(pdf);
    let (finder, parse_error) = {
        let mut helper = PageObjectHelper::from_object_handle(target.clone(), pdf);
        let mut finder = ResourceFinder::default();
        let parse_error = helper
            .parse_contents(&mut finder)
            .err()
            .map(|error| error.to_string());
        (finder, parse_error)
    };

    if let Some(parse_error) = parse_error {
        warn_resource_parse_failure(&target, Some(&parse_error))?;
        return Ok(false);
    }
    if diagnostic_count(pdf) > diagnostics_before {
        warn_resource_parse_failure(&target, None)?;
        return Ok(false);
    }

    let resources =
        PageObjectHelper::from_object_handle(target, pdf).get_attribute(b"/Resources", true)?;
    let resources_is_dictionary = resources.try_is_dictionary()?;
    let categories = [b"/Font".as_slice(), b"/XObject".as_slice()];
    let mut dictionaries = Vec::new();
    let mut known_names = BTreeSet::new();
    if resources_is_dictionary {
        for category in categories {
            let value = resources.try_get_key(category)?;
            value.try_dereference()?;
            if !value.try_is_dictionary()? {
                continue;
            }
            let dictionary = if value.is_indirect() {
                let copy = value.shallow_copy()?;
                resources.replace_key(category, copy.clone())?;
                copy
            } else {
                value
            };
            known_names.extend(
                dictionary
                    .try_get_keys()?
                    .into_iter()
                    .map(|key| key.strip_prefix(b"/").unwrap_or(key.as_slice()).to_vec()),
            );
            dictionaries.push(dictionary);
        }
    }

    let mut local_unresolved = false;
    for category in categories {
        if let Some(names) = finder.names_by_resource_type().get(&category[1..]) {
            for name in names.keys() {
                if !known_names.contains(name) {
                    unresolved.insert(name.clone());
                    local_unresolved = true;
                }
            }
        }
    }
    if local_unresolved && resources_is_dictionary {
        return Ok(false);
    }

    for dictionary in dictionaries {
        let remove = dictionary
            .try_get_keys()?
            .into_iter()
            .filter(|key| {
                let name = key.strip_prefix(b"/").unwrap_or(key.as_slice());
                !(is_page && unresolved.contains(name)) && !finder.names().contains(name)
            })
            .collect::<Vec<_>>();
        for key in remove {
            dictionary.remove_key(&key)?;
        }
    }

    Ok(true)
}

/// Mirror the `forEachFormXObject(true, ...)` phase inside qpdf's
/// `removeUnreferencedResources` call. The helper action prunes each Form
/// before its updated `/Resources` are inspected for child Forms, matching
/// `QPDFPageObjectHelper::forEachXObject`'s action-then-enqueue order.
fn remove_unreferenced_resources_in_form_xobjects<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root_target: ObjectHandle,
) -> Result<(BTreeSet<Vec<u8>>, bool)> {
    let root_resources = PageObjectHelper::from_object_handle(root_target.clone(), pdf)
        .get_attribute(b"/Resources", false)?;
    if !root_resources.try_is_dictionary()? {
        return Ok((BTreeSet::new(), false));
    }
    let mut pending = VecDeque::from(form_xobjects_in_resources(&root_resources)?);
    let mut visited: BTreeSet<QpdfObjGen> = BTreeSet::new();
    if let Some(object_gen) = root_target
        .qpdf_obj_gen()
        .filter(|object_gen| object_gen.is_indirect())
    {
        visited.insert(object_gen);
    }
    let mut unresolved = BTreeSet::new();
    let mut any_failures = false;

    while let Some(holder_handle) = pending.pop_front() {
        holder_handle.try_dereference()?;
        if !holder_handle.is_form_xobject()? {
            continue; // cov:ignore: form_xobjects_in_resources already filters to Form XObjects
        }
        if !remove_unreferenced_resources_helper(
            pdf,
            holder_handle.clone(),
            false,
            &mut unresolved,
        )? {
            any_failures = true;
        }

        // qpdf runs the selector action for every XObject entry before its
        // seen check controls recursive traversal. Repeated references must
        // therefore run the helper again, while their children are enqueued
        // only once.
        // cov:ignore-start: form_xobjects_in_resources enqueues only indirect handles
        let Some(object_gen) = holder_handle
            .qpdf_obj_gen()
            .filter(|object_gen| object_gen.is_indirect())
        else {
            continue;
        };
        // cov:ignore-end
        if !visited.insert(object_gen) {
            continue;
        }

        // qpdf dequeues the Form after the pruning callback and reads its live
        // resource dictionary then; children removed by pruning are not visited.
        let resources = PageObjectHelper::from_object_handle(holder_handle, pdf)
            .get_attribute(b"/Resources", false)?;
        if resources.try_is_dictionary()? {
            pending.extend(form_xobjects_in_resources(&resources)?);
        }
    }
    Ok((unresolved, any_failures))
}

/// Return indirect Form XObjects listed in a resource dictionary, retaining
/// each handle's raw qpdf identity for the caller's traversal seen set.
fn form_xobjects_in_resources(resources: &ObjectHandle) -> Result<Vec<ObjectHandle>> {
    let xobjects = resources.try_get_key(b"/XObject")?;
    xobjects.try_dereference()?;
    let Some(xobjects) = xobjects.try_as_dictionary()? else {
        return Ok(Vec::new());
    };
    let mut forms = Vec::new();
    for value in xobjects.values() {
        if value
            .qpdf_obj_gen()
            .filter(|object_gen| object_gen.is_indirect())
            .is_none()
        {
            continue;
        }
        // Resolve the live XObject value before applying the form predicate.
        value.try_dereference()?;
        if !value.is_form_xobject()? {
            continue;
        }
        forms.push(value.clone());
    }
    Ok(forms)
}

#[cfg(test)]
mod final_handle_tests {
    use super::{
        form_xobjects_in_resources, remove_unreferenced_resources_in_form_xobjects,
        remove_unreferenced_resources_on_target,
    };
    use crate::{ObjectHandle, ObjectRef, Pdf};
    use std::io::Cursor;
    use std::rc::Rc;

    fn fixture() -> Pdf<Cursor<Vec<u8>>> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/direct-root-one-page.pdf");
        Pdf::open_mem_owned(std::fs::read(path).expect("fixture exists")).expect("fixture opens")
    }

    fn form_with_resources(pdf: &Pdf<Cursor<Vec<u8>>>, contents: &[u8]) -> ObjectHandle {
        let form = pdf
            .new_stream_with_data(Rc::new(contents.to_vec()))
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
                    ObjectHandle::dictionary(vec![
                        (b"/F1".to_vec(), ObjectHandle::dictionary(Vec::new())),
                        (b"/Unused".to_vec(), ObjectHandle::dictionary(Vec::new())),
                    ]),
                )]),
            )
            .expect("form resources");
        form
    }

    #[test]
    fn form_resource_prepass_resolves_form_and_resource_handles() {
        let mut pdf = fixture();
        let page_ref = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page_ref);
        page_handle.try_is_scalar().expect("page resolves");
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

        let page = pdf.get_object_handle(page_ref);
        let (unresolved, failures) = remove_unreferenced_resources_in_form_xobjects(&mut pdf, page)
            .expect("form resource prepass");
        assert!(unresolved.is_empty());
        assert!(!failures);
    }

    #[test]
    fn form_resource_prepass_does_not_drop_a_raw_generation_form() {
        let mut pdf = fixture();
        let raw_ref = ObjectRef::new(50, 65_535);
        let form = ObjectHandle::direct_stream(
            ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
                (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            ]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object(raw_ref, form).expect("install raw form");
        let resources = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Fm0".to_vec(), pdf.get_object_handle(raw_ref))]),
        )]);

        let forms = form_xobjects_in_resources(&resources).expect("find raw form");

        assert_eq!(forms.len(), 1);
    }

    #[test]
    fn canonical_form_pruning_uses_the_clean_parse_boundary() {
        let mut pdf = fixture();
        let form = form_with_resources(&pdf, b"/F1 12 Tf");

        remove_unreferenced_resources_on_target(&mut pdf, form.clone())
            .expect("clean Form content should be pruned");

        let resources = form
            .as_stream_dict()
            .expect("form dictionary")
            .try_get_key(b"/Resources")
            .expect("resources");
        let fonts = resources.try_get_key(b"/Font").expect("fonts");
        assert!(!fonts.try_get_key(b"/F1").expect("used font").is_null());
        assert!(fonts
            .try_get_key(b"/Unused")
            .expect("unused font")
            .is_null());
    }

    #[test]
    fn canonical_form_pruning_skips_a_scope_with_parser_warnings() {
        let mut pdf = fixture();
        let form = form_with_resources(&pdf, b"<0g>");

        remove_unreferenced_resources_on_target(&mut pdf, form.clone())
            .expect("recoverable Form warnings should skip pruning");

        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("invalid character (g) in hexstring")));
        let resources = form
            .as_stream_dict()
            .expect("form dictionary")
            .try_get_key(b"/Resources")
            .expect("resources");
        let fonts = resources.try_get_key(b"/Font").expect("fonts");
        assert!(!fonts
            .try_get_key(b"/Unused")
            .expect("unused font")
            .is_null());
    }

    #[test]
    fn canonical_form_pruning_reports_a_parse_exception() {
        let mut pdf = fixture();
        let form = form_with_resources(&pdf, b"not-flate");
        form.as_stream_dict()
            .expect("form dictionary")
            .replace_key(b"/Filter", ObjectHandle::name(b"FlateDecode".to_vec()))
            .expect("filter");

        remove_unreferenced_resources_on_target(&mut pdf, form)
            .expect("recoverable Form parse failures should be warnings");

        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("Unable to parse content stream: stream inflate")));
    }

    #[test]
    fn page_resource_pruning_reports_a_parse_exception() {
        let mut pdf = fixture();
        let page_ref = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page_ref);
        page_handle.try_is_scalar().expect("page resolves");
        let replacement = page_handle.shallow_copy().expect("page is copyable");
        let contents = pdf
            .new_stream_with_data(Rc::new(b"not-flate".to_vec()))
            .expect("content stream");
        contents
            .as_stream_dict()
            .expect("content stream dictionary")
            .replace_key(b"/Filter", ObjectHandle::name(b"FlateDecode".to_vec()))
            .expect("filter");
        replacement
            .replace_key(b"/Contents", contents)
            .expect("page contents");
        pdf.replace_object(page_ref, replacement)
            .expect("replace page");

        let page = pdf.get_object_handle(page_ref);
        remove_unreferenced_resources_on_target(&mut pdf, page)
            .expect("recoverable page parse failures should be warnings");

        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("Unable to parse content stream: stream inflate")));
    }

    /// Build a page whose single declared Form XObject is malformed (either
    /// undecodable or unparseable, per `outer_stream`/`outer_filter`) but
    /// still declares a nested child Form. `remove_unreferenced_resources_in_form_xobjects`
    /// must record the failure and keep walking into that child rather than
    /// aborting — the child's own content references a resource name absent
    /// from its own `/Resources`, so seeing that name surface in the
    /// returned unresolved set is an externally observable proof the walk
    /// descended past the malformed parent.
    fn page_with_malformed_form_and_child(
        outer_stream: &[u8],
        outer_filter: Option<&[u8]>,
    ) -> (Pdf<std::io::Cursor<Vec<u8>>>, crate::ObjectRef) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/direct-root-one-page.pdf");
        let mut pdf = Pdf::open_mem_owned(std::fs::read(path).expect("fixture exists"))
            .expect("fixture opens");
        let page_ref = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page_ref);
        page_handle.try_is_scalar().expect("page resolves");
        let replacement = page_handle.shallow_copy().expect("page is copyable");

        let child_form = pdf
            .new_stream_with_data(Rc::new(b"/Ghost Do".to_vec()))
            .expect("child form stream");
        let child_dict = child_form.as_stream_dict().expect("child form dictionary");
        child_dict
            .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
            .expect("child form type");
        child_dict
            .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
            .expect("child form subtype");
        child_dict
            .replace_key(b"/Resources", ObjectHandle::dictionary(vec![]))
            .expect("child form resources");

        let outer_form = pdf
            .new_stream_with_data(Rc::new(outer_stream.to_vec()))
            .expect("outer form stream");
        let outer_dict = outer_form.as_stream_dict().expect("outer form dictionary");
        outer_dict
            .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
            .expect("outer form type");
        outer_dict
            .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
            .expect("outer form subtype");
        if let Some(filter) = outer_filter {
            outer_dict
                .replace_key(b"/Filter", ObjectHandle::name(filter.to_vec()))
                .expect("outer form filter");
        }
        outer_dict
            .replace_key(
                b"/Resources",
                ObjectHandle::dictionary(vec![(
                    b"/XObject".to_vec(),
                    ObjectHandle::dictionary(vec![(b"/FmChild".to_vec(), child_form)]),
                )]),
            )
            .expect("outer form resources");

        let resources = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Fm0".to_vec(), outer_form)]),
        )]);
        replacement
            .replace_key(b"/Resources", resources)
            .expect("page resources");
        pdf.replace_object(page_ref, replacement)
            .expect("replace page");
        (pdf, page_ref)
    }

    #[test]
    fn form_walk_continues_into_declared_children_after_a_decode_failure() {
        let (mut pdf, page_ref) =
            page_with_malformed_form_and_child(b"not-flate", Some(b"FlateDecode"));

        let page = pdf.get_object_handle(page_ref);
        let (unresolved, failures) = remove_unreferenced_resources_in_form_xobjects(&mut pdf, page)
            .expect("form resource prepass tolerates an undecodable Form");
        assert!(failures, "the undecodable outer Form must set any_failures");
        assert!(
            unresolved.contains(b"Ghost".as_slice()),
            "the child Form's own unresolved /Ghost use must surface, proving \
             the walk descended past the undecodable parent: {unresolved:?}"
        );
    }

    #[test]
    fn form_walk_continues_into_declared_children_after_a_parse_failure() {
        let (mut pdf, page_ref) = page_with_malformed_form_and_child(b"<0g>", None);

        let page = pdf.get_object_handle(page_ref);
        let (unresolved, failures) = remove_unreferenced_resources_in_form_xobjects(&mut pdf, page)
            .expect("form resource prepass tolerates an unparseable Form");
        assert!(failures, "the unparseable outer Form must set any_failures");
        assert!(
            unresolved.contains(b"Ghost".as_slice()),
            "the child Form's own unresolved /Ghost use must surface, proving \
             the walk descended past the unparseable parent: {unresolved:?}"
        );
    }
}
