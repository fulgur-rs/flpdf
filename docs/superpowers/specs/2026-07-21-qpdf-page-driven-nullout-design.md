# qpdf Page-Driven Null-Out Design

## Context

`flpdf-0hrl` reported that the former outline `action_chain` walker did not
consume its depth budget while descending through nested `/Next` arrays. That
API was removed by `flpdf-nm2o`, but `extract_pages` and `merge_documents`
still contain action- and destination-specific walkers.

qpdf 11.9.0 and 12.4.0 do not discover removed pages by interpreting action
graphs. For primary-input page selection, `QPDFJob` enumerates the original
page leaves and replaces every unselected page object with `null`. During
foreign-object copying, qpdf reserves a local object for a referenced page,
stops traversal at that page boundary, and leaves a non-top-level page as
`null`. The result depends on page membership, not on whether a reference was
carried by `/Dest`, `/A`, `/Next`, `/AA`, or a malformed container.

## Goals

- Make `extract_pages` and `merge_documents` choose null-out targets from the
  source page set rather than from action or destination semantics.
- Preserve references to removed pages while making their copied target object
  resolve to `null`, matching qpdf.
- Handle arbitrarily long chains of indirect array-holder objects iteratively
  through the existing page-closure BFS.
- Preserve existing object-number determinism and byte-identical gates.
- Remove the obsolete action-chain traversal and its independent depth policy
  where page-driven null-out makes them unnecessary.

## Non-goals

- Removing the existing inline-object nesting limit. It protects recursive
  descent within one direct PDF object and is separate from indirect holder
  chains, which the closure traverses iteratively.
- Changing qpdf-compatible primary document-level inheritance rules.
- Adding new action subtype parsing or validating malformed `/Next` values.
- Changing the CLI `--pages` pipeline, which already nulls
  `RebuildResult::removed_pages` directly.

## Architecture

The source page tree is the authority for page identity. For each source
document, the caller already obtains `all_pages`, derives the unique selected
page set, and computes the generic object closure required by the selected
pages and inherited document-level structures. The fix derives:

```text
copied_removed_pages = closure intersect (all_pages - selected_pages)
```

After `copy_objects` builds the complete source-to-target renumber map, every
mapped member of `copied_removed_pages` is replaced with `Object::Null`. Pages
that were never reachable are not added to the closure, receive no object
number, and therefore cannot perturb deterministic output. Pages reached
through any carrier are already closure leaves because `page_object_closure`
records a `/Type /Page` reference but does not traverse that page's contents.

This matches both qpdf cases:

- a primary unselected page is null regardless of the object that references
  it;
- a foreign page reached below the copied top-level page is a reserved null
  page-boundary object.

## Component Changes

### Shared copied-page null-out

Add a small crate-private helper near the existing shared page-copy helpers in
`page_extract.rs`. It receives `all_pages`, the selected-page set, the computed
closure, the copy map, and the target document. It nulls only source refs that
are all three of:

1. an original page-tree leaf;
2. absent from the selected set;
3. present in the closure and copy map.

`merge_documents` reuses this helper, as it already reuses inherited-attribute
and selection helpers from `page_extract`.

### `extract_pages`

Keep the generic closure and copy flow. Replace `neutralize_absent_dests` and
its action, annotation, additional-action, and bead-specific recursive helpers
with the shared copied-page null-out immediately after `copy_objects`.

The original carrier remains unchanged. A destination such as
`/D [4 0 R /Fit]` continues to contain a reference, but the copied equivalent
of page 4 resolves to `null`. This deliberately replaces the current behavior
that removes selected keys such as `/D` or `/P`.

### `merge_documents`

Replace `collect_removed_dest_targets` and
`collect_doc_level_removed_targets` with the same page-set calculation. The
generic closure already reaches references in annotations, additional actions,
outlines, name trees, and indirect action graphs while stopping at page
boundaries.

For a direct catalog `/OpenAction`, add a generic inline-root closure fold:
collect the direct object's indirect refs, then extend each referenced root
through the existing page-closure BFS. Wire the direct value onto the target
catalog by generically remapping its references through the copy map. This
removes the `/Next`-specific fold/remap recursion and preserves indirect holder
shape instead of interpreting action semantics.

Existing destination-specific code that remains necessary for named-tree or
outline construction is retained. Code used only to discover removed pages is
deleted.

## Error Handling and Bounds

Indirect reference chains are traversed by the closure queue and cycle-safe
visited set, so their length does not consume Rust call stack. Direct arrays and
dictionaries remain subject to `MAX_INLINE_DEPTH`; exceeding it returns the
existing `Error::Unsupported` rather than partially rewriting a document.

Missing or freed references continue to resolve as `Object::Null` under the
existing reader/copy contract. Page classification uses the original page-tree
leaf set, so a malformed non-page object referenced from a destination is never
mistaken for a page and nulled.

## Testing

TDD starts with two failing regressions built from tiny synthetic PDFs:

- `extract_pages`: an annotation `/A` reaches an unselected page through more
  than 64 indirect one-element array holders. Extraction must terminate, retain
  the `/Next` graph, and make the final page reference resolve to `null`.
- `merge_documents`: the primary inline `/OpenAction` uses the same holder
  chain. Merge must retain/remap the holder graph without the action-depth cap
  and null the copied unselected page.

Tests also assert that an unreferenced removed page is not allocated in the
target, protecting deterministic object numbering. Existing focused suites for
page extraction, page merge, outline/destination remapping, and CLI page
operations must remain green. Final gates are formatting, workspace clippy,
workspace tests, and the repository's byte-identical compatibility tests.

## Beads and Delivery

Implementation stays under `flpdf-0hrl`. The issue is closed only after the
focused regressions and repository quality gates pass. The branch, Git commit,
and Dolt-backed Beads state are pushed before handoff.
