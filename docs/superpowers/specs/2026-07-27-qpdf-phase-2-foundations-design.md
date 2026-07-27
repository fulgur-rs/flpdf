# qpdf Phase 2 foundations design

**Issue:** `flpdf-qxba`
**Date:** 2026-07-27
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd`)
**Oracle source:** `scripts/fetch-qpdf-source.sh --print-path`
**Prior design:** [qpdf component bottom-up refactor design](2026-07-25-qpdf-component-bottom-up-refactor-design.md)
**Correspondence:** [`docs/qpdf-correspondence.md`](../../qpdf-correspondence.md)

## Problem

`flpdf-qxba` Phase 1 has established the component-completion discipline: port a qpdf
responsibility, migrate every consumer in scope, and delete the old implementation. The next
step is not yet a full top-down rewrite. Several lower-level qpdf components are still absent,
while responsibilities needed by future top-down layers remain embedded in large existing
modules.

The most important missing mechanism is qpdf's `Pipeline` plus its `Pl_*` stages. It is used
throughout qpdf's writer, filters, crypto, JSON, stream inspection, and content processing.
Completing that entire graph would approach completing flpdf itself, including categories that
flpdf has not implemented. Treating "all pipelines" as one child of the current Epic would
therefore make the child unbounded.

At the same time, merely adding a `Pipeline` trait without moving a real production path would
create another unused abstraction. The foundation needs one or two dogfood consumers and must
delete their old routes.

Other qpdf-shaped responsibilities have the same problem:

- the xref entry value type is named `XrefOffset` and remains inside `xref.rs`, although it
  represents free, uncompressed, and compressed entries rather than only offsets;
- qpdf's `QPDF_optimization` object-user analysis is embedded in
  `linearization/plan.rs`;
- inherited page-attribute handling is split between that analysis and
  `linearization/inherited_attrs.rs`;
- future resource rewriting, logging, stream filtering, and crypto pipelines do not yet have
  the lower-level contracts they depend on.

## Goals

1. Add a bounded **Phase 2 Foundations** stage under `flpdf-qxba`.
2. Define a crate-private qpdf-shaped `Pipeline` contract and prove it on the linearization hint
   stream and hint reader paths.
3. Keep qpdf's separate `BitStream` and `BitWriter` responsibilities separate in flpdf.
4. Replace `XrefOffset` with a truthful `XrefEntry` component across every existing consumer.
5. Extract and complete `QPDF_optimization` responsibilities through vertical consumer cutovers.
6. Delete the old implementation in every slice before calling that slice complete.
7. Define a separate Epic for full `Pipeline` / `Pl_*` completion and its missing consumers.

## Non-goals

- Completing every qpdf `Pl_*` implementation in the foundations slice.
- Migrating all writer, filter, crypto, JSON, CLI logging, or object-stream consumers at once.
- Reproducing C++ inheritance, raw-pointer ownership, `dynamic_cast`, or pipeline-stack mechanics
  literally.
- Creating a large `qutil.rs` catch-all.
- Reimplementing MD5, SHA, AES, zlib, or other primitives whose Rust crate contract can preserve
  qpdf behavior.
- Replacing RC4 with the `rc4` crate when that would narrow qpdf's component contract.
- Changing work currently owned by `flpdf-qxba.7` or `flpdf-80b6`.

## Completion rule: vertical cutover

The Phase 1 Definition of Done remains in force. In particular, D2 means **one implementation
and one route**, not "the new module exists."

Before planning each implementation slice:

1. enumerate every related `pub` and `pub(crate)` definition, including `lib.rs` re-exports;
2. enumerate every production and test callsite in both workspace crates;
3. state the slice boundary and which consumers are deliberately outside it.

Before completing the slice:

1. migrate every consumer inside that boundary;
2. delete old types, functions, branches, and direct library routes;
3. do not leave compatibility aliases or forwarding wrappers;
4. use `rg` to prove that the old symbols and duplicate algorithms are gone;
5. keep existing byte baselines unchanged;
6. obtain fresh 100% patch coverage for changed executable lines;
7. update qpdf correspondence annotations truthfully.

The later full-Pipeline Epic follows the same rule stage by stage. It does not add every stage
first and defer all consumer migration to one final large PR.

## Foundation slice: Pipeline contract

### Responsibility

`pipeline.rs` owns the incremental byte-stage lifecycle shared by pipeline components. It mirrors
qpdf 11.9.0 `Pipeline.hh` / `Pipeline.cc` at the responsibility level:

```rust
pub(crate) trait Pipeline {
    fn identifier(&self) -> &str;
    fn write(&mut self, data: &[u8]) -> PipelineResult<()>;
    fn finish(&mut self) -> PipelineResult<()>;
}
```

The exact Rust names may be adjusted during the implementation plan, but these semantics are
fixed:

- `write` consumes a chunk without assuming it is the entire logical stream;
- `finish` finalizes local state and propagates completion downstream;
- the caller, not `Drop`, is responsible for calling `finish`;
- a stage stores a borrowed `&mut dyn Pipeline` downstream reference, or an optional borrowed
  reference where qpdf permits a terminal stage;
- no `Box<dyn Pipeline>` graph, `Rc<RefCell<_>>`, downcast, or global pipeline stack is required
  for the initial slice;
- individual stages preserve qpdf's own post-`finish` and reuse behavior rather than imposing one
  global state machine.

The borrowed chain is assembled from the terminal sink outward. After the outer stages are
finished and dropped, the owner can inspect terminal or tee buffers without shared mutable
ownership.

### Initial stages

The foundation implements only the stages needed to prove the contract:

| Rust module/type | qpdf responsibility | Required behavior |
|---|---|---|
| `pipeline/buffer.rs` / `Buffer` | `Pl_Buffer` | retain bytes; optionally forward; data readable only after `finish`; taking data empties the retained buffer |
| `pipeline/count.rs` / `Count` | `Pl_Count` | forward non-empty writes; retain total byte count and last byte; propagate `finish` |
| `pipeline/flate.rs` / `Flate` | `Pl_Flate` | incremental inflate/deflate; qpdf-compatible warning callback, compression level, and output-buffer behavior; output independent of input chunk boundaries; finalize the codec at `finish` |

The hint-stream dogfood uses only deflate, but the component is not marked complete with only that
path. `Pipeline`, `Pl_Buffer`, `Pl_Count`, `Pl_Flate`, `BitStream`, and `BitWriter` each satisfy
D1 in this slice. Other `Pl_*` components and additional `Pl_Flate` consumers belong to the
full-Pipeline Epic.

### Error model

Pipeline errors are internal but structured:

```rust
enum PipelineErrorKind {
    State,
    Io,
    Codec,
    Callback,
}
```

Each error carries the responsible stage identifier and preserves its source where available.
The public boundary maps it to a dedicated `flpdf::Error::Pipeline` representation rather than
panicking or collapsing it into an unrelated parse error.

Error propagation rules:

- the first failure is the returned failure;
- a codec `finish` failure still makes a best-effort downstream `finish` call;
- a secondary downstream failure never replaces the first failure;
- a write after a non-reusable stage has finished is a state error containing that stage's
  identifier;
- `Drop` neither finishes nor suppresses a missing finish.

This follows qpdf's `Pl_Flate::finish` cleanup behavior while making the first-error rule explicit
in Rust.

## Foundation slice: separate BitStream and BitWriter

qpdf separates `BitStream.cc` from `BitWriter.cc`; flpdf will do the same.

### `bit_stream.rs`

`BitStream<'a>` is a borrowed, MSB-first reader over `&'a [u8]` and mirrors the public
responsibility of qpdf's `BitStream`:

- construct from a byte slice;
- reset to the start;
- read unsigned bits;
- read signed bits with qpdf-compatible representation;
- read a value known to fit in the target integer;
- skip to the next byte boundary;
- report insufficient input and invalid widths without panicking.

Its first production consumer is `linearization/show.rs`. The private `show.rs::BitReader` and
its tests are deleted; all hint-stream reading goes through `BitStream`.

### `bit_writer.rs`

`BitWriter<'a>` writes MSB-first bits to `&'a mut dyn Pipeline` and mirrors qpdf's
`BitWriter`:

- write unsigned bits;
- write signed bits;
- write a value from the narrower integer form;
- flush a partial byte with zero padding;
- do not finish the pipeline implicitly.

Its first production consumer is `linearization/hint_stream.rs`. `HintStreamBuilder` is deleted,
including its public re-export. All hint bit packing goes through `BitWriter`.

There is no shared bit trait or combined reader/writer module. A writer-reader round-trip test
ties the two only through the MSB-first byte contract. Common code is introduced only if actual
implementation duplication justifies it.

## Dogfood: linearization hint stream

qpdf 11.9.0 `QPDF_linearization.cc::writeHintStream` assembles:

```text
BitWriter -> Pl_Count -> optional Pl_Flate -> Pl_Buffer
```

flpdf currently requires both raw and compressed hint bytes in `HintStreamBytes`, so the
foundation uses `Buffer`'s qpdf-compatible optional pass-through to retain the raw stream while
continuing to the compressor:

```text
BitWriter
   |
 Count              supplies uncompressed offsets S and O
   |
 Buffer(raw tee)
   |
 Flate(deflate)
   |
 Buffer(compressed)
```

`BitWriter::flush` emits the final padded byte. Pipeline `finish` then cascades from `Count`
through both buffers and `Flate`. `Count` supplies `/S` and `/O` at the same uncompressed byte
boundaries as qpdf.

Because `BitWriter` holds an exclusive borrowed reference to `Count`, the Rust route uses a
short-lived `BitWriter` for each byte-aligned hint section. It flushes and is dropped at the
section boundary, then the encoder reads `Count` to record `/S` or `/O`, and constructs the next
writer over the same reusable `Count`. This preserves qpdf's byte stream and counter semantics
without `RefCell`, downcast, or a raw pointer.

The retained values continue to populate the existing
`HintStreamBytes { uncompressed, compressed, shared_section_offset_in_uncompressed,
outline_section_offset_in_uncompressed }` contract.

The production cutover deletes:

- `HintStreamBuilder`;
- the direct `flate2::write::ZlibEncoder` path in `linearization/hint_stream.rs`;
- the private `BitReader` in `linearization/show.rs`;
- all hint-stream tests that instantiate either legacy helper.

There is no fallback route. Existing byte-identical linearization fixtures and the
`show/check-linearization` behavior decide whether the cutover is correct.

## Foundation slice: XRefEntry

`XrefOffset` is not merely an offset. It represents all three PDF xref entry kinds and is used
by reader, cache, writer, object streams, linearization, and tests. The replacement is a separate
`xref_entry.rs` component:

```rust
pub enum XrefEntry {
    Free { next: u32 },
    Uncompressed { offset: u64 },
    Compressed { stream: u32, index: u32 },
}
```

`XrefForm` and xref parsing orchestration remain in `xref.rs`; only the entry value
responsibility moves.

This slice is a pre-1.0 public API change. It migrates all workspace consumers and deletes
`XrefOffset` with no type alias. Variant spelling changes from `Offset` to `Uncompressed` so the
API expresses the PDF model rather than the storage detail. The implementation plan must refresh
the current callsite inventory because the symbol already has well over one hundred code and test
occurrences.

`flpdf-80b6` owns concurrent writer work. Any overlapping writer consumer migration waits for
that work or is stacked on its settled result; it is not edited around in a competing worktree.

## Foundation slices: Optimization

qpdf's `QPDF_optimization.cc` responsibility is already materially implemented, but it is
embedded in `linearization/plan.rs` and partially coupled to inherited-page processing. It is
completed in two vertical slices rather than one move-only PR.

### Slice A: object-user map cutover

Create `optimization.rs` and move the object-user model, traversal, classification, and map
construction used by linearization into it. Migrate every current consumer in the declared
scope, then delete those definitions from `linearization/plan.rs`.

The scope includes qpdf's important classification semantics:

- per-page and document-level object users;
- indirect identity versus direct/container traversal;
- stream dictionary traversal and indirect `/Length` treatment;
- page-tree inherited-key exclusions;
- thumbnail and first-page classification;
- object-stream membership using the union of member user sets.

This slice is not "byte-neutral extraction" if that phrase implies leaving the old route in
place. It is a complete consumer cutover whose output must nevertheless remain byte-identical.

### Slice B: inherited attributes and component completion

Resolve the boundary among:

- qpdf `pushInheritedAttributesToPage`;
- qpdf `updateObjectMaps` / `updateObjectMapsInternal`;
- flpdf `linearization/inherited_attrs.rs`;
- page-tree traversal and repair;
- ObjStm member user union.

The result must leave one traversal for each qpdf responsibility and delete duplicated
linearization-specific traversal. Page-tree structural repair remains owned by the page-tree
component; optimization consumes the repaired/effective page view instead of becoming a second
page-tree repair engine.

Only after this slice may `QPDF_optimization` be marked D1/D2 complete.

## Separate Epic: full Pipeline completion

The full-Pipeline Epic depends on the foundation but is not a child implementation slice of it.
It inventories qpdf's pipeline categories and completes them through small vertical cutovers.
Expected categories include:

- string/file/concatenate/null/debug and other pipeline utilities;
- remaining streaming filter stages and consumer cutovers;
- `QPDFStreamFilter`;
- LZW and PNG filters;
- TIFF Predictor 2;
- DCT handling;
- AES and RC4 pipeline stages;
- MD5 and SHA pipeline adapters;
- writer, object-stream, xref-stream, filter, JSON, and inspection consumers;
- `QPDFLogger` pipeline sinks;
- resource find/replace consumers.

The inventory is not a promise that each item requires a hand-written Rust implementation.
Responsibility and behavior parity are the decision criteria.

### ResourceFinder and ResourceReplacer

These are early candidates after the pipeline foundation and `flpdf-qxba.7`. They should use the
qpdf-shaped token-filter route rather than another content parser. Their issue dependency is
therefore:

```text
Pipeline foundation
       + flpdf-qxba.7
              |
 ResourceFinder / ResourceReplacer
```

### QPDFLogger

qpdf logger outputs (`info`, `warn`, `error`, and `save`) are pipeline sinks. flpdf-cli currently
contains many direct `println!` / `eprintln!` routes, so a truthful logger migration crosses CLI
and library boundaries. It belongs to the full-Pipeline Epic, not the foundation trait PR.

### QPDFStreamFilter and codec consumers

Streaming decode/encode stages are migrated consumer by consumer. Existing one-shot filter
functions remain only until their declared consumer scope has crossed; then the duplicate route
is deleted in that slice.

## Primitive reuse policy

The policy is **qpdf component-contract parity**, not "never hand-write crypto" and not "always
use a crate."

### MD5, SHA, AES, and zlib

Use established Rust crates when they can preserve:

- accepted input domain;
- incremental state behavior;
- finalization behavior;
- in-place/out-of-place requirements;
- output bytes and error timing.

Pipeline adapters may be flpdf code, while the cryptographic or codec primitive remains supplied
by the crate. Reimplementing those primitives has no value when the contract is representable.

### RC4

qpdf 11.9.0 `RC4` accepts a runtime `key_len`; `-1` means NUL-terminated input. The native
implementation imposes no positive 256-byte upper bound, retains state across repeated
`process` calls, and permits the same input/output pointer. `Pl_RC4` processes bounded chunks
through that single retained state.

The Rust `rc4` 0.1 crate expresses key size as a compile-time typenum and documents a 1–256-byte
domain. Dispatching only normal PDF key sizes can cover current PDF encryption paths, but it
cannot fully reproduce the qpdf RC4 component contract. Therefore:

- retain a hand-written RC4 primitive;
- extract the current one-shot KSA/PRGA into a dedicated stateful `rc4.rs`;
- make the one-shot compatibility helper delegate to that stateful type;
- make the future `PlRc4` retain the same state across chunks;
- remove the external `rc4` dependency if the final callsite inventory proves it unused.

RC4 parity tests compare flpdf with qpdf for:

- one-shot versus multiple `process` calls;
- key lengths 1, 5, 16, 256, and greater than 256;
- explicit key length versus NUL-terminated mode;
- in-place versus out-of-place processing;
- empty input;
- the `Pl_RC4` default 65,536-byte chunk boundary.

This is a contract-driven exception, not a general license to reimplement other primitives.

## Testing strategy

### Pipeline unit tests

- chunked writes propagate in order;
- `finish` propagates exactly once per call according to stage behavior;
- a fault-injecting sink verifies stage identifiers and first-error retention;
- downstream `finish` is attempted after codec-finalization failure;
- `Buffer` rejects reads before `finish`, supports optional pass-through, and empties after take;
- `Count` reports count and last byte, including empty writes;
- `Flate` output is invariant across input chunk boundaries and qpdf/zlib compatible;
- `Flate` inflate warnings, compression levels, and configurable output-buffer boundaries match
  qpdf;
- non-reusable stages reject write-after-finish.

### Bit unit and oracle tests

- zero-bit operations where qpdf permits them;
- reads and writes across byte boundaries;
- signed boundary values;
- flush/skip byte alignment;
- invalid bit widths and insufficient input;
- writer-to-reader round trip;
- observed qpdf output for representative bit sequences.

### Hint integration tests

- existing raw hint bytes remain unchanged;
- compressed bytes remain unchanged under `qpdf-zlib-compat`;
- `/S` and `/O` remain at qpdf-compatible uncompressed offsets;
- linearization output remains byte-identical;
- show and check-linearization paths parse the generated hints.

### Xref and optimization gates

- existing reader, xref, cache, writer, ObjStm, and linearization suites remain green;
- public compile tests use `XrefEntry`, with no `XrefOffset` alias;
- optimization fixtures cover inherited resources, page sharing, thumbnails, indirect lengths,
  arrays, streams, and ObjStm member unions;
- qpdf 11.9.0 source and observed output remain the oracle.

### Required gates per code PR

1. focused RED/GREEN tests for the slice;
2. `cargo fmt -- --check`;
3. workspace clippy with all targets and features;
4. focused crate/integration tests;
5. workspace tests;
6. qpdf module-correspondence check;
7. fresh 100% patch coverage against the immediate parent branch;
8. existing byte-identical gates for affected outputs.

## Delivery structure

After this design and its implementation plan are approved, create Beads issues rather than
editing the current Epic ad hoc.

Proposed structure:

1. a Phase 2 Foundations Epic under `flpdf-qxba`;
2. child: Pipeline contract + Buffer/Count/Flate + BitWriter/BitStream + hint/show cutover;
3. child: `XrefEntry` complete cutover;
4. child: Optimization object-user map cutover;
5. child: Optimization inherited-attribute boundary and D1/D2 completion;
6. a separate full-Pipeline completion Epic depending on the foundation.

Implementation uses isolated worktrees, RED→GREEN→REFACTOR, and small stacked PRs. The first four
code slices are stacked only where dependency requires it. The full-Pipeline Epic begins
separately so its unimplemented categories do not block closing the bounded foundations work.

## Concurrent-work safeguards

- `flpdf-qxba.7` is already in progress; do not modify or restack its branch from this work.
- `flpdf-80b6` owns writer work; defer or stack overlapping writer callsite migration after its
  result is stable.
- refresh `git worktree list`, Beads state, public definitions, and callsites immediately before
  each slice;
- if the live inventory changes a slice boundary, update its design/plan before implementation
  rather than silently preserving a stale list.

## Acceptance criteria

The foundations stage is complete when:

1. the Pipeline contract is exercised by the production hint write path;
2. the production hint read path uses the separate `BitStream`;
3. `HintStreamBuilder`, private `BitReader`, and the direct hint `ZlibEncoder` route are gone;
4. `XrefEntry` is the sole xref entry value type and `XrefOffset` no longer exists;
5. optimization object-user and inherited-attribute responsibilities have one qpdf-shaped
   implementation and all current consumers use it;
6. every component declared complete in the foundations slice meets D1/D2, including the full
   `Pl_Flate` responsibility even though the first production dogfood uses deflate only;
7. byte baselines, qpdf oracle checks, workspace tests, and fresh 100% patch coverage pass;
8. the separate full-Pipeline Epic has an explicit dependency and category inventory, without
   being treated as complete by the foundation work.
