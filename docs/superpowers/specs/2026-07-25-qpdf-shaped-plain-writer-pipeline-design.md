# qpdf-shaped plain writer pipeline design

**Issue:** flpdf-2tbp
**Date:** 2026-07-25
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit
`3b97c9bd266b7c32ea36d3536e22dab77412886d`)

## Problem

The reader now has a qpdf-shaped file-object path built bottom-up behind the
old production path and switched over by consumer. The plain writer has reached
a substantial qpdf compatibility floor, but its architecture does not provide
the same kind of foundation for closing the remaining gaps.

`crates/flpdf/src/writer.rs` is 8,443 lines and currently owns:

- top-level option validation and dispatch;
- incremental and full-rewrite paths;
- reachability and renumbering integration;
- object-stream planning and emission;
- stream filtering and framing;
- xref table and xref stream construction;
- trailer, ID, and version policy;
- encryption and copy-encryption;
- QDF-specific serialization;
- integration with the separate linearized writer.

Plain object-stream Generate and source-object-stream Preserve use specialized
early-return paths, while other combinations continue through the legacy full
rewrite. As a result, qpdf behavior that should be a shared serialization or
xref rule can remain path-specific. Fixes are difficult to classify as a
planning, object serialization, physical layout, or dispatch problem.

The old Beads descriptions for `flpdf-9hc.20.29` and `.30` say that the
object-stream/xref-stream golden harness and byte parity are missing. That
snapshot is stale. At the design baseline, the following committed-HEAD gates
already pass under `qpdf-zlib-compat`:

- `cmp_generate_objstm_tests`: 7/7;
- `cmp_diff_zero_tests`: 11/11.

This refactor must preserve and broaden that floor. It must not reimplement
already working qpdf parity from the stale issue description.

## Goal

Build a qpdf-shaped plain full-rewrite pipeline that separates:

1. option preflight and effective policy;
2. logical object placement;
3. physical body emission;
4. xref and trailer assembly.

`--object-streams=disable`, `preserve`, and `generate` must share the same body,
xref, and trailer pipeline. Their differences must be confined to the logical
placement strategy.

The first phase is complete when all three modes use the new pipeline, the old
plain emitters are removed, and a curated qpdf 11.9.0 corpus remains
byte-identical under deterministic oracle controls.

## Non-goals

The first phase does not:

- port the entire qpdf `QPDFWriter` state machine;
- change the public writer API or the PDF object model;
- change QDF, encryption, copy-encryption, linearization, or incremental
  routing;
- fix QDF normalization, string, trailer, null-visibility, or ObjStm contract
  gaps;
- extend the encryption algorithm matrix;
- redesign the linearization plan;
- add a public writer plugin or trait API;
- promise byte identity with qpdf when the default Pure Rust deflate backend is
  selected.

## Architecture

Plain full rewrite is split into four stages:

```text
Pdf + WriteOptions
  |
  v
preflight
  option validation, effective version, stream policy, ID policy
  |
  v
logical planner
  qpdf-visible graph, reachability, renumbering, ObjStm placement
  |
  v
body emitter
  ordinary objects and ObjStm containers, with physical positions recorded
  |
  v
xref/trailer assembler
  xref table or stream, trailer fields, IDs, and startxref
```

Logical planning must not predict byte offsets. Object offsets are not known
until serialization has occurred. The body emitter therefore returns a
separate physical result that the xref assembler consumes.

The initial model is intentionally specific to plain full rewrite:

```rust
struct PlainWritePlan {
    header: HeaderPlan,
    objects: Vec<PlannedIndirectObject>,
    trailer: TrailerPlan,
}

enum PlannedIndirectObject {
    Source {
        output_ref: ObjectRef,
        source_ref: ObjectRef,
    },
    ObjectStream {
        output_ref: ObjectRef,
        members: Vec<PlannedMember>,
    },
}

struct BodyLayout {
    uncompressed_offsets: BTreeMap<u32, usize>,
    compressed_locations: BTreeMap<u32, CompressedLocation>,
}
```

The concrete private types may additionally carry the renumber map, removed
reference set, placement lookup, chosen xref form, and ID policy required by
the existing qpdf-compatible algorithms. No other source-derived state is
retained without amending this design. The types must preserve these
boundaries:

- the logical plan contains identities, numbering, placement, and policies;
- the logical plan contains neither source object bodies nor physical offsets;
- the body result contains physical positions, not reachability decisions;
- xref/trailer assembly consumes the logical plan and physical result without
  rescanning the source graph.

## Component boundaries

The new implementation is a private child of `writer.rs`:

```text
writer.rs
└── writer/
    ├── object_streams.rs
    ├── serialize.rs
    └── plain/
        ├── mod.rs
        ├── plan.rs
        ├── body.rs
        └── xref.rs
```

### `writer.rs`

`writer.rs` remains the public facade and top-level dispatcher. It retains the
public APIs and all specialized modes that are out of scope. For an eligible
plain full rewrite, it delegates to `writer::plain`.

### `writer/serialize.rs`

This module owns physical representation of already prepared values:

- ordinary object bodies;
- ordinary stream framing;
- generated ObjStm payloads and fixed-order dictionaries;
- xref-stream payloads and fixed-order dictionaries.

It does not resolve source references, decide reachability, assign numbers, or
choose object-stream membership. Its core operations are pure over prepared
objects or byte slices so they can be tested without a `Pdf`.

### `writer/plain/plan.rs`

This module builds `PlainWritePlan`. It reuses, rather than duplicates:

- qpdf-null visibility from `qpdf_null`;
- Catalog-first and Generate numbering from `rewrite_renumber`;
- eligibility, source-container reconstruction, qpdf DFS order, and even
  splitting from `writer::object_streams`.

It decides:

- source-to-output numbering;
- plain versus compressed placement;
- generated or preserved container membership;
- xref table versus xref stream form;
- header, trailer, and ID policies.

The plan stores source references, not cloned object bodies. Large streams are
resolved and transformed once during body emission.

### `writer/plain/body.rs`

This module:

- resolves each planned source object;
- applies the plan's reference mapping and removed-reference semantics;
- applies the effective stream policy;
- serializes objects in ascending output-number order;
- records ordinary offsets and compressed member locations in `BodyLayout`.

It contains no mode-specific traversal or grouping algorithm.

### `writer/plain/xref.rs`

This module:

- converts `BodyLayout` into complete xref entries;
- computes minimal `/W` widths;
- applies qpdf's PNG Predictor 12 representation;
- omits `/Index` when the range is exactly `[0 Size]`;
- writes fixed-order xref-stream dictionaries;
- writes classic trailers in qpdf order;
- resolves static, random, and deterministic ID policy;
- writes `startxref` and the final file terminator.

### `writer/plain/mod.rs`

This module coordinates preflight, planning, validation, body emission, and
xref/trailer assembly. It contains no serialization rules or mode-specific
graph algorithms.

## qpdf algorithm preservation

The refactor must not collapse qpdf's intentionally different traversals into
one generic graph walk:

- Disable and Preserve standard enqueue use Catalog-first breadth-first order.
- Generate compressible candidates use the trailer-rooted depth-first order of
  `QPDF::getCompressibleObjGens`.
- Generate final numbering uses container-aware enqueue order.

Commonality begins at the resulting placement model. The traversal algorithms
remain separate and continue to use the already implemented parity helpers.

## Plan validation

`PlainWritePlan::validate()` runs before body emission and verifies:

- every output object number is unique;
- the source `/Root` has an output mapping;
- each object has exactly one physical role: plain, compressed member, or
  generated structural stream;
- ObjStm output members have generation zero and are not streams, xref
  objects, encryption dictionaries, or other forbidden structural objects
  (a nonzero source generation may be renumbered to output generation zero);
- container numbers do not collide with plain object numbers;
- every compressed member has one container/index pair;
- the plan can produce a complete xref range from object zero through `/Size`;
- forced PDF versions below 1.5 contain no ObjStm or xref stream;
- static and deterministic ID policies are mutually exclusive;
- an output mode that requires an xref stream has a compatible PDF version.

Invariant failures return a diagnostic `Error` containing the affected object,
container, or output number. They must not panic.

## Mutation and error behavior

The writer retains the current caller-visible behavior:

- preflight, planning, resolution, encoding, and invariant errors are returned
  before bytes are written to the caller's `W`;
- the complete plain output is built in memory and passed to `write_all` only
  after semantic success;
- final output I/O errors retain normal partial-write semantics;
- output-only Catalog mutations, including ADBE extension injection or
  stripping, are restored on success and failure;
- the new plan does not persist mutations in the caller's `Pdf`;
- malformed input that qpdf accepts with a warning is not independently made
  fatal by the writer; the writer consumes the reader's object graph and
  diagnostics.

## Bottom-up stacked delivery

Implementation is split into six dependent Beads children and stacked
branches. Every layer is independently green.

### Layer 1: physical serialization primitives

- Add `writer/serialize.rs`.
- Extract ordinary stream framing and the dedicated ObjStm/xref-stream byte
  encoders as pure operations.
- Pin current golden bytes with unit tests.
- Keep all production routing unchanged.

### Layer 2: xref and trailer assembly

- Add `BodyLayout` and pure xref entry construction.
- Add minimal `/W`, Predictor 12, `/Index`, fixed dictionary order, trailer,
  ID, and `startxref` assembly.
- Test synthetic physical layouts without opening a PDF.
- Keep all production routing unchanged.

### Layer 3: logical plain plan

- Add the logical placement types and plan validation.
- Build plans through the existing qpdf traversal, numbering, and ObjStm
  helpers.
- Shadow-compare new plans against the legacy output paths for numbering,
  membership, xref form, roots, and trailer policy.
- Keep all production output on the legacy paths.

### Layer 4: Disable routing

- Switch only unencrypted, non-QDF, non-linearized full rewrite with
  `ObjectStreamMode::Disable`.
- Preserve the classic-xref byte-identical floor.
- Leave every specialized mode on its existing path.

### Layer 5: Preserve routing

- Switch plain Preserve for inputs with and without source ObjStm containers.
- Preserve surviving source container membership and member index order.
- Move the classic-table versus xref-stream choice into the logical plan.

### Layer 6: Generate routing and cleanup

- Switch plain Generate.
- Verify container-first numbering, split boundaries, and multiple containers.
- Remove `write_pdf_generate` and superseded plain-only branches and helpers.
- Remove the shadow comparison after all three modes use the new pipeline.

The parent issue is `flpdf-2tbp`. The six child issues and dependency chain are
created immediately before the implementation plan is executed, so their
acceptance criteria can cite the final task boundaries and commands.

### Implementation status (2026-07-25)

The six-layer stack is implemented on these Beads, branches, and commits:

| Layer | Bead | Branch | Commits / final subject |
| --- | --- | --- | --- |
| 1 | `flpdf-2tbp.1` | `stack/flpdf-2tbp-serialize` | `350173c9` |
| 2 | `flpdf-2tbp.2` | `stack/flpdf-2tbp-xref` | `1740cffa`, `4499e6f0` |
| 3 | `flpdf-2tbp.3` | `stack/flpdf-2tbp-plan` | `4d24b66c`, `8b2de2ce` |
| 4 | `flpdf-2tbp.4` | `stack/flpdf-2tbp-disable` | `65772b42`, `4915b1b9`, `a2cdf6aa` |
| 5 | `flpdf-2tbp.5` | `stack/flpdf-2tbp-preserve` | `810af53e`, `645281f5`, `b849faea` |
| 6 | `flpdf-2tbp.6` | `stack/flpdf-2tbp-generate` | `refactor(writer): route generate through plain pipeline` |

Layer 6 routes eligible Generate rewrites through the same
`PlainWritePlan` → body → xref/trailer pipeline as Disable and Preserve. The
dedicated `write_pdf_generate`, `write_pdf_containerized_qpdf`, and
`generate_invariant` helpers and the plan-to-legacy shadow comparisons are
removed.

The final curated `qpdf-zlib-compat` corpus passes against qpdf 11.9.0 with
these exact counts:

- `cmp_diff_zero_tests`: 11/11;
- `cmp_generate_objstm_tests`: 9/9;
- `cmp_linearize_objstm_tests`: 126/126;
- `object_streams_writer_tests`: 22/22;
- `deterministic_id_xref_stream_tests`: 2/2;
- `newline_before_endstream_tests`: 18/18;
- `cmp_null_visibility_tests`: 51/51.

One- and two-page Generate goldens were generated by qpdf 11.9.0, while the
existing three-page Generate golden was regenerated and confirmed
byte-identical. The 130-member-boundary fixture produces two ObjStm containers
with 66 members each and dense type-2 xref indices `0..65`.

The shared plain pipeline remains deliberately excluded for QDF, output
encryption, copy-encryption, source-encrypted input, and requested Preserve or
Generate suppressed by `--force-version` below 1.5. Incremental writing and
linearization retain their separate top-level routes. Focused routing and
behavior tests cover all of these boundaries.

The roadmap issues `flpdf-9hc.20.29` and `.30` are closed as satisfied by the
committed shared serializers and final differential corpus. The fixed ObjStm
and xref-stream dictionary order, minimal `/W`, Predictor 12, `/Index`
omission, and byte parity are covered by the Layer 1–2 unit tests plus
`cmp_generate_objstm_tests`; the requested one-/two-/three-page Generate
goldens and harness are present and generated by the pinned qpdf 11.9.0
oracle.

## Differential corpus

The parity corpus covers, at minimum:

- one-, two-, and three-page documents;
- input with and without source ObjStm containers;
- 5, 100, 101, and 130 eligible-member boundaries;
- indirect `/Length` holders that are dropped and retained;
- dictionary-versus-array visibility for missing, free, real-null, and
  holder-chain references;
- raw streams, lone Flate streams, supported filter chains, and decode-failure
  passthrough;
- direct and indirect `/Info`, `/ID`, and direct trailer values;
- source xref tables and xref streams;
- forced PDF 1.4 downgrade.

The same applicable fixture is exercised under Disable, Preserve, and Generate
instead of maintaining unrelated per-mode examples. Tests compare qpdf 11.9.0
output under pinned static or deterministic controls.

## Verification gates

Every layer runs:

- `cargo fmt --all -- --check`;
- focused unit and integration tests for the changed component;
- `cargo test -p flpdf`;
- `cargo test -p flpdf-cli`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`.

The final layer also runs:

- `cargo test`;
- strict workspace rustdoc with private items;
- qpdf 11.9.0 differential tests for all curated fixtures and modes;
- byte comparisons with `qpdf-zlib-compat`;
- final committed-HEAD patch coverage at 100%.

The default Pure Rust backend retains all semantic tests. Raw compressed bytes
are required to match qpdf only in the `qpdf-zlib-compat` gate.

## Public compatibility

The following APIs and option meanings remain unchanged:

- `write_pdf`;
- `write_pdf_with_options`;
- `write_qdf`;
- `WriteOptions`;
- `ObjectStreamMode`.

No public type exposes `PlainWritePlan` or the internal emitter components.

## Completion criteria

The first phase is complete only when:

- plain Disable, Preserve, and Generate share the new plan/body/xref pipeline;
- their differences are confined to logical placement strategy;
- the old plain emitter and Generate early-return path are removed;
- the curated corpus is byte-identical to qpdf 11.9.0 under the compatibility
  backend and pinned ID controls;
- default-backend semantic tests remain green;
- final committed-HEAD patch coverage is 100%;
- all Beads state and every stacked Git branch have been pushed successfully.

After this phase, QDF is the first intended consumer follow-up. Its already
classified gaps—null visibility, ISO-Latin-1 string literals, multiline
trailers, token-preserving normalization, and the QDF/ObjStm contract—remain
separate changes rather than being folded into this refactor.
