# NNTree Raw Route Cleanup Design

## Goal

Remove the remaining generic raw `Object` fixture/projection route from
`nntree.rs` while preserving the qpdf 11.9.0 live `ObjectHandle` implementation
of NameTree and NumberTree traversal, lookup, repair, split, insertion, and
removal.

This is a bounded slice of `flpdf-egzr.3.2`, the only currently actionable
dependency path blocker of `flpdf-25kg.3.1`. It does not claim completion of the
aggregate consumer-cutover issue.

## Oracle and responsibility boundary

Pinned qpdf 11.9.0 is the semantic authority:

- `NNTreeIterator` stores live `QPDFObjectHandle` key/value pairs and traverses
  live tree nodes (`libqpdf/NNTree.cc:30-70,584-663`).
- `QPDFNameTreeObjectHelper` and `QPDFNumberTreeObjectHelper` expose
  handle-valued iterators, lookup, insertion, and removal
  (`libqpdf/QPDFNameTreeObjectHelper.cc:43-170`,
  `libqpdf/QPDFNumberTreeObjectHelper.cc:44-171`).
- The helpers have no raw `Object` snapshot or path-only identity equivalent.

The live probe confirms the public behavior: `/usr/bin/qpdf` reports version
11.9.0, and
`qpdf --json --json-key=attachments --json-stream-data=none
tests/fixtures/compat/attachment-two-page.pdf` returns `attachment.txt` with
Filespec `5 0 R` and preferred contents `8 0 R`.

## Current route inventory

The module currently has two routes:

1. Canonical: public `NameTree`/`NumberTree` wrappers and their typed cursors,
   backed by live `ObjectHandle` values. Production consumers use this route.
2. Legacy/mixed: generic `NNTree::new(Object)`, `TreeKey::from_object` and
   `to_object`, `NNTreeCursor` raw/current projections,
   `materialize_cursor_value`, path-only direct-child identity, legacy root
   synchronization, and the in-module raw fixture tests. These have no qpdf
   counterpart and are not called outside `nntree.rs` test code.

The production cutover is therefore `match + mixed`: split the shared engine at
the raw fixture boundary. The raw route must be removed, not repaired or
adapted.

## Design

### Canonical engine

- Make `NNTree` own only the canonical root `ObjectHandle` and its PDF
  ownership state.
- Keep the qpdf-shaped node/cursor state needed for live handles, diagnostics,
  cycle detection, and direct-kid handling.
- Remove raw root snapshots, raw cursor fields, raw key codecs, raw insert and
  remove wrappers, and synchronization methods whose sole purpose is to keep
  an `Object` projection alive.
- Keep `ObjectHandle` array mutation and ownership checks unchanged; this slice
  must not introduce a second tree algorithm or change qpdf traversal order.

### Tests

- Add a route-contract RED test before deleting production code. It must fail on
  the current `nntree.rs` because the raw constructor/projection tokens exist.
- Retain the external canonical `NameTree`/`NumberTree` tests. The former
  in-module suite was coupled to private raw constructors and projections, so
  delete it rather than preserve a second route; the external suite and
  parser-owned consumer tests retain the live-handle behavior coverage.
- Delete tests whose only assertion is about a synthetic raw `Object` route,
  path-only identity, legacy root synchronization, or materialization. Such
  behavior is not qpdf behavior and must not receive a compatibility adapter.
- Run the focused tests after every production/test transformation so that
  traversal, warning, repair, split, allocation, and ownership behavior stays
  covered.

### Documentation

- Change the module documentation from “generic raw fixture helpers remain” to
  state that traversal and mutation are entirely handle-native.
- Update the matching `docs/qpdf-correspondence.md` row to remove the remaining
  raw-helper caveat. Do not mark the whole correspondence row as complete
  unless its existing D1-D5 evidence supports that classification.
- Record the remaining raw routes elsewhere in the aggregate Beads issue; this
  slice does not claim that `Object` has disappeared from the whole workspace.

## Non-goals

- No changes to qpdf NNTree algorithms, split thresholds, warning text, repair
  order, or public NameTree/NumberTree behavior.
- No `Object -> ObjectHandle` compatibility adapter, alias, deprecated wrapper,
  sentinel, or new special case.
- No migration of reader, writer, page, CLI, JSON, or other consumers outside
  `nntree.rs` and its direct tests.
- No closure of `flpdf-egzr.3.2` or `flpdf-25kg.3.1`.

## Verification gates

The slice is acceptable only when all of these have fresh evidence:

- route-contract RED observed before the removal and GREEN afterward;
- focused external NNTree and parser-owned consumer tests pass;
- `cargo fmt --all -- --check` passes;
- strict private rustdoc passes with the CI flags;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  passes;
- workspace tests pass;
- qpdf module-doc and deviation-marker checks pass;
- qpdf 11.9.0 live/differential NNTree probes remain unchanged; and
- fresh parent-relative patch coverage reports no uncovered changed executable
  lines.
