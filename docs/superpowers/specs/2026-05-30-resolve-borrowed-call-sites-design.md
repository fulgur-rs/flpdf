# resolve_borrowed Call-Site Migration Design

## Goal

Migrate internal `Pdf::resolve()` call sites to `Pdf::resolve_borrowed()` wherever the caller only needs a borrowed view of the resolved object. This reduces unnecessary `Object` cloning while preserving the public `resolve()` API for external callers and for internal code that genuinely needs owned values.

The migration must be staged and reviewable because `resolve_borrowed()` ties the returned object lifetime to `&mut Pdf`, and that can interact with later calls that also need to mutate or resolve through the same reader.

## Current State

PR #246 added:

- `Pdf::resolve_borrowed(&mut self, ObjectRef) -> Result<&Object>`
- `Pdf::resolve()` implemented as `resolve_borrowed(...).clone()`
- Tests for cached borrowing, missing objects, and compressed-object edge cases

Most existing call sites still use `resolve()`. A broad grep shows call sites across `reader.rs`, `writer.rs`, `writer/object_streams.rs`, `pages.rs`, `resources.rs`, `page_split.rs`, `outline_dest_remap.rs`, `flpdf-cli/src/main.rs`, and tests.

## Non-Goals

- Do not remove or deprecate `Pdf::resolve()`.
- Do not change public API behavior or object ownership semantics visible to users.
- Do not perform unrelated writer, page, resource, or CLI refactors.
- Do not force migration where a caller needs to keep an object while resolving additional objects through the same `Pdf`.

## Classification

Each call site should be classified before editing:

- `BorrowOnly`: The resolved object is matched or inspected immediately and does not escape the local expression. Migrate to `resolve_borrowed()`.
- `OwnedNeeded`: The caller moves, stores, mutates, or returns the resolved `Object`, or needs to keep it across additional `Pdf` mutations. Keep `resolve()`.
- `MaybeLater`: The call site could benefit from borrowing but needs a small local restructuring to satisfy lifetimes. Defer until simpler call sites in the same module are done.

Prefer correctness and small diffs over maximizing the migrated count in one pass.

## Staging Plan

### Stage 1: Writer Object Streams

Target `crates/flpdf/src/writer/object_streams.rs` first. This module resolves many objects while deciding object-stream eligibility or writing object-stream members, so avoiding clones is useful. It is also more focused than the top-level writer.

Expected safe migrations include call sites that immediately test whether an object is stream-like, dictionary-like, or eligible for object-stream packing. Call sites that collect `(ObjectRef, Object)` pairs or write owned object payloads can stay on `resolve()` unless local borrowing remains simple.

### Stage 2: Top-Level Writer

Target `crates/flpdf/src/writer.rs`. Migrate read-only inspection paths first, especially matches that only branch on `Object::Dictionary` or `Object::Stream`. Keep owned resolution where the writer mutates a cloned object before emission or stores it in temporary owned collections.

### Stage 3: Library Traversal Helpers

Target `pages.rs`, `resources.rs`, `page_split.rs`, and `outline_dest_remap.rs`. These modules often traverse page trees and resource dictionaries. Migrate call sites that only inspect dictionaries or arrays in place. Keep `resolve()` where the code needs to clone or return composed objects.

### Stage 4: Reader Internals, CLI, And Tests

Target remaining obvious call sites in `reader.rs`, `flpdf-cli/src/main.rs`, and tests. CLI inspection commands can often borrow because they format values immediately. Tests can migrate where they validate borrowed behavior or avoid clones without obscuring assertions.

## Error Handling

`resolve_borrowed()` has the same error behavior as `resolve()` because `resolve()` delegates to it. Migration should not introduce new error conversions or diagnostics. Existing `?`, `map_err`, and `ok_or_else` flows should remain unchanged except for the method name and any necessary pattern matching against `&Object`.

## Borrowing Rules

When matching borrowed objects, prefer patterns that keep lifetimes narrow:

- Use `match pdf.resolve_borrowed(r)? { ... }` for immediate inspection.
- Bind references only for the smallest scope that needs them.
- Clone only the specific sub-value needed, not the whole `Object`, when an owned value must survive later `Pdf` access.
- If borrow checker changes require broader restructuring, leave the call site as `resolve()` and mark it as `MaybeLater` in the implementation notes or PR summary.

## Testing Strategy

For each stage, run focused tests that cover the touched module when available. At the end, run the workspace quality gates.

Recommended checks:

- Stage 1 and 2: `cargo test -p flpdf --test writer_tests`
- Stage 3: `cargo test -p flpdf --test reader_tests` and relevant page/resource CLI tests if touched behavior is exposed there
- Stage 4 CLI changes: `cargo test -p flpdf-cli --test cli_tests`
- Final: `cargo fmt -- --check`, `cargo test -p flpdf`, `cargo test`

## Acceptance Criteria

- All call sites under `crates/` are inventoried and classified at least informally.
- Every `BorrowOnly` call site that can be migrated without non-local restructuring is switched to `resolve_borrowed()`.
- Remaining `resolve()` call sites are explainable as `OwnedNeeded` or `MaybeLater`.
- Public API remains backward compatible.
- Focused and final tests pass.
