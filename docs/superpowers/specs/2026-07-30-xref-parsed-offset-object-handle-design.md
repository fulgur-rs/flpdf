# Xref and Parsed-Offset ObjectHandle Cutover Design

## Status

Approved in design review on 2026-07-30.

Parent Bead: `flpdf-egzr.3`

Roadmap:
`docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`,
Phases 1 and 2.

## Goal

Port qpdf 11.9.0's `test_xref` and `test_parsedoffset` helpers through the
production Rust API while correcting the object-model responsibility that
currently lives in `flpdf-qtest-tools`.

This is a pre-1.0 breaking cutover. API compatibility and change minimization
are subordinate to placing identity, lazy resolution, and source provenance in
the same layers that own them in qpdf:

- `Pdf` owns the input, xref table, object cache, and resolution;
- `ObjectHandle` owns object identity and exposes object operations;
- the object value owns parsed provenance;
- the parser assigns provenance while constructing the object graph; and
- qtest helpers consume the public core API and only format results.

The cutover also realizes the core `ObjectHandle` from the original flpdf
architecture instead of retaining the qtest-only `Handle` as a second object
model.

## Fixed qpdf 11.9.0 Facts

The pinned source installed by `scripts/fetch-qpdf-source.sh` is authoritative.

- `QPDFObjectHandle::getParsedOffset()` is a public exported API. Its contract
  says that a negative value means the object was created without parsing,
  file objects use file-relative offsets, and objects in streams use offsets
  relative to the stream start
  (`include/qpdf/QPDFObjectHandle.hh:415-419`).
- `QPDF::getXRefTable()` and `QPDF::getAllObjects()` are public exported APIs
  (`include/qpdf/QPDF.hh:311-315,648-651`).
- Parsed offset is stored on `QPDFValue`, starts at `-1`, and is set only while
  still negative (`libqpdf/qpdf/QPDFValue.hh:90-100,149-152`).
- Parsed null remains at `-1`; the parser constructs `QPDF_Null` without
  assigning a description or offset
  (`libqpdf/QPDFParser.cc:81-85`).
- Scalar objects receive the token start; arrays receive the `[` start; and
  dictionaries receive the `<<` start
  (`libqpdf/QPDFParser.cc:87-120,220-228,243-271,430-443`).
- A stream value receives the encoded stream-data start, while its dictionary
  remains the separately parsed dictionary handle
  (`libqpdf/QPDF.cc:1378-1399`).
- Object-stream members are parsed from a decoded buffer at
  `/First + member-offset`, so their offsets and all direct child offsets are
  relative to the decoded object-stream buffer
  (`libqpdf/QPDF.cc:1756-1828`).
- `getAllObjects()` first performs qpdf's dangling-reference preparation and
  then returns the indirect handles in object-cache order
  (`libqpdf/QPDF.cc:1285-1294`).
- `getXRefTable()` returns the table actually owned by the parsed document,
  not a writer reconstruction (`libqpdf/QPDF.cc:2370-2377`).

These are durable oracle facts. Exact Rust data structures and internal
synchronization remain implementation details to be settled by TDD as long as
they preserve the approved public behavior and ownership.

## Architecture

### `ObjectHandle`

`ObjectHandle` becomes the public object-operation surface.

It is cloneable and retains stable shared identity. Cloning a handle does not
deep-copy its value. A handle records whether it is direct or indirect; an
indirect handle retains its `ObjectRef` before, during, and after resolution.

The handle exposes qpdf-shaped operations including:

- initialized, direct, and indirect state;
- object number and generation for indirect objects;
- type code and type name;
- scalar accessors;
- array and dictionary traversal;
- stream dictionary and data access;
- parsed offset; and
- normal and resolved unparse behavior.

File I/O remains behind `Pdf` so `ObjectHandle` does not become generic over
the input reader. Once resolved, type, value, identity, and provenance
operations belong to the handle.

### Internal object value

The current public `Object` enum is replaced by a crate-private object-value
representation owned by a handle.

Arrays and dictionaries contain child `ObjectHandle` values rather than raw
recursive values. A parsed `N G R` in a nested position (an array element, a
dictionary value, or a stream dictionary value) therefore points at the
canonical indirect handle for that `ObjectRef`; it is not represented as a
separate raw `Reference` value.

This canonicalization rule does not apply to a bare `N G R` that is the
entire body of an indirect object or an object-stream member: qpdf parses
only the first integer there and emits an `expected endobj` diagnostic for
the file-object case, never producing a reference value at that position
(`top_level_no_reference`, implemented at `parser.rs:52-62`, pinned by
`parser.rs:1137-1178` — asserting a top-level bare reference integerizes
while the same reference nested in an array/dictionary/stream dictionary
does not — and by the regression tests at `reader.rs:3380-3395`, the
file-object case, and `reader.rs:4049-4070`, the object-stream-member
case). That existing behavior is unchanged by this design.

Streams retain distinct handles for:

- the stream object, whose parsed offset is the stream-data start; and
- the stream dictionary, whose parsed offset is the `<<` start.

Objects constructed through public factories are direct handles with parsed
offset `-1` until made indirect through `Pdf`.

### `Pdf`

`Pdf` owns:

- the seekable source;
- the effective source xref table;
- the trailer handle;
- the canonical indirect-handle cache;
- lazy object resolution;
- dangling-reference preparation; and
- repair diagnostics and adopted recovery state.

Opening a document registers an unresolved canonical handle for every
applicable xref/cache object without eagerly parsing each object body.

The public API includes:

- `get_object_handle(ObjectRef) -> ObjectHandle`;
- `resolve(&ObjectHandle) -> Result<()>`;
- `get_all_objects() -> Result<Vec<ObjectHandle>>`;
- `trailer() -> ObjectHandle`; and
- `get_xref_table() -> &BTreeMap<ObjectRef, XrefEntry>`.

`get_object_handle` does not force body parsing. `resolve` updates the same
shared handle instead of returning a cloned value. Value access on an
unresolved handle fails explicitly; it does not perform hidden file I/O.

`get_all_objects` performs the qpdf-equivalent dangling-reference preparation,
ensures every non-free source-xref object is represented by the cache,
resolves the required objects, and returns indirect handles in `ObjectRef`
order. Free entries (including the object-0 free-list head) are excluded:
qpdf's own object cache never contains them (`insertFreeXrefEntry` records a
free object in a separate `deleted_objects` set, never in `xref_table`), so
`getAllObjects()` never returns one, and the `test_parsedoffset` helper
contract below treats an enumerated free entry as fatal.

### Parser

The parser builds the handle graph directly and assigns parsed offsets during
node construction. It does not return a parallel metadata tree, and `Pdf` does
not reparse an object later solely to reconstruct provenance.

Parser-created indirect references request the existing canonical handle from
the document cache. Direct children are already resolved, but traversal never
implicitly descends into an indirect child.

## Parsed-Offset Contract

`ObjectHandle::get_parsed_offset()` returns a signed qpdf-compatible offset.
The no-offset sentinel is exactly `-1`, not `Option<u64>`.

The parser records:

| Object | Parsed offset |
|---|---|
| boolean, integer, real, name, string, operator | token start |
| array | `[` start |
| dictionary | `<<` start |
| parsed null | `-1` |
| stream handle | encoded stream-data start |
| stream dictionary handle | `<<` start |
| generated or replacement value | `-1` |

For an uncompressed file object, offsets are relative to qpdf's logical file
origin. For an object-stream member, offsets are relative to the beginning of
the decoded object-stream buffer and include the `/First` displacement. Direct
children use the same coordinate system as their containing parsed object.

The first nonnegative offset assigned to a value is retained. Resolution,
cache access, unparse, and writer planning do not recompute or replace it.

An absent, freed, dangling, cyclic, or otherwise unresolvable indirect object
retains its indirect identity but resolves to null with parsed offset `-1`,
subject to qpdf-compatible reader diagnostics.

## Xref Contract

`Pdf::get_xref_table()` exposes the reader's effective source table as
`BTreeMap<ObjectRef, XrefEntry>`.

The table:

- preserves `Uncompressed` and `Compressed` classifications;
- excludes free entries, including the object-0 free-list head: matching
  qpdf's own `xref_table` (`insertFreeXrefEntry` records a free object only
  in a separate `deleted_objects` set, never in `xref_table`, and
  `getXRefTable()` returns `xref_table` directly — the same fact already
  noted for `get_all_objects` above), so `test_xref`'s "free entry" output
  arm is unreachable for any object this table itself enumerates;
- preserves uncompressed byte offsets and object-stream number/index;
- reflects incremental-update precedence across classic xref tables and xref
  streams;
- reflects a recovery table if reader recovery replaced the damaged source
  table; and
- is never reconstructed from cached object values, a writer plan, or output
  bytes.

Malformed raw xref entry types fail in the xref reader. They are not converted
to a known `XrefEntry` variant or silently normalized by a helper.

## Resolution and Traversal

Array and dictionary access returns child handles. A direct child can be
inspected immediately. An indirect child retains its identity and is not
recursed into unless a consumer explicitly asks `Pdf` to resolve it.

A missing dictionary key returns the qpdf-compatible direct null handle.
Dictionary enumeration applies qpdf's visible-key semantics, including
omitting entries whose resolved value is null where the corresponding qpdf
accessor does so.

Reference chains preserve the first indirect handle identity while resolving
to the terminal value. Cycle and dangling handling remains a reader/cache
responsibility, not a helper-specific depth or error policy.

Reader, parser, cache, writer, page traversal, JSON, stream/filter, CLI, and
qtest consumers all move to the handle API. At final cutover the following are
removed:

- the public raw `Object` enum;
- `Pdf::resolve_borrowed`;
- clone-based resolution paths;
- `flpdf-qtest-tools::driver::Handle`; and
- compatibility aliases or wrappers that preserve the replaced object model.

## Helper Contracts

### `test_xref`

The Rust helper ports `qpdf/test_xref.cc:7-44`.

It accepts exactly one input path. Incorrect arity writes
`usage: test_xref INPUT.pdf` plus LF to stderr and exits 2.

On success it walks `get_xref_table()` in `ObjectRef` order and writes:

- `N/G, free entry`;
- `N/G, uncompressed, offset = D (0xH)`; or
- `N/G, compressed, stream number = S, stream index = I`.

Decimal and lowercase hexadecimal formatting match qpdf. Open, parse, xref,
write, and unknown-entry failures write the qpdf-compatible error to stderr and
exit 2. Success exits 0.

### `test_parsedoffset`

The Rust helper ports `qpdf/test_parsedoffset.cc:13-140`.

It accepts exactly one input path. Incorrect arity writes
`Usage: test_parsedoffset INPUT.pdf` plus LF to stderr and exits 2.

It:

1. obtains the effective xref table and `get_all_objects()`;
2. assigns uncompressed indirect objects to group 0 and compressed objects to
   the group named by their object-stream number;
3. records each indirect root;
4. recursively walks direct array and dictionary children only;
5. walks a stream's dictionary as a direct child;
6. sorts every group by `(parsed offset, description)`; and
7. prints the exact group headers, descriptions, and final `succeeded` line.

Descriptions include decimal and lowercase hexadecimal offsets, indirect
object number/generation or `direct`, and the qpdf type name. A free xref for
an enumerated object, an object absent from the xref table, or an unknown entry
type is stderr plus exit 2.

The helpers contain no parser, xref reconstruction, provenance sidecar, or
qtest-only object model.

## Delivery Stack and Beads Ownership

The work is one four-layer dependency stack with ownership placed in the phase
that owns each responsibility:

1. `flpdf-egzr.3.1`, a child of `flpdf-25kg.3`: ObjectHandle graph and reader
   cutover.
2. `flpdf-egzr.3.2`, a child of `flpdf-25kg.3`: consumer cutover and legacy
   Object-route removal.
3. `flpdf-egzr.3.3`, a child of `flpdf-25kg.3`: source xref and parsed-offset
   parity.
4. `flpdf-egzr.3.4`, a child of `flpdf-egzr.3`: helper binaries and production
   qtest wiring.

The first layer may contain the narrow transitional bridge required to keep
the stack buildable. The second layer removes that bridge completely; no
compatibility route survives the completed stack.

The three core layers are children of the Phase 2 epic and block the
`flpdf-25kg.3.1` `QPDFObjectHandle` consumer audit. The Phase 1 helper layer
depends on the final core layer. This lets the helper consume the correct
production API without moving object-model ownership into
`flpdf-qtest-tools`; no dependency points back from a core layer to the helper,
so the cross-phase stack contains no cycle.

Each stack layer has independent RED/GREEN evidence and fresh 100% changed
executable-line coverage measured against its actual parent branch.

## Test Strategy

### ObjectHandle and resolution

- clone identity is stable and does not deep-copy the value;
- direct and indirect identity survives resolution;
- repeated `ObjectRef` occurrences yield the same canonical handle;
- unresolved access fails without hidden I/O;
- direct children and indirect children retain distinct traversal behavior;
- reference chains, dangling objects, freed objects, self-cycles, and missing
  generations match qpdf;
- stream handles and stream-dictionary handles remain distinct; and
- all existing production consumers use the core handle path.

### Parsed offsets and xref

Use asymmetric fixtures that distinguish:

- leading whitespace before every scalar kind;
- `[` from the first array child;
- `<<` from the first key/value;
- string token start from decoded string bytes;
- stream dictionary start from stream-data start;
- object-stream header, `/First`, member offset, and xref index;
- file-relative from object-stream-relative coordinates;
- parsed null and generated-object `-1`;
- classic, stream, hybrid, incremental, and recovered xref tables; and
- free, uncompressed, compressed, malformed, and unknown raw entries.

Mutation-sensitive tests must fail if offsets are derived from xref locations,
writer output, container children, or reconstructed serialization rather than
the parser's source position.

### Helper differentials

The exact qpdf 11.9.0 exit status is compared for every case below, along
with output bytes: a merged-output comparison where cross-stream ordering
matters, plus an independent stdout/stderr comparison for every case —
`test_xref.cc`/`test_parsedoffset.cc` write usage errors and diagnostics
specifically to `stderr`, a distinction a merged-only comparison cannot
catch a port getting backwards. Cases:

- upstream `minimal.pdf`;
- upstream `digitally-signed.pdf`;
- usage errors;
- missing/open-failure paths;
- malformed xref metadata;
- xref/object-cache inconsistencies; and
- flpdf-authored differential fixtures for parser and ObjStm boundaries.

Upstream qpdf-qtest fixtures and goldens are used from the pinned source/test
tree and are not copied into this repository.

Final verification includes:

- `cargo fmt -- --check`;
- workspace all-feature Clippy with warnings denied;
- strict rustdoc and module-doc validation;
- focused tests for every stack layer;
- `cargo test --workspace --all-features`;
- pinned qpdf differential commands;
- fresh 100% patch coverage for every stack layer;
- all four `get-xref.test` and `parsed-offset.test` invocations; and
- full-survey before/after snapshots with zero allowlist regression.

## Non-goals

- C or C++ ABI and symbol compatibility.
- A qtest-only parsed-offset API.
- A parallel provenance sidecar or metadata graph.
- Reconstructing source metadata from writer output.
- Preserving the current public raw `Object` API.
- Copying qpdf-qtest fixtures into the flpdf repository.
- Completing unrelated Phase 2 repair or page-tree work.
