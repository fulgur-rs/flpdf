//! qpdf correspondence: `QPDFJob::handlePageSpecs` page-selection boundary.
//!
//! This module owns the part of qpdf's page operation that sits above the
//! page-document helpers: it resolves each page specification against its
//! source document, applies qpdf's spec-level collate order, and delegates
//! object copying to the canonical multi-document merge primitive. The source
//! documents stay alive for the whole operation, matching qpdf's page heap.

use crate::page_label_document_helper::LabelRange;
use crate::page_merge::{merge_documents, MergeInput};
use crate::page_plan::PagePlan;
use crate::{Error, PageRange, Pdf, Result};
use std::io::{Cursor, Read, Seek};

/// One parsed `--pages` specification, referring to a source in the job's
/// source-document array.
///
/// `source_index == 0` is the primary input. The primary may have no
/// corresponding page specification; qpdf still uses it as the catalog and
/// document-level base for the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpecInput {
    /// Index of the source document in [`QPDFJob::handle_page_specs`].
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
/// [`merge_documents`] and its page helpers. The job layer owns:
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
    let mut merged = merge_documents(&mut merge_inputs)?;
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
        crate::page_tree_rebuild::rebuild_page_tree(&mut merged, &final_refs)?;
    }

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
