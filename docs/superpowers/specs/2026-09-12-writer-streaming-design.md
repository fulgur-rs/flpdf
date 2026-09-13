# Non-linearized writer streaming design

## Goal

Translate qpdf 11.9.0's non-linearized `QPDFWriter::writeStandard` ownership
boundary into flpdf. Ordinary body objects, object-stream containers, xref,
and trailer bytes must be emitted through the configured output sink while the
writer retains only numbering/layout maps and the bounded buffers required by
individual stream/filter operations.

This issue covers every non-linearized writer route, not only the fast plain
Disable path: plain Disable/Preserve/Generate, QDF, encryption and encrypted
input fallbacks, extra-header/forced-version fallbacks, and PCLm. Any
`writer.rs` non-linearized coordinator that currently retains a complete body
or reassembles one is in scope. Linearized pass 1/pass 2 ownership remains the
separate `flpdf-ymuj.5` responsibility and is not routed through this design.

## Oracle model

The pinned qpdf tree is `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` at commit
`3b97c9bd266b7c32ea36d3536e22dab77412886d` (qpdf 11.9.0).

- `QPDFWriter::setOutputFile`, `setOutputMemory`, and `setOutputPipeline` install
  one active pipeline (`libqpdf/QPDFWriter.cc:65-140`; `include/qpdf/QPDFWriter.hh:103-140`).
- `writeString` and `writeBuffer` write to that active pipeline
  (`libqpdf/QPDFWriter.cc:875-884`).
- `enqueueObject` assigns numbers as the live queue grows, `unparseChild` queues
  an indirect child before writing its reference, and `writeObject` owns object
  framing/progress/length-holder order (`libqpdf/QPDFWriter.cc:1036-1157,1761-1809`).
- `writeStandard` emits the header, live queue, encryption dictionary, xref,
  trailer, and EOF in one pass (`libqpdf/QPDFWriter.cc:2991-3044`). Its xref
  offset is the active pipeline count (`:3021-3034`).
- A stream's encoded or filtered data is buffered only for that stream so its
  `/Length` can be written before the payload (`libqpdf/QPDFWriter.cc:1248-1314,
  1535-1566`). Object-stream bodies and xref-stream payloads have the same
  local length-before-data requirement (`:1621-1757,2391-2494`). None of these
  buffers is the complete PDF output.

## Proposed architecture

Introduce one crate-private `OutputSink` abstraction at the writer boundary.
It wraps any `std::io::Write` sink and owns a checked final-output byte-position
counter. Every successful partial write advances that counter by exactly the
number of bytes accepted. It exposes `write_all`, `position`, and the ordinary
formatting helpers needed by the serializer. The configured
`WriterOutputSink` remains responsible for translating file/Writer/Pipeline
failures into the existing qpdf-shaped error categories; `OutputSink` must not
swallow or rewrite those errors.

The final-output counter is not reused for local coordinates. Stream payload
buffers, object-stream member bodies/pair tables, and xref-stream payloads each
use their own local `Vec`/pipeline and are consumed exactly once when their
length and local offsets are known. Encrypted stream length is measured after
the encryption framing required by qpdf, while object-stream `/First` and pair
table offsets are measured from the object-stream body origin. None of these
local counters contributes to the final-output counter until the resulting
bytes are written to the final sink.

The serializer surface in `writer/object.rs` and `writer/serialize.rs` changes
from `&mut Vec<u8>` to the single `OutputSink` type. This is a canonical writer
surface change, not a second compatibility API. Unit tests that need a byte
vector use an `OutputSink` backed by a test `Vec`; production non-linearized
routes always wrap the configured sink. Existing per-object and per-container
temporary buffers remain explicit and bounded:

- a filtered stream keeps the qpdf-required stream payload buffer;
- an object-stream container keeps its member body/pair table until the
  container header and `/First` value are known;
- a filtered xref stream keeps its xref payload until `/Length` is known; and
- a Memory output sink keeps the final PDF because `get_buffer` promises that
  ownership to the caller.

`BodyLayout` retains only physical xref locations. `LiveObjectEmitter` and
`PlainObjectEmitter` write object bytes directly to `OutputSink`, recording
`position()` before each object and length-holder object. `append_xref_*` writes
classic xref rows/trailer directly to the same sink. Its xref-stream encoder may
write one bounded encoded xref payload to the sink after the payload length is
known. No body function returns a complete PDF-sized `Vec<u8>`.

The planned writer must not retain a `CachedStreamOutput` payload for every
stream while it plans the graph. More importantly, non-linearized planning must
not use a stream payload result to decide which dictionary references to
enqueue. qpdf obtains the actual `willFilterStream` result inside
`writeObject` and then walks the surviving stream-dictionary keys through
`unparseChild` (`libqpdf/QPDFWriter.cc:1144-1157,1248-1314,1535-1566`). The
flpdf live writer therefore becomes the canonical non-linearized traversal for
Disable, Preserve, Generate, and QDF: object-stream membership/container
placement is planned without reading stream payloads, while stream filtering,
dictionary-key visibility, child discovery, and output-number assignment happen
in final emission order. `CanonicalCatalogFirstRenumber` must not call a
payload-producing `stream_parameters_removed` callback for these routes.

This removes `PlainWritePlan::cached_stream_outputs` as a multi-stream payload
cache. A plan may retain object-stream membership, stream policy inputs, and
mutation fingerprints only when they do not require provider execution. A
stateful provider is executed by the writer-owned stream emission path exactly
where qpdf executes it; a recoverable filter failure follows qpdf's two-attempt
raw retry, while a sink or terminal provider failure stops the queue and never
causes a second unrelated stream to be requested. No discard probe is added as
a substitute for the emission-time decision.

The resulting live queue is also the source of truth for planned
Disable/Preserve/Generate/QDF output. Generate and Preserve may reserve all
members of a known object-stream container when the container is enqueued, and
QDF may reserve a stream length-holder at that same enqueue boundary. They may
not prewalk stream dictionaries to decide `/Filter` or `/DecodeParms` child
references. The stream's writer emission first obtains its qpdf-shaped filtered
payload, then emits the surviving dictionary and invokes the dynamic enqueue
callback for its indirect children. This is the explicit resolution of the
payload-dependent numbering cycle; retaining the old prewalk as a second route
would preserve the current qpdf mismatch.

The deterministic-ID path composes a digesting final sink with `OutputSink`.
The incremental digest covers exactly the bytes qpdf's `Pl_MD5` sees before
`generateID`, including body, classic-xref bytes, and xref-stream dictionary
bytes through the `/ID [` opening bracket, and stops before the identifier
bytes themselves (`libqpdf/QPDFWriter.cc:1009-1033,1215-1220,2391-2494`). The
xref-stream payload is emitted after that ID cutoff and is not included. The
`/Info`-derived seed keeps qpdf's raw-byte and first-NUL truncation rules. The
linearized writer's existing two-pass ID handling is unchanged, although its
callers must mechanically adapt to the serializer's sink type.

The PCLm route uses the same sink/position boundary for its sequential body and
xref output. Its existing page/content ordering plan and per-object
direct-root serialization remain owned by PCLm, but stream data is obtained
through the writer-owned `willFilterStream` decision and `pipe_stream_data`
path. This includes `isDataModified`, filter-on-write, metadata cleartext,
normalization, compression policy, and qpdf's initial `will_retry=true`
contract (`libqpdf/QPDFWriter.cc:1248-1314`; the raw public accessor at
`QPDF_Stream.cc:363` is not an equivalent replacement). The PCLm plan retains
only its page/content/image/synthetic/root initial enqueue order; its complete
child prewalk and fixed reference map are removed so stream-dictionary children
are dynamically enqueued after stream processing. `writer/pclm.rs` is therefore
part of this issue. It may retain one qpdf-required stream buffer, never a
complete PDF body. The
encrypted, extra-header, forced-version, and other non-plain fallbacks in
`writer.rs` use the same final sink boundary; they may retain only their
qpdf-required per-object/container staging buffers.

Pipeline lifecycle has three separate boundaries: a local stream/filter or
object-stream buffer is finished before its bytes are consumed; the temporary
stream/encryption stage connected to the final sink is finished at the qpdf
`PipelinePopper` scope; and the document output sink is finished only after
EOF. The internal sink contract exposes these segment/document finish
operations without resetting the final byte counter or deterministic digest.
An error from a stream-segment finish stops the queue before the next provider
request; it is not treated as a recoverable filter failure. A recoverable
filter result (`will_retry=true` with no terminal error) follows qpdf's second
raw attempt and remains limited to that same stream.

## Data flow

```text
configured Writer / Pipeline / Memory
                │
        WriterOutputSink
                │
        counted OutputSink
          ┌─────┴─────┐
      body emitter  xref/trailer
          │
  ObjectWriterEmission
          │
  per-stream / per-ObjStm / xref-stream bounded buffers only
```

The body is emitted before xref/trailer, matching qpdf. The counter is the only
offset authority; no serializer derives an xref offset from `Vec::len()` or
copies a completed body into a second output vector. On a sink error, the
already-written prefix is preserved and the original Writer/Pipeline error
boundary is returned. On a successful Memory sink, the memory sink's own Vec is
the only complete-output owner.

## Compatibility and invariants

- `WriterOutput::{Writer,Pipeline,Memory}` public behavior is unchanged.
- Linearized output and its pass-1 file are untouched by this issue.
- qpdf object numbering, progress timing, encryption key lifetime, warning
  order, stream filter retry behavior, `/Length` framing, xref forms, trailer
  order, and output bytes remain unchanged across all non-linearized routes.
- `BodyLayout` offsets are measured in final output space and checked for
  counter overflow before xref formatting. ObjStm pair-table offsets and
  stream lengths are never accidentally interpreted as final offsets.
- A partial `Write::write` is retried by `write_all` exactly as Rust's existing
  writer contract requires; successful bytes, not requested bytes, advance the
  position.
- `Interrupted` is retried, `WriteZero` becomes the existing I/O failure, and
  a sink failure never triggers a stream-filter retry. The final sink retains
  the successfully written prefix and the original error category.
- Non-linearized deterministic IDs are produced from an incremental digest at
  qpdf's `/ID [` cutoff; no complete output Vec is reconstructed merely to hash
  it.
- All qpdf `willFilterStream` branches are shared by PCLm and ordinary
  non-linearized writers; raw stream access is not a substitute for writer
  retry/filter state.
- Segment finish, document finish, and local-buffer finish are distinct
  operations. Segment finish errors are terminal for the current write and do
  not request later streams; a successful segment finish does not reset the
  final-output position or digest.
- qpdf-required local buffering is not mislabeled as a complete-output buffer,
  and no empty buffer, sentinel, or late output rewrite substitutes for a
  missing sink capability.
- Existing qpdf deviations remain documented. This issue does not touch qtest
  exception handling or introduce a raw-object bridge.

## Test strategy

The RED suite uses an instrumented arbitrary Writer sink and provider-backed
streams. It records event order, final bytes, payload-request order, live
payload ownership, allocation counts, and injected I/O failures. A write-count
assertion is not the core proof: an implementation could retain the full PDF
and split its final write. The causal tests instead require header/object bytes
to reach the sink before the next stream provider is requested, and require a
terminal first-stream failure not to request a later stream. A separate
allocation/lifetime harness uses a discard sink, fixed-size provider payloads,
and peak live allocation accounting to detect a hidden full-output copy; it
asserts only relational bounds derived from one-stream local buffering, not
platform-specific absolute RSS. Memory output explicitly permits one final
sink-owned Vec.

The source-level route guard also requires that non-linearized body emitters
accept the sink and return layout/result metadata rather than a complete
`Vec<u8>`. This guard complements, rather than replaces, the runtime
provider/ownership tests and prevents a hidden duplicate body buffer from being
reintroduced behind a chunked final write.

Focused tests cover one-page and large-stream output in every non-linearized
route, body-before-xref ordering, partial writes, `Interrupted`, `WriteZero`,
mid-body and finish failures, deterministic-ID parity, encryption, PCLm, and
Memory ownership. They assert that stream A's output and dictionary discovery
precede stream B's provider request, that a terminal A failure prevents B, that
a recoverable filter failure retries only A, and that planned stream payloads
are not kept alive for unrelated later streams. They also assert the
`take_buffer` ownership move directly: the per-stream buffer is consumed once
by the final sink and is not cloned into a second body-wide vector.

The matrix includes the qpdf branch where filtering is disabled and a provider
returns `false`: qpdf does not enter the retry branch in that case
(`libqpdf/QPDFWriter.cc:1304-1310`). Deterministic-ID tests also record that
the MD5 stage is popped after the final ID-bearing trailer work and that the
document sink is finished only afterward (`libqpdf/QPDFWriter.cc:3036-3043,
2187-2213`).

The failure tests distinguish a recoverable filter result from a terminal
provider/sink/segment-finish error. The former must retry the same stream and
may continue; the latter must preserve the written prefix and prevent any
later stream provider request. The ownership harness tracks peak live bytes
for the discard sink relative to a one-stream local-buffer baseline, while a
source-level guard rejects a body-wide output field or return type. Together
these checks catch an implementation that both streams incrementally and
retains a hidden complete-output Vec.

The final report records before/after allocation counts and live bytes by size
class for scalar-heavy, dictionary-heavy, stream-heavy, and multi-stream
outputs, plus maximum RSS and wall time for qpdf/flpdf 1,000/2,000/5,000/10,000
and 20,000-page `--check` inputs with one-page baselines. The measured tests
must explicitly report whether `take_buffer` is moved into the next writer
stage or copied, and any retained local buffer must name its qpdf source
responsibility. A pass based only on equal output bytes or a small write count
does not satisfy the memory acceptance criterion.

GREEN verification compares qpdf 11.9.0 and flpdf under
`qpdf-zlib-compat` for Disable/Preserve/Generate/QDF/PCLm, stream-data modes,
encryption, and representative large inputs. The required local gates are
workspace/all-features tests, strict private rustdoc, all-target/all-feature
clippy, qpdf module/deviation/route checks, fresh patch coverage, and the
explicit CI qpdf-zlib list. Large-page and large-stream `/usr/bin/time -v`
measurements use one-page baselines and keep generated artifacts outside the
repository.

## Scope boundaries

In scope: `writer/plain/body.rs`, `writer/plain/mod.rs`,
`writer/plain/plan.rs`, `writer/plain/xref.rs`, `writer/rewrite_renumber.rs`,
`writer/object.rs`, `writer/serialize.rs`, `writer/pclm.rs`, the
non-linearized coordinator and `write_pclm` path in `writer.rs`, plus the
relevant output/sink and tests/docs.
Every non-linearized fallback that currently assembles a complete body is
included in the inventory. The plan may refactor the existing live queue into a
single all-mode queue; it must not retain the old prewalk as a second canonical
route.

Out of scope: linearization pass ownership (`flpdf-ymuj.5`), qtest exception
changes, CLI behavior changes, public API expansion, qpdf fixture vendoring,
and unrelated QDF/linearization memory optimizations.
