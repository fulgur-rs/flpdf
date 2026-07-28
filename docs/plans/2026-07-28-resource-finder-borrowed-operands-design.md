# Borrowed ResourceFinder Operands Review Fix

## Context

PR #578 has two unresolved review threads about peak memory while processing
decoded content streams.

The first asks `QpdfTokenizer` to tokenize incrementally instead of buffering
pipeline input. That change is intentionally out of scope: qpdf 11.9.0
`Pl_QPDFTokenizer::write` appends to `Pl_Buffer`, and `finish` creates the input
source and tokenizes it. Delaying token callbacks until `finish` is part of the
pipeline lifecycle that flpdf mirrors. The thread will receive a source-backed
reply explaining why the implementation remains buffered.

The second points out that `ResourceCallbacks` clones every non-inline-image
`Object` before passing it to `ResourceFinder`. `ResourceFinder` only needs to
own the bytes of a `Name`; ordinary strings, arrays, dictionaries, and streams
only set the pending-operand flag. Cloning those objects can duplicate large
payloads without changing resource classification.

## Design

Add a crate-private borrowed classification entry point to `ResourceFinder`.
It accepts `&Object`, matches the same resource-finder state machine, and
clones only a `Name` when it becomes `last_name`. Operators and ordinary
operands are inspected by reference.

Keep the existing `ParserCallbacks` implementation as the parser-facing owned
entry point. It delegates to the borrowed classifier, preserving the public
parser callback contract and standalone resource-finder behavior.

`ResourceCallbacks` will call the borrowed classifier before consuming the
same `Object` in its inline-image and XObject traversal logic. This removes the
unconditional deep clone without duplicating finder state transitions or
changing parsing, error propagation, inline-image opacity, or encounter order.

## Alternatives

1. Special-case large object variants in `ResourceCallbacks`. This avoids some
   clones but duplicates `ResourceFinder`'s pending-operand rules and is easy to
   drift.
2. Change `ParserCallbacks` to borrow every parsed object. This is a broad API
   and ownership refactor unrelated to the review finding.
3. Stream tokens from `QpdfTokenizer::write`. This diverges from qpdf 11.9.0's
   buffered pipeline lifecycle and changes when filter errors and side effects
   become observable.

## Error and Compatibility Boundaries

- Resource names retain their owned bytes and raw offsets.
- Operator classification and pending-operand state remain identical.
- Inline-image payload objects remain opaque.
- `ResourceCallbacks` keeps consuming the owned object for inline-header and
  `Do` handling.
- Structural errors and incomplete-stream behavior remain unchanged.
- No new dependency or public API is introduced.

## Test Strategy

Use TDD to add focused tests for the borrowed classifier:

- a borrowed large ordinary operand marks operands pending without consuming
  the object;
- a borrowed name remains available to the caller while the finder owns the
  name needed for a following resource operator;
- the existing `ResourceCallbacks` resource-pruning and encounter-order tests
  remain green.

Then run formatting, Clippy, the full `flpdf` test suite, the qpdf 11.9.0
differential script, and fresh changed-line coverage at 100%. After the verified
commit is pushed, reply to both original threads with the implementation or
qpdf-source evidence. Resolution is separate from replying and is not part of
this request.
