# Drop Family Page Membership Design

## Goal

Make the extraction drop-family for structure-tree `/Pg`, article-bead `/P`,
and OBJR annotation `/P` act only on genuine original page-tree leaves that
were removed by page selection. Preserve references to non-page objects and to
page-like objects outside the original `/Pages/Kids` tree, matching qpdf 11.9.0.

## Oracle behavior

The pinned qpdf source is the repository-managed read-only tree printed by
`scripts/fetch-qpdf-source.sh --print-path`; the behavioral oracle is
`/usr/bin/qpdf` 11.9.0.

`QPDFJob::handlePageSpecs` takes `orig_pages` from `QPDF::getAllPages` and
replaces only unselected original page objects with null
(`libqpdf/QPDFJob.cc:2469-2470,2599-2608`). `getAllPages` traverses the
original `/Pages` and `/Kids` tree (`libqpdf/QPDF_pages.cc:79-151`), so a
page-like object outside that tree is not a removed page.

The writer suppresses dictionary children whose handles resolve to null
(`libqpdf/QPDFWriter.cc:1110-1160,1478-1505` and
`libqpdf/QPDF_Dictionary.cc:59-69`), while arrays retain null elements
(`libqpdf/QPDF_Array.cc:118-145`). qpdf has no dedicated `/Pg` or bead `/P`
drop routine. The observed key removal is the composition of page-only nulling
and generic dictionary visibility.

A live two-page probe with structure elements and article beads confirmed the
boundary: references to the removed page lost `/Pg`/`/P`; references to the
surviving page were remapped; references to both a non-page object and an
orphan `/Type /Page` outside `/Pages/Kids` remained.

## Current gap

`RebuildResult::removed_pages` already stores the exact original page-tree
leaves absent from the rebuilt selection. `struct_tree_pg.rs` currently treats
every `/Pg` absent from `ref_map` as removed. `thread_bead_p.rs` and
`objr_obj_annot_p.rs` additionally require the resolved value to look like a
`/Type /Page`, which still misclassifies an orphan page-like object. These
conditions are semantic gaps, not missing primitives.

## Design

1. Thread `&result.removed_pages` through each of the three drop passes.
2. Keep `ref_map` only for the surviving-page remap branch.
3. For structure `/Pg`, drop only when the target is in `removed_pages`; leave
   unknown/non-page references unchanged.
4. For thread and OBJR `/P`, retain the existing reference-chain normalization,
   then remap surviving targets, drop targets in `removed_pages`, and leave all
   other terminal targets unchanged. Remove the `is_page_dict` gates because
   page-tree membership, not `/Type`, is qpdf's ownership fact.
5. Update module docs and hand-built test results so `removed_pages` is
   populated explicitly. Do not migrate these modules' raw Object routes; that
   remains the responsibility of `flpdf-egzr.3.2.8.15`.

## Error and mutation behavior

Malformed non-reference values retain their current no-op behavior. Existing
reference-chain resolution and cycle/depth bounds remain unchanged. Only the
membership predicate changes, so surviving remap, key removal, dirty marking,
and subsequent reachability pruning keep their existing boundaries.

## Tests

- Add RED tests for a structure `/Pg` to a non-page object and to an orphan
  `/Type /Page` object.
- Add RED tests for thread-bead and OBJR annotation `/P` to an orphan page-like
  object; retain existing non-page and removed-page tests.
- Add a qpdf-generated fixture/golden or an equivalent live-oracle assertion
  covering removed, surviving, non-page, and orphan-page references.
- Run the existing structure-tree, thread-bead, OBJR, page-extraction,
  full-rewrite, CLI, and qpdf differential gates.

## Non-goals

This slice does not remove `resolve_borrowed` or raw `Object` from the three
modules, redesign the writer's null visibility, change merge_documents' drop
family (`flpdf-ahfu`), or rename any `canonical_*`/legacy symbols.
