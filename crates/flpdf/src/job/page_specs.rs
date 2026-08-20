//! qpdf correspondence: `QPDFJob::handlePageSpecs` page-selection boundary.
//!
//! This module owns the part of qpdf's page operation that sits above the
//! page-document helpers: it resolves each page specification against its
//! source document, applies qpdf's spec-level collate order, and delegates
//! object copying to the canonical multi-document merge primitive. The source
//! documents stay alive for the whole operation, matching qpdf's page heap.

use crate::page_label_document_helper::LabelRange;
use crate::page_merge::{merge_documents_with_resource_mode, MergeInput};
use crate::page_plan::PagePlan;
use crate::resources::RemoveUnreferencedResources;
use crate::{Error, Matrix, ObjectHandle, ObjectRef, PageObjectHelper, PageRange, Pdf, Result};
use std::collections::BTreeMap;
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

/// A selected page represented by its source and its occurrence within the
/// source's grouped merge input.
type OrderedPage = (usize, usize);

/// One qpdf-style reconstructed label, retaining raw `/P` presence in
/// addition to the typed compatibility projection.
type JobLabelEntry = (i64, LabelRange, bool);

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
/// grouped `/AcroForm /Fields` array. Clear those two occurrence-sensitive
/// carriers, seed the canonical foreign copier with the first output page for
/// each source page (the `/P` back-pointer qpdf preserves for repeated pages),
/// and replay the source annotations in `ordered_pages` order. The existing
/// `PageObjectHelper::copy_annotations_from` facade owns the qpdf-shaped field
/// tree/annotation transform and collision rename route.
fn rebuild_acroform_in_final_page_order<R: Read + Seek + 'static>(
    merged: &mut Pdf<Cursor<Vec<u8>>>,
    sources: &mut [Pdf<R>],
    grouped_pages: &[Vec<usize>],
    ordered_pages: &[OrderedPage],
    final_refs: &[ObjectRef],
) -> Result<()> {
    debug_assert_eq!(ordered_pages.len(), final_refs.len());

    // Remove the grouped merge's field list while retaining primary-owned
    // /DR, /DA, /SigFlags, and other AcroForm defaults. If no source carried an
    // AcroForm, the first annotation transform will create it only when needed.
    if let Some(root_ref) = merged.root_ref() {
        let root = merged.get_object_handle(root_ref);
        let acroform =
            merged.resolve_object_handle_to_terminal(&root.try_get_key(b"/AcroForm")?)?;
        if acroform.as_dictionary().is_some() {
            acroform.replace_key(b"/Fields", ObjectHandle::array(Vec::new()))?;
            merged.mark_object_handle_dirty(&acroform)?;
        }
    }

    // The page merge's annotations are the source-group copies. They must not
    // be appended to the per-occurrence copies below.
    for &page_ref in final_refs {
        let page = merged.get_object_handle(page_ref);
        page.remove_key(b"/Annots");
        merged.mark_object_handle_dirty(&page)?;
    }

    // Resolve source page refs once, before the occurrence replay mutably
    // borrows individual source documents.
    let source_page_refs: Vec<Vec<ObjectRef>> = sources
        .iter_mut()
        .map(crate::pages::page_refs)
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
    for (source_index, mappings) in first_output_for_source_page.into_iter().enumerate() {
        if mappings.is_empty() {
            continue;
        }
        let source_id = sources[source_index].unique_id();
        let mut object_map = merged.take_foreign_object_map(source_id);
        object_map.extend(mappings);
        merged.set_foreign_object_map(source_id, object_map);
    }

    for (output_index, &(source_index, group_index)) in ordered_pages.iter().enumerate() {
        let source_page_index = grouped_pages[source_index][group_index];
        let source_page_ref = source_page_refs[source_index][source_page_index];
        let source_page = sources[source_index].get_object_handle(source_page_ref);
        let mut destination = PageObjectHelper::new(final_refs[output_index], merged);
        destination.copy_annotations_from(
            source_page,
            Matrix::default(),
            &mut sources[source_index],
        )?;
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

/// Resolve and execute qpdf's page-specification operation.
///
/// This is the Rust boundary corresponding to
/// `QPDFJob::handlePageSpecs` (`libqpdf/QPDFJob.cc:2360-2632`). The low-level
/// page tree and foreign-object responsibilities remain in
/// [`crate::page_merge::merge_documents`] and its page helpers. The job layer owns:
///
/// - per-spec range resolution and source-index validation;
/// - qpdf's round-robin `--collate` order across specifications;
/// - retaining the source documents while copied objects are materialized;
/// - rebuilding the merged page tree in the final spec order; and
/// - reconstructing `/PageLabels` in that same final order.
///
/// The caller supplies source documents in qpdf order, with the primary at
/// index zero. `sources` must remain mutable because page-tree repair and
/// inherited-attribute materialization are part of the qpdf copy operation.
///
/// `collate` is `None` for the ordinary concatenation order and `Some(n)` for
/// qpdf's `--collate=n`; zero is rejected before any document is mutated.
pub fn handle_page_specs<R: Read + Seek + 'static>(
    job: &mut super::QPDFJob,
    sources: &mut [Pdf<R>],
    specs: &[PageSpecInput],
    collate: Option<usize>,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    handle_page_specs_with_resource_mode(
        job,
        sources,
        specs,
        collate,
        RemoveUnreferencedResources::Auto,
    )
}

/// Resolve and execute qpdf's page-specification operation with an explicit
/// page-resource mode.
///
/// qpdf's default is `auto`; the mode is a job-level policy and is therefore
/// threaded into the bounded merge consumer rather than changing the generic
/// [`crate::merge_documents`] library primitive.
pub fn handle_page_specs_with_resource_mode<R: Read + Seek + 'static>(
    job: &mut super::QPDFJob,
    sources: &mut [Pdf<R>],
    specs: &[PageSpecInput],
    collate: Option<usize>,
    resource_mode: RemoveUnreferencedResources,
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
    let mut merged = merge_documents_with_resource_mode(&mut merge_inputs, resource_mode)?;
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
    /// Execute qpdf's page-specification and multi-source copy operation.
    pub fn handle_page_specs<R: Read + Seek + 'static>(
        &mut self,
        sources: &mut [Pdf<R>],
        specs: &[PageSpecInput],
        collate: Option<usize>,
    ) -> Result<Pdf<Cursor<Vec<u8>>>> {
        handle_page_specs(self, sources, specs, collate)
    }

    /// Execute qpdf's page-specification operation with an explicit resource
    /// pruning mode.
    pub fn handle_page_specs_with_resource_mode<R: Read + Seek + 'static>(
        &mut self,
        sources: &mut [Pdf<R>],
        specs: &[PageSpecInput],
        collate: Option<usize>,
        resource_mode: RemoveUnreferencedResources,
    ) -> Result<Pdf<Cursor<Vec<u8>>>> {
        handle_page_specs_with_resource_mode(self, sources, specs, collate, resource_mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::QPDFJob;
    use crate::page_label_document_helper::LabelStyle;
    use crate::Object;
    use std::io::Cursor;

    fn three_page_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/three-page.pdf").to_vec(),
        )
        .expect("open three-page fixture")
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

    fn selected_page_count(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> usize {
        crate::pages::page_refs(pdf)
            .expect("read merged page tree")
            .len()
    }

    fn pdf_without_root() -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = Pdf::empty().expect("empty PDF");
        pdf.trailer_handle().remove_key(b"/Root");
        assert!(pdf.root_ref().is_none());
        pdf
    }

    #[test]
    fn rebuild_acroform_ignores_a_missing_merged_root() {
        let mut merged = pdf_without_root();
        let mut sources: Vec<Pdf<Cursor<Vec<u8>>>> = Vec::new();
        rebuild_acroform_in_final_page_order(&mut merged, &mut sources, &[], &[], &[])
            .expect("a missing merged root has no AcroForm to rebuild");
    }

    #[test]
    fn rebuild_acroform_propagates_annotation_copy_errors() {
        let mut merged = three_page_pdf();
        let invalid_final_ref = ObjectRef::new(999, 0);
        let mut sources = vec![three_page_pdf()];
        rebuild_acroform_in_final_page_order(
            &mut merged,
            &mut sources,
            &[vec![0]],
            &[(0, 0)],
            &[invalid_final_ref],
        )
        .expect_err("an invalid destination page must escape the foreign copy route");
    }

    #[test]
    fn handle_page_specs_default_materializes_inherited_resources_like_qpdf() {
        let mut sources = vec![inherited_resources_pdf()];
        let specs = [PageSpecInput::new(0, PageRange::parse("1,1").unwrap())];

        let mut merged = handle_page_specs(&mut QPDFJob::new(), &mut sources, &specs, None)
            .expect("merge inherited-resources page");
        let page_refs = crate::pages::page_refs(&mut merged).expect("read merged page tree");
        assert_eq!(page_refs.len(), 2);
        for page_ref in page_refs {
            let page = merged
                .resolve(page_ref)
                .expect("resolve merged page")
                .into_dict()
                .expect("merged page dictionary");
            assert!(
                matches!(page.get("Resources"), Some(Object::Dictionary(_))),
                "qpdf --pages default copies inherited /Resources directly onto the page: {page:?}"
            );
        }
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
        assert!(handle_page_specs(&mut QPDFJob::new(), &mut no_sources, &spec, None).is_err());

        let mut sources = vec![three_page_pdf()];
        assert!(handle_page_specs(&mut QPDFJob::new(), &mut sources, &[], None,).is_err());

        let mut sources = vec![three_page_pdf()];
        assert!(handle_page_specs(&mut QPDFJob::new(), &mut sources, &spec, Some(0),).is_err());

        let mut sources = vec![three_page_pdf()];
        let missing = [PageSpecInput::new(1, PageRange::parse("1").unwrap())];
        assert!(handle_page_specs(&mut QPDFJob::new(), &mut sources, &missing, None,).is_err());

        let mut sources = vec![three_page_pdf()];
        let out_of_range = [PageSpecInput::new(0, PageRange::parse("99").unwrap())];
        assert!(
            handle_page_specs(&mut QPDFJob::new(), &mut sources, &out_of_range, None,).is_err()
        );

        let mut sources = vec![three_page_pdf()];
        let empty = [PageSpecInput::new(0, PageRange::empty())];
        assert!(handle_page_specs(&mut QPDFJob::new(), &mut sources, &empty, None,).is_err());
    }

    #[test]
    fn handle_page_specs_restores_cross_source_order_and_collates() {
        let mut sources = vec![three_page_pdf(), three_page_pdf()];
        let reversed = [
            PageSpecInput::new(1, PageRange::parse("1").unwrap()),
            PageSpecInput::new(0, PageRange::parse("1").unwrap()),
        ];
        let mut reversed_output =
            handle_page_specs(&mut QPDFJob::new(), &mut sources, &reversed, None).unwrap();
        assert_eq!(selected_page_count(&mut reversed_output), 2);

        let mut sources = vec![three_page_pdf(), three_page_pdf()];
        let collated = [
            PageSpecInput::new(0, PageRange::parse("1-2").unwrap()),
            PageSpecInput::new(1, PageRange::parse("1-2").unwrap()),
        ];
        let mut collated_output =
            handle_page_specs(&mut QPDFJob::new(), &mut sources, &collated, Some(1)).unwrap();
        assert_eq!(selected_page_count(&mut collated_output), 4);
    }

    #[test]
    fn handle_page_specs_reconstructs_qpdf_page_labels_with_empty_prefix() {
        let mut sources = vec![labelled_pdf()];
        let specs = [PageSpecInput::new(0, PageRange::parse("1-2").unwrap())];
        let mut output =
            handle_page_specs(&mut QPDFJob::new(), &mut sources, &specs, None).unwrap();

        let catalog_ref = output.root_ref().unwrap();
        let catalog = output
            .resolve_borrowed(catalog_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        let Object::Dictionary(page_labels) = catalog.get("PageLabels").unwrap() else {
            panic!("merged PageLabels must be a direct dictionary"); // cov:ignore: test-shape guard, the helper always installs a dictionary
        };
        let Object::Array(nums) = page_labels.get("Nums").unwrap() else {
            panic!("merged PageLabels /Nums must be an array"); // cov:ignore: test-shape guard, the helper always installs an array
        };
        let Object::Dictionary(first_label) = &nums[1] else {
            panic!("first reconstructed label must be a dictionary"); // cov:ignore: test-shape guard, the fixture yields dictionaries
        };
        assert_eq!(first_label.get("P"), Some(&Object::String(Vec::new())));
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
