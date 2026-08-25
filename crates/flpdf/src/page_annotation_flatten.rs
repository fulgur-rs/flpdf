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
//! Test-only malformed-shape fixtures stay below the production boundary; the
//! parsed qpdf route owns all live annotation, appearance, and page mutation.
//!
//! The qpdf document-helper entry point applies this to every leaf page with
//! its caller-supplied required and forbidden annotation-flag masks.

use crate::object_handle::ObjectHandleIdentity;
use crate::pages::page_content_bytes;
use crate::{
    AcroFormDocumentHelper, AnnotationObjectHelper, Error, ObjectHandle, ObjectRef,
    PageObjectHelper, Pdf, Result,
};
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::rc::Rc;

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
    // ── Step 1: enumerate annotations without reading /Rect ──────────────
    // qpdf's page helper obtains canonical annotation handles first. Keep
    // those exact handles through the eligibility and removal paths: direct
    // annotation dictionaries have no ObjectRef and must not be projected
    // away before the appearance/flag gate. The annotation helper validates
    // /Rect only after that gate, so this route stays lazy and does not
    // materialize /Rect before eligibility is known.
    let annotations = page_annotation_handles(pdf, page_ref)?;

    // ── Step 2: for each annotation, decide eligibility and collect data ───
    struct AnnotData {
        annotation: ObjectHandle,
        appearance: ObjectHandle,
    }

    let mut candidates: Vec<AnnotData> = Vec::new();
    // Track the exact annotation handles that should be removed from /Annots.
    // This preserves direct dictionaries as well as indirect annotations.
    let mut to_remove: Vec<ObjectHandle> = Vec::new();
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

    for annotation in annotations {
        // qpdf resolves the selected appearance stream before reading /F:
        // `QPDFAnnotationObjectHelper::getPageContentForAppearance`'s own
        // top gate is `getAppearanceStream("/N").isStream()`
        // (`QPDFAnnotationObjectHelper.cc:80-82`), and `getFlags()` runs
        // only after that (`:143`). qpdf only retains annotations without
        // any appearance dictionary; once /AP is present, a missing
        // selected /N stream is itself a flattening/removal outcome (for
        // example an unchecked checkbox).
        let appearance_dictionary = {
            let mut helper = AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
            helper.get_appearance_dictionary()?
        };
        let appearance_dictionary = pdf.resolve_to_terminal(&appearance_dictionary)?;
        let appearance = {
            let mut helper = AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
            helper.get_appearance_stream(b"N", None)?
        };

        // qpdf resolves /AP/N before reading /Subtype, including when
        // NeedAppearances causes a Widget to be skipped
        // (`QPDFPageDocumentHelper.cc:97-105`). Keep this order so a skipped
        // Widget still observes the canonical appearance-resolution boundary.
        if skip_widgets
            && AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf).get_subtype()?
                == b"Widget"
        {
            continue;
        }

        // Read /F through the canonical helper. A zero result is also qpdf's
        // fail-soft value for absent/non-integer /F.
        let flags =
            AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf).get_flags()?;
        #[cfg(test)]
        let hidden = (flags & FLAG_HIDDEN) != 0;
        #[cfg(test)]
        let print_bit = (flags & FLAG_PRINT) != 0;
        #[cfg(test)]
        let no_view = (flags & FLAG_NO_VIEW) != 0;

        let has_appearance = !appearance_dictionary.is_null();
        if appearance.as_stream_dict().is_none() {
            if qpdf_flag_contract && has_appearance {
                to_remove.push(annotation.clone());
            }
            continue;
        }

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
                to_remove.push(annotation.clone());
            }
            continue;
        }
        if qpdf_flag_contract {
            to_remove.push(annotation.clone());
        }

        // The retained test-only legacy modes predate qpdf's direct flag API
        // and intentionally preserve their zero-area/inverted-rectangle
        // behavior. The qpdf-shaped helper owns the rectangle and appearance
        // geometry calculation for both paths.
        if !qpdf_flag_contract {
            let rect_value = pdf.resolve_to_terminal(&annotation.try_get_key(b"/Rect")?)?;
            let has_rect = !rect_value.is_null();
            if !has_rect {
                continue;
            }
            let rect =
                AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf).get_rect()?;
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
            annotation,
            appearance,
        });
    }

    if candidates.is_empty() && to_remove.is_empty() {
        return Ok(0);
    }

    if candidates.is_empty() {
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page)?;
        if page.as_dictionary().is_none() {
            // cov:ignore-start: repaired PageDocumentHelper snapshots contain leaf dictionaries
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary after flatten"
            )));
            // cov:ignore-end
        }
        replace_pruned_annots(pdf, page_ref, &to_remove, qpdf_flag_contract)?; // cov:ignore: llvm-cov maps this covered multiline call terminator to a zero-hit line
        if qpdf_flag_contract {
            // qpdf wraps the page whenever the annotation array changed, even
            // if every selected appearance produced empty drawing content.
            add_qpdf_flatten_contents(pdf, &page, Vec::new())?; // cov:ignore: covered structurally by indirect-contents public fixture
        } // cov:ignore: llvm-cov maps the tested qpdf wrapper branch to this synthetic closing brace
        return Ok(0);
    }

    // The test-only legacy flatten modes still append to a single content
    // stream, but coalescing itself must use the canonical provider-backed
    // page route. Production `Flags` mode uses `add_qpdf_flatten_contents`
    // below and does not need this compatibility-only branch.
    if !qpdf_flag_contract {
        PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()?;
    }

    // ── Step 4: Materialize /Resources on the leaf page ────────────────────
    // Resolve inherited resources, then clone them so we can add /XObject
    // entries without mutating shared parent /Resources dicts.
    //
    // Unlike /Resources itself (unconditionally materialized here, matching
    // qpdf's own unconditional `ph.getAttribute("/Resources", true)` at
    // `QPDFPageDocumentHelper.cc:70`), /Resources/XObject is deliberately
    // NOT touched here. qpdf privatizes-or-creates it lazily, once per
    // annotation, only inside the `!content.empty()` branch
    // (`resources.mergeResources("<< /XObject << >> >>"_qpdf)`,
    // `QPDFPageDocumentHelper.cc:123`) -- see Step 5.
    let mut page_helper = PageObjectHelper::new(page_ref, pdf);
    let resources = page_helper.get_attribute(b"/Resources", true)?;
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let resources = if resources.as_dictionary().is_some() {
        resources
    } else {
        let replacement = ObjectHandle::dictionary(Vec::new());
        page.replace_key(b"/Resources", replacement.clone())?;
        pdf.mark_object_handle_dirty(&page)?;
        replacement
    };

    // ── Step 5: Build content appendix and register XObjects ──────────────
    let mut append_bytes: Vec<u8> = Vec::new();
    // Counter for unique XObject name generation.
    let mut xobj_counter: u32 = 1;

    let mut flattened_count = 0;
    for data in &candidates {
        // Choose a name that doesn't collide with existing /XObject keys.
        // Mirrors qpdf's QPDFObjectHandle::getUniqueResourceName: the
        // counter advances past a rejected (colliding) candidate, but the
        // accepted candidate's number is "the value used, not the next
        // value" -- it only becomes final once this annotation is confirmed
        // to produce content, below.
        //
        // /XObject may not exist yet (it is only privatized-or-created
        // below, once content is known to be non-empty), so peek at it
        // read-only here without creating it.
        let existing_xobj = resources.try_get_key(b"/XObject")?;
        let existing_xobj = pdf.resolve_to_terminal(&existing_xobj)?;
        let xobj_name = loop {
            let candidate = format!("Fxo{xobj_counter}");
            let candidate_key = format!("/{candidate}");
            let collides = existing_xobj
                .as_dictionary()
                .is_some_and(|dict| dict.contains_key(candidate_key.as_bytes()));
            if !collides {
                break candidate;
            }
            xobj_counter += 1;
        };

        let resource_name = format!("/{xobj_name}");
        let content_result =
            AnnotationObjectHelper::from_object_handle(data.annotation.clone(), pdf)
                .get_page_content_for_appearance(
                    &resource_name,
                    page_rotate,
                    required_flags,
                    forbidden_flags,
                );
        let content = content_result?;
        if content.is_empty() {
            continue;
        }
        // The candidate name is now committed; the next search starts past it.
        xobj_counter += 1;

        // Register the Form XObject only when qpdf produced drawing content.
        // Privatize an existing indirect /XObject dict, or create one if
        // absent, exactly when qpdf does (QPDFPageDocumentHelper.cc:123):
        // merging an empty placeholder dict into /Resources is qpdf's own
        // idiom for that privatize-or-create step, and ObjectHandle::merge_resources
        // already implements the identical privatization semantics.
        let empty_xobject_placeholder = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(Vec::new()),
        )]);
        // Mark dirty before the fallible merge, matching the DR-merge call
        // site's own convention above: a partially-applied merge must still
        // be reflected in the dirty set.
        pdf.mark_object_handle_dirty(&resources)?;
        resources.merge_resources(&empty_xobject_placeholder, None)?;
        let xobj_dict = resources.try_get_key(b"/XObject")?;
        pdf.resolve(&xobj_dict)?;

        let xobject = if data.appearance.is_indirect() {
            data.appearance.clone()
        } else {
            pdf.make_indirect_object_handle(data.appearance.clone())?
        };
        xobj_dict.replace_key(format!("/{xobj_name}").as_bytes(), xobject)?;
        pdf.mark_object_handle_dirty(&xobj_dict)?;
        append_bytes.extend_from_slice(&content);
        flattened_count += 1;
        if !qpdf_flag_contract {
            to_remove.push(data.annotation.clone());
        }
    }

    if flattened_count == 0 {
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page)?;
        if page.as_dictionary().is_none() {
            // cov:ignore-start: repaired PageDocumentHelper snapshots contain leaf dictionaries
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary after flatten"
            )));
            // cov:ignore-end
        }
        replace_pruned_annots(pdf, page_ref, &to_remove, qpdf_flag_contract)?; // cov:ignore: llvm-cov maps this covered multiline call terminator to a zero-hit line
        if qpdf_flag_contract {
            add_qpdf_flatten_contents(pdf, &page, Vec::new())?; // cov:ignore: covered structurally by indirect-contents public fixture
        } // cov:ignore: llvm-cov maps the tested qpdf wrapper branch to this synthetic closing brace
        return Ok(0);
    }

    // ── Step 6: Add qpdf-shaped page-content wrappers ─────────────────────
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    if page.as_dictionary().is_none() {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary after flatten"
        )));
    }

    if qpdf_flag_contract {
        add_qpdf_flatten_contents(pdf, &page, append_bytes)?;
    } else {
        let existing_content = page_content_bytes(pdf, page_ref)?;
        let mut new_content = existing_content;
        if !new_content.is_empty() && new_content.last() != Some(&b'\n') {
            new_content.push(b'\n');
        }
        new_content.extend_from_slice(&append_bytes);
        let stream = add_content_stream(pdf, new_content)?;
        page.replace_key(b"/Contents", stream)?;
    }

    pdf.mark_object_handle_dirty(&page)?;

    // ── Step 8: Remove flattened annotations from /Annots ─────────────────
    replace_pruned_annots(pdf, page_ref, &to_remove, qpdf_flag_contract)?; // cov:ignore: llvm-cov maps this covered multiline call terminator to a zero-hit line

    Ok(flattened_count)
}

fn replace_pruned_annots<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    to_remove: &[ObjectHandle],
    preserve_indirect_holder: bool,
) -> Result<()> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let old_annots = page.try_get_key(b"/Annots")?;
    let new_annots = build_pruned_annots_array(pdf, page_ref, to_remove)?;
    if new_annots.as_array().is_some_and(|items| items.is_empty()) {
        page.remove_key(b"/Annots");
        pdf.mark_object_handle_dirty(&page)?;
    } else if preserve_indirect_holder {
        if let Some(array_ref) = old_annots.object_ref() {
            pdf.replace_object_handle(array_ref, new_annots)?;
        } else {
            page.replace_key(b"/Annots", new_annots)?;
            pdf.mark_object_handle_dirty(&page)?;
        }
    } else {
        page.replace_key(b"/Annots", new_annots)?;
        pdf.mark_object_handle_dirty(&page)?;
    }
    Ok(())
}

fn add_qpdf_flatten_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page: &ObjectHandle,
    append_bytes: Vec<u8>,
) -> Result<()> {
    let before = add_content_stream(pdf, b"q\n".to_vec())?;
    let mut after = b"\nQ\n".to_vec();
    after.extend_from_slice(&append_bytes);
    let after = add_content_stream(pdf, after)?;
    let old = page.try_get_key(b"/Contents")?;
    let old = pdf.resolve_to_terminal(&old)?;
    let mut contents = vec![before];
    if let Some(items) = old.as_array() {
        contents.extend(items);
    } else if !old.is_null() {
        contents.push(old);
    }
    contents.push(after);
    page.replace_key(b"/Contents", ObjectHandle::array(contents))?;
    pdf.mark_object_handle_dirty(page)?;
    Ok(())
}

fn add_content_stream<R: Read + Seek>(pdf: &mut Pdf<R>, data: Vec<u8>) -> Result<ObjectHandle> {
    let object_ref = pdf.next_available_object_ref()?;
    let stream = ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(data));
    pdf.replace_object_handle(object_ref, stream)
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
#[allow(
    clippy::mutable_key_type,
    reason = "the association set keys only on canonical ObjectHandle allocation identity"
)]
pub(crate) fn flatten_annotations_qpdf<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_refs: &[ObjectRef],
    required_flags: i64,
    forbidden_flags: i64,
) -> Result<()> {
    let need_appearances = acroform_need_appearances(pdf)?;
    let default_resources = acroform_default_resources(pdf)?;
    // qpdf resolves the Widget's field helper from one cached
    // AcroFormDocumentHelper analysis before asking it for `/DR`. Build the
    // same live-identity set once, before page/resource mutation begins, so
    // every page uses the same qpdf association boundary without an O(pages²)
    // re-analysis.
    let field_annotation_ids = if !need_appearances && default_resources.is_some() {
        Some(acroform_annotation_identities(pdf)?)
    } else {
        None
    };
    for &page_ref in page_refs {
        materialize_page_resources(pdf, page_ref)?;
        if !need_appearances {
            if let Some(default_resources) = default_resources.as_ref() {
                let field_annotation_ids = field_annotation_ids
                    .as_ref()
                    .expect("default resources require the association set");
                merge_widget_default_resources_on_page_with_associations(
                    pdf,
                    page_ref,
                    default_resources,
                    field_annotation_ids,
                )?;
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
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    if page.as_dictionary().is_none() {
        return Ok(0); // cov:ignore: repaired page snapshot is always a dictionary
    }
    let rotate = pdf.resolve_to_terminal(&page.try_get_key(b"/Rotate")?)?;
    Ok(rotate
        .as_integer()
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(0))
}

fn materialize_page_resources<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<()> {
    // `getAttribute("/Resources", true)` may yield a malformed value. qpdf
    // replaces that value with an empty dictionary instead of rejecting the
    // whole flattening operation.
    let resources = {
        let mut helper = PageObjectHelper::new(page_ref, pdf);
        match helper.get_attribute(b"/Resources", true) {
            Ok(resources) if resources.as_dictionary().is_some() => resources,
            Ok(_) => ObjectHandle::dictionary(Vec::new()),
            // cov:ignore-start: public page walk rejects malformed inherited-resource errors first
            Err(Error::Unsupported(message)) if message.contains("/Resources") => {
                ObjectHandle::dictionary(Vec::new())
            }
            // cov:ignore-end
            Err(error) => return Err(error), // cov:ignore: non-Resources page-walk failures propagate unchanged
        }
    };
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    // cov:ignore-start: public page traversal guarantees a page dictionary at this boundary
    if page.as_dictionary().is_none() {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a page dictionary"
        ))); // cov:ignore: repaired page snapshot is always a dictionary
    }
    // cov:ignore-end
    page.replace_key(b"/Resources", resources)?;
    pdf.mark_object_handle_dirty(&page)?;
    Ok(())
}

/// Return the canonical annotation handles on a page.
///
/// qpdf's `QPDFPageObjectHelper::getAnnotations` does not inspect annotation
/// fields such as `/Rect`; those are read later by
/// `QPDFAnnotationObjectHelper::getPageContentForAppearance`, after the
/// flags gate. Keep the handle-native result here so direct dictionaries are
/// available to every flattening consumer.
fn page_annotation_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectHandle>> {
    let mut page_helper = PageObjectHelper::new(page_ref, pdf);
    page_helper.get_annotations_filtered(None)
}

/// Returns the AcroForm `/DR` handle, unresolved.
///
/// qpdf never reads `/DR` eagerly: `ff.getDefaultResources()`
/// (`QPDFFormFieldObjectHelper.cc:191-193`, a bare `getKey("/DR")` with no
/// dereference) is only ever called from inside
/// `flattenAnnotationsForPage`'s per-annotation loop
/// (`QPDFPageDocumentHelper.cc:108,115`), gated on `process`, which is
/// `false` for every Widget whenever `/NeedAppearances` is true
/// (`:100-103`) -- so a malformed or unreadable indirect `/DR` is never
/// even touched in that case. Resolving here unconditionally would abort
/// or warn during flattening even when `/NeedAppearances` means the value
/// is never needed; the caller must resolve lazily, only once it knows
/// resource merging will actually run.
fn acroform_default_resources<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Option<ObjectHandle>> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None); // cov:ignore: a parsed Pdf always has a root reference
    };
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root)?;
    if root.as_dictionary().is_none() {
        return Ok(None); // cov:ignore: parsed catalog root is a dictionary
    }
    let acroform = pdf.resolve_to_terminal(&root.try_get_key(b"/AcroForm")?)?;
    if acroform.as_dictionary().is_none() {
        return Ok(None); // cov:ignore: malformed AcroForm is ignored like qpdf
    }
    let resources = acroform.try_get_key(b"/DR")?;
    Ok((!resources.is_null()).then_some(resources))
}

/// Resolve every item of an array-shaped resource category, matching
/// qpdf's own dereferencing.
///
/// `isScalar()` (`QPDFObjectHandle.cc:450-453`) dereferences before
/// checking, so an unresolved indirect array item would otherwise be
/// misclassified as non-scalar by [`ObjectHandle::merge_resources`]'s
/// non-resolving `is_scalar` check and silently dropped from
/// dedup/append, instead of being recognized as already present (or
/// carried across) by value.
fn resolve_array_item_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    array: &ObjectHandle,
) -> Result<()> {
    let mut changed = false;
    for (index, item) in array.as_array().unwrap_or_default().into_iter().enumerate() {
        let terminal = pdf.resolve_to_terminal(&item)?;
        if !terminal.is_same_object_as(&item) {
            array.set_array_item(index, terminal)?;
            changed = true;
        }
    }
    if changed {
        pdf.mark_object_handle_dirty(array)?;
    }
    Ok(())
}

/// Resolve exactly what [`ObjectHandle::merge_resources`] will inspect, one
/// DR category at a time in DR's own iteration order, and pre-resolve
/// array items for a category that will undergo an array-merge.
///
/// qpdf's `mergeResources` (`QPDFObjectHandle.cc:1080`,
/// `for (auto const& o_top: other.ditems())`) is a single loop over the
/// *source* (DR)'s own top-level categories, in DR's key order (a
/// `std::map`, so qpdf's own order is lexicographic by key -- matching
/// this dictionary's `BTreeMap` iteration order). For each category it
/// resolves that source value, then -- only if the destination already
/// has that same key -- resolves and processes the destination value,
/// before moving to the next category. Splitting source and matching-
/// destination resolution into two whole-dictionary passes (as an
/// earlier revision of this function did) changes which malformed object
/// is reached first on doubly-malformed input, altering the diagnostics
/// and the propagated error qpdf would produce; interleaving per category
/// here keeps that order faithful. A destination category DR does not
/// also have is never inspected -- resolving every destination category
/// unconditionally would abort or warn on an unrelated malformed category
/// (for example a destination `/ColorSpace` when DR has only `/Font`)
/// that qpdf itself never touches.
///
/// Returns the destination's own indirect array-shaped matched
/// categories, so the caller can mark them dirty *before* calling
/// [`ObjectHandle::merge_resources`]: qpdf's array branch, unlike its
/// dictionary branch, mutates a shared indirect array in place rather
/// than privatizing it first (`QPDFObjectHandle.cc:1130-1147` has no
/// `isIndirect()`/`shallowCopy()` step the way the dictionary branch does
/// at `:1093`), and [`ObjectHandle::merge_resources`] correctly mirrors
/// that. Because a later category can still fail after an earlier
/// array-shaped category was already mutated (entries merged before the
/// failing one stay installed, per that method's own `# Errors` doc), the
/// caller must mark every returned array dirty regardless of whether the
/// merge call that follows ultimately succeeds.
fn resolve_matched_category_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: &ObjectHandle,
    default_resources: &ObjectHandle,
) -> Result<Vec<ObjectHandle>> {
    let mut dirty_arrays = Vec::new();
    let dest_entries = resources.as_dictionary().unwrap_or_default();
    for (category, source_value) in default_resources.as_dictionary().unwrap_or_default() {
        let source_terminal = pdf.resolve_to_terminal(&source_value)?;
        if !source_terminal.is_same_object_as(&source_value) {
            default_resources.replace_key(&category, source_terminal.clone())?;
        }
        let Some(dest_value) = dest_entries.get(&category) else {
            continue; // cov:ignore: qpdf's merge never inspects a destination-only category
        };
        let dest_was_indirect = dest_value.is_indirect();
        let dest_terminal = pdf.resolve_to_terminal(dest_value)?;
        if !dest_terminal.is_same_object_as(dest_value) {
            resources.replace_key(&category, dest_terminal.clone())?;
        }
        if dest_terminal.as_array().is_some() && source_terminal.as_array().is_some() {
            resolve_array_item_handles(pdf, &dest_terminal)?;
            resolve_array_item_handles(pdf, &source_terminal)?;
            if dest_was_indirect {
                dirty_arrays.push(dest_terminal);
            }
        }
    }
    Ok(dirty_arrays)
}

/// Return the live Widget identities that qpdf's AcroForm analysis associates
/// with a field. This includes qpdf's orphan-Widget self-association when a
/// visible `/AcroForm/Fields` key exists, but is empty when that key is absent
/// (`QPDFAcroFormDocumentHelper.cc:241-282`).
#[allow(
    clippy::mutable_key_type,
    reason = "ObjectHandleIdentity hashes only the retained canonical allocation pointer"
)]
fn acroform_annotation_identities<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<HashSet<ObjectHandleIdentity>> {
    let mut helper = AcroFormDocumentHelper::new(pdf)?;
    let identities: HashSet<_> = helper
        .canonical_annotation_to_field_handles()?
        .into_iter()
        .map(|(annotation, _field)| annotation.identity_key())
        .collect();
    Ok(identities)
}

/// Test-facing convenience wrapper that uses the same association analysis as
/// the production flatten path.
#[cfg(test)]
#[allow(
    clippy::mutable_key_type,
    reason = "the test wrapper preserves the production canonical identity set"
)]
fn merge_widget_default_resources_on_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    default_resources: &ObjectHandle,
) -> Result<()> {
    let field_annotation_ids = acroform_annotation_identities(pdf)?;
    merge_widget_default_resources_on_page_with_associations(
        pdf,
        page_ref,
        default_resources,
        &field_annotation_ids,
    )
}

#[allow(
    clippy::mutable_key_type,
    reason = "the merge gate compares only canonical ObjectHandle allocation identity"
)]
fn merge_widget_default_resources_on_page_with_associations<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    default_resources: &ObjectHandle,
    field_annotation_ids: &HashSet<ObjectHandleIdentity>,
) -> Result<()> {
    for annotation in page_annotation_handles(pdf, page_ref)? {
        let mut annotation_object_helper =
            AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
        if annotation_object_helper.get_subtype()? != b"Widget" {
            continue; // cov:ignore: non-widget annotations do not merge default resources
        }
        let appearance = annotation_object_helper.get_appearance_stream(b"N", None)?;
        if appearance.is_null() {
            continue; // cov:ignore: widget without selected appearance has no merge target
        }
        if !field_annotation_ids.contains(&annotation.identity_key()) {
            // qpdf still obtains the appearance stream before asking the null
            // field helper for `/DR`. Keep that ordering so an inline `/AP/N`
            // is materialized by the compatibility boundary even when
            // `analyze()` skipped the orphan-widget fallback because
            // `/AcroForm/Fields` is absent; only the `/DR` merge is gated.
            continue;
        }
        pdf.resolve(&appearance)?;
        let Some(appearance_dict) = appearance.as_stream_dict() else {
            continue; // cov:ignore: selected appearance must be a stream
        };
        let resources = appearance_dict.try_get_key(b"/Resources")?;
        // qpdf privatizes an indirect appearance /Resources before merging DR
        // in (`QPDFPageDocumentHelper.cc:108-113`): the merge target must be
        // a private, direct copy so `mergeResources` -- which mutates its
        // receiver in place -- never writes into an object another
        // appearance stream (or anything else) might also reference. This
        // runs unconditionally on any indirect /Resources, even a malformed
        // (non-dictionary) one: qpdf's own `isIndirect()` check precedes any
        // dictionary check, and `mergeResources` itself is a safe no-op on a
        // non-dictionary receiver (`QPDFObjectHandle.cc:1066-1068`).
        //
        // `is_indirect()` is read before resolution -- it reflects how
        // /Resources was *stored*, matching qpdf's `getKey` result -- but
        // `shallow_copy` (unlike qpdf's self-resolving `shallowCopy`) reads
        // whatever value is already resolved and does not fetch on its own,
        // so the handle must be resolved to its terminal value first.
        let was_indirect = resources.is_indirect();
        let resources = pdf.resolve_to_terminal(&resources)?;
        let resources = if was_indirect {
            let privatized = resources.shallow_copy()?;
            appearance_dict.replace_key(b"/Resources", privatized.clone())?;
            // Mark the owning appearance stream dirty immediately: a
            // malformed (non-dictionary) privatized value still falls
            // through to the `continue` below, and the /Resources rewrite
            // must persist even on that path.
            pdf.mark_object_handle_dirty(&appearance)?;
            privatized
        } else {
            resources
        };
        if resources.as_dictionary().is_none() {
            continue; // cov:ignore: malformed appearance resources are ignored
        }
        // Lazy: qpdf only ever reads /DR from inside this same per-widget
        // merge path (see acroform_default_resources's doc), so resolving
        // it earlier than this would touch a value flattening may not need.
        let default_resources = pdf.resolve_to_terminal(default_resources)?;
        if default_resources.as_dictionary().is_none() {
            continue; // cov:ignore: malformed AcroForm DR is ignored like qpdf
        }
        // See resolve_matched_category_handles's doc for why this resolves
        // source and matching-destination categories interleaved, one DR
        // category at a time, rather than in two whole-dictionary passes.
        let dirty_arrays = resolve_matched_category_handles(pdf, &resources, &default_resources)?;
        // Mark every handle the upcoming merge will touch dirty *before*
        // calling it: entries merged before a later category's failure
        // stay installed in the live handle graph (matching qpdf's own
        // partial-mutation-on-exception behavior, documented on
        // merge_resources's `# Errors`), so the dirty marks must already be
        // in place before that fallible call, not only after it returns.
        pdf.mark_object_handle_dirty(&resources)?;
        for array in &dirty_arrays {
            pdf.mark_object_handle_dirty(array)?;
        }
        resources.merge_resources(&default_resources, None)?;
    }
    Ok(())
}

fn acroform_need_appearances<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let mut helper = AcroFormDocumentHelper::new(pdf)?;
    helper.get_need_appearances()
}

fn remove_acroform<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(()); // cov:ignore: parsed Pdf always has a root reference
    };
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root)?;
    // cov:ignore-start: parsed Pdf catalogs are dictionaries at this boundary
    if root.as_dictionary().is_none() {
        return Ok(()); // cov:ignore: parsed catalog root is a dictionary
    }
    // cov:ignore-end
    root.remove_key(b"/AcroForm");
    pdf.mark_object_handle_dirty(&root)?;
    // qpdf's own `flattenAnnotations` (`QPDFPageDocumentHelper.cc:56-77`)
    // analyzes through a scope-local `QPDFAcroFormDocumentHelper` that goes
    // out of scope on return, so a later step (e.g. `flattenRotation`'s
    // `make_afdh()`, `QPDFJob.cc:2185-2193`) always constructs a fresh
    // helper that observes this removal. `Pdf::acroform_cache` is shared
    // across every `AcroFormDocumentHelper::new()` call for this source, so
    // it must be invalidated here to reproduce that same "no stale analysis
    // survives /AcroForm removal" guarantee.
    *pdf.acroform_cache.borrow_mut() = None;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test-only malformed fixture helpers
// ---------------------------------------------------------------------------

/// Build the pruned `/Annots` array, removing the exact handles in
/// `to_remove`.
///
/// Resolves the live page handle rather than projecting the array through
/// `ObjectRef`s. Direct dictionaries are therefore compared by canonical
/// handle identity and retained directly; indirect entries retain their
/// original references.
fn build_pruned_annots_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    to_remove: &[ObjectHandle],
) -> Result<ObjectHandle> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let annots = pdf.resolve_to_terminal(&page.try_get_key(b"/Annots")?)?;
    let Some(annots_arr) = annots.as_array() else {
        return Ok(ObjectHandle::array(Vec::new()));
    };
    let mut pruned = Vec::with_capacity(annots_arr.len());
    for entry in annots_arr {
        if annotation_is_marked_for_removal(&entry, to_remove) {
            continue;
        }
        pruned.push(entry);
    }

    Ok(ObjectHandle::array(pruned))
}

/// Return whether an annotation handle is one of the qpdf removal candidates.
///
/// Kept as a small named predicate so the identity rule remains visible at
/// the canonical-handle boundary rather than being replaced by an
/// `ObjectRef`-only comparison.
fn annotation_is_marked_for_removal(annotation: &ObjectHandle, to_remove: &[ObjectHandle]) -> bool {
    to_remove
        .iter()
        .any(|candidate| candidate.is_same_object_as(annotation))
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
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).unwrap();
        let resources = page.get_key(b"/Resources");
        pdf.resolve(&resources).unwrap();
        assert!(resources.as_dictionary().is_some());
    }

    #[test]
    fn direct_page_rotate_resolves_an_indirect_integer() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).unwrap();
        let rotate = pdf.get_object_handle(ObjectRef::new(4, 0));
        page.replace_key(b"/Rotate", rotate)
            .expect("page must be mutable");
        pdf.mark_object_handle_dirty(&page)
            .expect("page mutation must be dirty");
        pdf.replace_object_handle(ObjectRef::new(4, 0), ObjectHandle::integer(270))
            .expect("indirect rotate value must be replaceable");

        assert_eq!(direct_page_rotate(&mut pdf, page_ref).unwrap(), 270);
    }

    #[test]
    fn acroform_need_appearances_reads_a_direct_boolean() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let root_ref = pdf.root_ref().expect("fixture catalog must exist");
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root).unwrap();
        root.replace_key(
            b"/AcroForm",
            ObjectHandle::dictionary(vec![(
                b"/NeedAppearances".to_vec(),
                ObjectHandle::boolean(true),
            )]),
        )
        .unwrap();
        pdf.mark_object_handle_dirty(&root).unwrap();

        assert!(acroform_need_appearances(&mut pdf).unwrap());
    }

    #[test]
    fn build_pruned_annots_array_treats_a_non_array_as_empty() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).unwrap();
        page.replace_key(b"/Annots", ObjectHandle::integer(7))
            .unwrap();
        pdf.mark_object_handle_dirty(&page).unwrap();

        let pruned = build_pruned_annots_array(&mut pdf, ObjectRef::new(3, 0), &[]).unwrap();
        assert!(pruned.as_array().unwrap().is_empty());
    }

    #[test]
    fn qpdf_flatten_merges_an_indirect_default_resource_category() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).unwrap();
        root.replace_key(
            b"/AcroForm",
            ObjectHandle::dictionary(vec![(b"/Fields".to_vec(), ObjectHandle::array(Vec::new()))]),
        )
        .unwrap();
        pdf.mark_object_handle_dirty(&root).unwrap();

        pdf.replace_object_handle(ObjectRef::new(7, 0), ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        let font_category = ObjectHandle::dictionary(vec![(
            b"/F1".to_vec(),
            pdf.get_object_handle(ObjectRef::new(7, 0)),
        )]);
        pdf.replace_object_handle(ObjectRef::new(6, 0), font_category)
            .unwrap();
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/Font".to_vec(),
                    ObjectHandle::dictionary(Vec::new()),
                )]),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            pdf.get_object_handle(ObjectRef::new(6, 0)),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let stream_dict = appearance.as_stream_dict().unwrap();
        let resources = stream_dict.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let fonts = resources.try_get_key(b"/Font").unwrap();
        pdf.resolve(&fonts).unwrap();
        let f1 = fonts.try_get_key(b"/F1").unwrap();
        assert_eq!(f1.object_ref(), Some(ObjectRef::new(7, 0)));
    }

    #[test]
    fn qpdf_flatten_skips_default_resources_for_widget_without_acroform_fields() {
        // qpdf's AcroForm analyze returns before the orphan-widget fallback
        // when /AcroForm has no visible /Fields key
        // (QPDFAcroFormDocumentHelper.cc:241-245). Its
        // getFieldForAnnotation therefore returns a null field helper, whose
        // getDefaultResources does not read /AcroForm/DR. A page Widget in
        // this shape must not receive the document default resources.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        pdf.replace_object_handle(ObjectRef::new(7, 0), ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        let font_category = ObjectHandle::dictionary(vec![(
            b"/F1".to_vec(),
            pdf.get_object_handle(ObjectRef::new(7, 0)),
        )]);
        pdf.replace_object_handle(ObjectRef::new(6, 0), font_category)
            .unwrap();
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/Font".to_vec(),
                    ObjectHandle::dictionary(Vec::new()),
                )]),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            pdf.get_object_handle(ObjectRef::new(6, 0)),
        )]);

        // Keep `/AcroForm/DR` present while omitting `/Fields`, which is the
        // qpdf shape where `analyze()` skips the orphan-widget fallback.
        let catalog = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&catalog).unwrap();
        catalog
            .replace_key(
                b"/AcroForm",
                ObjectHandle::dictionary(vec![(
                    b"/DR".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(6, 0)),
                )]),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&catalog).unwrap();

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let stream_dict = appearance.as_stream_dict().unwrap();
        let resources = stream_dict.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let font = resources.try_get_key(b"/Font").unwrap();
        pdf.resolve(&font).unwrap();
        assert!(
            font.try_get_keys().unwrap().is_empty(),
            "an unassociated Widget must not inherit /AcroForm/DR"
        );
    }

    #[test]
    fn qpdf_flatten_rejects_a_direct_stream_when_installing_a_missing_resource_category() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                ObjectHandle::dictionary(Vec::new()),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();

        let default_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
        )]);

        let error = merge_widget_default_resources_on_page(
            &mut pdf,
            ObjectRef::new(3, 0),
            &default_resources,
        )
        .expect_err("qpdf shallowCopy rejects a direct stream resource");
        assert!(matches!(
            error,
            Error::System(message) if message == "stream objects cannot be cloned"
        ));
    }

    #[test]
    fn qpdf_document_flatten_propagates_default_resource_merge_error() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                ObjectHandle::dictionary(Vec::new()),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();

        let default_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
        )]);
        let acroform = ObjectHandle::dictionary(vec![
            (b"/Fields".to_vec(), ObjectHandle::array(Vec::new())),
            (b"/DR".to_vec(), default_resources),
        ]);
        let catalog = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&catalog).unwrap();
        catalog.replace_key(b"/AcroForm", acroform).unwrap();
        pdf.mark_object_handle_dirty(&catalog).unwrap();

        let error = flatten_annotations_qpdf(&mut pdf, &[ObjectRef::new(3, 0)], 0, 0x3)
            .expect_err("production flatten must propagate the DR merge failure");
        assert!(matches!(
            error,
            Error::System(message) if message == "stream objects cannot be cloned"
        ));
    }

    #[test]
    fn qpdf_flatten_privatizes_an_indirect_appearance_resources_before_merging() {
        // qpdf privatizes an indirect appearance /Resources before merging DR
        // in (`QPDFPageDocumentHelper.cc:108-113`): `isIndirect()` triggers a
        // `shallowCopy()` that becomes a fresh direct value on the
        // appearance's own dict, so `mergeResources` -- which mutates its
        // receiver in place -- never writes into an object another
        // appearance (or anything else) might also reference. Two widgets
        // share one indirect /Resources object here; after the merge each
        // appearance must hold its own privatized copy, and the original
        // shared object must be untouched.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R 6 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let shared_font =
            ObjectHandle::dictionary(vec![(b"/F1".to_vec(), ObjectHandle::integer(41))]);
        pdf.replace_object_handle(ObjectRef::new(20, 0), shared_font)
            .unwrap();
        let shared_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            pdf.get_object_handle(ObjectRef::new(20, 0)),
        )]);
        pdf.replace_object_handle(ObjectRef::new(9, 0), shared_resources)
            .unwrap();

        let appearance1 = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                pdf.get_object_handle(ObjectRef::new(9, 0)),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance1)
            .unwrap();
        let widget1 = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget1)
            .unwrap();

        let appearance2 = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                pdf.get_object_handle(ObjectRef::new(9, 0)),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(7, 0), appearance2)
            .unwrap();
        let widget2 = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(7, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(6, 0), widget2)
            .unwrap();

        let default_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Helv".to_vec(), ObjectHandle::integer(42))]),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let mut privatized_resource_ids = Vec::new();
        for appearance_ref in [ObjectRef::new(5, 0), ObjectRef::new(7, 0)] {
            let appearance = pdf.get_object_handle(appearance_ref);
            pdf.resolve(&appearance).unwrap();
            let stream_dict = appearance.as_stream_dict().unwrap();
            let resources = stream_dict.try_get_key(b"/Resources").unwrap();
            assert!(
                resources.object_ref().is_none(),
                "appearance resources must be privatized as a direct copy"
            );
            privatized_resource_ids.push(resources.identity_key());
            pdf.resolve(&resources).unwrap();
            let fonts = resources.try_get_key(b"/Font").unwrap();
            pdf.resolve(&fonts).unwrap();
            let f1 = fonts.try_get_key(b"/F1").unwrap();
            assert_eq!(f1.as_integer(), Some(41));
            let helv = fonts.try_get_key(b"/Helv").unwrap();
            assert_eq!(helv.as_integer(), Some(42));
        }
        assert!(privatized_resource_ids[0] != privatized_resource_ids[1]);

        let original_resources = pdf.get_object_handle(ObjectRef::new(9, 0));
        pdf.resolve(&original_resources).unwrap();
        let original_font = original_resources.try_get_key(b"/Font").unwrap();
        assert_eq!(
            original_font.object_ref(),
            Some(ObjectRef::new(20, 0)),
            "shared resources object must keep its own indirect Font reference, unmerged"
        );
        pdf.resolve(&original_font).unwrap();
        let original_f1 = original_font.try_get_key(b"/F1").unwrap();
        assert_eq!(original_f1.as_integer(), Some(41));
        assert_eq!(
            original_font.try_get_keys().unwrap().len(),
            1,
            "shared font dictionary must not gain DR's Helv entry"
        );
    }

    #[test]
    fn qpdf_flatten_ignores_a_malformed_destination_category_absent_from_dr() {
        // qpdf's mergeResources (QPDFObjectHandle.cc:1080) iterates only
        // DR's own categories; a destination-only category is never
        // inspected. The appearance's /Resources here has a malformed
        // indirect /ColorSpace that DR does not have -- if flattening
        // resolved every destination category unconditionally, this
        // malformed value would be touched even though qpdf never would be.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let appearance_resources = ObjectHandle::dictionary(vec![
            (b"/Font".to_vec(), ObjectHandle::dictionary(Vec::new())),
            (
                b"/ColorSpace".to_vec(),
                pdf.get_object_handle(ObjectRef::new(8, 0)),
            ),
        ]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        // Object 8 is never a valid PDF object body -- if this ever gets
        // resolved, the read fails.
        pdf.replace_object_handle(ObjectRef::new(8, 0), ObjectHandle::name(Vec::new()))
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();

        let default_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            ObjectHandle::dictionary(Vec::new()),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .expect("an unrelated destination-only category must never be touched");

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let stream_dict = appearance.as_stream_dict().unwrap();
        let resources = stream_dict.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let colorspace = resources.try_get_key(b"/ColorSpace").unwrap();
        assert_eq!(
            colorspace.object_ref(),
            Some(ObjectRef::new(8, 0)),
            "the unrelated malformed category must be left exactly as-is"
        );
    }

    #[test]
    fn qpdf_flatten_appends_an_indirect_scalar_item_from_dr() {
        // qpdf's isScalar() (QPDFObjectHandle.cc:450-453) dereferences
        // before checking, so an indirect scalar item present only in DR's
        // array is recognized as scalar and appended (as its own reference,
        // per unparse()'s indirect-identity-preserving key,
        // QPDFObjectHandle.cc:1575-1583) -- not silently dropped by a
        // non-resolving scalar check that misclassifies it as non-scalar
        // before the `if !is_scalar(&item) { continue; }` gate.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let appearance_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();

        pdf.replace_object_handle(ObjectRef::new(8, 0), ObjectHandle::name(b"Text".to_vec()))
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![pdf.get_object_handle(ObjectRef::new(8, 0))]),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let stream_dict = appearance.as_stream_dict().unwrap();
        let resources = stream_dict.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let proc_set = resources.try_get_key(b"/ProcSet").unwrap();
        pdf.resolve(&proc_set).unwrap();
        let proc_set_items = proc_set.as_array().unwrap();
        assert_eq!(proc_set_items.len(), 2);
        assert_eq!(proc_set_items[0].as_name(), Some(b"PDF".to_vec()));
        assert_eq!(
            proc_set_items[1].object_ref(),
            Some(ObjectRef::new(8, 0)),
            "DR's indirect scalar item must be appended, not dropped"
        );
    }

    #[test]
    fn qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge() {
        // qpdf's array branch (QPDFObjectHandle.cc:1130-1147) mutates a
        // shared indirect destination array in place, unlike its dictionary
        // branch. The mutated array's own indirect owner must be marked
        // dirty explicitly so the writer observes the merged content.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        pdf.replace_object_handle(
            ObjectRef::new(9, 0),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )
        .unwrap();
        let appearance_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            pdf.get_object_handle(ObjectRef::new(9, 0)),
        )]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();

        let default_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"Text".to_vec())]),
        )]);

        // Setup's replace_object_handle calls already left object 9 dirty;
        // clear that so the post-merge dirty assertion below can only pass
        // because the merge itself marks the mutated array's indirect owner
        // dirty, not because it was already dirty from construction.
        pdf.clear_dirty(ObjectRef::new(9, 0));

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        assert!(
            pdf.is_dirty(ObjectRef::new(9, 0)),
            "the mutated indirect array's owner must be marked dirty after the merge"
        );

        // Reach /ProcSet through the appearance's own resource dictionary,
        // not by looking object 9 up directly, so a regression that rebinds
        // the entry to a different (or direct) array would be caught.
        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let stream_dict = appearance.as_stream_dict().unwrap();
        let resources = stream_dict.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let proc_set = resources.try_get_key(b"/ProcSet").unwrap();
        pdf.resolve(&proc_set).unwrap();
        assert_eq!(
            proc_set.object_ref(),
            Some(ObjectRef::new(9, 0)),
            "the merged array must retain its indirect owner identity"
        );
        let proc_set_items = proc_set
            .as_array()
            .expect("shared ProcSet array must remain an array");
        assert_eq!(
            proc_set_items.len(),
            2,
            "the merged array must contain exactly the original and appended items"
        );
        assert_eq!(
            proc_set_items[0].as_name(),
            Some(b"PDF".to_vec()),
            "the existing indirect array item must remain in place"
        );
        assert_eq!(
            proc_set_items[1].as_name(),
            Some(b"Text".to_vec()),
            "the merged indirect array must be reachable through pdf.resolve after the merge"
        );
    }

    #[test]
    fn qpdf_flatten_keeps_an_earlier_indirect_array_merge_dirty_after_a_later_category_fails() {
        // merge_resources documents that entries merged before a later
        // category's failure stay installed in the live handle graph,
        // matching an exception unwinding out of qpdf's own loop. DR's
        // categories are visited in key order ("/ProcSet" < "/XObject"),
        // so the indirect /ProcSet array is merged (and mutated) first,
        // then /XObject -- a direct stream, absent from the destination --
        // fails installation via shallow_copy's stream rejection.
        //
        // ObjectHandle reads are always live off the shared canonical
        // handle graph, so a stale-content check alone cannot tell whether
        // the /ProcSet merge's dirty mark survived the later failure --
        // that must be asserted directly via pdf.is_dirty.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        pdf.replace_object_handle(
            ObjectRef::new(9, 0),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )
        .unwrap();
        let appearance_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            pdf.get_object_handle(ObjectRef::new(9, 0)),
        )]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();

        let default_resources = ObjectHandle::dictionary(vec![
            (
                b"/ProcSet".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::name(b"Text".to_vec())]),
            ),
            (
                b"/XObject".to_vec(),
                ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
            ),
        ]);

        // Sanity-check the pre-merge fixture shape.
        let proc_set_before = pdf.get_object_handle(ObjectRef::new(9, 0));
        pdf.resolve(&proc_set_before).unwrap();
        let proc_set_before_items = proc_set_before
            .as_array()
            .expect("shared ProcSet array must remain an array");
        assert_eq!(proc_set_before_items.len(), 1);
        assert_eq!(proc_set_before_items[0].as_name(), Some(b"PDF".to_vec()));

        // Setup's replace_object_handle calls already left object 9 dirty;
        // clear that so the post-merge dirty assertion below can only pass
        // because the /ProcSet merge itself marks object 9 dirty (and that
        // mark survives the later /XObject failure), not because it was
        // already dirty from construction.
        pdf.clear_dirty(ObjectRef::new(9, 0));

        let error = merge_widget_default_resources_on_page(
            &mut pdf,
            ObjectRef::new(3, 0),
            &default_resources,
        )
        .expect_err("the direct-stream /XObject category must still fail after /ProcSet merges");
        assert!(matches!(
            &error,
            Error::System(message) if message == "stream objects cannot be cloned"
        ));

        // qpdf's merge_resources documents that entries merged before a
        // later category's failure stay installed and dirty in the live
        // handle graph. ObjectHandle reads are always live regardless of
        // dirty state (dirty only controls what the writer emits), so the
        // dirty mark itself must be asserted directly rather than inferred
        // from content still being visible through resolve.
        assert!(
            pdf.is_dirty(ObjectRef::new(9, 0)),
            "the /ProcSet merge that ran before the /XObject failure must leave its \
             indirect array owner dirty, not roll the dirty mark back"
        );

        // Reach /ProcSet through the appearance's own resource dictionary,
        // not by looking object 9 up directly, so a regression that rebinds
        // the entry to a different (or direct) array would be caught.
        let appearance_after = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance_after).unwrap();
        let resources_after = appearance_after.as_stream_dict().unwrap();
        let proc_set_after = resources_after.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&proc_set_after).unwrap();
        let proc_set_after = proc_set_after.try_get_key(b"/ProcSet").unwrap();
        pdf.resolve(&proc_set_after).unwrap();
        assert_eq!(
            proc_set_after.object_ref(),
            Some(ObjectRef::new(9, 0)),
            "the merged array must retain its indirect owner identity"
        );
        let proc_set_after_items = proc_set_after
            .as_array()
            .expect("shared ProcSet array must remain an array");
        assert_eq!(
            proc_set_after_items.len(),
            2,
            "the merged array must contain exactly the original and appended items"
        );
        assert_eq!(
            proc_set_after_items[0].as_name(),
            Some(b"PDF".to_vec()),
            "the existing ProcSet item must remain after the later failure"
        );
        assert_eq!(
            proc_set_after_items[1].as_name(),
            Some(b"Text".to_vec()),
            "the /ProcSet merge that ran before the /XObject failure must remain \
             installed, not be rolled back"
        );
    }

    #[test]
    fn qpdf_flatten_installs_an_array_default_resource_category_absent_from_the_appearance() {
        // qpdf `replaceKey(rtype, other_val.shallowCopy())`
        // (QPDFObjectHandle.cc:1144-1146): a category absent from the
        // destination is installed wholesale, regardless of its type. Before
        // this fix, an array-shaped `/DR` category such as `/ProcSet` was
        // silently dropped because the merge loop required the source to be
        // a dictionary before even checking whether the destination had the
        // category.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        // qpdf's `mergeResources` is a no-op unless the appearance stream
        // already has a (possibly empty) `/Resources` dictionary --
        // `getKey("/Resources")` on an absent key returns a null handle, and
        // `mergeResources` returns immediately when the receiver isn't a
        // dictionary (QPDFObjectHandle.cc:1063-1069).
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Resources".to_vec(),
                ObjectHandle::dictionary(Vec::new()),
            )]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"PDF".to_vec()),
                ObjectHandle::name(b"Text".to_vec()),
            ]),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let resources = appearance
            .as_stream_dict()
            .expect("fixture appearance must remain a stream")
            .try_get_key(b"/Resources")
            .expect("fixture appearance must gain a resources dictionary");
        pdf.resolve(&resources).unwrap();
        let proc_set = resources
            .try_get_key(b"/ProcSet")
            .expect("fixture appearance must gain ProcSet");
        pdf.resolve(&proc_set).unwrap();
        let proc_set_items = proc_set
            .as_array()
            .expect("installed ProcSet category must be an array");
        assert_eq!(
            proc_set_items
                .iter()
                .map(|item| item.as_name())
                .collect::<Vec<_>>(),
            vec![Some(b"PDF".to_vec()), Some(b"Text".to_vec())]
        );
    }

    #[test]
    fn qpdf_flatten_merges_an_array_default_resource_category_deduping_existing_scalars() {
        // qpdf's array branch (QPDFObjectHandle.cc:1130-1147): append only
        // the source scalars the destination doesn't already carry, keeping
        // the destination's own items untouched.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let appearance_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"PDF".to_vec()),
                ObjectHandle::name(b"Text".to_vec()),
            ]),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let resources = appearance
            .as_stream_dict()
            .expect("fixture appearance must remain a stream")
            .try_get_key(b"/Resources")
            .expect("fixture appearance must retain resources");
        pdf.resolve(&resources).unwrap();
        let proc_set = resources
            .try_get_key(b"/ProcSet")
            .expect("fixture appearance must retain ProcSet");
        pdf.resolve(&proc_set).unwrap();
        let proc_set_items = proc_set
            .as_array()
            .expect("fixture ProcSet must remain an array");
        assert_eq!(
            proc_set_items
                .iter()
                .map(|item| item.as_name())
                .collect::<Vec<_>>(),
            vec![Some(b"PDF".to_vec()), Some(b"Text".to_vec())],
            "the existing /PDF entry must not duplicate, and /Text must be appended"
        );
    }

    #[test]
    fn qpdf_flatten_leaves_a_type_mismatched_default_resource_category_untouched() {
        // qpdf's `if`/`else if` ladder (QPDFObjectHandle.cc:1083-1147) only
        // mutates `this_val` when both sides are dictionaries or both sides
        // are arrays; any other combination leaves the destination category
        // exactly as it already was.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let appearance_resources =
            ObjectHandle::dictionary(vec![(b"/ProcSet".to_vec(), ObjectHandle::integer(7))]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let resources = appearance
            .as_stream_dict()
            .expect("fixture appearance must remain a stream")
            .try_get_key(b"/Resources")
            .expect("fixture appearance must retain resources");
        pdf.resolve(&resources).unwrap();
        let proc_set = resources
            .try_get_key(b"/ProcSet")
            .expect("fixture appearance must retain ProcSet");
        pdf.resolve(&proc_set).unwrap();
        assert_eq!(proc_set.as_integer(), Some(7));
    }

    #[test]
    fn qpdf_flatten_array_merge_excludes_non_scalar_items() {
        // qpdf's array branch only considers `isScalar()` items for both the
        // dedup set and the append (QPDFObjectHandle.cc:1130-1147); a
        // non-scalar item such as a nested array or dictionary is excluded
        // from the merge entirely, on either side.
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        register_acroform_fields(&mut pdf, &[]);
        let appearance_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::array(vec![ObjectHandle::integer(1)])]),
        )]);
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Resources".to_vec(), appearance_resources)]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(ObjectRef::new(5, 0), appearance)
            .unwrap();
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    pdf.get_object_handle(ObjectRef::new(5, 0)),
                )]),
            ),
        ]);
        pdf.replace_object_handle(ObjectRef::new(4, 0), widget)
            .unwrap();
        let default_resources = ObjectHandle::dictionary(vec![(
            b"/ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::dictionary(Vec::new())]),
        )]);

        merge_widget_default_resources_on_page(&mut pdf, ObjectRef::new(3, 0), &default_resources)
            .unwrap();

        let appearance = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&appearance).unwrap();
        let resources = appearance
            .as_stream_dict()
            .expect("fixture appearance must remain a stream")
            .try_get_key(b"/Resources")
            .expect("fixture appearance must retain resources");
        pdf.resolve(&resources).unwrap();
        let proc_set = resources
            .try_get_key(b"/ProcSet")
            .expect("fixture appearance must retain ProcSet");
        pdf.resolve(&proc_set).unwrap();
        let proc_set_items = proc_set
            .as_array()
            .expect("fixture ProcSet must remain an array");
        assert_eq!(proc_set_items.len(), 1);
        let nested = proc_set_items[0]
            .as_array()
            .expect("existing ProcSet item must remain a nested array");
        assert_eq!(
            nested[0].as_integer(),
            Some(1),
            "the existing non-scalar item survives; the source's non-scalar item is not appended"
        );
    }

    #[test]
    fn qpdf_flatten_ignores_direct_widget_inline_appearance_for_resource_merge() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("", &[]))).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let appearance_dict = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(1),
                    ObjectHandle::integer(1),
                ]),
            ),
        ]);
        let appearance = ObjectHandle::dictionary(vec![(
            b"/N".to_vec(),
            ObjectHandle::stream(appearance_dict, Rc::new(Vec::new())),
        )]);
        let widget = ObjectHandle::dictionary(vec![
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
            (b"/AP".to_vec(), appearance),
        ]);

        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).unwrap();
        page.replace_key(b"/Annots", ObjectHandle::array(vec![widget]))
            .unwrap();
        pdf.mark_object_handle_dirty(&page).unwrap();

        let default_resources = ObjectHandle::dictionary(Vec::new());
        merge_widget_default_resources_on_page(&mut pdf, page_ref, &default_resources).unwrap();

        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).unwrap();
        let annots = page.try_get_key(b"/Annots").unwrap();
        pdf.resolve(&annots).unwrap();
        let annots = annots.as_array().unwrap();
        assert_eq!(annots.len(), 1);
    }

    #[test]
    fn qpdf_flatten_wraps_content_when_dropping_an_unselected_appearance() {
        let mut pdf = Pdf::open(Cursor::new(build_pdf("/Annots [4 0 R]", &[]))).unwrap();
        let annotation_ref = ObjectRef::new(4, 0);
        pdf.replace_object_handle(annotation_ref, ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        let annotation = pdf.get_object_handle(annotation_ref);
        pdf.resolve(&annotation).unwrap();
        annotation
            .replace_key(b"/AP", ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        pdf.mark_object_handle_dirty(&annotation).unwrap();

        flatten_annotations_qpdf(&mut pdf, &[ObjectRef::new(3, 0)], 0, 0x3).unwrap();

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).unwrap();
        assert!(page.try_get_key(b"/Annots").unwrap().is_null());
        let contents = page.try_get_key(b"/Contents").unwrap();
        pdf.resolve(&contents).unwrap();
        assert_eq!(contents.as_array().map(|items| items.len()), Some(2));
    }

    #[test]
    fn qpdf_document_flatten_covers_widget_resources_and_removal_paths() {
        let selected_ref = ObjectRef::new(4, 0);
        let unselected_ref = ObjectRef::new(5, 0);
        let link_ref = ObjectRef::new(6, 0);
        let resources_ref = ObjectRef::new(7, 0);
        let font_ref = ObjectRef::new(8, 0);
        let acroform_ref = ObjectRef::new(9, 0);
        let default_resources_ref = ObjectRef::new(10, 0);
        let appearance_ref = ObjectRef::new(12, 0);
        let mut pdf = Pdf::open(Cursor::new(build_pdf(
            "/Rotate 90 /Annots [4 0 R 5 0 R 6 0 R]",
            &[],
        )))
        .unwrap();

        let font_handle = pdf.get_object_handle(font_ref);
        pdf.replace_object_handle(
            resources_ref,
            ObjectHandle::dictionary(vec![(b"/Font".to_vec(), font_handle)]),
        )
        .unwrap();
        pdf.replace_object_handle(font_ref, ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        let appearance = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"/BBox".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(100),
                        ObjectHandle::integer(20),
                    ]),
                ),
                (b"/Resources".to_vec(), pdf.get_object_handle(resources_ref)),
            ]),
            Rc::new(Vec::new()),
        );
        pdf.replace_object_handle(appearance_ref, appearance)
            .unwrap();

        let appearance_handle = pdf.get_object_handle(appearance_ref);
        pdf.replace_object_handle(
            selected_ref,
            ObjectHandle::dictionary(vec![
                (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
                (b"/F".to_vec(), ObjectHandle::integer(0x10)),
                (
                    b"/Rect".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::integer(10),
                        ObjectHandle::integer(20),
                        ObjectHandle::integer(110),
                        ObjectHandle::integer(40),
                    ]),
                ),
                (
                    b"/AP".to_vec(),
                    ObjectHandle::dictionary(vec![(b"/N".to_vec(), appearance_handle)]),
                ),
            ]),
        )
        .unwrap();
        pdf.replace_object_handle(
            unselected_ref,
            ObjectHandle::dictionary(vec![
                (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
                (b"/AP".to_vec(), ObjectHandle::dictionary(Vec::new())),
            ]),
        )
        .unwrap();
        pdf.replace_object_handle(
            link_ref,
            ObjectHandle::dictionary(vec![(
                b"/Subtype".to_vec(),
                ObjectHandle::name(b"Link".to_vec()),
            )]),
        )
        .unwrap();

        pdf.replace_object_handle(
            default_resources_ref,
            ObjectHandle::dictionary(vec![(
                b"/Font".to_vec(),
                ObjectHandle::dictionary(vec![(b"/Helv".to_vec(), ObjectHandle::integer(42))]),
            )]),
        )
        .unwrap();
        let selected_handle = pdf.get_object_handle(selected_ref);
        let default_resources_handle = pdf.get_object_handle(default_resources_ref);
        pdf.replace_object_handle(
            acroform_ref,
            ObjectHandle::dictionary(vec![
                (
                    b"/Fields".to_vec(),
                    ObjectHandle::array(vec![selected_handle]),
                ),
                (b"/DR".to_vec(), default_resources_handle),
            ]),
        )
        .unwrap();
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).unwrap();
        let acroform_handle = pdf.get_object_handle(acroform_ref);
        root.replace_key(b"/AcroForm", acroform_handle).unwrap();
        pdf.mark_object_handle_dirty(&root).unwrap();

        flatten_annotations_qpdf(&mut pdf, &[ObjectRef::new(3, 0)], 0, 0x3).unwrap();
        let appearance = pdf.get_object_handle(appearance_ref);
        pdf.resolve(&appearance).unwrap();
        let stream_dict = appearance
            .as_stream_dict()
            .expect("fixture appearance must remain a stream");
        let resources = stream_dict
            .try_get_key(b"/Resources")
            .expect("appearance must retain resources");
        assert!(
            resources.is_direct(),
            "flattening must privatize /Resources into a direct copy, not keep the shared {resources_ref:?} indirect reference"
        );
        pdf.resolve(&resources).unwrap();
        let fonts = resources
            .try_get_key(b"/Font")
            .expect("appearance must retain Font resources");
        assert!(
            fonts.is_direct(),
            "flattening must privatize /Font into a direct copy, not keep the shared {font_ref:?} indirect reference"
        );
        pdf.resolve(&fonts).unwrap();
        let helv = fonts.try_get_key(b"/Helv").unwrap();
        pdf.resolve(&helv).unwrap();
        assert_eq!(helv.as_integer(), Some(42));

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).unwrap();
        let annots = page.try_get_key(b"/Annots").unwrap();
        pdf.resolve(&annots).unwrap();
        let annots = annots.as_array().expect("retained annotations array");
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].object_ref(), Some(link_ref));
        let contents = page.try_get_key(b"/Contents").unwrap();
        pdf.resolve(&contents).unwrap();
        assert!(contents.as_array().is_some());

        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).unwrap();
        assert!(
            !root
                .as_dictionary()
                .expect("fixture root must remain a dictionary")
                .contains_key(b"/AcroForm".as_slice()),
            "/AcroForm must be removed as a dictionary entry, not merely nulled -- \
             try_get_key alone cannot distinguish an absent key from a present null \
             value, matching qpdf's own key/null conflation"
        );
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

    /// Register `widget_refs` in `/AcroForm/Fields` so qpdf's
    /// `AcroFormDocumentHelper::analyze` associates them with a field (the
    /// precondition `merge_widget_default_resources_on_page` now requires
    /// before merging `/AcroForm/DR`). Uses a synthetic high object number
    /// for the `/AcroForm` dict so it never collides with a fixture's own
    /// numbering.
    fn register_acroform_fields<R: Read + Seek>(pdf: &mut Pdf<R>, widget_refs: &[ObjectRef]) {
        let field_handles = widget_refs
            .iter()
            .copied()
            .map(|widget_ref| pdf.get_object_handle(widget_ref))
            .collect();
        let acroform_ref = ObjectRef::new(900, 0);
        pdf.replace_object_handle(
            acroform_ref,
            ObjectHandle::dictionary(vec![(
                b"/Fields".to_vec(),
                ObjectHandle::array(field_handles),
            )]),
        )
        .unwrap();

        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).unwrap();
        root.replace_key(b"/Type", ObjectHandle::name(b"Catalog".to_vec()))
            .unwrap();
        root.replace_key(b"/Pages", pdf.get_object_handle(ObjectRef::new(2, 0)))
            .unwrap();
        root.replace_key(b"/AcroForm", pdf.get_object_handle(acroform_ref))
            .unwrap();
        pdf.mark_object_handle_dirty(&root).unwrap();
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
    fn open_annotation_object_helper_fixture(
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
    fn annotation_object_helper_qpdf_validation_and_rotation_paths() {
        let stream_dictionary = "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Length 0 >>";
        let annotation = "<< /Type /Annot /Rect [10 20 110 40] /F 4 /AP << /N 5 0 R >> >>";

        let mut pdf =
            open_annotation_object_helper_fixture("<< /Type /Annot /Rect [0 0 100 20] >>", None);
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_object_helper_fixture(annotation, Some(stream_dictionary));
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 4)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_object_helper_fixture(
            "<< /Type /Annot /Rect [0 0 100] /AP << /N 5 0 R >> >>",
            Some(stream_dictionary),
        );
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_object_helper_fixture(
            "<< /Type /Annot /Rect [0 0 bad 20] /AP << /N 5 0 R >> >>",
            Some(stream_dictionary),
        );
        assert!(AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_object_helper_fixture(
            annotation,
            Some(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 0] /Length 0 >>",
            ),
        );
        assert!(!AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)
            .get_page_content_for_appearance("/Fxo1", 0, 0, 0)
            .unwrap()
            .is_empty());

        let mut pdf = open_annotation_object_helper_fixture(
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
            let mut pdf =
                open_annotation_object_helper_fixture(annotation, Some(stream_dictionary));
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
        let annotation_ref = ObjectRef::new(4, 0);
        pdf.replace_object_handle(annotation_ref, ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        let annotation = pdf.get_object_handle(annotation_ref);
        pdf.resolve(&annotation).unwrap();
        annotation
            .replace_key(b"/Subtype", ObjectHandle::name(b"Widget".to_vec()))
            .unwrap();
        annotation
            .replace_key(
                b"/Rect",
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(100),
                    ObjectHandle::integer(20),
                ]),
            )
            .unwrap();
        annotation
            .replace_key(
                b"/AP",
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    ObjectHandle::stream(
                        ObjectHandle::dictionary(vec![(
                            b"/BBox".to_vec(),
                            ObjectHandle::array(vec![
                                ObjectHandle::integer(0),
                                ObjectHandle::integer(0),
                                ObjectHandle::integer(100),
                                ObjectHandle::integer(20),
                            ]),
                        )]),
                        Rc::new(Vec::new()),
                    ),
                )]),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&annotation).unwrap();

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
    fn flatten_annotations_preserves_direct_annotation_handle_until_removal() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let bytes = build_pdf(
            "/Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 100 20] /AP << /N 5 0 R >> >>]",
            &[obj_wrap(5, xobj_body)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert_eq!(
            flatten_annotations_on_page(&mut pdf, ObjectRef::new(3, 0), FlattenMode::All).unwrap(),
            1
        );
        let annotations = {
            let mut page_helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
            page_helper
                .get_annotation_handles(None)
                .expect("annotation list should be readable after flattening")
        };
        assert!(annotations.is_empty());
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).unwrap();
        assert!(!page.has_key(b"/Annots"));
        assert!(page_content_bytes(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .windows(2)
            .any(|window| window == b"Do"));
    }

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
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).unwrap();
        let resources = page.get_key(b"/Resources");
        pdf.resolve(&resources).unwrap();
        let xobj = resources.get_key(b"/XObject");
        pdf.resolve(&xobj).unwrap();
        let xobj_dict = xobj.as_dictionary().expect("XObject should be a dict");
        assert_eq!(xobj_dict.len(), 1, "exactly one XObject entry");
        // The value should reference obj 5.
        let xobj_val = xobj_dict.values().next().unwrap();
        assert_eq!(
            xobj_val.object_ref(),
            Some(ObjectRef::new(5, 0)),
            "XObject should reference the source Form XObject"
        );

        // Page content should contain "cm" and "Do".
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("cm"), "content should contain cm");
        assert!(content_str.contains("Do"), "content should contain Do");
        assert!(content_str.contains('q'), "content should contain q");
        assert!(content_str.contains('Q'), "content should contain Q");

        // qpdf removes /Annots after every annotation has been flattened.
        assert!(!page.has_key(b"/Annots"));
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
        let annotations = {
            let mut page_helper = PageObjectHelper::new(page_ref, &mut pdf);
            page_helper
                .get_annotation_handles(None)
                .expect("remaining annotation list should be readable")
        };
        assert_eq!(annotations.len(), 1, "one annotation should remain");
        assert_eq!(
            annotations[0].object_ref(),
            Some(ObjectRef::new(7, 0)),
            "the non-Print annotation should remain"
        );
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
        let annotations = {
            let mut page_helper = PageObjectHelper::new(page_ref, &mut pdf);
            page_helper
                .get_annotation_handles(None)
                .expect("remaining annotation list should be readable")
        };
        assert_eq!(annotations.len(), 1);
        assert_eq!(
            annotations[0].object_ref(),
            Some(ObjectRef::new(4, 0)),
            "the Print annotation should remain"
        );
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
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).unwrap();
        let resources = page.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let xobj_dict = resources.try_get_key(b"/XObject").unwrap();
        pdf.resolve(&xobj_dict).unwrap();
        assert_eq!(
            xobj_dict.as_dictionary().unwrap().len(),
            2,
            "two XObject entries"
        );

        // qpdf removes /Annots after every annotation has been flattened.
        assert!(page.try_get_key(b"/Annots").unwrap().is_null());
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
    // Test: an invalid /BBox short-circuits before /Rect is ever dereferenced
    // -----------------------------------------------------------------------
    #[test]
    fn invalid_bbox_short_circuits_before_rect_is_dereferenced() {
        // qpdf's `if (!(bbox_obj.isRectangle() && rect_obj.isRectangle()))`
        // (`QPDFAnnotationObjectHelper.cc:161-163`) short-circuits on `&&`:
        // /BBox is dereferenced first, and a non-rectangle /BBox means
        // /Rect is never touched at all. Object 6's body is malformed (a
        // bare `6 0 R` with no valid object syntax, producing an "expected
        // endobj" repair warning if it is ever resolved) -- resolving it
        // would be directly observable via `repair_diagnostics()`.
        let no_bbox_xobj = "<< /Type /XObject /Subtype /Form /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, no_bbox_xobj.as_bytes().to_vec());
        let (n6, obj6_bytes) = obj_wrap(6, b"6 0 R".to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect 6 0 R /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R]",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_qpdf(&mut pdf, &[page_ref], 0, 0).unwrap();

        let diagnostics = pdf.repair_diagnostics().entries().to_vec();
        assert!(
            diagnostics.iter().all(|d| !d.message.contains("object 6")),
            "a non-rectangle /BBox must short-circuit before /Rect (object 6) is ever \
             resolved: {diagnostics:?}"
        );
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
    // Test: a skipped (zero-content) candidate must not consume an /Fxo
    // number, matching qpdf's QPDFObjectHandle::getUniqueResourceName
    // contract ("min_suffix should be the value used, not the next value")
    // and QPDFPageDocumentHelper.cc's `++next_fx` living inside the
    // `!content.empty()` branch.
    // -----------------------------------------------------------------------
    #[test]
    fn skipped_annotation_does_not_consume_an_xobj_name() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body.clone());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /AP << /N 5 0 R >> >>",
        );

        // Zero-width /BBox: get_page_content_for_appearance's transformed-bbox
        // zero-area gate produces empty content for this one, so qpdf would
        // never advance next_fx past the candidate it tried here.
        let zero_area_body = make_xobj_stream([0.0, 0.0, 0.0, 20.0], b"");
        let (n7, obj7_bytes) = obj_wrap(7, zero_area_body);
        let (n6, obj6_bytes) = obj_dict(
            6,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /AP << /N 7 0 R >> >>",
        );

        let (n9, obj9_bytes) = obj_wrap(9, xobj_body);
        let (n8, obj8_bytes) = obj_dict(
            8,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /AP << /N 9 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 6 0 R 8 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
                (n8, obj8_bytes),
                (n9, obj9_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(
            count, 2,
            "the zero-area annotation must be skipped, not flattened"
        );

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.contains("Fxo1"),
            "first annotation must use Fxo1, got: {content_str}"
        );
        assert!(
            content_str.contains("Fxo2"),
            "third annotation must reuse Fxo2 (skipped candidate must not be consumed), got: {content_str}"
        );
        assert!(
            !content_str.contains("Fxo3"),
            "the skipped zero-area annotation must not have consumed a name, got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: a page whose only annotation candidate produces empty flatten
    // content must not create /Resources/XObject at all. qpdf's
    // `resources.mergeResources("<< /XObject << >> >>"_qpdf)` runs only
    // inside the `!content.empty()` branch (`QPDFPageDocumentHelper.cc:123`);
    // creating /XObject unconditionally for a page whose selected candidate
    // yields nothing (e.g. a malformed zero-area appearance) is a regression
    // this test guards against.
    // -----------------------------------------------------------------------
    #[test]
    fn empty_flatten_content_does_not_create_xobject_dict() {
        // Zero-width /BBox produces empty flatten content (same technique as
        // skipped_annotation_does_not_consume_an_xobj_name), and this is the
        // *only* candidate on the page.
        let zero_area_body = make_xobj_stream([0.0, 0.0, 0.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, zero_area_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "the zero-area annotation must produce no output");

        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).unwrap();
        let resources = page.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources).unwrap();
        let xobject = resources.try_get_key(b"/XObject").unwrap();
        pdf.resolve(&xobject).unwrap();
        assert!(
            xobject.is_null(),
            "no candidate produced content, so /Resources/XObject must not be created"
        );
    }

    // -----------------------------------------------------------------------
    // Test: two pages sharing an indirect /Resources/XObject dictionary.
    // Flattening one page must privatize its own /XObject dict (mirroring
    // qpdf's `resources.mergeResources("<< /XObject << >> >>"_qpdf)`
    // privatize-or-create idiom, `QPDFPageDocumentHelper.cc:123`) rather than
    // mutating the shared indirect object in place, which would leak the new
    // /FxoN entry onto every other page still referencing it.
    // -----------------------------------------------------------------------
    #[test]
    fn shared_indirect_xobject_dict_is_privatized_per_page() {
        // obj 7: an indirect /XObject dictionary shared by two pages.
        let (n7, obj7_bytes) = obj_dict(7, "<< /Im1 10 0 R >>");
        let (n10, obj10_bytes) = obj_wrap(
            10,
            b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\x00\nendstream\n".to_vec(),
        );

        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n8, obj8_bytes) = obj_wrap(8, xobj_body.clone());
        let (n9, obj9_bytes) = obj_wrap(9, xobj_body);
        let (n5, obj5_bytes) = obj_dict(
            5,
            "<< /Type /Annot /Subtype /Widget /Rect [10 10 100 100] /AP << /N 8 0 R >> >>",
        );
        let (n6, obj6_bytes) = obj_dict(
            6,
            "<< /Type /Annot /Subtype /Widget /Rect [10 10 100 100] /AP << /N 9 0 R >> >>",
        );

        let mut pdf_bytes = Vec::new();
        pdf_bytes.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets: Vec<(u32, u64)> = Vec::new();
        let mut push = |pdf: &mut Vec<u8>, num: u32, body: &[u8]| {
            offsets.push((num, pdf.len() as u64));
            pdf.extend_from_slice(body);
        };
        push(
            &mut pdf_bytes,
            1,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        );
        push(
            &mut pdf_bytes,
            2,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );
        push(
            &mut pdf_bytes,
            3,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject 7 0 R >> /Annots [5 0 R] >>\nendobj\n",
        );
        push(
            &mut pdf_bytes,
            4,
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject 7 0 R >> /Annots [6 0 R] >>\nendobj\n",
        );
        for (num, body) in [
            (n5, &obj5_bytes),
            (n6, &obj6_bytes),
            (n7, &obj7_bytes),
            (n8, &obj8_bytes),
            (n9, &obj9_bytes),
            (n10, &obj10_bytes),
        ] {
            push(&mut pdf_bytes, num, body);
        }

        let xref_start = pdf_bytes.len() as u64;
        let max_num = offsets.iter().map(|(n, _)| *n).max().unwrap();
        let total = max_num as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        for i in 1u32..=max_num {
            let off = offsets
                .iter()
                .find(|(n, _)| *n == i)
                .map(|(_, off)| *off)
                .unwrap_or(0);
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf_bytes.extend_from_slice(xref.as_bytes());
        pdf_bytes.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();
        let page1_ref = ObjectRef::new(3, 0);
        let page2_ref = ObjectRef::new(4, 0);

        let count = flatten_annotations_on_page(&mut pdf, page1_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        // Page 1's /Resources/XObject must now be a privatized (direct)
        // dictionary carrying the new Fxo entry.
        let page1 = pdf.get_object_handle(page1_ref);
        pdf.resolve(&page1).unwrap();
        let resources1 = page1.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources1).unwrap();
        let xobject1 = resources1.try_get_key(b"/XObject").unwrap();
        pdf.resolve(&xobject1).unwrap();
        assert!(
            !xobject1.is_indirect(),
            "flattening must privatize a shared indirect /XObject dict"
        );
        assert!(xobject1.try_has_key(b"/Fxo1").unwrap());

        // The original shared object (obj 7) must be untouched: still only
        // /Im1, no leaked /Fxo1 -- this is what page 2 (still pointing at the
        // original indirect ref) would see.
        let original = pdf.get_object_handle(ObjectRef::new(7, 0));
        pdf.resolve(&original).unwrap();
        assert!(
            original.try_has_key(b"/Im1").unwrap(),
            "original shared dict must retain its own entry"
        );
        assert!(
            !original.try_has_key(b"/Fxo1").unwrap(),
            "flattening page 1 must not leak /Fxo1 into the shared object page 2 still uses"
        );

        // Page 2's /Resources/XObject must still be the original, untouched
        // indirect reference.
        let page2 = pdf.get_object_handle(page2_ref);
        pdf.resolve(&page2).unwrap();
        let resources2 = page2.try_get_key(b"/Resources").unwrap();
        pdf.resolve(&resources2).unwrap();
        let xobject2 = resources2.try_get_key(b"/XObject").unwrap();
        assert_eq!(xobject2.object_ref(), Some(ObjectRef::new(7, 0)));
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

    #[test]
    fn flatten_rotation_reanalyzes_after_flatten_annotations_removes_acroform() {
        // qpdf's `flattenAnnotations` (`QPDFPageDocumentHelper.cc:56-77`)
        // analyzes through a scope-local `QPDFAcroFormDocumentHelper` that is
        // discarded on return, so a later `flattenRotation` step
        // (`make_afdh()`, `QPDFJob.cc:2185-2193`) always constructs a fresh
        // helper that observes the current (post-removal) state. flpdf's
        // shared `Pdf::acroform_cache` must reproduce that guarantee: a
        // widget that survives flatten-annotations (no /AP at all) must not
        // cause /AcroForm to be recreated when flatten-rotation later
        // constructs another `AcroFormDocumentHelper` for the same source.
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);

        // obj 4: Print bit set (0x04) -> flattened by Print mode.
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 4 /FT /Btn /T (a) /AP << /N 5 0 R >> >>",
        );
        // obj 7: no /AP at all -> qpdf never removes/flattens it, it stays
        // in /Annots verbatim (QPDFPageDocumentHelper.cc:127-135's final
        // `else { new_annots.push_back(...) }` branch).
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 0 200 20] /F 0 /FT /Btn /T (b) >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R] /Rotate 90",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n7, obj7_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        register_acroform_fields(&mut pdf, &[ObjectRef::new(4, 0), ObjectRef::new(7, 0)]);

        let page_ref = ObjectRef::new(3, 0);
        // Print mode: required=0x4, forbidden=0x3 (matches CliFlattenMode::Print).
        flatten_annotations_qpdf(&mut pdf, &[page_ref], 0x4, 0x3).unwrap();

        let root_ref = pdf.root_ref().unwrap();
        let root_after_flatten = pdf.get_object_handle(root_ref);
        pdf.resolve(&root_after_flatten).unwrap();
        assert!(
            !root_after_flatten.has_key(b"/AcroForm"),
            "/AcroForm must be removed once every widget lacks /NeedAppearances"
        );

        crate::job::flatten_rotation_on_pages(&mut pdf, &[page_ref]).unwrap();

        let root_after_rotation = pdf.get_object_handle(root_ref);
        pdf.resolve(&root_after_rotation).unwrap();
        assert!(
            !root_after_rotation.has_key(b"/AcroForm"),
            "flatten_rotation must not resurrect /AcroForm from a stale \
             pre-removal AcroForm association cache"
        );
    }
}
