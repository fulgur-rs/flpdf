# Outline Action and Destination qpdf Parity Design

**Issue:** `flpdf-nm2o`

**Oracle:** qpdf 11.9.0, especially:

- `/tmp/qpdf-1190/libqpdf/QPDFOutlineObjectHelper.cc`
- `/tmp/qpdf-1190/libqpdf/QPDFOutlineDocumentHelper.cc`
- `/tmp/qpdf-1190/include/qpdf/QPDFOutlineObjectHelper.hh`
- `/tmp/qpdf-1190/include/qpdf/QPDFOutlineDocumentHelper.hh`

## Goal

Make the action and destination surface of `OutlineDocumentHelper` reproduce
qpdf 11.9.0. Remove flpdf's qpdf-incompatible typed action and action-chain APIs
immediately, and expose the same raw destination object that qpdf's
`QPDFOutlineObjectHelper::getDest()` returns.

This is an intentional breaking change in flpdf 0.2.x. No deprecation period is
required.

## Scope

This change covers only outline actions and destinations:

- the public representation of an outline destination;
- `/Dest` versus `/A` precedence;
- recognition of `/A /GoTo /D`;
- legacy and name-tree named-destination lookup;
- the destination-page convenience accessor;
- removal of typed action and action-chain APIs and their tests.

The existing eager `OutlineNode` materialization, outline-tree traversal,
writer/remapper behavior, and raw PDF object round-trip remain unchanged.

The following separately tracked qpdf parity gaps are outside this scope:

- `flpdf-7nu4`: add qpdf-compatible `get_outlines_for_page`;
- `flpdf-x5yi`: accept direct outline dictionaries;
- `flpdf-3g9k`: match qpdf's depth-50 silent truncation;
- `flpdf-guru`: match qpdf title decoding and count clamping.

## Public API

`OutlineNode` remains an owned snapshot. Its destination becomes a raw PDF
object:

```rust
pub struct OutlineNode {
    // Existing non-action fields remain unchanged.
    pub dest: Object,
}

impl OutlineNode {
    pub fn dest_page(&self) -> Object;
}
```

`dest` is qpdf `getDest()`'s result. It may be an array, dictionary, integer,
string, name, or `Object::Null`; flpdf must not force it into an explicit
destination array. `dest_page()` mirrors qpdf `getDestPage()`: it clones and
returns array element zero when `dest` is a non-empty array, and returns
`Object::Null` otherwise. An indirect page reference inside the array stays an
`Object::Reference`.

Delete these public APIs immediately:

- `OutlineNode::action`;
- `OutlineAction`;
- `OutlineDocumentHelper::action_chain`;
- `OutlineDocumentHelper::action_chain_with_max_depth`;
- `DEFAULT_MAX_ACTION_CHAIN_DEPTH`;
- their `lib.rs` re-exports.

Keep the public `Dest` type and `Dest::page`. They remain the normalized return
type of the flpdf-specific `legacy_dests()` and `name_tree_dests()` enumeration
APIs and their diagnostic helpers. `Dest` is no longer the type of
`OutlineNode::dest`, but removing it would cause an unrelated breaking change
outside qpdf's outline-object action/destination surface.

Delete the private typed-action parser and action-chain walker when they have no
remaining callers. Raw `/A` dictionaries and `/Next` values remain ordinary PDF
objects and continue to round-trip through the reader and writer.

## Destination Resolution

Build each node's `dest` in this order, matching
`QPDFOutlineObjectHelper::getDest()`:

1. If the outline item has a `/Dest` key, select its value. Key presence, not
   successful normalization, controls this branch. Never fall back to `/A` when
   `/Dest` exists.
2. Otherwise resolve `/A` enough to inspect its type. Select `/D` only when
   `/A` is a dictionary, `/S` resolves to the exact name `/GoTo`, and `/D` is
   present.
3. If neither branch yields a candidate, return `Object::Null`.
4. Resolve an indirect candidate holder to its concrete object so the detached
   `OutlineNode` preserves qpdf's observable destination type. Do not resolve
   references nested inside an array or dictionary.
5. If the candidate is a Name, look it up only in the catalog's legacy
   `/Dests` dictionary.
6. If the candidate is a String, decode it with qpdf `getUTF8Value()` semantics
   and look it up only in the `/Names /Dests` name tree.
7. If a named lookup succeeds, return its value in its original object shape,
   resolving only an indirect holder needed to materialize the detached node.
   Do not unwrap a dictionary's `/D` or recursively follow aliases.
8. If a named lookup fails, return `Object::Null`.
9. For every other candidate type, return the object unchanged.

The existing `Result` boundary remains. I/O and parse errors encountered while
resolving objects continue to propagate to the caller.

An `/A` array is not an action dictionary and therefore produces
`Object::Null`. Action `/Next` is never interpreted by this helper.

## Internal Boundaries

Keep action selection separate from named-destination lookup:

- `resolve_node_dest` owns `/Dest` presence and `/A /GoTo /D` selection;
- a small candidate materializer resolves the selected holder and dispatches
  Name versus String versus other object types;
- legacy lookup handles only Name candidates;
- name-tree lookup handles only decoded String candidates.

Do not reuse the existing recursive `dest_from_value` behavior unchanged. It
normalizes destination dictionaries, recursively follows aliases, and searches
both named-destination stores, all of which differ from qpdf 11.9.0. Keep that
normalizer only for the flpdf-specific `legacy_dests()` and
`name_tree_dests()` APIs that still return `Option<Dest>`.

## Test Design

Replace typed-action and action-chain assertions with a qpdf destination matrix
in `crates/flpdf/tests/outline_document_helper_tests.rs`:

- root `/A [10 0 R]` returns `Object::Null`;
- `/Dest 42` returns the integer and suppresses a valid `/A` fallback;
- an unresolved named `/Dest` returns `Object::Null` and suppresses `/A`;
- `/A << /S /GoTo /D [...] >>` returns the destination;
- non-name `/S`, non-GoTo `/S`, missing `/D`, and non-dictionary `/A` return
  `Object::Null`;
- Name candidates consult only legacy `/Dests`;
- String candidates consult only `/Names /Dests`;
- String keys exercise PDFDocEncoding or UTF-16 decoding rather than only ASCII;
- a named destination whose value is a dictionary stays a dictionary;
- indirect candidate and lookup-result holders materialize to qpdf's observable
  object type;
- `dest_page()` returns only the first array element and returns null for an
  empty array or non-array;
- raw action dictionaries and `/Next` values survive a write/read round-trip.

Use the installed qpdf 11.9.0 command as the live oracle for representative
fixtures:

```sh
qpdf --json --json-key=outlines input.pdf
```

Normal Rust tests must not require qpdf. Encode oracle-confirmed results as
deterministic expectations in source-near synthetic fixtures.

Remove tests whose only subject is `OutlineAction`, subtype classification,
action-chain depth, action-chain cycles, or repeated `/Next` array entries.
Preserve or rewrite any test that also proves raw object round-trip or
destination behavior.

## Verification

Run, in order:

```sh
cargo fmt --all -- --check
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf
cargo test
```

After the implementation is committed, run:

```sh
scripts/patch-coverage.sh --base main
```

Changed executable lines in `crates/flpdf` must have 100% patch coverage, as
required by CI. If the first coverage run reports uncovered changed lines, add
focused tests for those branches before completion. Use `cov:ignore` only for a
genuinely unreachable or tool-artifact line and document the reason next to it.

## Non-Goals

- Do not redesign `OutlineNode` into a borrowing or lazy qpdf-style object
  handle.
- Do not change outline sibling/child traversal, cycle handling, or depth caps.
- Do not add `get_outlines_for_page` in this issue.
- Do not change `legacy_dests()` or `name_tree_dests()` return types or their
  normalized `Dest` behavior.
- Do not change page extraction's separate action neutralization logic.
- Do not alter writer or destination-remapping behavior except where tests must
  access raw objects after round-trip.
