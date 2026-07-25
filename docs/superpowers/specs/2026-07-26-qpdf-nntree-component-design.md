# qpdf NNTree component design

**Issue:** `flpdf-qxba.8`
**Date:** 2026-07-26
**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `libqpdf/NNTree.cc`,
`libqpdf/qpdf/NNTree.hh`, `include/qpdf/QPDFNameTreeObjectHelper.hh`,
`include/qpdf/QPDFNumberTreeObjectHelper.hh`,
`libqpdf/QPDFNameTreeObjectHelper.cc`, and
`libqpdf/QPDFNumberTreeObjectHelper.cc`
**Oracle path:** Resolve with `scripts/fetch-qpdf-source.sh --print-path`.

## Context

flpdf already has several pieces of name-tree and number-tree behavior, but
they do not form one qpdf-shaped component:

- `name_number_tree.rs` contains generic full-tree readers and rebuilding
  helpers for name and number trees.
- `name_tree_dests.rs` contains destination-specific insert and delete
  operations implemented as collect, modify, and rebuild.
- `outline_document_helper.rs` contains a second private implementation of
  targeted name-tree lookup, bidirectional-boundary logic, structural
  diagnostics, and repair.
- `embedded_files.rs`, `page_label_document_helper.rs`, and `json_inspect.rs`
  call the generic readers and builders directly.

The original Bead statement that insertion and splitting are absent is stale.
`build_name_tree`, `build_number_tree`, and the `/Names /Dests` writer already
implement rebuilding and leaf splitting. The missing work is to make qpdf's
iterator-oriented `NNTree` algorithm the single production implementation and
to expose the complete name-tree and number-tree helper surface.

This component depends only on stable reader services: resolving an object,
installing or replacing an indirect object, allocating an object number, and
recording a warning. It does not depend on completion of the whole `QPDF.cc`
correspondence.

## Goals

1. Port qpdf 11.9.0 `NNTreeImpl` and `NNTreeIterator` responsibilities:
   bidirectional traversal, targeted find, insertion, removal, limit
   maintenance, splitting, structural validation, and auto-repair.
2. Port the public name-tree and number-tree helper APIs with Rust names and
   Rust error handling.
3. Preserve qpdf's key ordering, split threshold and split order, warning
   order and wording, repair retry behavior, and mutation of malformed direct
   and indirect roots.
4. Route every current production consumer through the new component.
5. Retain the existing crate-root free functions as thin compatibility
   wrappers while removing their independent algorithms.
6. End with one production implementation of name-tree and number-tree
   traversal, lookup, mutation, splitting, and repair.

## Non-goals

- No unrelated catalog or page-label schema behavior is changed.
- No generic shared-object or interior-mutable PDF object model is introduced.
- No `unsafe` pointers are used to make a Rust cursor look like a C++
  iterator.
- No new traversal, repair, or split budgets are added beyond qpdf behavior
  and existing termination guards required for hostile cyclic input.
- No qpdf 12-only API is added.
- Existing consumer-specific catalog wiring and garbage collection stay in
  their consumer modules.

## Definition of done

`flpdf-qxba.8` is complete only when:

- every public member of qpdf 11.9.0
  `QPDFNameTreeObjectHelper.hh` and `QPDFNumberTreeObjectHelper.hh` has a Rust
  counterpart;
- `libtests/nntree.cc` behavior is represented by flpdf tests;
- iterator movement, `find`, `insert`, `insert_after`, `remove`, split, limit
  maintenance, and repair match live qpdf 11.9.0 observations;
- `name_number_tree.rs` contains compatibility forwarding only;
- `outline_document_helper.rs` contains no private NNTree algorithm;
- embedded files, page labels, JSON inspection, named destinations, and
  outline destination lookup use `nntree.rs`;
- existing qpdf-parity and byte-identical tests remain unchanged and green;
- each stacked PR has 100% changed-line coverage against its own parent;
- formatting, workspace clippy, workspace tests, and strict private-item
  rustdoc checks pass.

## Source inventory

### Current definitions

| Responsibility | Current location |
|---|---|
| Name-tree full traversal | `name_number_tree.rs::read_name_tree` |
| Number-tree full traversal | `name_number_tree.rs::read_number_tree` |
| Name-tree rebuild and split | `name_number_tree.rs::build_name_tree` |
| Number-tree rebuild and split | `name_number_tree.rs::build_number_tree` |
| Named-destination insert/delete | `name_tree_dests.rs` |
| Targeted find and kid binary search | `outline_document_helper.rs::find_name_tree_value` and helpers |
| Iterator-begin preflight | `outline_document_helper.rs::name_tree_begin_preflight` |
| Structural enumeration and repair | `outline_document_helper.rs::enumerate_name_tree_entries` and `repair_name_tree` |
| Repaired-tree split | `outline_document_helper.rs::split_repaired_name_tree_node` |

### Current consumers

| Consumer | Tree use |
|---|---|
| `embedded_files.rs` | read, rebuild, insert, delete |
| `page_label_document_helper.rs` | number-tree read and rebuild |
| `json_inspect.rs` | raw name-tree and number-tree enumeration |
| `name_tree_dests.rs` | destination catalog wiring and mutation |
| `outline_document_helper.rs` | targeted `/Names /Dests` find and auto-repair |

The final migration must cover all of these definitions and consumers.

## Architecture

### Shared engine

`crates/flpdf/src/nntree.rs` owns one internal engine parameterized by a key
codec:

```rust
trait TreeKey {
    type Public: Clone + Ord;

    const ITEMS_KEY: &'static str;

    fn from_object(object: &Object) -> Option<Self::Public>;
    fn to_object(key: &Self::Public) -> Object;
    fn compare(left: &Self::Public, right: &Self::Public) -> Ordering;
}
```

`NameKey` uses PDF string objects and compares their qpdf-normalized UTF-8 byte
values. `NumberKey` accepts integer objects and compares signed `i64` values.
The codec is the only algorithmic difference between name and number trees.

The engine stores:

- the current root object;
- `auto_repair`, defaulting to `true`;
- the split threshold, defaulting to 32;
- a path of indirect-node identities and kid indexes for the current cursor;
- the leaf item index and current key/value pair; and
- cycle state needed to terminate malformed graphs.

The engine calls `Pdf::resolve`-family APIs only at object boundaries. New
nodes are indirect objects allocated through the existing object-number
allocation path. Existing indirect nodes are updated with `Pdf::set_object`.

### Root mutation contract

qpdf can mutate a direct dictionary through shared `QPDFObjectHandle`
semantics. flpdf's `Object` is owned and cloning it does not share mutation.
The Rust tree therefore owns its current root and exposes it after every
operation:

```rust
pub fn root(&self) -> &Object;
pub fn into_root(self) -> Object;
```

Consumer adapters write a changed direct root back to the exact catalog slot
from which it came. Indirect roots remain the same reference while their
referents are updated in `Pdf`. This is an approved representation
substitution, not a behavioral deviation: the serialized object graph,
warnings, repairs, and object allocation order remain the oracle.

No drop-time writeback, raw pointer, or whole-`Pdf` interior mutability is
introduced.

### Cursor model

The internal cursor is an owned path and item position. Public typed cursors
do not hold a mutable Rust reference to `Pdf`; movement and mutation methods
receive the tree and `Pdf` explicitly. This permits qpdf's mutable cursor
operations without unsafe aliasing:

```rust
tree.next(pdf, &mut cursor)?;
tree.previous(pdf, &mut cursor)?;
tree.insert_after(pdf, &mut cursor, key, value)?;
tree.remove_at(pdf, &mut cursor)?;
```

`begin`, `end`, `last`, and `find` return typed cursors. A cursor exposes
`valid` and the current cloned key/value pair. Moving forward from `end`
selects the first item; moving backward from `end` selects the last item,
matching qpdf. Removal advances to the next item. `insert_after` advances to
the inserted item.

The Rust cursors intentionally do not implement `std::iter::Iterator`.
Standard `Iterator` is forward-only and its `next(&mut self)` signature cannot
perform the required PDF mutations or return fallible repair diagnostics.

## Public API

`QPDF` is treated as a C++ namespace prefix, so the public types are
`NameTree`, `NumberTree`, `NameTreeCursor`, and `NumberTreeCursor`.

Both tree types provide:

- `new(root, auto_repair)`;
- `new_empty(pdf, auto_repair)`;
- `root` and `into_root`;
- `begin`, `end`, and `last`;
- `find(key, return_previous_if_missing)`;
- `insert`;
- `remove`;
- `as_map`; and
- `set_split_threshold`.

Both cursor types provide:

- `valid`;
- `current`;
- forward and backward movement through the owning tree;
- `insert_after`; and
- `remove`.

`NameTree` additionally provides:

- `has_name`;
- `find_object`.

Name APIs accept UTF-8 byte slices so non-UTF-8 Rust `str` conversion is never
implicit. A `&str` convenience delegates through `as_bytes`.

`NumberTree` additionally provides:

- `min`;
- `max`;
- `has_index`;
- `find_object`;
- `find_object_at_or_below`, returning the value and signed offset.

Existing crate-root APIs remain:

- `read_name_tree`;
- `read_number_tree`;
- `build_name_tree`;
- `build_number_tree`;
- `insert_name_tree_dest`;
- `delete_name_tree_dest`;
- `DEFAULT_MAX_TREE_DEPTH`;
- `DEFAULT_MAX_NAME_TREE_DESTS_DEPTH`; and
- `LEAF_MAX`.

They delegate to `NameTree` or `NumberTree` and retain their current signatures
and consumer policy. They do not retain independent walkers or builders.

## Behavior

### Traversal and lookup

- A leaf uses `/Names` or `/Nums`; an intermediate node uses `/Kids`.
- Keys and values are alternating pairs.
- Kid selection and item lookup use qpdf's binary-search results, including
  the previous-item option.
- `/Limits` guide targeted lookup and are validated exactly where qpdf
  validates them.
- Direct kids encountered during qpdf's begin preflight are converted to
  indirect objects in the same order and produce the same warnings.
- Cyclic indirect paths terminate with the current qpdf-compatible structural
  warning or error rather than hanging.

### Mutation and splitting

- Inserting an existing key replaces its value.
- `insert_after` trusts the caller's ordering, matching qpdf's dangerous fast
  path.
- Removing an item updates ancestor limits and collapses empty structure only
  where qpdf does.
- A node splits only after exceeding the configured threshold.
- The default threshold is 32.
- Split indexes, first-half/second-half ordering, new indirect-object
  allocation order, root promotion, and ancestor splitting follow
  `NNTreeIterator::split`.
- `/Limits` are reset from the first and last reachable entry after mutation.
- The root itself omits `/Limits`, matching qpdf and the existing byte gates.

### Repair

With `auto_repair = true`, a structural failure during find:

1. records qpdf's "attempting to repair after error" warning;
2. enumerates every reachable valid pair while warning about invalid kids,
   keys, short arrays, and loops in qpdf order;
3. installs an empty root when no valid entries survive;
4. rebuilds entries through the same insertion and splitting engine;
5. updates the original direct or indirect root location through the consumer
   adapter; and
6. retries the original find once.

With `auto_repair = false`, the original structural error is returned without
mutation.

Existing `outline_document_helper_tests` are the primary regression oracle for
warning order, malformed input, direct-root updates, holder chains, and parent
split order.

## Errors and warnings

Public operations return the crate's existing `Result<T>`.

- Object resolution and allocation errors propagate unchanged.
- Structural NNTree errors use `Error::Parse` with qpdf-compatible
  "Name/Number tree node" diagnostics.
- Unsupported key types encountered by public insert/find calls are rejected
  before mutation.
- Recoverable traversal defects are recorded with `Pdf::push_warning`.
- No panic is used for malformed PDF input.
- Internal invariant failures remain debug assertions only when the invariant
  is established entirely by code in the same operation.

## Stacked delivery

### Layer 1 — shared engine

Branch: `feature/flpdf-qxba-8-1-engine`

- Add the internal key codec, tree state, cursor, traversal, lookup, mutation,
  splitting, limit maintenance, and repair to `nntree.rs`.
- Add focused internal tests ported from `libtests/nntree.cc`.
- Keep all production consumers on their existing paths.
- Do not expose a second permanent public API yet.

### Layer 2 — typed helpers and ordinary consumers

Branch: `feature/flpdf-qxba-8-2-helpers`

- Add `NameTree`, `NumberTree`, and typed cursor public APIs.
- Port the public helper tests from `libtests/nntree.cc`.
- Convert `name_number_tree.rs` to compatibility forwarding.
- Migrate `embedded_files.rs`, `page_label_document_helper.rs`,
  `json_inspect.rs`, and `name_tree_dests.rs`.
- Preserve catalog wiring, GC, and caller-specific decoding in those modules.

### Layer 3 — outline repair and final consolidation

Branch: `feature/flpdf-qxba-8-3-outline`

- Migrate targeted outline destination lookup and auto-repair.
- Delete the private lookup, binary-search, iterator-preflight, enumeration,
  repair, and split algorithms from `outline_document_helper.rs`.
- Run the malformed-tree live-oracle matrix and byte comparisons.
- Verify mechanically that no second production NNTree algorithm remains.
- Update `docs/qpdf-correspondence.md` to mark the component complete.

Each layer is a dependent PR. No layer may rely on tests introduced only in a
later layer for its changed-line coverage.

## Testing

TDD starts with a failing port of the relevant qpdf test before each behavior
is implemented.

Focused suites:

- new `crates/flpdf/tests/nntree_tests.rs`;
- existing `name_number_tree` unit tests;
- `crates/flpdf/tests/name_tree_dests_tests.rs`;
- `crates/flpdf/tests/embedded_files_tests.rs`;
- `crates/flpdf/tests/page_label_document_helper_tests.rs`;
- `crates/flpdf/tests/outline_document_helper_tests.rs`;
- JSON inspection and CLI JSON tests that consume number/name trees.

The qpdf test port covers:

- empty, begin, end, last, forward, and backward cursor movement;
- exact and previous-item lookup;
- name and number key behavior;
- min/max and at-or-below offset;
- insertion, replacement, fast `insert_after`, and removal;
- split thresholds that force leaf and internal-node splits;
- `/Limits` after mutation;
- direct kids and indirect kids;
- malformed arrays, keys, kids, limits, and cycles;
- repair enabled and disabled; and
- map materialization.

Each stacked branch runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test nntree_tests
cargo test -p flpdf --test name_tree_dests_tests
cargo test -p flpdf --test embedded_files_tests
cargo test -p flpdf --test page_label_document_helper_tests
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf
cargo test
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Patch coverage is measured from each committed branch head against that PR's
parent branch with a fresh workspace LCOV report and must be 100% for changed
`crates/flpdf/src` lines.

## Rejected approaches

### Separate name-tree and number-tree engines

This maps the public C++ wrappers directly but duplicates traversal, mutation,
split, and repair logic. It preserves the exact responsibility-smearing this
epic exists to remove.

### Keep only the existing free functions

This is a smaller refactor but leaves qpdf's cursor, targeted find, mutation,
and public helper responsibilities unported. The outline helper would still
own a second algorithm.

### Collect, modify, and rebuild for every mutation

The current destination writer uses this safely for its narrow API, but it
does not reproduce qpdf's iterator mutation, allocation order, local split,
limit propagation, or partial repair behavior. It cannot be the shared engine.

### Make `Pdf` globally interior-mutable

Wrapping the document or all objects in `Rc<RefCell<_>>` would make the cursor
API resemble qpdf's shared handles, but it is a repository-wide architectural
change far beyond this component. The explicit root writeback contract
provides the required behavior without that expansion.
