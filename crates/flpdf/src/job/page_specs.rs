//! qpdf correspondence: `QPDFJob::handlePageSpecs` page-selection boundary.
//!
//! This module owns the part of qpdf's page operation that sits above the
//! page-document helpers: it resolves each page specification against its
//! source document, applies qpdf's spec-level collate order, and delegates
//! object copying to the canonical multi-document merge primitive. The source
//! documents stay alive for the whole operation, matching qpdf's page heap.

use super::page_merge::{
    merge_documents_with_resource_mode, merge_documents_with_resource_mode_and_preserve_primary,
    source_top_level_field_names, MergeInput,
};
use super::page_plan::PagePlan;
use super::resource_pruning::{should_remove_unreferenced_resources, RemoveUnreferencedResources};
use crate::form_field_object_helper::FormFieldObjectHelper;
use crate::page_label_document_helper::LabelRange;
use crate::pages::tree_rebuild::RebuildResult;
use crate::{
    AcroFormDocumentHelper, Error, Matrix, ObjectHandle, ObjectRef, PageObjectHelper, PageRange,
    Pdf, Result,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek};

/// One parsed `--pages` specification, referring to a source in the job's
/// source-document array.
///
/// `source_index == 0` is the primary input. The primary may have no
/// corresponding page specification; qpdf still uses it as the catalog and
/// document-level base for the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpecInput {
    /// Index of the source document in
    /// [`QPDFJob::handle_page_specs`](super::QPDFJob::handle_page_specs).
    pub source_index: usize,
    /// qpdf page-range expression for this source occurrence.
    pub range: PageRange,
}

/// Result of qpdf's page-specification boundary.
///
/// qpdf keeps a one-source page job in its primary `QPDF` and therefore
/// preserves the primary object's live identities. Multi-source jobs use the
/// existing fresh primary-based merge target because the copied foreign graph
/// must outlive the source-page operation. The caller must keep the source
/// documents alive while using either result's returned document.
pub enum PageSpecJobOutput<'a, R: Read + Seek + 'static> {
    /// The primary document was updated in place by the single-source page
    /// job. `result` is the page-tree rebuild result used by later job stages.
    InPlace {
        /// The primary document carrying the selected page tree.
        pdf: &'a mut Pdf<R>,
        /// The qpdf-shaped page-tree rebuild mapping.
        result: RebuildResult,
        /// The effective page-resource pruning mode selected before rebuild.
        prune_mode: RemoveUnreferencedResources,
    },
    /// A fresh target produced by the multi-source foreign-copy route.
    Merged(Box<Pdf<std::io::Cursor<Vec<u8>>>>),
}

/// Select pages from one already-opened source, retaining source identities.
///
/// This is the planning and page-tree half of qpdf's single-source
/// `QPDFJob::handlePageSpecs`: range resolution remains in `PagePlan`, while
/// the source document itself is rebuilt in place so the writer can emit the
/// original object identities (`QPDFJob.cc:2514-2600`).
fn select_single_source_pages<R: Read + Seek>(
    source: &mut Pdf<R>,
    specs: &[PageSpecInput],
    collate: Option<usize>,
) -> Result<Vec<super::page_plan::SelectedPage>> {
    if specs.is_empty() {
        return Err(Error::Unsupported(
            "--pages: no page specifications were supplied".into(),
        ));
    }
    if collate == Some(0) {
        return Err(Error::Unsupported(
            "--collate chunk size must be >= 1; got 0".into(),
        ));
    }

    let mut plans = Vec::with_capacity(specs.len());
    for (spec_index, spec) in specs.iter().enumerate() {
        if spec.source_index != 0 {
            return Err(Error::Unsupported(format!(
                "--pages: single-source specification {spec_index} refers to source {}",
                spec.source_index
            )));
        }
        plans.push(PagePlan::build(source, &spec.range).map_err(|error| {
            Error::Unsupported(format!(
                "--pages: source 0 specification {spec_index}: {error}"
            ))
        })?);
    }

    let mut selected = Vec::new();
    if let Some(chunk) = collate {
        let mut cursors = vec![0usize; plans.len()];
        loop {
            let mut emitted = false;
            for (plan_index, plan) in plans.iter().enumerate() {
                let start = cursors[plan_index];
                let end = start.saturating_add(chunk).min(plan.pages().len());
                selected.extend(plan.pages()[start..end].iter().cloned());
                emitted |= end > start;
                cursors[plan_index] = end;
            }
            if !emitted {
                break;
            }
        }
    } else {
        selected.extend(plans.into_iter().flat_map(|plan| plan.pages().to_vec()));
    }

    if selected.is_empty() {
        // cov:ignore-start: PagePlan rejects empty source page selections, so
        // a non-empty validated plan always contributes at least one page.
        return Err(Error::Unsupported(
            "--pages: page selection is empty".into(),
        ));
        // cov:ignore-end
    }
    Ok(selected)
}

/// Apply qpdf's same-document duplicate-page annotation copy while adding a
/// repeated primary page (`QPDFJob.cc:2564-2585`).
pub fn copy_duplicate_page_annotations<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<()> {
    let mut first_occurrence: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    for page_refs in result.ref_map.values() {
        let Some(&first_page) = page_refs.first() else {
            continue;
        };
        for &duplicate_page in page_refs.iter().skip(1) {
            first_occurrence.insert(duplicate_page, first_page);
        }
    }
    for &new_page in &result.new_kids {
        let Some(&source_page_ref) = first_occurrence.get(&new_page) else {
            continue;
        };
        let source_page = pdf.get_object_handle(source_page_ref);
        let destination_page = pdf.get_object_handle(new_page);
        destination_page.remove_key(b"/Annots");
        pdf.mark_object_handle_dirty(&destination_page)?;
        PageObjectHelper::new(new_page, pdf).copy_annotations(source_page, Matrix::default())?;
    }
    Ok(())
}

/// Execute the qpdf-shaped in-place page-tree portion for one source.
fn handle_single_source_page_specs<R: Read + Seek>(
    job: &mut super::QPDFJob,
    source: &mut Pdf<R>,
    specs: &[PageSpecInput],
    collate: Option<usize>,
    resource_mode: RemoveUnreferencedResources,
) -> Result<(RebuildResult, RemoveUnreferencedResources)> {
    let selected = select_single_source_pages(source, specs, collate)?;
    let prune_mode = if resource_mode == RemoveUnreferencedResources::Auto
        && !should_remove_unreferenced_resources(source)?
    {
        RemoveUnreferencedResources::No
    } else {
        resource_mode
    };
    let selected_refs: Vec<_> = selected.iter().map(|page| page.page_ref).collect();
    let result = crate::pages::tree_rebuild::rebuild_page_tree(source, &selected_refs)?;
    copy_duplicate_page_annotations(source, &result)?;

    let mut labels = source.page_labels();
    if labels.has_page_labels()? {
        let src_indices: Vec<i64> = selected
            .iter()
            .map(|page| i64::from(page.index_1based) - 1)
            .collect();
        let entries = labels.labels_for_selection_with_prefix_presence(&src_indices, 0)?;
        let folded = crate::merge_adjacent_ranges_with_prefix_presence(entries);
        labels.write_reconstructed_labels_with_prefix_presence(&folded)?;
    }
    job.record_document_warnings(source);
    Ok((result, prune_mode))
}

impl PageSpecInput {
    /// Construct one source-indexed page specification.
    #[must_use]
    pub const fn new(source_index: usize, range: PageRange) -> Self {
        Self {
            source_index,
            range,
        }
    }
}

fn merge_preserving_primary<R: Read + Seek>(
    inputs: &mut [MergeInput<'_, R>],
    resource_mode: RemoveUnreferencedResources,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    merge_documents_with_resource_mode_and_preserve_primary(inputs, resource_mode, true)
}

/// A selected page represented by its source and its occurrence within the
/// source's grouped merge input.
type OrderedPage = (usize, usize);

/// One qpdf-style reconstructed label, retaining raw `/P` presence in
/// addition to the typed compatibility projection.
type JobLabelEntry = (i64, LabelRange, bool);

fn collect_primary_fields(
    merged: &mut Pdf<Cursor<Vec<u8>>>,
    pages: &[ObjectRef],
) -> Result<Vec<ObjectHandle>> {
    let mut field_refs = BTreeSet::new();
    let mut candidate_fields = Vec::new();
    {
        let mut acroform = AcroFormDocumentHelper::new_for_field_tree(merged)?;
        for &page_ref in pages {
            let widgets = acroform.get_widget_annotations_for_page(page_ref)?;
            for widget in widgets {
                let field = acroform.get_field_for_annotation_handle(widget)?;
                if let Some(field_ref) = field.object_ref() {
                    candidate_fields.push(field_ref);
                } // cov:ignore: structural brace has no LLVM executable counter
            }
        }
    }
    for field_ref in candidate_fields {
        let (top_level, _) = FormFieldObjectHelper::new(field_ref, merged).get_top_level_field()?;
        field_refs.insert(top_level);
    }
    let primary_order = {
        let mut acroform = AcroFormDocumentHelper::new_for_field_tree(merged)?;
        acroform.top_level_fields()?
    };
    Ok(primary_order
        .into_iter()
        .filter(|field_ref| field_refs.contains(field_ref))
        .map(|field_ref| merged.get_object_handle(field_ref))
        .collect())
}

/// Rebuild the merged primary's `/AcroForm /Fields` with the primary's
/// first-occurrence fields, mirroring the *gate and removal* half of qpdf's
/// end-of-selection field prune (`QPDFJob.cc:2609-2629`). qpdf only touches
/// `/AcroForm` when the *original* `/Fields` resolved to an array
/// (`had_fields_array`, the same gate in the job-owned page-merge module's
/// `PrimaryAcroForm::had_fields_array` captures); an `/AcroForm` with no
/// `/Fields` array (e.g. only `/NeedAppearances`) is left untouched. When the
/// gate passes, qpdf keeps a non-empty rebuilt array or removes `/AcroForm`
/// entirely if nothing survived -- it never leaves a stray empty `/Fields`.
///
/// This does *not* port qpdf's `referenced_fields` accumulation or its
/// end-of-loop call timing: qpdf's prune runs once, after every page's copy
/// is complete, against a `pdf` that still holds every original field
/// (including ones on pages that end up unselected). flpdf's caller runs
/// this *before* the per-occurrence copy loop and scopes `fields` to the
/// primary's first-occurrence pages only. As a result, a name still held by
/// an unselected primary field is never copied into `merged` at all, so it
/// cannot participate in the loop's collision-avoidance check -- a repeated
/// or foreign field that would collide with that name in qpdf can receive
/// the wrong `+N` suffix here.
fn replace_merged_fields(
    merged: &mut Pdf<Cursor<Vec<u8>>>,
    fields: Vec<ObjectHandle>,
    had_fields_array: bool,
) -> Result<()> {
    if !had_fields_array {
        return Ok(());
    }
    let Some(root_ref) = merged.root_ref() else {
        return Ok(());
    };
    let root = merged.get_object_handle(root_ref);
    let acroform = merged.resolve_handle(&root.try_get_key(b"/AcroForm")?)?;
    let Some(_) = acroform.as_dictionary() else {
        return Ok(());
    };
    if fields.is_empty() {
        root.remove_key(b"/AcroForm");
        merged.mark_object_handle_dirty(&root)?;
    } else {
        acroform.replace_key(b"/Fields", ObjectHandle::array(fields))?;
        merged.mark_object_handle_dirty(&acroform)?;
    }
    Ok(())
}

/// Repair grouped-copy annotation `/P` values after the final page order is
/// restored. qpdf installs this back-pointer at each final page-copy event,
/// while the structural merge initially copies pages in source-group order.
fn set_annotation_page_refs(
    merged: &mut Pdf<Cursor<Vec<u8>>>,
    page_ref: ObjectRef,
    first_output_page: ObjectRef,
) -> Result<()> {
    let annotations = PageObjectHelper::new(page_ref, merged).get_annotation_handles(None)?;
    let page = merged.get_object_handle(first_output_page);
    for annotation in annotations {
        if annotation.try_has_key(b"/P")? {
            annotation.replace_key(b"/P", page.clone())?;
            merged.mark_object_handle_dirty(&annotation)?;
        }
    }
    Ok(())
}

/// Rebuild the copied page annotations and form fields in qpdf's final page
/// order.
///
/// `QPDFJob.cc:2517-2584` invokes `fixCopiedAnnotations` for every foreign page
/// and for every repeated page from the primary. The page-group merge below
/// deliberately keeps one object-copy map per source for page/resource sharing,
/// but that is not the AcroForm boundary: `fixCopiedAnnotations` makes a fresh
/// annotation/field-tree copy for each page occurrence and then lets
/// `addAndRenameFormFields` resolve names against the fields already added.
///
/// The initial page merge has already copied page-owned annotations and built a
/// grouped `/AcroForm /Fields` array. Retain the primary's first-occurrence
/// fields and no-AcroForm foreign annotations, while replaying only the
/// occurrence-sensitive copies in `ordered_pages` order. The existing
/// `PageObjectHelper` facades own the same-document and foreign field-tree
/// transforms and collision-rename routes.
fn rebuild_acroform_in_final_page_order<R: Read + Seek + 'static>(
    merged: &mut Pdf<Cursor<Vec<u8>>>,
    sources: &mut [Pdf<R>],
    source_page_refs: &[Vec<ObjectRef>],
    grouped_pages: &[Vec<usize>],
    ordered_pages: &[OrderedPage],
    final_refs: &[ObjectRef],
) -> Result<()> {
    debug_assert_eq!(ordered_pages.len(), final_refs.len());

    // qpdf analyzes the primary AcroForm before its final unselected-page
    // prune (`QPDFJob.cc:2516-2521,2600-2629`). Keep every original primary
    // top-level name visible to each later copy event, even when that field's
    // page is not selected and its object is absent from the rebuilt `/Fields`.
    let primary_field_names: BTreeSet<Vec<u8>> = match sources.first_mut() {
        Some(primary) => source_top_level_field_names(primary)?
            .into_iter()
            .filter_map(|(_, name)| name)
            .collect(),
        None => BTreeSet::new(),
    };

    // qpdf's branch is based on document ownership. The primary's first
    // occurrence remains on the primary route; repeated primary pages use the
    // same-document copier; only a secondary source with an AcroForm enters
    // the foreign resource/field route.
    let source_has_acroform: Vec<bool> = sources
        .iter_mut()
        .map(|source| source.acroform()?.has_acro_form())
        .collect::<Result<_>>()?;

    // qpdf's foreign ObjCopier has already mapped each source page to the first
    // output page inserted for that source page. When a copied widget's `/P`
    // is encountered during a later field-tree copy, it therefore retains the
    // first occurrence's destination page, including repeated primary pages.
    let mut first_output_for_source_page: Vec<BTreeMap<ObjectRef, ObjectRef>> =
        vec![BTreeMap::new(); sources.len()];
    for (output_index, &(source_index, group_index)) in ordered_pages.iter().enumerate() {
        let source_page_index = grouped_pages[source_index][group_index];
        let source_page_ref = source_page_refs[source_index][source_page_index];
        first_output_for_source_page[source_index]
            .entry(source_page_ref)
            .or_insert(final_refs[output_index]);
    }

    // The grouped merge already copied the primary's first-occurrence field
    // trees. Keep those handles, discard secondary grouped fields, and let
    // the per-occurrence routes append only the copies qpdf would append.
    let primary_first_pages: Vec<ObjectRef> = ordered_pages
        .iter()
        .enumerate()
        .filter_map(|(output_index, &(source_index, group_index))| {
            if source_index != 0 {
                return None;
            }
            let source_page_index = grouped_pages[source_index][group_index];
            let source_page_ref = source_page_refs[source_index][source_page_index];
            (first_output_for_source_page[source_index].get(&source_page_ref)
                == Some(&final_refs[output_index]))
            .then_some(final_refs[output_index])
        })
        .collect();
    // qpdf's field prune (`QPDFJob.cc:2609-2610`, `hasAcroForm() &&
    // fields.isArray()`) only fires when the primary's *original* `/Fields`
    // was an array; an `/AcroForm` with no `/Fields` (e.g. only
    // `/NeedAppearances`) is left untouched. The primary page tree has already
    // been flattened by the merge, but its AcroForm dictionary remains live;
    // this reads the original field-array gate from that same source document.
    let had_fields_array = match sources.first_mut() {
        Some(primary) => primary.acroform()?.has_fields_array()?,
        None => false,
    };
    let primary_fields = collect_primary_fields(merged, &primary_first_pages)?;
    replace_merged_fields(merged, primary_fields, had_fields_array)?;

    for (source_index, mappings) in first_output_for_source_page.iter().enumerate() {
        if mappings.is_empty() {
            continue;
        }
        let source_id = sources[source_index].unique_id();
        let mut object_map = merged.take_foreign_object_map(source_id);
        object_map.extend(mappings.iter().map(|(&source, &target)| (source, target)));
        merged.set_foreign_object_map(source_id, object_map);
    }

    for (output_index, &(source_index, group_index)) in ordered_pages.iter().enumerate() {
        let source_page_index = grouped_pages[source_index][group_index];
        let source_page_ref = source_page_refs[source_index][source_page_index];
        let first_output_page = first_output_for_source_page[source_index]
            .get(&source_page_ref)
            .copied()
            .ok_or(Error::Missing("first output page for source page"))?;
        let is_primary_first = source_index == 0 && first_output_page == final_refs[output_index];

        if is_primary_first || (!source_has_acroform[source_index] && source_index != 0) {
            // The grouped page copy already owns these annotations. Keep them
            // for qpdf's primary-first and foreign-no-AcroForm branches, but
            // repair `/P` after the grouped→final page-order reconstruction.
            set_annotation_page_refs(merged, final_refs[output_index], first_output_page)?;
            continue;
        }

        let destination_page = merged.get_object_handle(final_refs[output_index]);
        destination_page.remove_key(b"/Annots");
        merged.mark_object_handle_dirty(&destination_page)?;

        if source_index == 0 {
            // A repeated primary page is a same-document transform: it must
            // not create or merge a foreign destination `/DR`.
            let source_page = merged.get_object_handle(first_output_page);
            PageObjectHelper::new(final_refs[output_index], merged)
                .copy_annotations_with_field_tree_only(
                    source_page,
                    Matrix::default(),
                    &primary_field_names,
                )?; // cov:ignore: valid page selections exercise this route; malformed copy errors are covered by helper tests.
        } else {
            let source_page = sources[source_index].get_object_handle(source_page_ref);
            let source = &mut sources[source_index];
            let mut destination = PageObjectHelper::new(final_refs[output_index], merged);
            destination.copy_annotations_from_with_field_tree_only(
                source_page,
                Matrix::default(),
                source,
                &primary_field_names,
            )?; // cov:ignore: valid page selections exercise this route; malformed copy errors are covered by helper tests.
        }
    }

    Ok(())
}

fn merge_job_label_ranges(ranges: Vec<JobLabelEntry>) -> Vec<JobLabelEntry> {
    let mut out: Vec<JobLabelEntry> = Vec::with_capacity(ranges.len());
    for (idx, range, prefix_present) in ranges {
        if let Some((previous_idx, previous, previous_prefix_present)) = out.last() {
            let expected_start = idx
                .checked_sub(*previous_idx)
                .and_then(|gap| previous.start.checked_add(gap));
            if let Some(expected_start) = expected_start {
                if previous.style == range.style
                    && previous.prefix == range.prefix
                    && *previous_prefix_present == prefix_present
                    && range.start == expected_start
                {
                    continue;
                }
            }
        }
        out.push((idx, range, prefix_present));
    }
    out
}

/// Crate-internal implementation of [`super::QPDFJob::handle_page_specs`];
/// see that method for the parameter contract. Kept as a free function,
/// rather than inlined into the method, so tests can drive it directly
/// without spinning up a whole `QPDFJob`.
fn handle_page_specs<R: Read + Seek + 'static>(
    job: &mut super::QPDFJob,
    sources: &mut [Pdf<R>],
    specs: &[PageSpecInput],
    collate: Option<usize>,
    resource_mode: RemoveUnreferencedResources,
    preserve_unreferenced: bool,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    if sources.is_empty() {
        return Err(Error::Unsupported(
            "--pages: a primary source document is required".into(),
        ));
    }
    if specs.is_empty() {
        return Err(Error::Unsupported(
            "--pages: no page specifications were supplied".into(),
        ));
    }
    if collate == Some(0) {
        return Err(Error::Unsupported(
            "--collate chunk size must be >= 1; got 0".into(),
        ));
    }

    // Build one PagePlan per *specification*, rather than one per source.
    // Repeated occurrences of the same file are distinct qpdf specs and must
    // remain distinct for collate ordering even though their object-copy
    // state is grouped by source below.
    let mut plans = Vec::with_capacity(specs.len());
    for (spec_index, spec) in specs.iter().enumerate() {
        let source = sources.get_mut(spec.source_index).ok_or_else(|| {
            Error::Unsupported(format!(
                "--pages: specification {spec_index} refers to missing source {}",
                spec.source_index
            ))
        })?;
        let plan = PagePlan::build(source, &spec.range).map_err(|error| {
            Error::Unsupported(format!(
                "--pages: source {} specification {spec_index}: {error}",
                spec.source_index
            ))
        })?;
        plans.push(plan);
    }

    // `grouped_pages[source]` is the order in which that source is handed to
    // merge_documents. `ordered_pages` records the corresponding final order
    // as (source, occurrence-in-group), allowing us to restore an arbitrary
    // cross-source collate order after each source has been copied once.
    let mut grouped_pages: Vec<Vec<usize>> = vec![Vec::new(); sources.len()];
    let mut ordered_pages: Vec<OrderedPage> = Vec::new();

    let mut append_page = |source_index: usize, page_index_1based: u32| {
        let page_index = page_index_1based as usize - 1;
        let group_index = grouped_pages[source_index].len();
        grouped_pages[source_index].push(page_index);
        ordered_pages.push((source_index, group_index));
    };

    if let Some(chunk) = collate {
        let mut cursors = vec![0usize; plans.len()];
        loop {
            let mut emitted = false;
            for (spec_index, plan) in plans.iter().enumerate() {
                let start = cursors[spec_index];
                let end = start.saturating_add(chunk).min(plan.pages().len());
                for page in &plan.pages()[start..end] {
                    append_page(specs[spec_index].source_index, page.index_1based);
                }
                if end > start {
                    emitted = true;
                }
                cursors[spec_index] = end;
            }
            if !emitted {
                break;
            }
        }
    } else {
        for (spec_index, plan) in plans.iter().enumerate() {
            for page in plan.pages() {
                append_page(specs[spec_index].source_index, page.index_1based);
            }
        }
    }

    if ordered_pages.is_empty() {
        return Err(Error::Unsupported(
            "--pages: page selection is empty".into(),
        ));
    }

    // qpdf's label accumulator is populated in final output order, not in
    // source-group order. Capture it before borrowing all sources for the
    // merge. When any source has labels, sources without labels still
    // contribute qpdf's default decimal label for their selected pages.
    let mut any_page_labels = false;
    for source in sources.iter_mut() {
        any_page_labels |= source.page_labels().has_page_labels()?;
    }

    let mut label_entries = Vec::new();
    if any_page_labels {
        label_entries.reserve(ordered_pages.len());
        for (output_index, &(source_index, group_index)) in ordered_pages.iter().enumerate() {
            let source_page_index = grouped_pages[source_index][group_index];
            let source = &mut sources[source_index];
            let entries = source
                .page_labels()
                .labels_for_selection(&[source_page_index as i64], output_index as i64)?;
            let prefix_present = source
                .page_labels()
                .label_prefix_is_present(source_page_index as i64)?;
            // labels_for_selection always emits one entry for one selected
            // page; keep the defensive branch for malformed number-tree
            // implementations without panicking.
            if let Some(entry) = entries.into_iter().next() {
                label_entries.push((entry.0, entry.1, prefix_present));
            }
        }
    }

    // Capture source page identities before the primary page tree is flattened
    // by the job-owned merge. The qpdf source keeps these handles alive after
    // `removePage` has emptied the primary tree, and the AcroForm occurrence
    // replay below needs the same source-space mapping.
    let source_page_refs: Vec<Vec<ObjectRef>> = sources
        .iter_mut()
        .enumerate()
        .map(|(source_index, source)| {
            if source_index != 0 && grouped_pages[source_index].is_empty() {
                // An unused secondary is intentionally not read by the merge
                // route; preserve that qpdf-shaped fast path here as well.
                Ok(Vec::new())
            } else {
                crate::pages::page_refs(source)
            }
        })
        .collect::<Result<_>>()?;

    // qpdf aggregates all occurrences of one source document into one live
    // source QPDF. That gives merge_documents one copy map per source while
    // `ordered_pages` above preserves the original spec order and repeated
    // page occurrences.
    let mut merge_inputs: Vec<MergeInput<'_, R>> = sources
        .iter_mut()
        .zip(grouped_pages.iter())
        .map(|(source, pages)| MergeInput {
            source,
            pages: pages.clone(),
        })
        .collect();
    let mut merged = if preserve_unreferenced {
        merge_preserving_primary(&mut merge_inputs, resource_mode)?
    } else {
        merge_documents_with_resource_mode(&mut merge_inputs, resource_mode)?
    };
    drop(merge_inputs);

    // merge_documents emits source-group order. Rebuild the target tree with
    // the exact page-spec order. The copied page refs are already distinct for
    // duplicate occurrences, so this rebuild does not create a second copy.
    let grouped_refs = crate::pages::page_refs(&mut merged)?;
    // cov:ignore-start: merge_documents guarantees one copied page per selected page; this is a defensive postcondition guard.
    if grouped_refs.len() != ordered_pages.len() {
        return Err(Error::Unsupported(format!(
            "--pages: merge produced {} pages for {} selected pages",
            grouped_refs.len(),
            ordered_pages.len()
        )));
    }
    // cov:ignore-end
    let offsets: Vec<usize> = grouped_pages
        .iter()
        .scan(0usize, |offset, pages| {
            let current = *offset;
            *offset += pages.len();
            Some(current)
        })
        .collect();
    let final_refs: Vec<_> = ordered_pages
        .iter()
        .map(|&(source_index, group_index)| grouped_refs[offsets[source_index] + group_index])
        .collect();
    if final_refs != grouped_refs {
        crate::pages::tree_rebuild::rebuild_page_tree(&mut merged, &final_refs)?;
    }

    rebuild_acroform_in_final_page_order(
        &mut merged,
        sources,
        &source_page_refs,
        &grouped_pages,
        &ordered_pages,
        &final_refs,
    )?; // cov:ignore: public page selection supplies validated refs; the fallible continuation is covered by the direct helper error test

    if any_page_labels {
        let folded = merge_job_label_ranges(label_entries);
        merged
            .page_labels()
            .write_reconstructed_labels_with_prefix_presence(&folded)?;
    }

    for source in sources.iter() {
        job.record_document_warnings(source);
    }
    job.record_document_warnings(&merged);
    Ok(merged)
}

impl super::QPDFJob {
    /// Complete qpdf's page-subset cleanup after the page tree and navigation
    /// references have been updated.
    ///
    /// The page-document helper owns per-page `/Font` and `/XObject` pruning;
    /// the writer reachability module owns xref-level orphan cleanup. This
    /// method is the job boundary that preserves qpdf's ordering without
    /// exposing the old mixed resource/reachability module as a public API.
    /// `Auto` must be the effective mode returned by the caller's pre-rebuild
    /// qpdf shared-resource heuristic.
    pub fn prune_after_subset<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        mode: RemoveUnreferencedResources,
    ) -> Result<()> {
        super::page_subset::prune_after_subset(pdf, mode)
    }

    /// Apply the qpdf `QPDFJob::handlePageSpecs` final AcroForm field filter
    /// for the single-document page-selection route.
    ///
    /// qpdf performs this operation inline after all selected-page copies and
    /// after unselected primary pages have been nulled
    /// (`QPDFJob.cc:2597-2632`). The multi-source path above owns its
    /// occurrence-aware copy and field filtering in the same job module; the
    /// single-source CLI path supplies its rebuilt-page result here so the
    /// operation is still owned by `job/`, not by CLI orchestration.
    ///
    /// The lower-level field-tree walk remains the one canonical
    /// `acroform_field_prune` implementation. This method only establishes
    /// the QPDFJob operation boundary and adds no alternate repair or
    /// compatibility route.
    pub fn prune_acroform_after_subset<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        result: &RebuildResult,
    ) -> Result<()> {
        super::acroform_field_prune::prune_acroform_after_subset(pdf, result)
    }

    /// Resolve and execute qpdf's page-specification operation
    /// (`QPDFJob::handlePageSpecs`, `libqpdf/QPDFJob.cc:2360-2632`). The
    /// low-level page tree and foreign-object responsibilities remain in
    /// [`crate::job::merge_documents`] and its page helpers. This method
    /// owns:
    ///
    /// - per-spec range resolution and source-index validation;
    /// - qpdf's round-robin `--collate` order across specifications;
    /// - retaining the source documents while copied objects are materialized;
    /// - rebuilding the page tree in the final spec order; and
    /// - reconstructing `/PageLabels` in that same final order.
    ///
    /// `sources` supplies source documents in qpdf order, with the primary
    /// at index zero, and must remain mutable because page-tree repair and
    /// inherited-attribute materialization are part of the qpdf copy
    /// operation. `collate` is `None` for the ordinary concatenation order
    /// and `Some(n)` for qpdf's `--collate=n`; zero is rejected before any
    /// document is mutated. `resource_mode` is qpdf's
    /// `--remove-unreferenced-resources={auto,yes,no}` job-level policy
    /// (qpdf's default is `auto`). `preserve_unreferenced` is qpdf's
    /// `--preserve-unreferenced` writer policy (`QPDFWriter.cc:2907-2913`),
    /// applied to the primary input's own otherwise-unreachable objects.
    /// Both settings live on `QPDFJob`'s member variables in qpdf
    /// (`m->remove_unreferenced_page_resources`,
    /// `m->preserve_unreferenced_objects`); qpdf has exactly one
    /// `handlePageSpecs`, so this stays the single Rust entry point rather
    /// than growing a family of same-named overloads with one more
    /// parameter each.
    pub fn handle_page_specs<'a, R: Read + Seek + 'static>(
        &mut self,
        sources: &'a mut [Pdf<R>],
        specs: &[PageSpecInput],
        collate: Option<usize>,
        resource_mode: RemoveUnreferencedResources,
        preserve_unreferenced: bool,
    ) -> Result<PageSpecJobOutput<'a, R>> {
        if sources.len() == 1 && specs.iter().all(|spec| spec.source_index == 0) {
            // cov:ignore-start: len()==1 guarantees first_mut() succeeds.
            let source = sources.first_mut().ok_or_else(|| {
                Error::Unsupported("--pages: a primary source is required".to_owned())
            })?;
            // cov:ignore-end
            let (result, prune_mode) =
                handle_single_source_page_specs(self, source, specs, collate, resource_mode)?;
            return Ok(PageSpecJobOutput::InPlace {
                pdf: source,
                result,
                prune_mode,
            });
        }

        handle_page_specs(
            self,
            sources,
            specs,
            collate,
            resource_mode,
            preserve_unreferenced,
        )
        .map(|merged| PageSpecJobOutput::Merged(Box::new(merged)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::QPDFJob;
    use crate::page_label_document_helper::LabelStyle;
    use crate::ObjectHandle;
    use std::io::Cursor;

    fn three_page_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/three-page.pdf").to_vec(),
        )
        .expect("open three-page fixture")
    }

    fn acroform_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/acroform-sig-widget.pdf").to_vec(),
        )
        .expect("open AcroForm fixture")
    }

    fn inherited_resources_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/inherited-resources-one-page.pdf")
                .to_vec(),
        )
        .expect("open inherited-resources fixture")
    }

    fn labelled_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/json-diff/direct-outlines.pdf").to_vec(),
        )
        .expect("open labelled fixture")
    }

    /// Build a minimal PDF from a flat list of contiguously-numbered object
    /// bodies (1-indexed from 1).
    fn assemble_pdf(objects: &[&[u8]]) -> Vec<u8> {
        use std::io::Write;
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(bytes.len());
            bytes.extend_from_slice(object);
        }
        let start_xref = bytes.len();
        let _ = writeln!(&mut bytes, "xref\n0 {}", objects.len() + 1);
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for &offset in &offsets {
            let _ = writeln!(&mut bytes, "{offset:010} 00000 n ");
        }
        let _ = writeln!(
            &mut bytes,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            objects.len() + 1,
            start_xref
        );
        bytes
    }

    /// Single-page PDF whose `/AcroForm` has `/NeedAppearances` but no
    /// `/Fields` key at all (qpdf's `had_fields_array` gate is false).
    fn acroform_no_fields_array_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = assemble_pdf(&[
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /AcroForm << /NeedAppearances true >> >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        ]);
        Pdf::open_mem_owned(bytes).expect("open AcroForm-no-fields-array fixture")
    }

    /// Two-page PDF: page 1 has no widgets; page 2 (left unselected by the
    /// tests below) carries the document's only field, "Orphan".
    fn acroform_all_fields_on_page_two_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = assemble_pdf(&[
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /AcroForm << /Fields [5 0 R] /NeedAppearances true >> >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [5 0 R] >>\nendobj\n",
            b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (Orphan) \
              /Rect [0 0 10 10] /P 4 0 R >>\nendobj\n",
        ]);
        Pdf::open_mem_owned(bytes).expect("open AcroForm-orphan-field fixture")
    }

    fn selected_page_count(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> usize {
        crate::pages::page_refs(pdf)
            .expect("read merged page tree")
            .len()
    }

    fn resolved_object(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef) -> ObjectHandle {
        let object = pdf.get_object_handle(object_ref);
        pdf.resolve(&object).expect("resolve object handle");
        object
    }

    fn resolved_key(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        owner: &ObjectHandle,
        key: &[u8],
    ) -> ObjectHandle {
        let value = owner.get_key(key);
        pdf.resolve(&value).expect("resolve child handle");
        value
    }

    fn pdf_without_root() -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = Pdf::empty().expect("empty PDF");
        pdf.trailer().remove_key(b"/Root");
        assert!(pdf.root_ref().is_none());
        pdf
    }

    #[test]
    fn rebuild_acroform_ignores_a_missing_merged_root() {
        let mut merged = pdf_without_root();
        let mut sources: Vec<Pdf<Cursor<Vec<u8>>>> = Vec::new();
        rebuild_acroform_in_final_page_order(&mut merged, &mut sources, &[], &[], &[], &[])
            .expect("a missing merged root has no AcroForm to rebuild");
    }

    #[test]
    fn replace_merged_fields_ignores_a_missing_merged_root_when_gated_open() {
        let mut merged = pdf_without_root();
        replace_merged_fields(&mut merged, Vec::new(), true)
            .expect("a missing merged root has no AcroForm to rebuild, even past the gate");
    }

    #[test]
    fn replace_merged_fields_removes_acroform_when_no_fields_survive() {
        let mut merged = acroform_pdf();
        replace_merged_fields(&mut merged, Vec::new(), true)
            .expect("an empty survivor list removes /AcroForm");
        let root_ref = merged.root_ref().expect("root");
        assert!(
            resolved_object(&mut merged, root_ref)
                .get_key(b"/AcroForm")
                .is_null(),
            "qpdf removes /AcroForm entirely once the filtered field count reaches zero"
        );
    }

    #[test]
    fn rebuild_acroform_propagates_annotation_copy_errors() {
        let mut merged = three_page_pdf();
        let invalid_final_ref = ObjectRef::new(999, 0);
        let mut sources = vec![three_page_pdf()];
        let source_page_refs = sources
            .iter_mut()
            .map(crate::pages::page_refs)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        rebuild_acroform_in_final_page_order(
            &mut merged,
            &mut sources,
            &source_page_refs,
            &[vec![0]],
            &[(0, 0)],
            &[invalid_final_ref],
        )
        .expect_err("an invalid destination page must escape the foreign copy route");
    }

    #[test]
    fn rebuild_acroform_uses_foreign_copy_route_for_secondary_acroform() {
        let mut merged = three_page_pdf();
        let mut sources = vec![three_page_pdf(), acroform_pdf()];
        let source_page_refs = sources
            .iter_mut()
            .map(crate::pages::page_refs)
            .collect::<Result<Vec<_>>>()
            .unwrap();

        rebuild_acroform_in_final_page_order(
            &mut merged,
            &mut sources,
            &source_page_refs,
            &[vec![], vec![0]],
            &[(1, 0)],
            &[ObjectRef::new(3, 0)],
        )
        .expect("a secondary AcroForm page must use the foreign copy route");
    }

    #[test]
    fn collect_primary_fields_ignores_direct_field_handles() {
        let mut merged = three_page_pdf();
        let page_ref = crate::pages::page_refs(&mut merged).unwrap()[0];
        let page = merged.get_object_handle(page_ref);
        let widget = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Annot".to_vec())),
            (b"Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
        ]);
        page.replace_key(b"/Annots", ObjectHandle::array(vec![widget]))
            .unwrap();
        merged.mark_object_handle_dirty(&page).unwrap();

        assert!(
            collect_primary_fields(&mut merged, &[page_ref])
                .unwrap()
                .is_empty(),
            "direct field handles have no stable top-level object identity"
        );
    }

    #[test]
    fn handle_page_specs_default_materializes_inherited_resources_like_qpdf() {
        let mut sources = vec![inherited_resources_pdf()];
        let specs = [PageSpecInput::new(0, PageRange::parse("1,1").unwrap())];

        let mut merged = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &specs,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .expect("merge inherited-resources page");
        let page_refs = crate::pages::page_refs(&mut merged).expect("read merged page tree");
        assert_eq!(page_refs.len(), 2);
        for page_ref in page_refs {
            let page = resolved_object(&mut merged, page_ref);
            assert!(
                page.get_key(b"/Resources").as_dictionary().is_some(),
                "qpdf --pages default copies inherited /Resources directly onto the page"
            );
        }
    }

    #[test]
    fn handle_page_specs_skips_an_unused_secondary_source() {
        let mut sources = vec![three_page_pdf(), pdf_without_root()];
        let specs = [PageSpecInput::new(0, PageRange::parse("1").unwrap())];

        let mut merged = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &specs,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .expect("an unused secondary source must not be read");

        assert_eq!(selected_page_count(&mut merged), 1);
    }

    #[test]
    fn qpdf_job_keeps_a_single_source_page_job_in_place() {
        let mut sources = vec![three_page_pdf()];
        let specs = [PageSpecInput::new(0, PageRange::parse("2").unwrap())];
        let mut job = QPDFJob::new();

        let output = job
            .handle_page_specs(
                &mut sources,
                &specs,
                None,
                RemoveUnreferencedResources::Auto,
                false,
            )
            .expect("single-source page job");
        assert!(matches!(&output, PageSpecJobOutput::InPlace { .. }));
        if let PageSpecJobOutput::InPlace {
            pdf,
            result,
            prune_mode: _,
        } = output
        {
            assert_eq!(result.new_kids.len(), 1);
            assert_eq!(selected_page_count(pdf), 1);
            assert!(pdf
                .repair_diagnostics()
                .entries()
                .iter()
                .all(|entry| !entry.message.contains("foreign object")));
        }
    }

    #[test]
    fn qpdf_job_in_place_page_job_copies_repeated_page_annotations() {
        let mut sources = vec![acroform_pdf()];
        let specs = [
            PageSpecInput::new(0, PageRange::parse("1").unwrap()),
            PageSpecInput::new(0, PageRange::parse("1").unwrap()),
        ];
        let mut job = QPDFJob::new();

        let output = job
            .handle_page_specs(
                &mut sources,
                &specs,
                None,
                RemoveUnreferencedResources::Auto,
                false,
            )
            .expect("repeated single-source page job");
        assert!(matches!(&output, PageSpecJobOutput::InPlace { .. }));
        if let PageSpecJobOutput::InPlace { pdf, result, .. } = output {
            assert_eq!(result.new_kids.len(), 2);
            assert_eq!(selected_page_count(pdf), 2);
        }
    }

    #[test]
    fn single_source_page_planner_reports_its_input_errors() {
        let mut source = three_page_pdf();
        assert!(select_single_source_pages(&mut source, &[], None).is_err());
        assert!(select_single_source_pages(&mut source, &[], Some(0)).is_err());
        assert!(select_single_source_pages(
            &mut source,
            &[PageSpecInput::new(1, PageRange::parse("1").unwrap())],
            None,
        )
        .is_err());
        assert!(select_single_source_pages(
            &mut source,
            &[PageSpecInput::new(0, PageRange::parse("999").unwrap())],
            None,
        )
        .is_err());
    }

    #[test]
    fn handle_page_specs_preserves_an_acroform_with_no_fields_array_across_sources() {
        let mut sources = vec![acroform_no_fields_array_pdf(), three_page_pdf()];
        let specs = [
            PageSpecInput::new(0, PageRange::parse("1").unwrap()),
            PageSpecInput::new(1, PageRange::parse("1").unwrap()),
        ];

        let mut merged = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &specs,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .expect("merge across a fields-less-AcroForm primary and a secondary source");
        let root_ref = merged.root_ref().expect("merged root");
        let root = resolved_object(&mut merged, root_ref);
        let acroform = resolved_key(&mut merged, &root, b"/AcroForm");
        assert_eq!(
            resolved_key(&mut merged, &acroform, b"/NeedAppearances").as_boolean(),
            Some(true)
        );
        assert!(
            acroform.get_key(b"/Fields").is_null(),
            "no /Fields array existed originally; the merge must not manufacture one"
        );
    }

    #[test]
    fn handle_page_specs_removes_acroform_when_the_only_field_page_is_unselected() {
        let mut sources = vec![acroform_all_fields_on_page_two_pdf(), three_page_pdf()];
        let specs = [
            // Only page 1 (no widgets); page 2's "Orphan" field is dropped.
            PageSpecInput::new(0, PageRange::parse("1").unwrap()),
            PageSpecInput::new(1, PageRange::parse("1").unwrap()),
        ];

        let mut merged = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &specs,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .expect("merge with the only AcroForm field on an unselected page");
        let root_ref = merged.root_ref().expect("merged root");
        assert!(
            resolved_object(&mut merged, root_ref)
                .get_key(b"/AcroForm")
                .is_null(),
            "qpdf removes /AcroForm entirely once its filtered field count reaches zero"
        );
    }

    #[test]
    fn qpdf_job_owns_single_document_acroform_prune_boundary() {
        let mut pdf = acroform_all_fields_on_page_two_pdf();
        let original_pages = crate::pages::page_refs(&mut pdf).expect("original page refs");
        let result = crate::pages::tree_rebuild::rebuild_page_tree(&mut pdf, &[original_pages[0]])
            .expect("rebuild the selected page");

        QPDFJob::prune_acroform_after_subset(&mut pdf, &result)
            .expect("job-owned AcroForm pruning");

        let root_ref = pdf.root_ref().expect("root");
        assert!(
            resolved_object(&mut pdf, root_ref)
                .get_key(b"/AcroForm")
                .is_null(),
            "the job boundary must remove an empty AcroForm after page selection"
        );
    }

    #[test]
    fn page_spec_input_constructor_keeps_source_and_range() {
        let range = PageRange::parse("1-2").unwrap();
        assert_eq!(PageSpecInput::new(3, range.clone()).source_index, 3);
        assert_eq!(
            PageSpecInput::new(3, range).range,
            PageRange::parse("1-2").unwrap()
        );
    }

    #[test]
    fn handle_page_specs_rejects_invalid_job_inputs_and_selections() {
        let mut no_sources: Vec<Pdf<Cursor<Vec<u8>>>> = Vec::new();
        let spec = [PageSpecInput::new(0, PageRange::parse("1").unwrap())];
        assert!(handle_page_specs(
            &mut QPDFJob::new(),
            &mut no_sources,
            &spec,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .is_err());

        let mut sources = vec![three_page_pdf()];
        assert!(handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &[],
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .is_err());

        let mut sources = vec![three_page_pdf()];
        assert!(handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &spec,
            Some(0),
            RemoveUnreferencedResources::Auto,
            false,
        )
        .is_err());

        let mut sources = vec![three_page_pdf()];
        let missing = [PageSpecInput::new(1, PageRange::parse("1").unwrap())];
        assert!(handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &missing,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .is_err());

        let mut sources = vec![three_page_pdf()];
        let out_of_range = [PageSpecInput::new(0, PageRange::parse("99").unwrap())];
        assert!(handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &out_of_range,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .is_err());

        let mut sources = vec![three_page_pdf()];
        let empty = [PageSpecInput::new(0, PageRange::empty())];
        assert!(handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &empty,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .is_err());
    }

    #[test]
    fn handle_page_specs_restores_cross_source_order_and_collates() {
        let mut sources = vec![three_page_pdf(), three_page_pdf()];
        let reversed = [
            PageSpecInput::new(1, PageRange::parse("1").unwrap()),
            PageSpecInput::new(0, PageRange::parse("1").unwrap()),
        ];
        let mut reversed_output = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &reversed,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .unwrap();
        assert_eq!(selected_page_count(&mut reversed_output), 2);

        let mut sources = vec![three_page_pdf(), three_page_pdf()];
        let collated = [
            PageSpecInput::new(0, PageRange::parse("1-2").unwrap()),
            PageSpecInput::new(1, PageRange::parse("1-2").unwrap()),
        ];
        let mut collated_output = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &collated,
            Some(1),
            RemoveUnreferencedResources::Auto,
            false,
        )
        .unwrap();
        assert_eq!(selected_page_count(&mut collated_output), 4);
    }

    #[test]
    fn handle_page_specs_reconstructs_qpdf_page_labels_with_empty_prefix() {
        let mut sources = vec![labelled_pdf()];
        let specs = [PageSpecInput::new(0, PageRange::parse("1-2").unwrap())];
        let mut output = handle_page_specs(
            &mut QPDFJob::new(),
            &mut sources,
            &specs,
            None,
            RemoveUnreferencedResources::Auto,
            false,
        )
        .unwrap();

        let catalog_ref = output.root_ref().unwrap();
        let catalog = resolved_object(&mut output, catalog_ref);
        let page_labels = resolved_key(&mut output, &catalog, b"/PageLabels");
        let nums = resolved_key(&mut output, &page_labels, b"/Nums")
            .as_array()
            .expect("merged PageLabels /Nums must be an array");
        let first_label = nums.get(1).cloned().expect("first reconstructed label");
        output
            .resolve(&first_label)
            .expect("resolve first reconstructed label");
        assert_eq!(first_label.get_key(b"/P").as_string(), Some(Vec::new()));
    }

    #[test]
    fn merge_job_label_ranges_preserves_prefix_presence_and_checked_arithmetic() {
        let decimal = |start| LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start,
        };
        let merged = merge_job_label_ranges(vec![(0, decimal(1), true), (1, decimal(2), true)]);
        assert_eq!(merged.len(), 1);

        let prefix_presence_mismatch =
            merge_job_label_ranges(vec![(0, decimal(1), true), (1, decimal(2), false)]);
        assert_eq!(prefix_presence_mismatch.len(), 2);

        let underflow =
            merge_job_label_ranges(vec![(1, decimal(1), false), (0, decimal(1), false)]);
        assert_eq!(underflow.len(), 2);
        let overflow =
            merge_job_label_ranges(vec![(0, decimal(i64::MAX), false), (1, decimal(0), false)]);
        assert_eq!(overflow.len(), 2);
    }
}
