# XObject Encounter-Order Preservation Design

## Context

`ResourceCallbacks` currently stores valid `Do` operands in a
`BTreeSet<Vec<u8>>`. This deduplicates repeated names before cloning them, but
also sorts the names lexicographically. `collect_from_stream` therefore recurses
into Form XObjects in name order instead of content-stream encounter order.

That reordering is observable when an earlier Form produces a structural
resolution error and a later Form returns `Ok(false)` for an incomplete decode:
the later name may sort first, causing traversal to stop before the earlier
error is propagated.

## Design

Store each distinct XObject name together with the offset of its first valid
`Do` operator:

```text
BTreeMap<name, first_operator_offset>
```

The map preserves the existing clone bound: lookup uses the borrowed name and
allocates an owned key only for the first occurrence. Before recursive
traversal, collect borrowed map entries and sort them by the stored offset.
Traverse in that order.

This uses only standard-library containers, stores one owned copy per distinct
name, and preserves the parser's first-seen order. The existing traversal
deduplication set becomes unnecessary because the map already contains one
entry per name.

## Error and Compatibility Semantics

- Structural Form resolution errors continue to propagate as `Err`.
- Incomplete Form decode or tokenisation continues to return `Ok(false)` and
  conservatively retain resources.
- If both occur, the result follows content-stream encounter order.
- Repeated `Do` operations for the same name do not trigger extra name clones or
  repeated traversal.
- qpdf resource-finder parsing and inline-image behavior are unchanged.

## Test Strategy

Add an integration regression fixture with content `/Z Do /A Do`:

- `/Z` resolves through a malformed indirect object and must produce a
  structural parse error.
- `/A` is a Form with corrupt `FlateDecode` data and would return `Ok(false)`.

The current lexicographic traversal visits `A` first and incorrectly returns
success; the corrected traversal visits `Z` first and propagates the error.
Existing repeated-name tests continue to guard deduplication.

Verification includes the focused regression, the resource-pruning suite,
`cargo test -p flpdf`, denied-warning Clippy, formatting, the qpdf 11.9.0
differential script, and fresh 100% changed-line coverage.
