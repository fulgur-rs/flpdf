# flpdf qpdf 11.9.0 writer full-rewrite cutover

Date: 2026-08-10
Status: design approved for implementation planning
Owner: `flpdf-25kg.6.2`

## Decision

flpdf will model qpdf 11.9.0's `QPDFWriter` as the only PDF document-output
writer. The writer will always create a new full-rewrite output and will not
provide an incremental-update output route.

This is a qpdf-convergence change, not a compatibility-preserving change.
The existing flpdf free-function surface and options are not retained merely
to keep callers source-compatible. The qpdf 11.9.0 writer responsibility and
observable output contract take precedence over the current flpdf API shape.

## Oracle basis

The pinned qpdf source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` is the
semantic and responsibility oracle.

- `include/qpdf/QPDFWriter.hh:53-428` defines one writer object with output
  setup, writer settings, and a single `write()` operation.
- `libqpdf/QPDFWriter.cc:88-109` opens a fresh output with `wb+` for filename
  output.
- `libqpdf/QPDFWriter.cc:2008-2025` removes `/Prev` and other output-sensitive
  trailer entries before writing.
- `libqpdf/QPDFWriter.cc:2187-2203` performs setup, preparation, standard or
  linearized writing, and pipeline finalization from `write()`.
- `libqpdf/QPDFWriter.cc:2991-3044` writes a fresh header, object body, xref,
  `startxref`, and `%%EOF` for standard output.

Therefore qpdf has no incremental writer whose bytes or append revisions could
serve as a writer oracle. qpdf's incremental `/Prev` behavior remains a
reader-side concern only.

The source is checked with:

```text
scripts/fetch-qpdf-source.sh --print-path
rg -n "QPDFWriter|incremental|writeStandard|writeLinearized" \
  /home/ubuntu/.cache/flpdf/qpdf-11.9.0/include/qpdf/QPDFWriter.hh \
  /home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDFWriter.cc
```

## Scope

### In scope

- Replace the current incremental/full-rewrite writer selection with a single
  qpdf-shaped full-rewrite writer lifecycle.
- Port the qpdf writer setting contract for standard, QDF, ObjStm/xref-stream,
  linearized, ID, version, stream, and encryption output.
- Remove the PDF document incremental output implementation and its consumers.
- Remove flpdf-only writer flags and signature-preservation policy that have no
  qpdf writer counterpart.
- Preserve qpdf-compatible reading of existing incremental PDFs and `/Prev`
  chains.
- Preserve JSON and Pipeline incremental serialization, which are separate
  output responsibilities and do not represent PDF document revisions.
- Replace incremental-output acceptance with qpdf full-rewrite differential and
  byte gates plus explicit no-`/Prev` output assertions.

### Out of scope

- Removing incremental xref parsing from the reader.
- Removing JSON stream/chunk incremental delivery.
- Adding an flpdf-specific append-only PDF writer after this cutover.
- Preserving the old `write_pdf`, `write_pdf_with_options`, or
  `WriteOptions` source API solely for backward compatibility.

## Current route inventory

| Responsibility | qpdf 11.9.0 owner | Current flpdf route | Decision |
|---|---|---|---|
| New PDF output | `QPDFWriter::write` | `write_pdf_full_rewrite_inner` plus specialized writer paths | Make the canonical writer object and route all consumers through it |
| Incremental PDF output | None | `write_pdf_incremental` and its object/xref/trailer helpers | Delete |
| Existing incremental input | `QPDF::read_xref` and `/Prev` traversal | `reader.rs` / `xref.rs` | Keep |
| ObjStm and xref form | `QPDFWriter` planner and emitters | plain writer plus legacy writer/object-stream modules | Keep only the qpdf-responsibility paths; converge duplicated emitters in bounded slices |
| Signature inspection | qpdf object/document inspection | `signatures.rs` | Keep read-only inspection |
| Signature-preserving PDF output | No `QPDFWriter` incremental route | `SignatureWriteMode::Incremental` and append-only checks | Delete writer-preservation semantics; full rewrite invalidation matches qpdf |
| JSON/Pipeline incremental delivery | qpdf JSON/pipeline writers | `json`, `json_inspect`, and `pipeline` modules | Keep; it is not PDF incremental output |

The current public default is documented as incremental in
`crates/flpdf/src/lib.rs:12-14` and implemented by
`crates/flpdf/src/writer.rs:898-960`. That default is the primary route to
remove.

## Target architecture

### Writer lifecycle

The public writer will have the same conceptual lifecycle as `QPDFWriter`:

1. Construct a writer around a live `Pdf`.
2. Configure output and qpdf writer settings.
3. Query any final-version information that qpdf exposes before emission.
4. Execute `write()` exactly once.
5. Finish the output pipeline and expose writer results such as written xref
   or renumber information only where that is a qpdf writer responsibility.

The output is always a new PDF. No writer branch may copy the source PDF as a
prefix, append a new xref revision, or preserve a `/Prev` chain in the output.
Output and Rust error propagation must still leave the sink behavior consistent
with qpdf's partial-output contract; this is verified by focused failure-path
tests rather than inferred from the old incremental implementation.

### Writer settings

The writer settings will correspond to qpdf's public writer controls:

- object-stream mode;
- stream-data/decode/compression/recompression mode;
- content normalization;
- QDF mode and original-object-ID suppression;
- preserve-unreferenced-objects;
- newline before `endstream`;
- minimum and forced PDF version/extension level;
- static and deterministic IDs;
- static AES IV for test-only deterministic encryption;
- encryption preservation and direct/donor encryption parameters;
- linearization and pass-one output;
- extra header text and output sink.

`full_rewrite` is not a setting because qpdf has no alternate incremental
writer mode. The CLI must map directly to these writer controls rather than
promote options conditionally to escape the incremental route.

> **[provisional — settled by TDD, not by this document]**
>
> The exact Rust spelling of the writer type, output-sink constructor, setter
> methods, and result-returning `write()` signature will be selected while the
> canonical RED tests are migrated. The durable contract is the qpdf lifecycle
> and the absence of a PDF incremental-output mode; method names and borrow
> arrangement are implementation details.
>
> **[/provisional]**

### Signature behavior

The writer does not promise signed-byte preservation. A signed source written
through the canonical writer is a full rewrite, so existing signed byte ranges
are invalidated exactly as they are under qpdf's full writer.

The signature module may continue to expose signature fields, byte ranges,
`/SigFlags`, and inspection helpers. It must not select an incremental writer or
claim that an ordinary write preserves an existing signed range.

### Reader and non-PDF incremental paths

Reader-side parsing continues to follow classic and stream xref `/Prev` chains,
including historical and recovered incremental fixtures. This is required to
read documents produced by qpdf and other PDF producers; it does not imply that
flpdf emits incremental revisions.

JSON writer output and Pipeline stage lifecycle remain incremental where qpdf's
corresponding JSON/pipeline responsibility is incremental. Their tests and
APIs are not removed by this PDF writer cutover.

## Migration sequence

### 1. Add canonical RED gates

Before deleting the old route, add qpdf-backed tests for the target writer
contract:

- standard full rewrite emits a new header and no `/Prev`;
- default object-stream and xref-form selection matches qpdf;
- QDF and stream-data modes match qpdf;
- version and extension-level promotion/rejection matches qpdf;
- static/deterministic IDs match the pinned qpdf behavior;
- direct encryption, preserved encryption, and donor-copy encryption match
  qpdf's semantic and byte gates where deterministic inputs permit;
- linearized output passes qpdf `--check-linearization` and byte gates;
- warning-only writes finish the output and preserve the qpdf exit contract;
- signed input is rewritten rather than prefix-preserved;
- existing incremental fixtures remain readable through the reader.

### 2. Introduce and migrate the canonical writer object

Migrate library and CLI production consumers in bounded slices. Each slice must
use the canonical writer object and pass its focused qpdf differential tests.
No new caller may be added to the incremental route. The old free functions and
`WriteOptions.full_rewrite` remain only until all production consumers have
cut over, then are removed together with their public exports.

### 3. Remove the incremental writer

After a no-callers check, delete `write_pdf_incremental`, incremental ObjStm
packing, incremental xref/trailer emitters, source-prefix bookkeeping, and
writer-only incremental tests. Remove only fields and helpers proven to have no
reader, JSON, Pipeline, or mutation consumer.

### 4. Remove flpdf-only CLI and signature branches

Remove `--full-rewrite` and any conditional routing whose only purpose was to
choose between incremental and full-rewrite output. Keep qpdf flags whose
semantics remain valid and wire them directly to the canonical writer.

Remove `SignatureWriteMode::Incremental` and any append-only write decision;
retain signature inspection and explicit signature-removal transformations
where qpdf exposes the corresponding operation.

### 5. Update tracker and integration artifacts

Repurpose `flpdf-25kg.6.2` from incremental-output parity coverage to the
qpdf-writer cutover/removal acceptance contract. The existing incremental
matrix PR #710 is not an implementation basis for this design and must not be
merged as-is. A new implementation branch and PR will be based on the
canonical writer cutover after the plan is approved.

## Acceptance criteria

- The library has one canonical PDF output writer with qpdf's full-rewrite
  lifecycle and no PDF incremental-output mode.
- `write_pdf_incremental` and all PDF incremental append emitters have no
  production callers and are removed.
- The public writer configuration has no `full_rewrite` selector or
  incremental signature mode.
- Standard, QDF, ObjStm/xref-stream, linearized, ID, version, direct-encryption,
  and donor-copy-encryption outputs pass the applicable pinned qpdf semantic,
  structural, and deterministic byte gates.
- Canonical output contains no `/Prev` chain created by flpdf.
- Signed input follows qpdf full-rewrite behavior; no append-only preservation
  claim remains.
- Existing incremental PDFs remain readable, including classic/xref-stream
  `/Prev` chains and recovery fixtures.
- JSON and Pipeline incremental serialization behavior remains covered and
  unchanged by this cutover.
- qpdf 11.9.0 is required for Linux writer oracle tests; missing or mismatched
  qpdf is a CI failure for applicable gates, not an automatic success.
- `cargo fmt -- --check`, focused qpdf writer/reader tests, the workspace test
  suite, and Beads dependency/closure checks pass.

## Risks and controls

| Risk | Control |
|---|---|
| Removing incremental output breaks signed-document workflows | Add explicit qpdf-aligned signed-source tests and document full-rewrite invalidation |
| Dirty/source bookkeeping is shared with non-PDF outputs | Build a caller inventory before deletion; remove only after no-callers checks |
| Legacy and plain full-rewrite routes diverge | Migrate one qpdf responsibility at a time and keep qpdf differential tests as the acceptance authority |
| Existing incremental fixtures are mistaken for an output requirement | Keep them under reader tests and assert that output tests contain no `/Prev` |
| Old PR/Bead acceptance remains stale | Update `flpdf-25kg.6.2`, close or supersede PR #710, and read back all tracker state before implementation handoff |
