//! qpdf correspondence: QPDFPageObjectHelper.cc annotation flattening split from the page helper.
//! Annotation flattening: burn annotation appearances into page content.
//!
//! [`flatten_annotations_on_page`] processes every eligible annotation on a
//! single leaf page:
//!
//! 1. Selects the annotation's `/AP/N` appearance stream (a Form XObject).
//! 2. Delegates qpdf's `/BBox` + `/Matrix` + `/Rect` placement and content
//!    construction to [`crate::AnnotationObjectHelper`].
//! 3. Registers the selected stream in the page `/Resources/XObject` dictionary.
//! 4. Appends the helper's `q\n... cm\n/name Do\nQ\n` content to the page.
//! 5. Removes the annotation from the page `/Annots` array.
//!
//! `resolve_ap_n`, `read_annot_flags`, and
//! `annotation_has_appearance_dictionary` remain a bounded compatibility
//! bridge for `Pdf::set_object`-constructed holder chains. They are not the
//! parsed qpdf route and do not own placement or emitted content.
//!
//! The qpdf document-helper entry point applies this to every leaf page with
//! its caller-supplied required and forbidden annotation-flag masks.

use crate::page_annotation_enum::enumerate_page_annotations;
use crate::pages::{coalesce_page_contents, page_content_bytes, resolve_inherited_resources};
use crate::ref_chain::resolve_ref_chain;
use crate::{
    AnnotationObjectHelper, Dictionary, Error, Matrix, Object, ObjectHandle, ObjectRef,
    PageObjectHelper, Pdf, Rectangle, Result, Stream,
};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Controls which annotations are included in flattening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlattenMode {
    #[cfg(test)]
    /// Flatten all annotations that have an appearance, except Hidden ones.
    All,
    #[cfg(test)]
    /// Flatten only annotations that have the Print bit set (and are not Hidden).
    Print,
    #[cfg(test)]
    /// Flatten only annotations that do *not* have the Print bit set
    /// (and are not Hidden or NoView).
    Screen,
    /// qpdf's direct required/forbidden annotation-flag contract.
    #[doc(hidden)]
    Flags {
        required: i64,
        forbidden: i64,
        skip_widgets: bool,
        page_rotate: i32,
    },
}

// ---------------------------------------------------------------------------
// Annotation /F flag bit constants (1-indexed per PDF spec)
// ---------------------------------------------------------------------------
/// Bit 2 (0x02): Hidden — do not display or print.
#[cfg(test)]
const FLAG_HIDDEN: i64 = 0x2;
/// Bit 3 (0x04): Print — print when printing.
#[cfg(test)]
const FLAG_PRINT: i64 = 0x4;
/// Bit 6 (0x20): NoView — do not display on screen.
#[cfg(test)]
const FLAG_NO_VIEW: i64 = 0x20;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Flatten eligible annotations on one leaf page.
///
/// Returns the number of annotations that were flattened (removed from
/// `/Annots` and burned into the page content).
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_ref` does not resolve to a `/Type /Page`
///   dictionary.
/// - Any error from [`Pdf::resolve`] or content-stream decoding.
fn flatten_annotations_on_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    mode: FlattenMode,
) -> Result<usize> {
    // ── Step 1: enumerate annotation handles without reading /Rect ────────
    // qpdf's page helper obtains annotation handles first; the annotation
    // helper validates /Rect only after the flags gate in
    // getPageContentForAppearance. Keep this route lazy instead of using
    // enumerate_page_annotations, whose public projection intentionally
    // materializes /Rect for its callers.
    let annot_refs = {
        let mut page_helper = PageObjectHelper::new(page_ref, pdf);
        page_helper.get_annotations()?
    };

    // ── Step 2: for each annotation, decide eligibility and collect data ───
    enum AppearanceTarget {
        Canonical(ObjectHandle),
        Bridge(ObjectRef),
    }

    struct AnnotData {
        annot_ref: ObjectRef,
        appearance: AppearanceTarget,
        flags: i64,
    }

    let mut candidates: Vec<AnnotData> = Vec::new();
    // Track annot_refs that should be removed from /Annots (same set).
    let mut to_remove: Vec<ObjectRef> = Vec::new();
    #[cfg(test)]
    let (qpdf_flag_contract, skip_widgets, page_rotate, required_flags, forbidden_flags) =
        match mode {
            FlattenMode::Flags {
                required,
                forbidden,
                skip_widgets,
                page_rotate,
                ..
            } => (true, skip_widgets, page_rotate, required, forbidden),
            _ => (false, false, 0, 0, 0),
        };
    #[cfg(not(test))]
    let (qpdf_flag_contract, skip_widgets, page_rotate, required_flags, forbidden_flags) =
        match mode {
            FlattenMode::Flags {
                required,
                forbidden,
                skip_widgets,
                page_rotate,
                ..
            } => (true, skip_widgets, page_rotate, required, forbidden),
        };

    for annot_ref in annot_refs {
        if skip_widgets && AnnotationObjectHelper::new(annot_ref, pdf).get_subtype()? == b"Widget" {
            continue;
        }
        // Read /F through the canonical helper first. A zero result is also
        // qpdf's fail-soft value for absent/non-integer /F. Only a detected
        // Pdf::set_object holder redirect may use the compatibility bridge.
        let canonical_flags = AnnotationObjectHelper::new(annot_ref, pdf).get_flags()?;
        let legacy_flag_redirect =
            canonical_flags == 0 && has_bare_reference_redirect(pdf, annot_ref, "F")?;
        let flags = if legacy_flag_redirect {
            read_annot_flags(pdf, annot_ref)?
        } else {
            canonical_flags
        };
        #[cfg(test)]
        let hidden = (flags & FLAG_HIDDEN) != 0;
        #[cfg(test)]
        let print_bit = (flags & FLAG_PRINT) != 0;
        #[cfg(test)]
        let no_view = (flags & FLAG_NO_VIEW) != 0;

        // qpdf only retains annotations without any appearance dictionary.
        // Once /AP is present, a missing selected /N stream is itself a
        // flattening/removal outcome (for example an unchecked checkbox).
        let (appearance_dictionary, appearance) = {
            let mut helper = AnnotationObjectHelper::new(annot_ref, pdf);
            (
                helper.get_appearance_dictionary()?,
                helper.get_appearance_stream(b"N", None)?,
            )
        };
        let has_appearance = if appearance_dictionary.is_null() {
            false
        } else if appearance_dictionary.as_reference().is_some() {
            // A bare-reference value is the in-memory holder shape, not a
            // qpdf parsed object. Ask the bridge whether its terminal value
            // is actually null before deciding whether to prune it.
            annotation_has_appearance_dictionary(pdf, annot_ref)?
        } else {
            true
        };
        let legacy_appearance_redirect = has_bare_reference_redirect(pdf, annot_ref, "AP")?
            || has_bare_reference_redirect_in_handle(pdf, &appearance_dictionary, b"/N")?
            || if let Some(appearance_ref) = appearance_dictionary.object_ref() {
                has_bare_reference_redirect(pdf, appearance_ref, "N")?
            } else {
                false
            };
        let appearance = if appearance.as_stream_dict().is_some() {
            if let Some(appearance_ref) = appearance.object_ref() {
                let legacy_geometry_redirect =
                    has_bare_reference_redirect(pdf, appearance_ref, "BBox")?
                        || has_bare_reference_redirect(pdf, appearance_ref, "Matrix")?;
                if legacy_flag_redirect || legacy_geometry_redirect {
                    AppearanceTarget::Bridge(appearance_ref)
                } else {
                    AppearanceTarget::Canonical(appearance)
                }
            } else {
                AppearanceTarget::Canonical(appearance)
            }
        } else {
            if !legacy_appearance_redirect {
                if qpdf_flag_contract && has_appearance {
                    to_remove.push(annot_ref);
                }
                continue;
            }
            match resolve_ap_n(pdf, annot_ref)? {
                Some(appearance_ref) => AppearanceTarget::Bridge(appearance_ref),
                None => {
                    if qpdf_flag_contract && has_appearance {
                        to_remove.push(annot_ref);
                    }
                    continue;
                }
            }
        };

        // Mode eligibility.
        let eligible = match mode {
            #[cfg(test)]
            FlattenMode::All => !hidden,
            #[cfg(test)]
            FlattenMode::Print => print_bit && !hidden,
            #[cfg(test)]
            FlattenMode::Screen => !print_bit && !hidden && !no_view,
            FlattenMode::Flags {
                required,
                forbidden,
                ..
            } => (flags & forbidden) == 0 && (flags & required) == required,
        };
        if !eligible {
            if qpdf_flag_contract {
                to_remove.push(annot_ref);
            }
            continue;
        }
        if qpdf_flag_contract {
            to_remove.push(annot_ref);
        }

        // The retained test-only legacy modes predate qpdf's direct flag API
        // and intentionally preserve their zero-area/inverted-rectangle
        // behavior. The qpdf-shaped helper owns the rectangle and appearance
        // geometry calculation for both paths.
        if !qpdf_flag_contract {
            let has_rect = pdf
                .resolve_borrowed(annot_ref)?
                .as_dict()
                .is_some_and(|dict| !matches!(dict.get("Rect"), None | Some(Object::Null)));
            if !has_rect {
                continue;
            }
            let rect = AnnotationObjectHelper::new(annot_ref, pdf).get_rect()?;
            let (llx, urx) = if rect.llx <= rect.urx {
                (rect.llx, rect.urx)
            } else {
                (rect.urx, rect.llx)
            };
            let (lly, ury) = if rect.lly <= rect.ury {
                (rect.lly, rect.ury)
            } else {
                (rect.ury, rect.lly)
            };
            if (urx - llx).abs() < 1e-6 || (ury - lly).abs() < 1e-6 {
                continue;
            }
        }

        candidates.push(AnnotData {
            annot_ref,
            appearance,
            flags,
        });
    }

    if candidates.is_empty() && to_remove.is_empty() {
        return Ok(0);
    }

    if candidates.is_empty() {
        let Object::Dictionary(mut page_dict) = pdf.resolve(page_ref)? else {
            // cov:ignore-start: repaired PageDocumentHelper snapshots contain leaf dictionaries
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary after flatten"
            )));
            // cov:ignore-end
        };
        replace_pruned_annots(pdf, &mut page_dict, &to_remove, qpdf_flag_contract)?;
        if qpdf_flag_contract {
            // qpdf wraps the page whenever the annotation array changed, even
            // if every selected appearance produced empty drawing content.
            add_qpdf_flatten_contents(pdf, &mut page_dict, Vec::new())?; // cov:ignore: covered structurally by indirect-contents public fixture
        } // cov:ignore: llvm-cov maps the tested qpdf wrapper branch to this synthetic closing brace
        pdf.set_object(page_ref, Object::Dictionary(page_dict));
        return Ok(0);
    }

    // qpdf's document helper appends wrapper streams around the original
    // contents. The legacy page-level API still coalesces its input.
    if !qpdf_flag_contract {
        coalesce_page_contents(pdf, page_ref)?;
    }

    // ── Step 4: Materialize /Resources on the leaf page ────────────────────
    // Resolve inherited resources, then clone them so we can add /XObject
    // entries without mutating shared parent /Resources dicts.
    let inherited_resources = resolve_inherited_resources(pdf, page_ref)?;
    let mut resources_dict = inherited_resources.unwrap_or_default();

    // Get existing /XObject sub-dict (or create empty).
    let mut xobj_dict: Dictionary = match resources_dict.remove("XObject") {
        Some(Object::Dictionary(d)) => d,
        Some(Object::Reference(r)) => match pdf.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => Dictionary::new(),
        },
        _ => Dictionary::new(),
    };

    // ── Step 5: Build content appendix and register XObjects ──────────────
    let mut append_bytes: Vec<u8> = Vec::new();
    // Counter for unique XObject name generation.
    let mut xobj_counter: u32 = 1;

    let mut flattened_count = 0;
    for data in &candidates {
        // Choose a name that doesn't collide with existing /XObject keys.
        let xobj_name = loop {
            let candidate = format!("Fxo{xobj_counter}");
            xobj_counter += 1;
            if xobj_dict.get(candidate.as_str()).is_none() {
                break candidate;
            }
        };

        let resource_name = format!("/{xobj_name}");
        let content_result = match &data.appearance {
            AppearanceTarget::Canonical(_) => AnnotationObjectHelper::new(data.annot_ref, pdf)
                .get_page_content_for_appearance(
                    &resource_name,
                    page_rotate,
                    required_flags,
                    forbidden_flags,
                ),
            AppearanceTarget::Bridge(appearance_ref) => {
                // This branch is limited to Pdf::set_object holder chains.
                // The bridge normalizes only input values; qpdf placement,
                // flag gating, stream mutation, and emitted content remain
                // owned by AnnotationObjectHelper.
                match read_xobj_bbox_and_matrix(pdf, *appearance_ref)? {
                    (Some(bbox), matrix) => AnnotationObjectHelper::new(data.annot_ref, pdf)
                        .get_page_content_for_selected_appearance_with_geometry(
                            &resource_name,
                            *appearance_ref,
                            page_rotate,
                            required_flags,
                            forbidden_flags,
                            crate::annotation_helper::AppearanceContentOverrides::with_geometry(
                                bbox, matrix, data.flags,
                            ),
                        ),
                    (None, _) => Ok(Vec::new()),
                }
            }
        };
        let content = content_result?;
        if content.is_empty() {
            continue;
        }

        // Register the Form XObject only when qpdf produced drawing content.
        let xobject = match &data.appearance {
            AppearanceTarget::Canonical(appearance) => match appearance.object_ref() {
                Some(appearance_ref) => Object::Reference(appearance_ref),
                None => appearance.materialize()?,
            },
            AppearanceTarget::Bridge(appearance_ref) => Object::Reference(*appearance_ref),
        };
        xobj_dict.insert(xobj_name.as_str(), xobject);
        append_bytes.extend_from_slice(&content);
        flattened_count += 1;
        if !qpdf_flag_contract {
            to_remove.push(data.annot_ref);
        }
    }

    if flattened_count == 0 {
        let Object::Dictionary(mut page_dict) = pdf.resolve(page_ref)? else {
            // cov:ignore-start: repaired PageDocumentHelper snapshots contain leaf dictionaries
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary after flatten"
            )));
            // cov:ignore-end
        };
        replace_pruned_annots(pdf, &mut page_dict, &to_remove, qpdf_flag_contract)?;
        if qpdf_flag_contract {
            add_qpdf_flatten_contents(pdf, &mut page_dict, Vec::new())?; // cov:ignore: covered structurally by indirect-contents public fixture
        } // cov:ignore: llvm-cov maps the tested qpdf wrapper branch to this synthetic closing brace
        pdf.set_object(page_ref, Object::Dictionary(page_dict));
        return Ok(0);
    }

    // ── Step 6: Add qpdf-shaped page-content wrappers ─────────────────────
    let page_obj = pdf.resolve(page_ref)?;
    let Object::Dictionary(mut page_dict) = page_obj else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary after flatten"
        )));
    };

    if qpdf_flag_contract {
        add_qpdf_flatten_contents(pdf, &mut page_dict, append_bytes)?;
    } else {
        let existing_content = page_content_bytes(pdf, page_ref)?;
        let mut new_content = existing_content;
        if !new_content.is_empty() && new_content.last() != Some(&b'\n') {
            new_content.push(b'\n');
        }
        new_content.extend_from_slice(&append_bytes);
        let stream_ref = add_content_stream(pdf, new_content)?;
        page_dict.insert("Contents", Object::Reference(stream_ref));
    }

    // Write updated /Resources with the new /XObject entries.
    resources_dict.insert("XObject", Object::Dictionary(xobj_dict));
    page_dict.insert("Resources", Object::Dictionary(resources_dict));

    // ── Step 8: Remove flattened annotations from /Annots ─────────────────
    replace_pruned_annots(pdf, &mut page_dict, &to_remove, qpdf_flag_contract)?;

    pdf.set_object(page_ref, Object::Dictionary(page_dict));

    Ok(flattened_count)
}

fn replace_pruned_annots<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_dict: &mut Dictionary,
    to_remove: &[ObjectRef],
    preserve_indirect_holder: bool,
) -> Result<()> {
    let old_annots = page_dict.get("Annots").cloned();
    let new_annots = build_pruned_annots_array(pdf, page_dict, to_remove)?;
    if new_annots.is_empty() {
        page_dict.remove("Annots");
    } else if preserve_indirect_holder {
        if let Some(Object::Reference(array_ref)) = old_annots {
            pdf.set_object(array_ref, Object::Array(new_annots));
        } else {
            page_dict.insert("Annots", Object::Array(new_annots));
        }
    } else {
        page_dict.insert("Annots", Object::Array(new_annots));
    }
    Ok(())
}

fn add_qpdf_flatten_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_dict: &mut Dictionary,
    append_bytes: Vec<u8>,
) -> Result<()> {
    let before = add_content_stream(pdf, b"q\n".to_vec())?;
    let mut after = b"\nQ\n".to_vec();
    after.extend_from_slice(&append_bytes);
    let after = add_content_stream(pdf, after)?;
    let old = page_dict.remove("Contents");
    let mut contents = vec![Object::Reference(before)];
    match old {
        Some(Object::Array(items)) => contents.extend(items), // cov:ignore: direct and indirect arrays share qpdf expansion; indirect holder is covered
        Some(Object::Reference(reference)) => {
            match resolve_ref_chain(pdf, &Object::Reference(reference))?.0 {
                Object::Array(items) => contents.extend(items),
                _ => contents.push(Object::Reference(reference)),
            }
        }
        Some(value) => contents.push(value), // cov:ignore: malformed direct Contents retained conservatively
        None => {}
    }
    contents.push(Object::Reference(after));
    page_dict.insert("Contents", Object::Array(contents));
    Ok(())
}

fn add_content_stream<R: Read + Seek>(pdf: &mut Pdf<R>, data: Vec<u8>) -> Result<ObjectRef> {
    let stream_ref = next_object_ref(pdf)?;
    let mut dictionary = Dictionary::new();
    dictionary.insert("Length", Object::Integer(data.len() as i64));
    pdf.set_object(stream_ref, Object::Stream(Stream::new(dictionary, data)));
    Ok(stream_ref)
}

fn annotation_has_appearance_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot_ref: ObjectRef,
) -> Result<bool> {
    let Object::Dictionary(annot) = pdf.resolve(annot_ref)? else {
        // cov:ignore: enumerator yields resolved annotation dictionaries
        return Ok(false); // cov:ignore: enumerator yields resolved annotation dictionaries
    };
    let Some(ap) = annot.get("AP").cloned() else {
        return Ok(false);
    };
    Ok(!matches!(resolve_ref_chain(pdf, &ap)?.0, Object::Null))
}

/// Flatten eligible annotations on every leaf page in the document.
///
/// Returns the total number of annotations flattened across all pages.
///
/// # Errors
///
/// Propagates any error from [`flatten_annotations_on_page`] or
/// [`crate::pages::page_refs`].
#[cfg(test)]
fn flatten_annotations<R: Read + Seek>(pdf: &mut Pdf<R>, mode: FlattenMode) -> Result<usize> {
    let page_refs = crate::pages::page_refs(pdf)?;
    let mut total = 0;
    for page_ref in page_refs {
        total += flatten_annotations_on_page(pdf, page_ref, mode)?;
    }
    Ok(total)
}

/// qpdf `QPDFPageDocumentHelper::flattenAnnotations` boundary.
pub(crate) fn flatten_annotations_qpdf<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_refs: &[ObjectRef],
    required_flags: i64,
    forbidden_flags: i64,
) -> Result<()> {
    let need_appearances = acroform_need_appearances(pdf)?;
    let default_resources = acroform_default_resources(pdf)?;
    for &page_ref in page_refs {
        materialize_page_resources(pdf, page_ref)?;
        if !need_appearances {
            if let Some(default_resources) = default_resources.as_ref() {
                merge_widget_default_resources_on_page(pdf, page_ref, default_resources)?;
            }
        }
        let page_rotate = direct_page_rotate(pdf, page_ref)?;
        flatten_annotations_on_page(
            pdf,
            page_ref,
            FlattenMode::Flags {
                required: required_flags,
                forbidden: forbidden_flags,
                skip_widgets: need_appearances,
                page_rotate,
            },
        )?; // cov:ignore: helper error propagation has no distinct observable branch
    }
    if !need_appearances {
        remove_acroform(pdf)?;
    }
    Ok(())
}

fn direct_page_rotate<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<i32> {
    let Object::Dictionary(page) = pdf.resolve(page_ref)? else {
        // cov:ignore: repaired PageDocumentHelper snapshots contain leaf dictionaries
        return Ok(0); // cov:ignore: repaired page snapshot is always a dictionary
    };
    let Some(rotate) = page.get("Rotate").cloned() else {
        return Ok(0);
    };
    let rotate = match rotate {
        Object::Integer(value) => value,
        Object::Reference(reference) => match pdf.resolve(reference)? {
            // cov:ignore: malformed indirect Rotate fallback
            Object::Integer(value) => value,
            _ => 0, // cov:ignore: malformed indirect Rotate fallback
        },
        _ => 0, // cov:ignore: malformed direct Rotate fallback
    };
    Ok(i32::try_from(rotate).unwrap_or(0))
}

fn materialize_page_resources<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<()> {
    // `getAttribute("/Resources", true)` may yield a malformed value. qpdf
    // replaces that value with an empty dictionary instead of rejecting the
    // whole flattening operation.
    let resources = match resolve_inherited_resources(pdf, page_ref) {
        Ok(Some(resources)) => resources,
        Ok(None) => Dictionary::new(),
        Err(Error::Unsupported(message)) if message.contains("/Resources") => Dictionary::new(),
        Err(error) => return Err(error), // cov:ignore: non-Resources page-walk failures propagate unchanged
    };
    let Object::Dictionary(mut page) = pdf.resolve(page_ref)? else {
        // cov:ignore-start: repaired PageDocumentHelper snapshots contain leaf dictionaries
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a page dictionary"
        )));
        // cov:ignore-end
    };
    page.insert("Resources", Object::Dictionary(resources));
    pdf.set_object(page_ref, Object::Dictionary(page));
    Ok(())
}

fn acroform_default_resources<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Option<Object>> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None); // cov:ignore: a parsed Pdf always has a root reference
    };
    let Object::Dictionary(root) = pdf.resolve(root_ref)? else {
        return Ok(None); // cov:ignore: parsed catalog root is a dictionary
    };
    let Some(acroform) = root.get("AcroForm").cloned() else {
        return Ok(None);
    };
    let acroform = match resolve_ref_chain(pdf, &acroform)?.0 {
        Object::Dictionary(dict) => dict,
        _ => return Ok(None), // cov:ignore: malformed AcroForm is ignored like qpdf
    };
    Ok(acroform.get("DR").cloned())
}

fn merge_widget_default_resources_on_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    default_resources: &Object,
) -> Result<()> {
    for annotation in enumerate_page_annotations(pdf, page_ref)? {
        if !annotation.is_widget {
            continue; // cov:ignore: non-widget annotations do not merge default resources
        }
        let Some(appearance_ref) = resolve_ap_n(pdf, annotation.annot_ref)? else {
            continue; // cov:ignore: widget without selected appearance has no merge target
        };
        let Object::Stream(mut appearance) = pdf.resolve(appearance_ref)? else {
            continue; // cov:ignore: selected appearance must be a stream
        };
        let Some(resources) = appearance.dict.get("Resources").cloned() else {
            continue; // cov:ignore: qpdf mergeResources no-ops without appearance resources
        };
        let mut resources = match resolve_ref_chain(pdf, &resources)?.0 {
            Object::Dictionary(dict) => dict,
            _ => continue, // cov:ignore: malformed appearance resources are ignored
        };
        let default_resources = match resolve_ref_chain(pdf, default_resources)?.0 {
            Object::Dictionary(dict) => dict,
            _ => continue, // cov:ignore: malformed DR is ignored like qpdf
        };
        for (category, source) in default_resources.iter() {
            let (source, _) = resolve_ref_chain(pdf, source)?;
            let Object::Dictionary(source) = source else {
                continue; // cov:ignore: qpdf ignores non-dictionary default resource categories
            };
            let mut destination = match resources.remove(category) {
                Some(Object::Dictionary(dict)) => dict,
                Some(Object::Reference(reference)) => {
                    match resolve_ref_chain(pdf, &Object::Reference(reference))?.0 {
                        Object::Dictionary(dict) => dict,
                        _ => Dictionary::new(), // cov:ignore: malformed category holder is replaced
                    }
                }
                _ => Dictionary::new(), // cov:ignore: absent or invalid category is materialized
            };
            for (name, value) in source.iter() {
                if destination.get(name).is_none() {
                    destination.insert(name, value.clone());
                }
            }
            resources.insert(category, Object::Dictionary(destination));
        }
        appearance
            .dict
            .insert("Resources", Object::Dictionary(resources));
        pdf.set_object(appearance_ref, Object::Stream(appearance));
    }
    Ok(())
}

fn acroform_need_appearances<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false); // cov:ignore: parsed Pdf always has a root reference
    };
    let Object::Dictionary(root) = pdf.resolve(root_ref)? else {
        return Ok(false); // cov:ignore: parsed catalog root is a dictionary
    };
    let Some(acroform) = root.get("AcroForm").cloned() else {
        return Ok(false);
    };
    let acroform = match resolve_ref_chain(pdf, &acroform)?.0 {
        Object::Dictionary(dict) => dict,
        _ => return Ok(false), // cov:ignore: malformed direct AcroForm is ignored like qpdf
    };
    let Some(value) = acroform.get("NeedAppearances").cloned() else {
        return Ok(false);
    };
    Ok(matches!(
        resolve_ref_chain(pdf, &value)?.0,
        Object::Boolean(true)
    ))
}

fn remove_acroform<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(()); // cov:ignore: parsed Pdf always has a root reference
    };
    let Object::Dictionary(mut root) = pdf.resolve(root_ref)? else {
        return Ok(()); // cov:ignore: parsed catalog root is a dictionary
    };
    root.remove("AcroForm");
    pdf.set_object(root_ref, Object::Dictionary(root));
    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return whether a raw dictionary key has the in-memory holder shape created
/// by `Pdf::set_object`: the key contains one reference whose stored value is
/// another bare reference. Parsed PDF objects do not use this representation;
/// their first referenced value is the terminal PDF object. This predicate is
/// therefore the boundary that permits the compatibility bridge without
/// broadening it to ordinary parsed indirect values.
fn has_bare_reference_redirect<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    owner_ref: ObjectRef,
    key: &str,
) -> Result<bool> {
    let value = match pdf.resolve(owner_ref)? {
        Object::Dictionary(dict) => dict.get(key).cloned(),
        Object::Stream(stream) => stream.dict.get(key).cloned(),
        _ => None,
    };
    let Some(Object::Reference(reference)) = value else {
        return Ok(false);
    };
    Ok(matches!(pdf.resolve(reference)?, Object::Reference(_)))
}

/// Return whether a resolved canonical dictionary handle contains a
/// `Pdf::set_object` holder redirect at `key`.
///
/// This is the direct-dictionary counterpart to
/// [`has_bare_reference_redirect`]. A direct `/AP` dictionary has no
/// `ObjectRef` of its own, so its `/N` child must be inspected through the
/// live `ObjectHandle` rather than through the legacy object cache.
fn has_bare_reference_redirect_in_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    owner: &ObjectHandle,
    key: &[u8],
) -> Result<bool> {
    let value = owner.get_key(key);
    let Some(reference) = value.object_ref() else {
        return Ok(false);
    };
    Ok(matches!(pdf.resolve(reference)?, Object::Reference(_)))
}

/// Read the annotation's `/F` flags integer, resolving indirect references.
/// Returns 0 if absent (absence = no special flags).
fn read_annot_flags<R: Read + Seek>(pdf: &mut Pdf<R>, annot_ref: ObjectRef) -> Result<i64> {
    let obj = pdf.resolve_borrowed(annot_ref)?;
    let Some(dict) = obj.as_dict() else {
        return Ok(0);
    };
    let flags_val = match dict.get("F").cloned() {
        None | Some(Object::Null) => return Ok(0),
        Some(v) => v,
    };
    let resolved = resolve_ref_chain(pdf, &flags_val)?.0;
    Ok(resolved.as_integer().unwrap_or(0))
}

/// Resolve an annotation's `/AP/N` to a Form XObject object ref.
///
/// Handles three `/AP/N` forms:
/// - `Reference → Stream`: returns the ref as-is (no clone, review-pattern #1).
/// - Inline `Stream`: materializes as a new indirect object, returns its ref.
/// - Sub-dictionary (state dict, e.g. checkbox): selects the stream indicated
///   by `/AS` on the annotation dict; if `/AS` is absent or missing, returns `None`.
///
/// Returns `None` if `/AP` or `/AP/N` is absent, or if the state cannot be
/// resolved.
fn resolve_ap_n<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // Read /AP dict from annotation (resolve indirect /AP reference).
    let ap_val = {
        let obj = pdf.resolve_borrowed(annot_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(None);
        };
        dict.get("AP").cloned()
    };
    let ap_val = match ap_val {
        None | Some(Object::Null) => return Ok(None),
        Some(v) => v,
    };
    let (ap_value, ap_ref) = resolve_ref_chain(pdf, &ap_val)?;
    let mut ap_dict: Dictionary = match ap_value {
        Object::Dictionary(d) => d,
        _ => return Ok(None),
    };

    // Get /N value from /AP dict.
    let n_val = match ap_dict.get("N").cloned() {
        None | Some(Object::Null) => return Ok(None),
        Some(v) => v,
    };

    let (n_resolved_for_type, n_terminal_ref) = resolve_ref_chain(pdf, &n_val)?;

    match n_resolved_for_type {
        Object::Stream(s) => {
            if let Some(stream_ref) = n_terminal_ref {
                return Ok(Some(stream_ref));
            }
            // Inline stream in malformed PDF — materialize as new indirect object.
            let new_ref = next_object_ref(pdf)?;
            pdf.set_object(new_ref, Object::Stream(s));
            ap_dict.insert("N", Object::Reference(new_ref));
            replace_appearance_dictionary(pdf, annot_ref, ap_ref, ap_dict)?;
            Ok(Some(new_ref))
        }
        Object::Dictionary(mut state_dict) => {
            // Sub-dictionary: select stream by annotation's /AS name.
            let as_name: Vec<u8> = {
                let obj = pdf.resolve_borrowed(annot_ref)?;
                let Some(adict) = obj.as_dict() else {
                    return Ok(None);
                };
                let Some(as_value) = adict.get("AS").cloned() else {
                    return Ok(None);
                };
                match resolve_ref_chain(pdf, &as_value)?.0 {
                    Object::Name(n) => n,
                    _ => return Ok(None),
                }
            };
            let Some(state_value) = state_dict.get(as_name.as_slice()).cloned() else {
                return Ok(None);
            };
            let (state_value, state_terminal_ref) = resolve_ref_chain(pdf, &state_value)?;
            match state_value {
                Object::Stream(s) => {
                    if let Some(stream_ref) = state_terminal_ref {
                        return Ok(Some(stream_ref));
                    }
                    // Inline stream in state dict of malformed PDF.
                    let new_ref = next_object_ref(pdf)?;
                    pdf.set_object(new_ref, Object::Stream(s));
                    state_dict.insert(as_name, Object::Reference(new_ref));
                    if let Some(state_ref) = n_terminal_ref {
                        pdf.set_object(state_ref, Object::Dictionary(state_dict));
                    } else {
                        ap_dict.insert("N", Object::Dictionary(state_dict));
                        replace_appearance_dictionary(pdf, annot_ref, ap_ref, ap_dict)?;
                    }
                    Ok(Some(new_ref))
                }
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// Replace the terminal `/AP` dictionary after materializing an inline stream.
///
/// qpdf mutates the selected `QPDFObjectHandle` in place. Rust values are
/// owned, so retain the same object identity by replacing the terminal indirect
/// dictionary, or the annotation's direct `/AP` value when it is inline.
fn replace_appearance_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot_ref: ObjectRef,
    ap_ref: Option<ObjectRef>,
    ap_dict: Dictionary,
) -> Result<()> {
    if let Some(ap_ref) = ap_ref {
        pdf.set_object(ap_ref, Object::Dictionary(ap_dict));
        return Ok(());
    }
    let Object::Dictionary(mut annotation) = pdf.resolve(annot_ref)? else {
        return Ok(()); // cov:ignore: enumeration guarantees an annotation dictionary
    };
    annotation.insert("AP", Object::Dictionary(ap_dict));
    pdf.set_object(annot_ref, Object::Dictionary(annotation));
    Ok(())
}

/// Read `/BBox` and `/Matrix` from a Form XObject stream dictionary.
///
/// Returns `(Some([x0,y0,x1,y1]), [a,b,c,d,e,f])`.
/// `/BBox` is required; returns `(None, identity)` if absent or invalid.
/// `/Matrix` defaults to identity if absent.
fn read_xobj_bbox_and_matrix<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobj_ref: ObjectRef,
) -> Result<(Option<Rectangle>, Matrix)> {
    let identity = Matrix::default();

    let obj = pdf.resolve(xobj_ref)?;
    let stream_dict = match obj {
        Object::Stream(s) => s.dict,
        _ => return Ok((None, identity)),
    };

    // Read /BBox — must be a 4-element array (review-pattern #2: resolve ref).
    let bbox_val = stream_dict.get("BBox").cloned();
    let bbox = match bbox_val {
        None | Some(Object::Null) => return Ok((None, identity)),
        Some(v) => v,
    };
    let bbox_arr = match resolve_ref_chain(pdf, &bbox)?.0 {
        Object::Array(a) => a,
        _ => return Ok((None, identity)),
    };
    if bbox_arr.len() != 4 {
        return Ok((None, identity));
    }
    let mut bbox_vals = [0.0f64; 4];
    for (i, elem) in bbox_arr.iter().take(4).enumerate() {
        let (elem, _) = resolve_ref_chain(pdf, elem)?;
        bbox_vals[i] = match elem {
            Object::Integer(n) => n as f64,
            Object::Real(r) | Object::RealLiteral { value: r, .. } => r,
            _ => return Ok((None, identity)),
        };
    }

    // Read /Matrix — 6-element array, defaults to identity (review-pattern #2).
    // `stream_dict` is already an owned clone, so reuse it — no second resolve.
    let matrix_val = stream_dict.get("Matrix").cloned();
    let ap_matrix = match matrix_val {
        None | Some(Object::Null) => identity,
        Some(matrix) => match resolve_ref_chain(pdf, &matrix)?.0 {
            Object::Array(a) if a.len() == 6 => {
                let mut m = [0.0f64; 6];
                let mut valid = true;
                for (i, elem) in a.iter().take(6).enumerate() {
                    let (elem, _) = resolve_ref_chain(pdf, elem)?;
                    m[i] = match elem {
                        Object::Integer(n) => n as f64,
                        Object::Real(r) | Object::RealLiteral { value: r, .. } => r,
                        _ => {
                            valid = false;
                            break;
                        }
                    };
                }
                if valid {
                    Matrix::from(m)
                } else {
                    identity
                }
            }
            _ => identity,
        },
    };

    Ok((Some(Rectangle::from(bbox_vals)), ap_matrix))
}

/// Build the pruned `/Annots` array, removing all refs in `to_remove`.
///
/// Resolves the existing `/Annots` value (which may be an indirect reference)
/// from `page_dict`. Returns a direct array without the removed entries.
fn build_pruned_annots_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_dict: &Dictionary,
    to_remove: &[ObjectRef],
) -> Result<Vec<Object>> {
    let annots_val = match page_dict.get("Annots").cloned() {
        None | Some(Object::Null) => return Ok(Vec::new()),
        Some(v) => v,
    };
    let annots_arr = match resolve_ref_chain(pdf, &annots_val)?.0 {
        Object::Array(a) => a,
        _ => return Ok(Vec::new()),
    };

    let pruned: Vec<Object> = annots_arr
        .into_iter()
        .filter(|entry| match entry {
            Object::Reference(r) => !to_remove.contains(r),
            _ => true, // keep non-ref entries (unusual, but don't drop them)
        })
        .collect();

    Ok(pruned)
}

/// Allocate the next unused indirect-object reference.
///
/// Uses the same idiom as `page_rotate::next_object_ref`: one past the current
/// highest object number in the cache.
fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let n = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::{page_content_bytes, page_refs};
    use crate::writer::write_qpdf_to_memory;
    use crate::{Object, ObjectRef, Pdf};
    use std::io::Cursor;

    #[test]
    fn qpdf_document_flatten_empty_page_exercises_public_contract() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        flatten_annotations_qpdf(&mut pdf, &[ObjectRef::new(3, 0)], 0, 0x3).unwrap();
        let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("fixture page must remain a dictionary"); // cov:ignore: fixture invariant
        };
        assert!(matches!(page.get("Resources"), Some(Object::Dictionary(_))));
    }

    #[test]
    fn direct_page_rotate_resolves_an_indirect_integer() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("fixture page must be a dictionary"); // cov:ignore: fixture invariant
        };
        page.insert("Rotate", Object::Reference(ObjectRef::new(4, 0)));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));
        pdf.set_object(ObjectRef::new(4, 0), Object::Integer(270));

        assert_eq!(
            direct_page_rotate(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            270
        );
    }

    #[test]
    fn acroform_need_appearances_reads_a_direct_boolean() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let mut acroform = Dictionary::new();
        acroform.insert("NeedAppearances", Object::Boolean(true));
        let mut catalog = Dictionary::new();
        catalog.insert("AcroForm", Object::Dictionary(acroform));
        pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

        assert!(acroform_need_appearances(&mut pdf).unwrap());
    }

    #[test]
    fn qpdf_flatten_expands_a_multihop_contents_array() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("fixture page must be a dictionary"); // cov:ignore: fixture invariant
        };
        page.insert("Contents", Object::Reference(ObjectRef::new(6, 0)));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Array(vec![
                Object::Reference(ObjectRef::new(8, 0)),
                Object::Reference(ObjectRef::new(9, 0)),
            ]),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Stream(Stream::new(Dictionary::new(), b"BT ET\n".to_vec())),
        );
        pdf.set_object(
            ObjectRef::new(9, 0),
            Object::Stream(Stream::new(Dictionary::new(), b"q Q\n".to_vec())),
        );

        let Object::Dictionary(mut page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("fixture page must be a dictionary"); // cov:ignore: fixture invariant
        };
        add_qpdf_flatten_contents(&mut pdf, &mut page, Vec::new()).unwrap();

        assert!(matches!(
            page.get("Contents"),
            Some(Object::Array(items))
                if items.len() == 4
                    && items[1] == Object::Reference(ObjectRef::new(8, 0))
                    && items[2] == Object::Reference(ObjectRef::new(9, 0))
        ));
    }

    #[test]
    fn qpdf_flatten_merges_an_indirect_default_resource_category() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut appearance_resources = Dictionary::new();
        appearance_resources.insert("Font", Object::Dictionary(Dictionary::new()));
        let mut appearance = Dictionary::new();
        appearance.insert("Resources", Object::Dictionary(appearance_resources));
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Stream(Stream::new(appearance, Vec::new())),
        );
        let mut ap = Dictionary::new();
        ap.insert("N", Object::Reference(ObjectRef::new(5, 0)));
        let mut widget = Dictionary::new();
        widget.insert("Subtype", Object::Name(b"Widget".to_vec()));
        widget.insert("AP", Object::Dictionary(ap));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(widget));
        let mut font_category = Dictionary::new();
        font_category.insert("F1", Object::Reference(ObjectRef::new(7, 0)));
        pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(font_category));
        pdf.set_object(ObjectRef::new(7, 0), Object::Dictionary(Dictionary::new()));
        let mut default_resources = Dictionary::new();
        default_resources.insert("Font", Object::Reference(ObjectRef::new(6, 0)));

        merge_widget_default_resources_on_page(
            &mut pdf,
            ObjectRef::new(3, 0),
            &Object::Dictionary(default_resources),
        )
        .unwrap();

        let Object::Stream(appearance) = pdf.resolve(ObjectRef::new(5, 0)).unwrap() else {
            panic!("fixture appearance must remain a stream"); // cov:ignore: fixture invariant
        };
        let Some(Object::Dictionary(resources)) = appearance.dict.get("Resources") else {
            panic!("fixture appearance must retain resources"); // cov:ignore: fixture invariant
        };
        let Some(Object::Dictionary(fonts)) = resources.get("Font") else {
            panic!("fixture appearance must retain Font resources"); // cov:ignore: fixture invariant
        };
        assert_eq!(
            fonts.get("F1"),
            Some(&Object::Reference(ObjectRef::new(7, 0)))
        );
    }

    #[test]
    fn acroform_need_appearances_resolves_a_multihop_boolean() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let mut acroform = Dictionary::new();
        acroform.insert("NeedAppearances", Object::Reference(ObjectRef::new(4, 0)));
        let mut catalog = Dictionary::new();
        catalog.insert("AcroForm", Object::Dictionary(acroform));
        pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Reference(ObjectRef::new(5, 0)),
        );
        pdf.set_object(ObjectRef::new(5, 0), Object::Boolean(true));

        assert!(acroform_need_appearances(&mut pdf).unwrap());
    }

    #[test]
    fn qpdf_flatten_wraps_content_when_dropping_an_unselected_appearance() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut annotation = Dictionary::new();
        annotation.insert("AP", Object::Dictionary(Dictionary::new()));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annotation));

        flatten_annotations_qpdf(&mut pdf, &[ObjectRef::new(3, 0)], 0, 0x3).unwrap();

        let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("fixture page must remain a dictionary"); // cov:ignore: fixture invariant
        };
        assert!(page.get("Annots").is_none());
        assert!(matches!(
            page.get("Contents"),
            Some(Object::Array(items)) if items.len() == 2
        ));
    }

    #[test]
    fn qpdf_document_flatten_covers_widget_resources_and_removal_paths() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf(
            "/Rotate 90 /Annots [4 0 R 5 0 R 6 0 R]",
            &[],
        )))
        .unwrap();

        let mut appearance_resources = Dictionary::new();
        appearance_resources.insert("Font", Object::Reference(ObjectRef::new(8, 0)));
        let mut appearance = Dictionary::new();
        appearance.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        appearance.insert("Resources", Object::Reference(ObjectRef::new(7, 0)));
        pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(Dictionary::new()));
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(appearance_resources),
        );
        pdf.set_object(ObjectRef::new(8, 0), Object::Dictionary(Dictionary::new()));
        pdf.set_object(
            ObjectRef::new(12, 0),
            Object::Stream(Stream::new(appearance, Vec::new())),
        );

        let mut selected_ap = Dictionary::new();
        selected_ap.insert("N", Object::Reference(ObjectRef::new(12, 0)));
        let mut selected = Dictionary::new();
        selected.insert("Subtype", Object::Name(b"Widget".to_vec()));
        selected.insert("F", Object::Integer(0x10));
        selected.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(10),
                Object::Integer(20),
                Object::Integer(110),
                Object::Integer(40),
            ]),
        );
        selected.insert("AP", Object::Dictionary(selected_ap));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(selected));

        let mut unselected = Dictionary::new();
        unselected.insert("Subtype", Object::Name(b"Widget".to_vec()));
        unselected.insert("AP", Object::Dictionary(Dictionary::new()));
        pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(unselected));
        let mut link = Dictionary::new();
        link.insert("Subtype", Object::Name(b"Link".to_vec()));
        pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(link));

        let mut dr_fonts = Dictionary::new();
        dr_fonts.insert("Helv", Object::Integer(42));
        let mut dr = Dictionary::new();
        dr.insert("Font", Object::Dictionary(dr_fonts));
        pdf.set_object(ObjectRef::new(10, 0), Object::Dictionary(dr));
        let mut acroform = Dictionary::new();
        acroform.insert("DR", Object::Reference(ObjectRef::new(10, 0)));
        pdf.set_object(ObjectRef::new(9, 0), Object::Dictionary(acroform));
        let Object::Dictionary(mut root) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
            panic!("fixture root must be a dictionary"); // cov:ignore: fixture invariant
        };
        root.insert("AcroForm", Object::Reference(ObjectRef::new(9, 0)));
        pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(root));

        flatten_annotations_qpdf(&mut pdf, &[ObjectRef::new(3, 0)], 0, 0x3).unwrap();
        let Object::Dictionary(page) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("fixture page must be a dictionary"); // cov:ignore: fixture invariant
        };
        assert_eq!(
            page.get("Annots"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                6, 0
            ))]))
        );
        assert!(page.get("Contents").is_some());
        let Object::Dictionary(root) = pdf.resolve(ObjectRef::new(1, 0)).unwrap() else {
            panic!("fixture root must be a dictionary"); // cov:ignore: fixture invariant
        };
        assert!(root.get("AcroForm").is_none());
    }

    // -----------------------------------------------------------------------
    // Minimal PDF builder
    // -----------------------------------------------------------------------

    /// Build a minimal PDF byte vector.
    ///
    /// `page_extra` is appended to the page dict (e.g. `/Annots [4 0 R]`).
    /// `extra_objects` is `(object_number, raw_bytes)`.
    fn build_pdf(page_extra: &str, extra_objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        let page_body = format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page_extra} >>\nendobj\n"
        );
        pdf.extend_from_slice(page_body.as_bytes());

        let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
        for &(num, ref body) in extra_objects.iter() {
            let off = pdf.len() as u64;
            extra_offsets.push((num, off));
            pdf.extend_from_slice(body);
        }

        let xref_start = pdf.len() as u64;
        let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
        let total = max_num as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{:010} 00000 n \n", off1));
        xref.push_str(&format!("{:010} 00000 n \n", off2));
        xref.push_str(&format!("{:010} 00000 n \n", off3));
        for i in 4u32..=max_num {
            if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
                xref.push_str(&format!("{:010} 00000 n \n", off));
            } else {
                xref.push_str("0000000000 65535 f \n");
            }
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build a minimal Form XObject stream with given /BBox.
    fn make_xobj_stream(bbox: [f64; 4], content: &[u8]) -> Vec<u8> {
        let inner = format!(
            "<< /Type /XObject /Subtype /Form /BBox [{} {} {} {}] /Length {} >>",
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3],
            content.len()
        );
        let mut out = inner.into_bytes();
        out.extend_from_slice(b"\nstream\n");
        out.extend_from_slice(content);
        out.extend_from_slice(b"\nendstream\n");
        out
    }

    /// Wrap raw stream bytes with object header/footer.
    fn obj_wrap(num: u32, body: Vec<u8>) -> (u32, Vec<u8>) {
        let header = format!("{num} 0 obj\n").into_bytes();
        let footer = b"endobj\n".to_vec();
        let mut out = header;
        out.extend_from_slice(&body);
        out.extend_from_slice(&footer);
        (num, out)
    }

    /// Wrap a dictionary string as an indirect object.
    fn obj_dict(num: u32, dict: &str) -> (u32, Vec<u8>) {
        let body = format!("{dict}\n").into_bytes();
        obj_wrap(num, body)
    }

    /// Build a minimal annotation fixture for direct ObjectHandle placement
    /// tests. `stream_dictionary` is already a complete stream dictionary and
    /// is wrapped as object 5 when present.
    fn open_annotation_helper_fixture(
        annotation: &str,
        stream_dictionary: Option<&str>,
    ) -> Pdf<Cursor<Vec<u8>>> {
        let mut objects = vec![obj_dict(4, annotation)];
        if let Some(stream_dictionary) = stream_dictionary {
            let mut stream = stream_dictionary.as_bytes().to_vec();
            stream.extend_from_slice(b"\nstream\n\nendstream\n");
            objects.push(obj_wrap(5, stream));
        }
        Pdf::open(Cursor::new(build_pdf("", &objects))).unwrap()
    }

    #[test]
    fn annotation_helper_qpdf_validation_and_rotation_paths() {
        let stream_dictionary = "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Length 0 >>";
        let annotation = "<< /Type /Annot /Rect [10 20 110 40] /F 4 /AP << /N 5 0 R >> >>";

        let mut pdf = open_annotation_helper_fixture("<< /Type /Annot /Rect [0 0 100 20] >>", None);
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_helper_fixture(annotation, Some(stream_dictionary));
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 4)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_helper_fixture(
            "<< /Type /Annot /Rect [0 0 100] /AP << /N 5 0 R >> >>",
            Some(stream_dictionary),
        );
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_helper_fixture(
            "<< /Type /Annot /Rect [0 0 bad 20] /AP << /N 5 0 R >> >>",
            Some(stream_dictionary),
        );
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_helper_fixture(
            annotation,
            Some(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 0] /Length 0 >>",
            ),
        );
        assert!(!AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_helper_fixture(
            annotation,
            Some(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 bad 1 0 0] /Length 0 >>",
            ),
        );
        assert!(!AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        for rotate in [180, 270, 45] {
            let annotation = "<< /Type /Annot /Rect [10 20 110 40] /F 16 /AP << /N 5 0 R >> >>";
            let mut pdf = open_annotation_helper_fixture(annotation, Some(stream_dictionary));
            assert!(
                !AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
                    .get_page_content_for_appearance("/Fxo1", rotate, 0, 0)
                    .unwrap()
                    .is_empty(),
                "qpdf NoRotate path must produce content for rotation {rotate}"
            );
        }
    }

    #[test]
    fn flatten_annotations_uses_canonical_inline_appearance() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut appearance = Dictionary::new();
        appearance.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        let mut ap = Dictionary::new();
        ap.insert("N", Object::Stream(Stream::new(appearance, Vec::new())));
        let mut annotation = Dictionary::new();
        annotation.insert("Subtype", Object::Name(b"Widget".to_vec()));
        annotation.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        annotation.insert("AP", Object::Dictionary(ap));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annotation));

        assert_eq!(
            flatten_annotations_on_page(&mut pdf, ObjectRef::new(3, 0), FlattenMode::All).unwrap(),
            1
        );
        assert!(page_content_bytes(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .windows(2)
            .any(|window| window == b"Do"));
    }

    #[test]
    fn flatten_annotations_uses_canonical_indirect_appearance_dictionary() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n4, obj4_bytes) = obj_dict(4, "<< /Type /Annot /Rect [0 0 100 20] /AP 5 0 R >>");
        let (n5, obj5_bytes) = obj_dict(5, "<< /N 6 0 R >>");
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body);
        let bytes = build_pdf(
            "/Annots [4 0 R]",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert_eq!(
            flatten_annotations_on_page(&mut pdf, ObjectRef::new(3, 0), FlattenMode::All).unwrap(),
            1
        );
        assert!(page_content_bytes(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .windows(2)
            .any(|window| window == b"Do"));
    }

    #[test]
    fn flatten_annotations_bridge_handles_direct_appearance_dictionary_holder() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut annotation = Dictionary::new();
        annotation.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        let mut appearance = Dictionary::new();
        appearance.insert("N", Object::Reference(ObjectRef::new(6, 0)));
        annotation.insert("AP", Object::Dictionary(appearance));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annotation));
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );
        let mut stream_dictionary = Dictionary::new();
        stream_dictionary.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Stream(Stream::new(stream_dictionary, Vec::new())),
        );

        let (appearance_dictionary, selected_appearance) = {
            let mut helper = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
            (
                helper.get_appearance_dictionary().unwrap(),
                helper.get_appearance_stream(b"N", None).unwrap(),
            )
        };
        assert!(appearance_dictionary.as_dictionary().is_some());
        assert_eq!(
            appearance_dictionary.get_key(b"/N").object_ref(),
            Some(ObjectRef::new(6, 0))
        );
        assert!(selected_appearance.is_null());
        assert!(
            has_bare_reference_redirect_in_handle(&mut pdf, &appearance_dictionary, b"/N").unwrap()
        );
        assert_eq!(
            resolve_ap_n(&mut pdf, ObjectRef::new(4, 0)).unwrap(),
            Some(ObjectRef::new(7, 0))
        );

        assert_eq!(
            flatten_annotations_on_page(
                &mut pdf,
                ObjectRef::new(3, 0),
                FlattenMode::Flags {
                    required: 0,
                    forbidden: 0,
                    skip_widgets: false,
                    page_rotate: 0,
                },
            )
            .unwrap(),
            1,
            "a direct /AP dictionary must still reach its /N holder-chain stream"
        );
        assert!(page_content_bytes(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .windows(2)
            .any(|window| window == b"Do"));
    }

    #[test]
    fn flatten_annotations_bridge_handles_holder_selection_and_bad_bbox() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut annotation = Dictionary::new();
        annotation.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        annotation.insert("AP", Object::Reference(ObjectRef::new(5, 0)));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annotation));
        let mut appearance = Dictionary::new();
        appearance.insert("N", Object::Reference(ObjectRef::new(6, 0)));
        pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(appearance));
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );
        let mut stream_dictionary = Dictionary::new();
        stream_dictionary.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Stream(Stream::new(stream_dictionary, Vec::new())),
        );
        assert_eq!(
            resolve_ap_n(&mut pdf, ObjectRef::new(4, 0)).unwrap(),
            Some(ObjectRef::new(7, 0))
        );
        let (appearance_dictionary, selected_appearance) = {
            let mut helper = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
            (
                helper.get_appearance_dictionary().unwrap(),
                helper.get_appearance_stream(b"N", None).unwrap(),
            )
        };
        assert_eq!(
            appearance_dictionary.object_ref(),
            Some(ObjectRef::new(5, 0))
        );
        assert!(selected_appearance.is_null());
        assert_eq!(
            flatten_annotations_on_page(
                &mut pdf,
                ObjectRef::new(3, 0),
                FlattenMode::Flags {
                    required: 0,
                    forbidden: 0,
                    skip_widgets: false,
                    page_rotate: 0,
                },
            )
            .unwrap(),
            1
        );

        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut annotation = Dictionary::new();
        annotation.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        annotation.insert("AP", Object::Reference(ObjectRef::new(5, 0)));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annotation));
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Reference(ObjectRef::new(6, 0)),
        );
        pdf.set_object(ObjectRef::new(6, 0), Object::Dictionary(Dictionary::new()));
        assert_eq!(
            flatten_annotations_on_page(
                &mut pdf,
                ObjectRef::new(3, 0),
                FlattenMode::Flags {
                    required: 0,
                    forbidden: 0,
                    skip_widgets: false,
                    page_rotate: 0,
                },
            )
            .unwrap(),
            0
        );

        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let mut annotation = Dictionary::new();
        annotation.insert(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        annotation.insert("AP", Object::Reference(ObjectRef::new(5, 0)));
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(annotation));
        let mut appearance = Dictionary::new();
        appearance.insert("N", Object::Reference(ObjectRef::new(6, 0)));
        pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(appearance));
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );
        let mut stream_dictionary = Dictionary::new();
        stream_dictionary.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
            ]),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Stream(Stream::new(stream_dictionary, Vec::new())),
        );
        assert_eq!(
            flatten_annotations_on_page(
                &mut pdf,
                ObjectRef::new(3, 0),
                FlattenMode::Flags {
                    required: 0,
                    forbidden: 0,
                    skip_widgets: false,
                    page_rotate: 0,
                },
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn bare_reference_redirect_ignores_non_container_owner() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        pdf.set_object(ObjectRef::new(4, 0), Object::Integer(1));
        assert!(!has_bare_reference_redirect(&mut pdf, ObjectRef::new(4, 0), "F").unwrap());
    }

    // -----------------------------------------------------------------------
    // Test: basic widget flattening with All mode
    // -----------------------------------------------------------------------
    #[test]
    fn flatten_widget_all_mode() {
        // obj 4 = annotation with /AP/N pointing to obj 5 (a Form XObject)
        // obj 5 = Form XObject stream /BBox [0 0 100 20]
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"0.5 g 0 0 100 20 re f");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);

        // Annotation with /AP/N referencing obj 5, /Rect [50 50 150 70]
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        // Page /Resources/XObject should have one entry pointing to the xobj.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();

        let resources = match page_dict.get("Resources").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("Resources should be a dict"),
        };
        let xobj_dict = match resources.get("XObject").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("XObject should be a dict"),
        };
        assert_eq!(xobj_dict.iter().count(), 1, "exactly one XObject entry");
        // The value should reference obj 5.
        let xobj_val = xobj_dict.iter().next().unwrap().1;
        assert_eq!(xobj_val, &Object::Reference(ObjectRef::new(5, 0)));

        // Page content should contain "cm" and "Do".
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("cm"), "content should contain cm");
        assert!(content_str.contains("Do"), "content should contain Do");
        assert!(content_str.contains('q'), "content should contain q");
        assert!(content_str.contains('Q'), "content should contain Q");

        // qpdf removes /Annots after every annotation has been flattened.
        let page_obj2 = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict2 = page_obj2.as_dict().unwrap();
        assert!(page_dict2.get("Annots").is_none());
    }

    // -----------------------------------------------------------------------
    // Test: placement matrix values
    // -----------------------------------------------------------------------
    #[test]
    fn placement_matrix_values() {
        // BBox [0 0 100 20] identity matrix, Rect [50 50 150 70]
        // sx = (150-50)/(100-0) = 1.0, sy = (70-50)/(20-0) = 1.0
        // A = [1 0 0 1 50 50]
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);

        // Matrix should be "1 0 0 1 50 50 cm"
        assert!(
            content_str.contains("1 0 0 1 50 50 cm"),
            "expected identity+translate matrix, got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: Print mode — only annotations with Print bit
    // -----------------------------------------------------------------------
    #[test]
    fn print_mode_only_prints_print_bit_annotations() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body.clone());
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body);

        // obj 4: Print bit set (0x04)
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 4 /AP << /N 5 0 R >> >>",
        );
        // obj 7: No Print bit (F=0)
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 0 200 20] /F 0 /AP << /N 6 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Print).unwrap();
        assert_eq!(
            count, 1,
            "only the Print-bit annotation should be flattened"
        );

        // The non-Print annotation (obj 7) should still be in /Annots.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let annots = match page_dict.get("Annots").unwrap() {
            Object::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        assert_eq!(annots.len(), 1, "one annotation should remain");
        assert_eq!(annots[0], Object::Reference(ObjectRef::new(7, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: Screen mode — only annotations without Print bit
    // -----------------------------------------------------------------------
    #[test]
    fn screen_mode_only_flattens_non_print_annotations() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body.clone());
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body);

        // obj 4: Print bit set
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 4 /AP << /N 5 0 R >> >>",
        );
        // obj 7: No Print bit (screen annotation)
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 0 200 20] /F 0 /AP << /N 6 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Screen).unwrap();
        assert_eq!(count, 1, "only the no-Print annotation should be flattened");

        // The Print annotation (obj 4) should still be in /Annots.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let annots = match page_dict.get("Annots").unwrap() {
            Object::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0], Object::Reference(ObjectRef::new(4, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: Hidden annotation is skipped in All mode
    // -----------------------------------------------------------------------
    #[test]
    fn hidden_annotation_skipped_in_all_mode() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        // Hidden bit = 0x2
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 2 /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "hidden annotation should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: annotation without /AP is skipped (no error)
    // -----------------------------------------------------------------------
    #[test]
    fn annotation_without_ap_is_skipped() {
        let (n4, obj4_bytes) =
            obj_dict(4, "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] >>");

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0);
    }

    // -----------------------------------------------------------------------
    // Test: checkbox state dict (/AP/N is a sub-dict with /AS selection)
    // -----------------------------------------------------------------------
    #[test]
    fn checkbox_state_dict_with_as_selection() {
        // obj 5 = Form XObject for /On state
        let xobj_on = make_xobj_stream([0.0, 0.0, 20.0, 20.0], b"1 g 0 0 20 20 re f");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_on);

        // obj 4 = checkbox annotation with /AP/N as state dict, /AS /On
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [10 10 30 30] \
             /AS /On /AP << /N << /On 5 0 R /Off 0 0 R >> >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1, "checkbox On state should be flattened");

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("Do"), "content should contain Do");
    }

    // -----------------------------------------------------------------------
    // Test: qpdf writer round-trip — output is valid parseable PDF
    // -----------------------------------------------------------------------
    #[test]
    fn pdf_writer_round_trip_is_valid() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"0.5 g 0 0 100 20 re f");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();

        // Write and re-open the PDF.
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out)).unwrap();
        let pages = page_refs(&mut pdf2).unwrap();
        assert_eq!(pages.len(), 1);

        // Content must contain Do.
        let content = page_content_bytes(&mut pdf2, pages[0]).unwrap();
        assert!(content.windows(2).any(|w| w == b"Do"));
    }

    // -----------------------------------------------------------------------
    // Test: non-identity /Matrix in the Form XObject
    // -----------------------------------------------------------------------
    #[test]
    fn non_identity_matrix_in_xobj() {
        // /Matrix [2 0 0 2 0 0] scales BBox [0 0 50 10] → transformed bbox [0 0 100 20]
        // Rect [50 50 150 70]: rx_width=100, ry_height=20
        // tx_width=100, ty_height=20 → sx=1.0, sy=1.0
        // A = [1 0 0 1 50 50]
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 50 10] /Matrix [2 0 0 2 0 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, xobj_str.as_bytes().to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        // transformed bbox is [0 0 100 20], rx0=50 ry0=50
        // sx=100/100=1, sy=20/20=1, e=50-1*0=50, f=50-1*0=50
        assert!(
            content_str.contains("1 0 0 1 50 50 cm"),
            "expected A=[1 0 0 1 50 50], got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: return count is correct for multiple annotations
    // -----------------------------------------------------------------------
    #[test]
    fn multiple_annotations_flattened_count() {
        let xobj_body1 = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let xobj_body2 = make_xobj_stream([0.0, 0.0, 50.0, 50.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body1);
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body2);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /AP << /N 5 0 R >> >>",
        );
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 100 150 150] /AP << /N 6 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 2);

        // Both XObjects in /Resources.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let resources = match page_dict.get("Resources").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("expected dict"),
        };
        let xobj_dict = match resources.get("XObject").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("expected dict"),
        };
        assert_eq!(xobj_dict.iter().count(), 2, "two XObject entries");

        // qpdf removes /Annots after every annotation has been flattened.
        assert!(page_dict.get("Annots").is_none());
    }

    // -----------------------------------------------------------------------
    // Test: flatten_annotations (document-level)
    // -----------------------------------------------------------------------
    #[test]
    fn flatten_annotations_document_level() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let count = flatten_annotations(&mut pdf, FlattenMode::All).unwrap();
        assert_eq!(count, 1);
    }

    // -----------------------------------------------------------------------
    // Test: annotation with no /Rect is skipped (line 125)
    // -----------------------------------------------------------------------
    #[test]
    fn annotation_without_rect_is_skipped() {
        // Build annotation with /AP but no /Rect
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) =
            obj_dict(4, "<< /Type /Annot /Subtype /Widget /AP << /N 5 0 R >> >>");

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "annotation without /Rect should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: inverted /Rect (llx>urx, lly>ury) is normalized (lines 132,137)
    // -----------------------------------------------------------------------
    #[test]
    fn inverted_rect_normalized_and_flattened() {
        // /Rect [150 70 50 50] → swapped to [50 50 150 70]
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [150 70 50 50] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1, "inverted rect should be normalized and flattened");

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("cm"), "content should contain cm");
    }

    // -----------------------------------------------------------------------
    // Test: degenerate /Rect (zero-dimension) is skipped (line 142)
    // -----------------------------------------------------------------------
    #[test]
    fn degenerate_zero_dim_rect_is_skipped() {
        // /Rect [50 50 50 70] → zero width → skipped
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 50 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "zero-dim rect should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: XObject without /BBox is skipped (line 149)
    // -----------------------------------------------------------------------
    #[test]
    fn xobj_without_bbox_is_skipped() {
        // Form XObject stream without /BBox entry
        let no_bbox_xobj = "<< /Type /XObject /Subtype /Form /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, no_bbox_xobj.as_bytes().to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "XObject without /BBox should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: degenerate transformed BBox (zero-area matrix) is skipped (line 175)
    // -----------------------------------------------------------------------
    #[test]
    fn degenerate_transformed_bbox_is_skipped() {
        // /Matrix [0 0 0 0 0 0] collapses all corners to (0,0) → tw=0, th=0
        let xobj_str = "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [0 0 0 0 0 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, xobj_str.as_bytes().to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "degenerate transformed BBox should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: /XObject as indirect ref in /Resources (lines 203-206)
    // -----------------------------------------------------------------------
    #[test]
    fn resources_xobject_as_indirect_ref() {
        // Build a PDF with /Resources/XObject as an indirect reference
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );
        // obj 6: an XObject dictionary as indirect object (will be referenced by /Resources)
        let (n6, obj6_bytes) = obj_dict(6, "<< /ExistingEntry 5 0 R >>");

        // Page has /Resources with /XObject pointing to obj 6 (indirect ref)
        let bytes = build_pdf(
            "/Annots [4 0 R] /Resources << /XObject 6 0 R >>",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(
            content.windows(2).any(|w| w == b"Do"),
            "content should contain Do"
        );
    }

    // -----------------------------------------------------------------------
    // Test: XObject name collision forces loop to find unique name (line 223)
    // -----------------------------------------------------------------------
    #[test]
    fn xobj_name_collision_forces_unique_name() {
        // Pre-populate /Resources/XObject with Fxo1 so the loop must increment
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R] /Resources << /XObject << /Fxo1 5 0 R >> >>",
            &[(n4, obj4_bytes), (n5, obj5_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        // The content should contain Fxo2 (since Fxo1 was taken)
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.contains("Fxo2"),
            "expected Fxo2 due to name collision, got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: page content not ending in newline gets one appended (line 247)
    // -----------------------------------------------------------------------
    #[test]
    fn page_content_without_trailing_newline_gets_newline() {
        // Content stream that does NOT end in '\n'
        // We need to put raw content bytes in the page — use a content stream obj
        let content_data = b"BT /F1 12 Tf 100 700 Td (hello) Tj ET"; // no trailing newline
        let content_len = content_data.len();
        let stream_header = format!("<< /Length {content_len} >>\nstream\n");
        let mut stream_bytes = stream_header.into_bytes();
        stream_bytes.extend_from_slice(content_data);
        stream_bytes.extend_from_slice(b"\nendstream\n");
        let (n6, obj6_bytes) = obj_wrap(6, stream_bytes);

        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        // Page with /Contents pointing to obj 6 and /Annots
        let bytes = build_pdf(
            "/Annots [4 0 R] /Contents 6 0 R",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(
            content.windows(2).any(|w| w == b"Do"),
            "content should contain Do"
        );
    }

    // -----------------------------------------------------------------------
    // Test: Screen mode with NoView flag skips annotation (lines 110,112)
    // -----------------------------------------------------------------------
    #[test]
    fn screen_mode_skips_noview_annotation() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        // F=0x20 = NoView bit set, no Print bit
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 32 /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Screen).unwrap();
        assert_eq!(
            count, 0,
            "NoView annotation should be skipped in Screen mode"
        );
    }

    // -----------------------------------------------------------------------
    // Test: Hidden annotation skipped in Print mode (line 109)
    // -----------------------------------------------------------------------
    #[test]
    fn print_mode_skips_hidden_annotation() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        // F = Hidden(0x2) | Print(0x4) = 0x6
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 6 /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Print).unwrap();
        assert_eq!(
            count, 0,
            "Hidden+Print annotation should be skipped in Print mode"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for read_xobj_bbox_and_matrix private fn
    // -----------------------------------------------------------------------

    /// Build a minimal PDF with one stream object and return a Pdf handle + its ref.
    fn build_pdf_with_stream_obj(stream_dict_str: &str, data: &[u8]) -> (Vec<u8>, ObjectRef) {
        let data_len = data.len();
        let header = format!("{stream_dict_str} /Length {data_len}");
        // Build it as obj 4
        let mut body = format!("<< {header} >>\nstream\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, body);
        let pdf_bytes = build_pdf("", &[(n4, obj4_bytes)]);
        (pdf_bytes, ObjectRef::new(4, 0))
    }

    #[test]
    fn read_bbox_matrix_non_stream_object_returns_none_bbox() {
        // obj 4 is a plain dict (not a stream) → should return (None, identity)
        let (n4, obj4_bytes) = obj_dict(4, "<< /Foo /Bar >>");
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let xobj_ref = ObjectRef::new(4, 0);

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, xobj_ref).unwrap();
        assert!(bbox.is_none(), "non-stream should return None bbox");
        assert_eq!(
            matrix,
            Matrix::default(),
            "non-stream should return identity matrix"
        );
    }

    #[test]
    fn read_bbox_matrix_missing_bbox_returns_none() {
        // Stream dict without /BBox → (None, identity)
        let (pdf_bytes, xobj_ref) = build_pdf_with_stream_obj("/Type /XObject /Subtype /Form", b"");
        let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, xobj_ref).unwrap();
        assert!(bbox.is_none());
        assert_eq!(matrix, Matrix::default());
    }

    #[test]
    fn read_bbox_matrix_bbox_wrong_length_returns_none() {
        // /BBox with 3 elements (not 4) → (None, identity)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_none(), "BBox with wrong length should return None");
        assert_eq!(matrix, Matrix::default());
    }

    #[test]
    fn read_bbox_matrix_bbox_with_real_values() {
        // /BBox with Real values (e.g. 0.5) covers the Object::Real arm (line 487)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0.5 0.5 100.5 20.5] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some(), "real-valued BBox should succeed");
        let b = bbox.unwrap();
        assert!((b.llx - 0.5).abs() < 1e-10);
        assert!((b.urx - 100.5).abs() < 1e-10);
        assert_eq!(matrix, Matrix::default());
    }

    #[test]
    fn read_bbox_matrix_bbox_non_numeric_element_returns_none() {
        // /BBox with a non-numeric element → (None, identity)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 /Bad 20] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, _matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(
            bbox.is_none(),
            "non-numeric BBox element should return None"
        );
    }

    #[test]
    fn read_bbox_matrix_matrix_with_real_values() {
        // /Matrix with Real values covers the Object::Real arm for matrix (line 503)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1.5 0.0 0.0 1.5 0.0 0.0] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert!((matrix.a - 1.5).abs() < 1e-10, "matrix.a should be 1.5");
        assert!((matrix.d - 1.5).abs() < 1e-10, "matrix.d should be 1.5");
    }

    #[test]
    fn read_bbox_matrix_matrix_with_non_numeric_element_falls_back_to_identity() {
        // /Matrix with a non-numeric element → identity (lines 505-506, 513)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 0 /Bad 0 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(matrix, Matrix::default(), "should fall back to identity");
    }

    #[test]
    fn read_bbox_matrix_matrix_wrong_length_falls_back_to_identity() {
        // /Matrix with wrong length (5 elements instead of 6) → identity (line 538)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 0 1 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            Matrix::default(),
            "wrong-length matrix falls back to identity"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for read_xobj_bbox_and_matrix: /BBox and /Matrix as indirect refs
    // -----------------------------------------------------------------------

    #[test]
    fn read_bbox_via_indirect_ref() {
        // /BBox as indirect reference to an Array object (lines 474-476)
        // obj 5 = Array [0 0 100 20]
        // obj 4 = Stream with /BBox 5 0 R
        let (n5, obj5_bytes) = {
            let body = "[0 0 100 20]\n";
            obj_wrap(5, body.as_bytes().to_vec())
        };
        // Build stream with /BBox pointing to obj 5
        let stream_header = "<< /Type /XObject /Subtype /Form /BBox 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // We need to register obj5 as an Array in the pdf
        // Since the parser may not handle a bare array as an indirect object,
        // let's set it directly using set_object
        let array_ref = ObjectRef::new(5, 0);
        pdf.set_object(
            array_ref,
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some(), "BBox via indirect ref should be parsed");
        let b = bbox.unwrap();
        assert_eq!(b.urx as i64, 100);
        assert_eq!(matrix, Matrix::default());
    }

    #[test]
    fn read_bbox_indirect_ref_non_array_returns_none() {
        // /BBox indirect ref resolving to non-array (line 476)
        // Set up stream with /BBox pointing to obj 5, which is a dict (not array)
        let stream_header = "<< /Type /XObject /Subtype /Form /BBox 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_none(), "BBox ref → non-array should return None");
        assert_eq!(matrix, Matrix::default());
    }

    #[test]
    fn read_matrix_via_indirect_ref() {
        // /Matrix as indirect reference (lines 516-536)
        // Build stream with /BBox [0 0 100 20] and /Matrix pointing to obj 5
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Override obj 5 to be a proper 6-element array via set_object
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Array(vec![
                Object::Real(2.0),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(2.0),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert!(
            (matrix.a - 2.0).abs() < 1e-10,
            "matrix.a via indirect ref should be 2.0"
        );
        assert!(
            (matrix.d - 2.0).abs() < 1e-10,
            "matrix.d via indirect ref should be 2.0"
        );
    }

    #[test]
    fn read_matrix_indirect_ref_non_array_returns_identity() {
        // /Matrix ref → non-array → identity (line 536)
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(matrix, Matrix::default(), "non-array matrix ref → identity");
    }

    #[test]
    fn read_matrix_indirect_ref_with_non_numeric_element_falls_back_to_identity() {
        // /Matrix ref → 6-element array with non-numeric element → identity (lines 524-526, 532-533)
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Set obj5 to 6-element array with a bad element
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Name(b"BadElement".to_vec()), // non-numeric
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            Matrix::default(),
            "bad element in matrix ref → identity"
        );
    }

    #[test]
    fn read_matrix_indirect_ref_wrong_length_returns_identity() {
        // /Matrix ref → array with wrong length (not 6) → identity (line 517 guard fails)
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Set obj5 to a 4-element array (wrong length)
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(1),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            Matrix::default(),
            "wrong-length matrix ref → identity"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for read_annot_flags private fn
    // -----------------------------------------------------------------------

    #[test]
    fn read_annot_flags_non_dict_returns_zero() {
        // annot_ref resolves to a non-dict object → returns 0 (line 327)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Register obj 10 as an Integer (not a dict)
        let annot_ref = ObjectRef::new(10, 0);
        pdf.set_object(annot_ref, Object::Integer(42));

        let flags = read_annot_flags(&mut pdf, annot_ref).unwrap();
        assert_eq!(flags, 0, "non-dict annot should return flags=0");
    }

    #[test]
    fn read_annot_flags_f_as_indirect_ref() {
        // /F value is an indirect reference to an Integer (line 335)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Integer 4 (Print bit)
        let flag_ref = ObjectRef::new(11, 0);
        pdf.set_object(flag_ref, Object::Integer(4));

        // obj 10 = annotation dict with /F → 11 0 R
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("Type", Object::Name(b"Annot".to_vec()));
        annot_dict.insert("F", Object::Reference(flag_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let flags = read_annot_flags(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            flags, 4,
            "/F via indirect ref should resolve to 4 (Print bit)"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for resolve_ap_n private fn
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_ap_n_non_dict_annot_returns_none() {
        // annot_ref resolves to non-dict → None (line 359)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        pdf.set_object(annot_ref, Object::Integer(99));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "non-dict annot should return None");
    }

    #[test]
    fn resolve_ap_n_ap_null_returns_none() {
        // /AP is null → None (line 364)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("AP", Object::Null);
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_ap_n_ap_as_indirect_ref_to_dict() {
        // /AP is an indirect ref to a dict (lines 369-370)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Form XObject stream
        let xobj_ref = ObjectRef::new(11, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        // obj 12 = AP dict {N: 11 0 R} as indirect object
        let ap_dict_ref = ObjectRef::new(12, 0);
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(xobj_ref));
        pdf.set_object(ap_dict_ref, Object::Dictionary(ap_dict));

        // obj 10 = annotation with /AP as indirect ref → obj 12
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("Type", Object::Name(b"Annot".to_vec()));
        annot_dict.insert("AP", Object::Reference(ap_dict_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            result,
            Some(xobj_ref),
            "/AP as indirect ref should resolve to xobj"
        );
    }

    #[test]
    fn resolve_ap_n_ap_ref_to_non_dict_returns_none() {
        // /AP is indirect ref to non-dict → None (line 371)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let bad_ref = ObjectRef::new(11, 0);
        pdf.set_object(bad_ref, Object::Integer(42));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("AP", Object::Reference(bad_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "/AP ref → non-dict should return None");
    }

    #[test]
    fn resolve_ap_n_ap_direct_non_dict_returns_none() {
        // /AP is a direct non-dict value (e.g. Integer) → None (line 373)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("AP", Object::Integer(99));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "non-dict /AP should return None");
    }

    #[test]
    fn resolve_ap_n_n_null_returns_none() {
        // /N is null → None (line 378)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Null);
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "/N null should return None");
    }

    #[test]
    fn resolve_ap_n_n_ref_to_dict_selects_by_as() {
        // /N is ref → dict (state dict case), selects by /AS (lines 391,393)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Form XObject stream (the "On" state)
        let xobj_ref = ObjectRef::new(11, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        // obj 12 = state dict {On: 11 0 R, Off: ...}
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Reference(xobj_ref));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        // obj 10 = annotation with /AP/N as ref to state dict, /AS /On
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            result,
            Some(xobj_ref),
            "state dict /AS selection should return correct xobj"
        );
    }

    #[test]
    fn resolve_ap_n_n_ref_to_non_stream_non_dict_returns_none() {
        // /N ref → non-stream/non-dict → None (line 393)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let bad_ref = ObjectRef::new(11, 0);
        pdf.set_object(bad_ref, Object::Integer(99));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(bad_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_none(),
            "/N ref → non-stream/dict should return None"
        );
    }

    #[test]
    fn resolve_ap_n_n_direct_integer_returns_none() {
        // /N is a direct non-stream/dict/ref value → None (line 404)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Integer(42));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "direct integer /N should return None");
    }

    #[test]
    fn resolve_ap_n_as_via_indirect_ref() {
        // /AS is an indirect ref → Name (lines 423-424)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Form XObject stream
        let xobj_ref = ObjectRef::new(11, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        // obj 13 = Name "On" (indirect)
        let name_ref = ObjectRef::new(13, 0);
        pdf.set_object(name_ref, Object::Name(b"On".to_vec()));

        // obj 12 = state dict
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Reference(xobj_ref));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        // obj 10 = annotation with /AS as indirect ref → Name "On"
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Reference(name_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(result, Some(xobj_ref), "/AS via indirect ref should work");
    }

    #[test]
    fn resolve_ap_n_as_ref_to_non_name_returns_none() {
        // /AS ref → non-Name → None (line 425)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 13 = Integer (not a Name)
        let bad_ref = ObjectRef::new(13, 0);
        pdf.set_object(bad_ref, Object::Integer(42));

        // obj 12 = state dict
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Integer(0));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Reference(bad_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "/AS ref → non-name should return None");
    }

    #[test]
    fn resolve_ap_n_state_dict_as_absent_returns_none() {
        // No /AS in annotation when state dict selected → None (line 427)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 12 = state dict
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Integer(0));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        // No /AS key
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "missing /AS should return None");
    }

    #[test]
    fn resolve_ap_n_state_dict_entry_ref_to_non_stream_returns_none() {
        // State dict entry ref → non-stream → None (line 434)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Integer (not a stream)
        let non_stream_ref = ObjectRef::new(11, 0);
        pdf.set_object(non_stream_ref, Object::Integer(99));

        // obj 12 = state dict with /On → bad ref
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Reference(non_stream_ref));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_none(),
            "state entry ref → non-stream should return None"
        );
    }

    #[test]
    fn resolve_ap_n_state_dict_missing_key_returns_none() {
        // State dict does not have the /AS key → None (line 442)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 12 = state dict without "On" key
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("Off", Object::Integer(0));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec())); // key not in state dict
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_none(),
            "missing state dict key should return None"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for build_pruned_annots_array private fn
    // -----------------------------------------------------------------------

    #[test]
    fn pruned_annots_no_annots_entry_returns_empty() {
        // page_dict has no /Annots → empty vec (line 554)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let page_dict = Dictionary::new();
        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[]).unwrap();
        assert!(result.is_empty(), "no /Annots should return empty vec");
    }

    #[test]
    fn pruned_annots_annots_as_indirect_ref_to_array() {
        // /Annots is an indirect ref to an array (lines 559-560)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let keep_ref = ObjectRef::new(11, 0);

        // Set up obj 20 = array [annot_ref, keep_ref]
        let arr_ref = ObjectRef::new(20, 0);
        pdf.set_object(
            arr_ref,
            Object::Array(vec![
                Object::Reference(annot_ref),
                Object::Reference(keep_ref),
            ]),
        );

        let mut page_dict = Dictionary::new();
        page_dict.insert("Annots", Object::Reference(arr_ref));

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[annot_ref]).unwrap();
        assert_eq!(result.len(), 1, "one annot should be pruned");
        assert_eq!(result[0], Object::Reference(keep_ref));
    }

    #[test]
    fn pruned_annots_annots_ref_to_non_array_returns_empty() {
        // /Annots indirect ref → non-array → empty (line 561)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let bad_ref = ObjectRef::new(20, 0);
        pdf.set_object(bad_ref, Object::Integer(42));

        let mut page_dict = Dictionary::new();
        page_dict.insert("Annots", Object::Reference(bad_ref));

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[]).unwrap();
        assert!(result.is_empty(), "ref → non-array should return empty");
    }

    #[test]
    fn pruned_annots_annots_direct_non_array_returns_empty() {
        // /Annots is a direct non-array value → empty (line 563)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let mut page_dict = Dictionary::new();
        page_dict.insert("Annots", Object::Integer(99));

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[]).unwrap();
        assert!(
            result.is_empty(),
            "direct non-array /Annots should return empty"
        );
    }

    #[test]
    fn pruned_annots_non_ref_entries_are_kept() {
        // Array with non-ref entries (unusual) — these should be kept (line 570)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let keep_ref = ObjectRef::new(11, 0);
        let remove_ref = ObjectRef::new(10, 0);

        let mut page_dict = Dictionary::new();
        page_dict.insert(
            "Annots",
            Object::Array(vec![
                Object::Reference(remove_ref),
                Object::Integer(42), // non-ref entry — keep
                Object::Reference(keep_ref),
            ]),
        );

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[remove_ref]).unwrap();
        assert_eq!(result.len(), 2, "non-ref entries should be kept");
        assert_eq!(result[0], Object::Integer(42));
        assert_eq!(result[1], Object::Reference(keep_ref));
    }

    // -----------------------------------------------------------------------
    // Test: /AP/N direct stream materializes as new indirect object (lines 402, 408-412)
    // -----------------------------------------------------------------------
    #[test]
    fn resolve_ap_n_direct_stream_materializes() {
        // /AP/N is a direct Object::Stream (defensive path for malformed PDFs)
        // This requires constructing the object directly via set_object
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build an inline (direct) stream for /N
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        let inline_stream = Object::Stream(Stream::new(xobj_dict, b"q Q".to_vec()));

        // obj 10 = annotation dict with indirect /AP and direct /N stream
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", inline_stream);
        let ap_ref = ObjectRef::new(11, 0);
        pdf.set_object(ap_ref, Object::Dictionary(ap_dict));
        annot_dict.insert("AP", Object::Reference(ap_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_some(),
            "direct stream /N should be materialized and returned"
        );
        assert_eq!(
            resolve_ap_n(&mut pdf, annot_ref).unwrap(),
            result,
            "repeated resolution must reuse the materialized /AP/N stream"
        );
    }

    // -----------------------------------------------------------------------
    // Test: state dict with direct stream entry materializes (lines 436-440)
    // -----------------------------------------------------------------------
    #[test]
    fn resolve_ap_n_state_dict_direct_stream_entry_materializes() {
        // State dict entry is a direct Object::Stream (defensive path)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build inline stream for /On state entry
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        let inline_stream = Object::Stream(Stream::new(xobj_dict, vec![]));

        // obj 10 = annotation with /AP/N as direct state dict
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", inline_stream);
        ap_dict.insert("N", Object::Dictionary(state_dict));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_some(),
            "state dict direct stream entry should be materialized"
        );
        assert_eq!(
            resolve_ap_n(&mut pdf, annot_ref).unwrap(),
            result,
            "repeated resolution must reuse the materialized state stream"
        );
    }

    #[test]
    fn resolve_ap_n_indirect_state_dict_direct_stream_entry_materializes() {
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Stream(Stream::new(xobj_dict, Vec::new())));
        let state_ref = ObjectRef::new(11, 0);
        pdf.set_object(state_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            resolve_ap_n(&mut pdf, annot_ref).unwrap(),
            result,
            "indirect state holder must retain the materialized stream reference"
        );
    }

    // -----------------------------------------------------------------------
    // Test: /XObject in resources is ref → non-dict → creates empty dict (line 206)
    // -----------------------------------------------------------------------
    #[test]
    fn resources_xobject_indirect_ref_to_non_dict_uses_empty_dict() {
        // /Resources/XObject is an indirect ref to a non-dict object (e.g. Integer)
        // This should fall back to an empty XObject dict
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );
        // obj 6: a non-dict object (Integer), used as /XObject ref
        let (n6, obj6_bytes) = obj_dict(6, "42"); // integer as standalone object

        let bytes = build_pdf(
            "/Annots [4 0 R] /Resources << /XObject 6 0 R >>",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Override obj 6 to be a non-dict value to trigger the fallback path
        pdf.set_object(ObjectRef::new(6, 0), Object::Integer(42));

        let page_ref = ObjectRef::new(3, 0);
        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1, "should flatten even when /XObject ref → non-dict");

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(content.windows(2).any(|w| w == b"Do"));
    }

    // -----------------------------------------------------------------------
    // Test: /BBox as a direct non-array value (line 478)
    // -----------------------------------------------------------------------
    #[test]
    fn read_bbox_direct_non_array_value_returns_none() {
        // /BBox is a direct non-array, non-ref value (e.g., a Name) → (None, identity)
        // Must use set_object since the parser normalizes, but we can inject it directly
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build stream object with /BBox as an Integer directly
        let xobj_ref = ObjectRef::new(10, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        // Set /BBox to an Integer (not an array) — malformed
        xobj_dict.insert("BBox", Object::Integer(99));
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, xobj_ref).unwrap();
        assert!(bbox.is_none(), "direct non-array /BBox should return None");
        assert_eq!(matrix, Matrix::default());
    }
}
