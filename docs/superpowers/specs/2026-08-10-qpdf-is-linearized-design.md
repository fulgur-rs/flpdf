# qpdf `isLinearized` canonical detector

## Context

`flpdf-25kg.3.29` ports qpdf 11.9.0 `QPDF::isLinearized` from
`libqpdf/QPDF_linearization.cc:84-155`. The current `Pdf::linearized_hint_ref`
route is a compatibility bridge: it assumes object `(1, 0)`, omits `/L`, and
propagates candidate-resolution parse errors. The writer consumer was removed
by `flpdf-25kg.6.4`; `check.rs` is now the only production caller.

The qpdf source is authoritative. `QPDF::isLinearized` is only the shallow
predicate; `/N`, `/O`, `/H`, `/T`, and `/P` belong to the separate
`checkLinearization` responsibility.

## Design

Add one canonical reader-owned predicate and cut `check.rs` over to it:

1. Read the first 1024 logical bytes from the resolver source.
2. Scan from the first digit and use the shared qpdf-shaped tokenizer to find
   the first `integer integer obj <<` sequence.
3. Use the first integer as the candidate object number and resolve generation
   `0`. A missing, malformed, unresolved, or non-dictionary candidate is false.
4. Accept `/Linearized` only when it is numeric and its finite numeric floor is
   `1`.
5. If `/L` is an integer, compare it with the actual source length. If `/L` is
   absent or non-integer, ignore it.
6. Do not inspect any other linearization parameter in this predicate.

The source scan must use the resolver's live seek/read seam and existing
tokenizer. It must not introduce a new full-file buffer or build the predicate
on the legacy bounded-window helper. Candidate resolution failures are
translated to the qpdf observable false result; the existing check-layer
diagnostic boundary remains responsible for any source-operation error that is
still surfaced.

## Route cleanup

After the canonical route is wired:

- delete `Pdf::linearized_hint_ref` rather than retaining an alias;
- delete the old check wrapper and update all production/test/doc references;
- keep `linearization/check.rs`'s deep hint-table validation as the
  `checkLinearization` analogue;
- keep `linearization/show.rs`'s show-data route in its separate
  `flpdf-egzr.3.2.9` consumer-boundary slice.

No compatibility bridge or second `isLinearized` API is added.

## Verification contract

The canonical tests must cover the qpdf fixture whose parameter dictionary is
object `3`, a non-object-1 candidate, the first-1024-byte scan boundary, a
false digit candidate, non-dictionary and unresolved candidates, numeric-floor
acceptance, absent/non-integer/matching/mismatching `/L`, and the fact that
other parameters do not affect detection. The old `linearized_hint_ref` test is
rewritten rather than retained.

Run focused tests first, then formatting, all-target/all-feature clippy, the
workspace test suite, qpdf probes, and fresh changed-line patch coverage.
