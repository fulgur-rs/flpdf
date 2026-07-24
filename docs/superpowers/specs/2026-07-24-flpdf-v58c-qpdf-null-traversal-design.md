# qpdf Null-Aware Writer Traversal Design

**Issue:** flpdf-v58c  
**Date:** 2026-07-24  
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit
`3b97c9bd266b7c32ea36d3536e22dab77412886d`)

## Goal

Make plain, non-linearized full rewrites reproduce qpdf 11.9.0's treatment of
dictionary values and array elements that resolve to PDF `null`.

The immediate target is unencrypted, non-QDF output in all three object-stream
modes:

- `--object-streams=disable`;
- `--object-streams=preserve`;
- `--object-streams=generate`.

The implementation must port qpdf's existing traversal algorithms rather than
introduce a new graph model. Disable and Preserve retain qpdf's standard
enqueue order. Generate retains qpdf's separate compressible-object DFS,
even-split grouping, and container-first numbering.

The longer-term target is one qpdf-compatible null-resolution primitive shared
by non-linearized and linearized writers. That migration is deliberately split
into dependent changes so the already byte-identical linearization paths are
not rewritten together with the flpdf-v58c behavior fix.

## qpdf 11.9.0 Model

### Dictionary visibility

`QPDF_Dictionary::getKeys()` returns keys in sorted order and omits every key
whose value's `isNull()` is true. `isNull()` dereferences indirect handles.
Consequently all of these dictionary values are absent for traversal and
serialization:

- direct `null`;
- `0 0 R`;
- a missing-xref reference;
- a free-xref reference;
- an indirect object whose real body is `null`;
- a holder chain whose terminal value is `null`.

`QPDF_Dictionary::getAsMap()` retains the raw entries, but qpdf's writer filters
null values before enqueueing or serializing them. flpdf's
`Dictionary::iter()` exposes every stored entry, so it is not equivalent to
qpdf's `getKeys()`.

### Standard enqueue and numbering

`QPDFWriter::enqueueObjectsStandard()` enqueues `/Root` and then each visible
trimmed-trailer value. `QPDFWriter::enqueueObject()` behaves as follows:

1. An indirect object is numbered on first encounter and appended to the
   object queue, or redirects to its source/generated ObjStm container.
2. A direct array recursively enqueues every element in array order.
3. A direct dictionary recursively enqueues only non-null values in key order.
4. Other direct scalar values do not enqueue anything.

The object queue is consumed in insertion order, so recursive discovery through
indirect bodies is breadth-first. An indirect null reached from an array keeps
its identity, receives an output number, and emits a `null` body. The same
handle in a dictionary value is invisible and contributes no edge.

### Generate-mode compressible-object order

`QPDF::getCompressibleObjGens()` is a distinct stack-based depth-first walk
seeded with the trailer:

1. Pop one handle.
2. For an indirect handle, apply first-visit/current-generation checks and add
   it to the candidate list unless it is a stream, signature dictionary, or
   encryption dictionary.
3. For a stream, push visible dictionary values in reverse key order, omitting
   `/Length`.
4. For a dictionary, push visible values in reverse key order.
5. For an array, push elements in reverse index order.

Reverse pushing makes the resulting visit order ascending-key and ascending
array-index order. qpdf then even-splits the candidates into ObjStm groups and
uses `enqueueObject()` for final container-first numbering. The DFS and BFS
must remain separate because they intentionally define different orders.

### Serialization

`QPDFWriter::unparseObject()` shallow-copies a dictionary and serializes only
entries whose values are not null. Arrays serialize every element. Therefore
membership, numbering, reachability garbage collection, and emitted bytes all
observe the same position-dependent rule.

## Existing flpdf Implementation

Most of the qpdf algorithms are already present:

- `CatalogFirstRenumber::build` implements the standard Catalog-first BFS used
  by Disable and Preserve.
- `GenerateRenumber::build` extends that BFS with generated ObjStm
  container-first numbering.
- `compressible_objgens` implements qpdf's stack DFS and eligibility order.
- `even_split_into_streams` implements qpdf's ObjStm grouping.
- `resurrectable_null_refs` implements an array-versus-dictionary walk for
  linearization.
- `qpdf_reference_resolves_to_null` in JSON inspection already follows holder
  chains with cycle protection.

The remaining divergence is narrow:

- `collect_refs` traverses all dictionary entries rather than qpdf-visible
  entries.
- `push_dict_children` pushes all dictionary entries.
- non-linearized reference rewriting errors on an unmapped dictionary value
  rather than omitting the key.
- `compressible_objgens` considers a live indirect `null` eligible because its
  current filter distinguishes xref liveness, not resolved nullness.
- the linearization helpers use `!live` as a null approximation and therefore
  do not cover live REAL-null objects.

This work extracts and reuses the missing semantic primitive. It does not
replace the existing BFS, DFS, grouping, or numbering algorithms.

## Design

### Shared null-resolution primitive

Introduce a crate-private writer-facing helper that answers whether a value is
qpdf-null:

```rust
fn qpdf_value_is_null<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &Object,
) -> Result<bool>
```

Its behavior mirrors `QPDFObjectHandle::isNull()`:

- `Object::Null` is true;
- a valid indirect reference is followed through its complete holder chain;
- missing, free, deleted, broken-compressed, and object-0 references resolve to
  true under the reader's qpdf-compatible resolution behavior;
- a cycle terminates as null rather than looping;
- every other scalar, array, dictionary, or stream is false.

The implementation must reuse or generalize the existing chain-aware JSON
helper rather than create a second resolution algorithm. It must not mutate the
source object graph. Parse, I/O, and encryption errors propagate through
`Result`.

### qpdf-visible dictionary entries

Add a crate-private helper that snapshots dictionary entries in lexicographic
order while applying `qpdf_value_is_null`:

```rust
fn qpdf_visible_dict_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    skip_length: bool,
) -> Result<Vec<(Vec<u8>, Object)>>
```

The owned snapshot avoids holding an immutable borrow of a cached object while
lazy resolution mutably borrows `Pdf`. It preserves key order and reference
identity. `skip_length` is used only for stream dictionaries in algorithms
where qpdf omits `/Length`.

This helper is the Rust equivalent of iterating qpdf `getKeys()` followed by
`getKey()`. It is not added to the public `Dictionary` API because visibility
depends on a `Pdf` resolver, not on the stored `Object` alone.

### Disable and Preserve

Refactor `CatalogFirstRenumber::build` and its recursive direct-object walk to
match `enqueueObject()`:

- arrays recurse over all elements;
- dictionaries and stream dictionaries recurse over qpdf-visible entries;
- stream `/Length` remains excluded when direct stream lengths are emitted;
- indirect handles retain the existing BFS queue and first-encounter numbering.

Reference rewriting receives the same resolver-aware dictionary visibility.
It rebuilds dictionaries without qpdf-null entries and rewrites all surviving
references. Arrays keep every slot. A null-resolving array reference must have
an assigned indirect null body when qpdf gives it an object identity; object 0
and direct null remain inline `null`.

Preserve retains source ObjStm membership and index order. Members made
unreachable by dictionary-key elision fall out through the existing renumber
membership filter. No Preserve-specific reachability algorithm is introduced.

### Generate

Refactor `compressible_objgens` without changing its stack discipline:

- trailer, dictionary, and stream children come from qpdf-visible entries;
- stream `/Length` remains omitted;
- arrays still push every item;
- indirect nulls reached from arrays retain their qpdf identity and eligibility;
- indirect nulls reachable only through dictionary values are never pushed.

`GenerateRenumber::build` uses the same null-aware direct-object recursion as
the standard enqueue path. Existing even splitting, member sorting, container
allocation, xref type-2 entries, and emission order remain unchanged.

The serializer omits qpdf-null dictionary entries before reference rewriting.
This prevents a dropped edge from triggering an unmapped-reference error and
prevents the target null object from consuming a number or ObjStm slot.

### Linearization follow-up

After the plain modes are byte-identical, a dependent change replaces:

- `resurrectable_null_refs`'s `!live` approximation;
- linearization writer `is_null_resolving_ref` and
  `is_null_resolving_value`;
- any closure/user traversal that intends qpdf dictionary visibility.

The follow-up reuses the shared null primitive but preserves the existing
linearization-specific page, thumbnail, object-user, section, and physical
ordering algorithms. Existing linearization goldens must remain byte-identical
unless a new qpdf 11.9.0 oracle fixture demonstrates a current REAL-null gap.

## Stack Structure

The work is split into dependent branches and PRs:

1. **Core + Disable/Preserve**
   - shared qpdf null-resolution and visible-entry helpers;
   - standard BFS and non-linearized serializer integration;
   - Disable and Preserve oracle matrix.
2. **Generate / flpdf-v58c**
   - `compressible_objgens` and `GenerateRenumber` integration;
   - ObjStm membership, split-boundary, numbering, and byte tests;
   - close flpdf-v58c.
3. **Linearization convergence**
   - replace local null approximations with the shared primitive;
   - prove existing linearized bytes unchanged or pin demonstrated qpdf gaps.

QDF, encryption, and copy-encryption are excluded from these three layers.
They receive follow-up issues/stacks after the plain paths are stable.

## Oracle Matrix

Fixtures cover this reference-kind matrix:

- direct `null`;
- `0 0 R`;
- missing xref;
- free xref;
- live indirect REAL-null;
- one-hop and multi-hop holders ending in null;
- cyclic holder chains.

Every kind is exercised in:

- a dictionary value;
- a stream dictionary value;
- an array element;
- a direct dictionary containing an array;
- an array containing a direct dictionary;
- a graph where one ref is present in both a dropped dictionary edge and a
  surviving array edge.

Expected invariants:

- a qpdf-null dictionary or stream-dictionary key is absent;
- array length and element positions never change;
- object 0 and direct null serialize inline as `null`;
- an object-number-bearing null reached by a surviving array edge retains an
  indirect null body;
- a null object reachable only from dropped dictionary edges is garbage
  collected;
- a shared null object remains when any surviving array edge reaches it.

Generate fixtures also place null cases immediately before and after the
100-member grouping boundary. This pins DFS order, even splitting, ObjStm
membership, member index, container numbering, `/Size`, and xref type-2 rows.

## Test Strategy

Follow red-green-refactor.

1. Generate each expected output with the installed qpdf 11.9.0 binary.
2. Add focused unit tests for:
   - chain-aware null classification;
   - qpdf-visible dictionary order;
   - standard BFS order;
   - compressible DFS order;
   - dictionary/array shared-reference behavior;
   - resolution errors and nesting limits.
3. Add strict byte tests for:
   - `--object-streams=disable --static-id`;
   - ObjStm-bearing input with `--object-streams=preserve --static-id`;
   - `--object-streams=generate --static-id`.
4. Run the complete existing compatibility suites after every layer.
5. Run `qpdf --check` on every new golden output.

Existing flpdf bytes are not an oracle. If an existing golden changes, compare
the new output to qpdf 11.9.0 before accepting it. Only a demonstrated
difference inside this null-aware traversal contract permits a golden update.

## Acceptance Criteria

- Plain Disable, Preserve, and Generate match qpdf 11.9.0 byte-for-byte for the
  complete oracle matrix.
- Dictionary-key elision affects reachability, numbering, ObjStm membership,
  `/Size`, and serialization consistently.
- Array resurrection behavior remains byte-identical to the already-pinned
  qpdf fixtures.
- Existing BFS, DFS, even-split, and container-first ordering tests remain
  unchanged except where a qpdf-null edge was previously traversed.
- No source `Pdf` object is mutated by visibility analysis.
- QDF and encryption behavior is unchanged.
- Every stack layer passes:
  - `cargo fmt --all -- --check`;
  - focused unit and integration tests;
  - relevant strict qpdf compatibility tests with
    `--features qpdf-zlib-compat`;
  - `cargo test`;
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
  - qpdf validation of new goldens;
  - patch coverage at 100% from the final committed `HEAD`.

## Non-Goals

- Changing qpdf version from 11.9.0.
- Replacing qpdf's two traversals with one generalized graph engine.
- Refactoring unrelated writer ordering or serialization.
- Adding QDF, encryption, or copy-encryption parity in this stack.
- Preserving flpdf output that conflicts with a demonstrated qpdf 11.9.0
  oracle.
