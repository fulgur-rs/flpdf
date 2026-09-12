# Non-linearized writer streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Translate qpdf 11.9.0's non-linearized `QPDFWriter::writeStandard`
output ownership so every non-linearized flpdf route writes body, xref, and
trailer bytes directly to its configured sink without retaining a complete
PDF-sized intermediate.

**Architecture:** Add one crate-private counted final-output sink with explicit
segment/document finish operations and an incremental deterministic-ID digest.
Migrate the writer serializer and all non-linearized body/xref/trailer routes to
that sink. Keep only qpdf-required per-stream, per-object-stream, and
per-xref-stream local buffers; move stream-dependent child discovery and
numbering into final emission order.

**Tech Stack:** Rust workspace, `std::io::Write`, flpdf `Pipeline`, qpdf 11.9.0
source/live oracle, qpdf-zlib compatibility tests, `cargo llvm-cov`, Beads,
GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-12-writer-streaming-design.md`

## Global Constraints

- The pinned qpdf source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` with commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` and `/usr/bin/qpdf version 11.9.0` are the semantic and byte-output authority.
- qpdf installs one active output pipeline (`QPDFWriter.cc:65-140,875-884`) and `writeStandard` emits header, live queue, xref, trailer, and EOF in one pass (`QPDFWriter.cc:2991-3044`); flpdf must use one final sink for the same order.
- Final output position/digest, stream payload length, ObjStm local `/First`/pair offsets, and xref-stream payload coordinates are separate counters; local coordinates must never be added to final position twice.
- Non-linearized stream filtering, dictionary visibility, child discovery, and output numbering occur at emission after the actual `willFilterStream` decision; no payload-producing `stream_parameters_removed` prewalk or plan-wide `CachedStreamOutput` payload cache remains.
- PCLm is non-linearized and uses the full writer stream policy, including `isDataModified`, metadata handling, compression/normalization, and `will_retry=true`; its initial page/content/image/synthetic/root order is retained but its complete child prewalk is removed.
- Linearized pass 1/pass 2 ownership remains out of scope, although linearized callers may receive mechanical serializer-type adapters.
- `WriterOutput::Memory` may own the final output Vec because `get_buffer` promises it; arbitrary Writer and Pipeline sinks must not receive a hidden complete-output Vec.
- Per-stream, per-ObjStm, and xref-stream buffers remain only where qpdf needs length-before-data; each is consumed once by move/direct write, with no `take_buffer()?.to_vec()` duplicate.
- Partial writes retry `Interrupted`, convert `WriteZero` through the existing I/O error boundary, and advance position/digest only for accepted bytes. A terminal sink or segment-finish error never starts a later stream or filter retry.
- Deterministic non-linearized ID hashing is incremental through qpdf's `/ID [` cutoff and excludes the identifier bytes; xref-stream payload after that cutoff is not hashed. Preserve `/Info` raw bytes and first-NUL truncation.
- `ObjectHandle::is_same_object_as`, object numbering, warning order, progress timing, encryption key lifetime, stream retry behavior, xref forms, trailer order, and output bytes remain qpdf-compatible.
- Do not modify qtest exceptions, qtest fixtures, CLI public behavior, public APIs, linearized pass ownership, or unrelated writer/linearization memory issues. Do not add raw-object bridges, sentinels, output rewrites, or a second serializer.
- Use `apply_patch` for source/docs edits. Add tests for defensive branches rather than coverage ignores; run focused tests after each changed Rust path.

## File Map

- Create `crates/flpdf/src/writer/output.rs`: crate-private final-output target, counted sink, digest state, segment/document finish contract, and unit tests for partial writes and lifecycle.
- Modify `crates/flpdf/src/writer.rs`: register the output module; adapt `WriterOutputSink` to the target contract; route all non-linearized coordinator/PCLm output through the counted sink; preserve `write_complete` only for linearized output if still required.
- Modify `crates/flpdf/src/writer/object.rs` and `crates/flpdf/src/writer/serialize.rs`: use `OutputSink` for serializer output; retain local Vec only for bounded object/stream payload construction and test adapters.
- Modify `crates/flpdf/src/writer/plain/body.rs`, `plain/mod.rs`, `plain/plan.rs`, and `plain/xref.rs`: remove complete body ownership, migrate emitters/xref/trailer to the final sink, remove stream payload cache, and use dynamic emission-time enqueue.
- Modify `crates/flpdf/src/writer/rewrite_renumber.rs`: remove the non-linearized payload-dependent stream-policy callback/prewalk; retain only planning primitives needed by linearized or object-stream membership setup.
- Modify `crates/flpdf/src/writer/pclm.rs`: retain only PCLm initial ordering, remove complete child prewalk/fixed reference map, and use writer-owned dynamic stream policy/enqueue.
- Modify `crates/flpdf/src/writer/object_streams/emission.rs` only when needed to consume one local ObjStm body into `OutputSink` without a second PDF-wide buffer.
- Create/modify focused writer tests under `crates/flpdf/src/writer.rs`, `writer/plain/*`, `writer/pclm.rs`, and integration allocation/output tests under `crates/flpdf/tests/`.
- Modify `docs/qpdf-correspondence.md` only in the writer rows to record the final sink/coordinate/lifetime mapping and retained qpdf-required local buffers.

---

### Task 1: Add RED streaming, provider-order, ownership, and failure contracts

**Files:**
- Create: `crates/flpdf/tests/writer_streaming_allocation_tests.rs`
- Modify: `crates/flpdf/src/writer.rs` test module
- Modify: `crates/flpdf/src/writer/plain/body.rs` and `crates/flpdf/src/writer/pclm.rs` test modules only when their private helpers are required

**Interfaces:**
- Consumes: public `Pdf`, `PdfWriter`, `ObjectHandle`, stream factories, and crate-private writer test helpers.
- Produces: failing tests that constrain final-sink streaming, provider order, local-buffer ownership, partial writes, and finish/error behavior before serializer migration.

- [x] **Step 1: Build a provider-backed two-stream fixture before output measurement.**

  Use an existing one-page fixture or `Pdf::empty`, attach two indirect streams
  to the live Catalog in a deterministic key order, and register retry-aware
  providers that append `provider:A` and `provider:B` to a shared event log.
  Each provider writes a distinct payload through the supplied pipeline and
  calls `finish`; do not allocate the final output in the provider.

- [x] **Step 2: Add the causal RED test.**

  Install an event-recording arbitrary Writer with `PdfWriter::set_output_writer`
  and run a non-linearized Disable write. Assert that a sink-write event occurs
  between the first and second provider events. The current body-wide Vec route
  requests both providers before its one final `write_all`, so this test must
  fail before the implementation. Assert final bytes are non-empty but do not
  assert an exact write count or chunk size.

- [x] **Step 3: Add terminal-error ordering tests.**

  Add one provider/sink test where the first stream has a terminal error and
  assert the second provider is never requested. Add a separate output-pipeline
  test whose first stream segment `finish` fails and assert later providers are
  not requested. Keep a recoverable `will_retry=true` filter failure test that
  asserts only the same stream is retried; do not conflate it with terminal
  sink/finish failure.

- [x] **Step 4: Add partial-write and memory ownership RED tests.**

  Implement test sinks that return a short positive write, `Interrupted`, and
  `WriteZero`; assert the eventual successful output or exact error category.
  Add a Memory sink test that obtains `get_buffer` once and proves the final
  bytes match the Writer sink output. Add a source-level contract assertion or
  verification script that the non-linearized body result does not expose a
  complete `Vec<u8>` field/return; this must fail against `LiveBodyOutput.bytes`.

- [x] **Step 5: Add peak live-allocation RED evidence.**

  In the dedicated integration binary, install a process-global allocator that
  tracks successful alloc/alloc_zeroed/realloc/dealloc live bytes and peak live
  bytes relative to a fixture baseline. Build a fixed number of equal-size
  provider streams and write to a discard sink. Compare two-stream and
  four-stream peak deltas using a bound derived from one stream's permitted
  local buffer; do not use allocator addresses or platform-specific absolute
  RSS thresholds. The current whole-output Vec must scale with total emitted
  payload and fail the relational assertion.

- [x] **Step 6: Run and record RED.**

  Run the new focused integration and writer tests. Record the expected RED
  failures, exact current whole-body behavior, and all baseline successes in
  `.superpowers/sdd/2026-09-12-writer-streaming/task-1-report.md`.

- [x] **Step 7: Commit only the RED tests.**

  ```bash
  git add crates/flpdf/tests/writer_streaming_allocation_tests.rs crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain/body.rs crates/flpdf/src/writer/pclm.rs
  git commit -m "test: pin non-linearized writer streaming contracts"
  ```

---

### Task 2: Introduce counted output and lifecycle primitives

**Files:**
- Create: `crates/flpdf/src/writer/output.rs`
- Modify: `crates/flpdf/src/writer.rs`

**Interfaces:**
- Consumes: existing `WriterOutputSink`, `WriterOutput::{Writer,Pipeline,Memory}`, and qpdf `Pl_Count`/`PipelinePopper` semantics.
- Produces: `pub(crate) trait OutputTarget`, `pub(crate) struct OutputSink<'a>`, `OutputSink::write_bytes`, `position`, `last_byte`, `begin_digest`, `take_digest`, `finish_segment`, and `finish_document` for all writer routes.

- [x] **Step 1: Define the target/lifecycle contract.**

  Add an internal target trait with fallible chunk writes plus distinct
  `finish_segment` and `finish_document` operations. Implement it for
  `WriterOutputSink`: Writer/file targets flush at a segment/document boundary,
  Pipeline targets call `Pipeline::finish` at a segment boundary and again only
  through the existing document-finalization contract, and Memory targets do
  not finish early. Preserve `WriterOutputSink.failure` so outer writer code
  can return the original file/Pipeline error rather than a generic wrapper.

- [x] **Step 2: Implement accepted-byte counting.**

  `OutputSink` must loop over the target's partial writes, retry only
  `ErrorKind::Interrupted`, convert zero progress to `WriteZero`, and update
  checked `u64` final position and `last_byte` only after bytes are accepted.
  Counter overflow returns the existing qpdf-shaped unsupported/I/O error
  boundary. Do not use local buffer length as the final position.

- [x] **Step 3: Implement incremental digest state.**

  Add an optional MD5 state to the final sink. When enabled, every accepted
  final-output byte updates it; `suspend_digest` is called immediately after
  writing the `/ID [` opening marker and before writing identifier bytes.
  Local stream/ObjStm/xref buffers never update this final digest. Provide a
  fallible helper for the writer's qpdf-shaped second MD5 seed using existing
  `/Info` raw bytes and first-NUL truncation.

- [x] **Step 4: Add unit tests for the primitive.**

  Test short writes, `Interrupted`, `WriteZero`, position overflow, last-byte
  tracking, segment finish versus document finish counts, finish failure, and
  digest cutoff. Use a test `VecOutputTarget` only as a target adapter; do not
  introduce a second serializer or a complete-output buffer inside
  `OutputSink`.

- [x] **Step 5: Run primitive tests and commit.**

  ```bash
  cargo test -p flpdf --lib writer::output
  cargo fmt --all -- --check
  git diff --check
  git add crates/flpdf/src/writer/output.rs crates/flpdf/src/writer.rs
  git commit -m "refactor: add counted writer output sink"
  ```

---

### Task 3: Migrate the canonical serializer to OutputSink

**Files:**
- Modify: `crates/flpdf/src/writer/object.rs`
- Modify: `crates/flpdf/src/writer/serialize.rs`
- Modify: `crates/flpdf/src/writer/object_streams/emission.rs` when its callback type requires adaptation
- Adapt mechanical linearized callers in `crates/flpdf/src/linearization/` without changing their pass ownership

**Interfaces:**
- Consumes: `OutputSink` from Task 2 and existing `ObjectWriterEmission` methods.
- Produces: the same crate-private serializer methods with `&mut OutputSink` output, while test-only callers use a target-backed sink.

- [x] **Step 1: Change ObjectWriterEmission signatures once.**

  Replace production `out: &mut Vec<u8>` parameters and string-writer
  callbacks with `out: &mut OutputSink`. Keep method names and argument order
  unchanged so writer ownership remains one canonical surface. Update every
  implementation and caller in the writer and linearized modules; do not add
  parallel `*_to_sink`/`*_to_vec` production methods.

- [x] **Step 2: Convert serializer writes to fallible sink writes.**

  Replace `push`, `extend_from_slice`, and `write!` calls for final output with
  `OutputSink::write_bytes`/format helpers. Preserve qpdf null-key visibility,
  dictionary ordering, encryption flags, QDF indentation, stream framing, and
  dynamic child callback order. A serializer error must stop before any next
  provider request.

- [x] **Step 3: Keep local buffers explicit and move-owned.**

  For stream payload, ObjStm member body, and xref-stream payload helpers, wrap
  their local Vec in a target-backed sink only while constructing that local
  payload. Replace every `take_buffer()?.to_vec()` with the direct moved Vec or
  one sink write. Do not pass local coordinates into the final `OutputSink`
  counter until the payload is actually emitted.

- [x] **Step 4: Adapt test helpers and linearized callers.**

  Update existing serializer tests to construct a target-backed `OutputSink`
  and compare its Vec. Linearization may retain its own pass buffers and
  back-patch regions, but must use the migrated serializer surface through a
  local sink adapter; no linearized ownership redesign is allowed here.

- [x] **Step 5: Run serializer and writer focused tests.**

  ```bash
  cargo test -p flpdf --lib writer::object
  cargo test -p flpdf --lib writer::serialize
  cargo test -p flpdf --lib writer::plain::body
  cargo test -p flpdf --lib linearization
  cargo fmt --all -- --check
  git diff --check
  ```

- [x] **Step 6: Commit the canonical serializer migration.**

  ```bash
  git add crates/flpdf/src/writer/object.rs crates/flpdf/src/writer/serialize.rs crates/flpdf/src/writer/object_streams/emission.rs crates/flpdf/src/linearization
  git commit -m "refactor: serialize writer objects through counted sink"
  ```

---

### Task 4: Make the plain live queue the all-mode non-linearized body owner

**Files:**
- Modify: `crates/flpdf/src/writer/plain/body.rs`
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/src/writer/plain/xref.rs`
- Modify: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/writer/rewrite_renumber.rs`

**Interfaces:**
- Consumes: `OutputSink`, migrated serializer, object-stream membership plans, and existing `LiveQueue`/`WriteObject` primitives.
- Produces: non-linearized live body/xref/trailer functions that write to the final sink and return only `WriterResult`/layout metadata.

#### Staged execution

Because this task crosses planning, emission, object streams, and xref
ownership, execute it as two reviewable stages while preserving the same
acceptance contract. Stage A completes Steps 1–2 and the emission-time stream
child-discovery/numbering portion of Step 3 using the existing body buffer;
this prevents a planned route from guessing the surviving dictionary while
keeping final-output ownership unchanged. Stage B completes the final-sink
portion of Step 3, Steps 4–7, and the complete-PDF ownership removal. Neither
stage may claim Task 4 complete until both stages have passed their scoped
review.

- [ ] **Step 1: Extract or generalize LiveQueue without changing its proven Disable order.**

  Retain root/trailer seed order, first-enqueue numbering, source ownership
  checks, null dictionary visibility, and dynamic child callback behavior from
  `flpdf-3yn9.48.53`. Add planned Preserve/Generate container membership and
  QDF stream-length-holder reservation as setup metadata. Keep object-stream
  member numbers reserved when a container is first enqueued, matching
  `QPDFWriter.cc:1057-1069,1939-2006`.

- [ ] **Step 2: Remove payload-dependent planning callbacks.**

  Delete the non-linearized use of `StreamParametersRemoved` and the
  `stream_parameters_removed` callback from `CanonicalCatalogFirstRenumber`
  and `PlainWritePlan`. Keep any linearized-only policy probe separate and
  explicitly named. Remove `CachedStreamOutput` payload data and its
  plan-wide fingerprint cache from the non-linearized plan.

- [ ] **Step 3: Emit normal objects directly to OutputSink.**

  Change `LiveObjectEmitter` to hold `&mut OutputSink`; record each object
  offset from `position()` immediately before framing, run the actual stream
  filter policy, and call the dynamic enqueue callback only after the surviving
  stream dictionary is known. Write object and length-holder framing directly
  to the final sink. Preserve progress-before-unparse and qpdf encryption key
  setup/clear order.

- [ ] **Step 4: Emit ObjStm containers with local-only bodies.**

  Build one ObjStm member/pair body in its local buffer, calculate `/First` and
  pair offsets in that buffer's coordinate system, write the container header
  and moved payload to the final sink, then drop the local buffer before the
  next unrelated container. Do not append container bytes to a body-wide Vec.
  Ensure member dynamic references are queued during the same emission.

- [ ] **Step 5: Stream classic xref, xref-stream object, trailer, and EOF.**

  Record the xref offset from `OutputSink::position()`. Write classic rows and
  trailer entries directly. For xref streams, retain only the encoded xref
  payload required to know `/Length`; register the xref object's final offset
  before writing its dictionary, and never include its post-ID payload in the
  deterministic digest. Preserve `/W`, `/Index`, compression, and removed-row
  semantics.

- [ ] **Step 6: Make the source-level and causal RED tests GREEN.**

  Rerun Task 1 provider-order, terminal-failure, partial-write, Memory, and
  source guard tests. Add assertions that body bytes reach the sink before a
  later provider and that xref offsets parse successfully. Do not weaken a RED
  assertion to a write-count-only check.

- [ ] **Step 7: Run plain mode differential tests and commit.**

  ```bash
  cargo test -p flpdf --lib writer::plain
  cargo test -p flpdf --test writer_streaming_allocation_tests -- --nocapture
  cargo test -p flpdf --test cmp_diff_zero_tests --features qpdf-zlib-compat
  cargo fmt --all -- --check
  git diff --check
  git add crates/flpdf/src/writer/plain crates/flpdf/src/writer/rewrite_renumber.rs
  git commit -m "refactor: stream plain writer bodies and xrefs"
  ```

---

### Task 5: Remove planned payload retention and migrate PCLm dynamic discovery

**Files:**
- Modify: `crates/flpdf/src/writer/pclm.rs`
- Modify: `crates/flpdf/src/writer.rs` non-linearized PCLm path
- Modify: `crates/flpdf/src/writer/plain/plan.rs` residual plan structures
- Modify: `crates/flpdf/src/writer/plain/body.rs` stream policy helpers

**Interfaces:**
- Consumes: all-mode live queue, `OutputSink`, writer-owned `willFilterStream` policy, and PCLm initial ordering plan.
- Produces: PCLm/non-linearized plan metadata with no complete child prewalk or multi-stream payload cache.

- [ ] **Step 1: Reduce PCLm Plan to initial enqueue ordering.**

  Retain qpdf's page/content/image/synthetic/root seed order and output-number
  reservation, but remove the `collect_canonical_children` loop and fixed map
  that prewalks every source object. The emission queue discovers indirect
  children after actual object/stream serialization. Keep the existing late
  trailer-reference behavior and synthetic image-strip ordering.

- [ ] **Step 2: Implement full PCLm willFilterStream policy.**

  Replace `get_raw_stream_data` with the canonical writer stream helper that
  considers `is_data_modified`, `filter_on_write`, metadata cleartext,
  normalization, compression, filter retries, and the first `will_retry=true`
  provider call. Keep one local stream payload buffer and consume it once into
  `OutputSink`. Preserve PCLm's forced uncompressed output settings where qpdf
  actually sets them in `doWriteSetup`.

- [ ] **Step 3: Migrate PCLm body/xref/trailer to final sink.**

  Replace the main `bytes: Vec<u8>` with `OutputSink`, use its position for
  offsets, stream each item in qpdf order, and write late trailer references
  without reassembling a complete body. Any direct-root serialization remains
  a bounded value/entry buffer, not a PDF-wide output Vec.

- [ ] **Step 4: Add PCLm causal/retry/failure tests.**

  Test provider order, `false` with filtering disabled (no unconditional
  retry), recoverable filter retry, segment-finish failure, sink failure,
  late trailer references, and deterministic-ID output. Compare PCLm bytes and
  exit behavior with qpdf 11.9.0.

- [ ] **Step 5: Run and commit PCLm checks.**

  ```bash
  cargo test -p flpdf --lib writer::pclm
  cargo test -p flpdf --lib writer::output
  cargo fmt --all -- --check
  git diff --check
  git add crates/flpdf/src/writer/pclm.rs crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain
  git commit -m "refactor: stream PCLm writer output"
  ```

---

### Task 6: Route every remaining non-linearized fallback and finalize ID/finish semantics

**Files:**
- Modify: `crates/flpdf/src/writer.rs` non-linearized coordinator and fallback body loops
- Modify: `crates/flpdf/src/writer/plain/xref.rs` deterministic ID writer
- Modify: `crates/flpdf/src/writer/serialize.rs` final stream framing helpers
- Modify: `crates/flpdf/src/writer/object.rs` final sink/dynamic child helpers
- Modify: relevant non-linearized writer tests

**Interfaces:**
- Consumes: all-mode plain/PCLm sink paths and `OutputSink` digest/lifecycle contract.
- Produces: no non-linearized `writer.rs` body aggregation or `write_complete` call; exact qpdf deterministic ID, encryption, segment finish, and sink-error behavior.

- [ ] **Step 1: Inventory every remaining non-linearized Vec.**

  Search `writer.rs`, `plain`, `pclm`, `object`, `serialize`, and `object_streams`
  for body-sized `Vec<u8>`, `body.bytes`, `emit_bodies` complete-output returns,
  `write_complete`, `split_off`, and `take_buffer()?.to_vec()`. Classify each
  result as final Memory sink, one object/stream/container local buffer, or
  prohibited whole-document retention. Remove or source-cite every prohibited
  result before continuing.

- [ ] **Step 2: Route encryption/extra-header/forced-version fallbacks.**

  Make the non-plain coordinator use the same final `OutputSink` and live
  emission queue. Preserve output object data-key set/clear, AES length/IV,
  metadata exemption, encrypted input behavior, extra headers, forced version,
  and specialized writer error boundaries. Do not route sink failures through
  stream filter retry or restore a legacy body Vec.

- [ ] **Step 3: Implement incremental deterministic ID at the exact cutoff.**

  Replace flat non-linearized `write_deterministic_id_inline` Vec hashing with
  `OutputSink` digest operations. Write `/ID [` through the digesting sink,
  snapshot the digest, suspend digesting, compute qpdf's second seed MD5, and
  emit ID bytes. Confirm classic xref bytes are included, xref-stream payload
  after the xref dictionary ID is excluded, and EOF/document finish happens
  after ID emission. Leave linearized placeholder/back-patch behavior intact.

- [ ] **Step 4: Enforce qpdf segment/document finish order.**

  Ensure each stream's local/filter/encryption pipeline is finished at the
  qpdf `PipelinePopper` boundary, a finish error stops subsequent provider
  requests, and the final configured sink finishes only after EOF. The digest
  and final position survive segment finish unchanged. Add tests for MD5 pop,
  segment finish count, document finish count, and finish failures.

- [ ] **Step 5: Run all non-linearized route tests.**

  ```bash
  cargo test -p flpdf --lib writer
  cargo test -p flpdf --test writer_streaming_allocation_tests -- --nocapture
  cargo test -p flpdf --test cmp_diff_zero_tests --features qpdf-zlib-compat
  cargo test -p flpdf-cli --test cli_byte_identical --features qpdf-zlib-compat
  cargo test -p flpdf-qtest-tools --test e2e --features qpdf-zlib-compat
  cargo fmt --all -- --check
  git diff --check
  ```

- [ ] **Step 6: Commit the fallback/ID/finish cutover.**

  ```bash
  git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain crates/flpdf/src/writer/object.rs crates/flpdf/src/writer/serialize.rs
  git commit -m "refactor: complete non-linearized writer streaming"
  ```

---

### Task 7: Verify measurements, documentation, coverage, and PR delivery

**Files:**
- Modify: `docs/qpdf-correspondence.md` writer rows
- Read: `.github/workflows/ci.yml`, `scripts/patch-coverage.sh`
- Modify: Beads issue `flpdf-ymuj.4` notes only after committed evidence

**Interfaces:**
- Consumes: all non-linearized sink/queue/serializer commits and focused evidence.
- Produces: final qpdf correspondence, fresh measurement report, rebased Draft PR, Beads evidence, and Ready PR after every required check succeeds.

- [ ] **Step 1: Update writer correspondence from final code.**

  Document qpdf active pipeline/writeStandard mapping, final counted sink,
  separate local stream/ObjStm/xref coordinates, emission-time stream child
  discovery, PCLm full `willFilterStream`, deterministic-ID cutoff, segment vs
  document finish, and the only retained local buffers. Do not claim qpdf has
  a reverse containment index or that all buffers disappeared.

- [ ] **Step 2: Run final local quality gates.**

  ```bash
  cargo fmt --all -- --check
  cargo test --workspace --all-features --quiet
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
  python3 scripts/qpdf-module-docs.py --check
  python3 scripts/check-qpdf-deviation-markers.py --check
  python3 scripts/check-qpdf-route-matrix.py --check
  ```

- [ ] **Step 3: Run the explicit qpdf-zlib CI list.**

  Execute every command in `.github/workflows/ci.yml` under
  `bytes-identical zlib compat (Linux amd64)`, including flpdf, flpdf-cli,
  and flpdf-qtest-tools. Record each command's pass total and keep qtest
  exception handling unchanged.

- [ ] **Step 4: Run fresh allocation and qpdf page measurements.**

  Run the dedicated allocator with `--nocapture`, recording allocation count,
  live bytes, size classes, and retained local-buffer rationale. Generate
  qpdf 11.9.0 one-page, 1,000, 2,000, 5,000, 10,000, and 20,000-page inputs
  outside the repository. Run identical `/usr/bin/time -v` `--check` commands
  for `/usr/bin/qpdf` and release `target/release/flpdf`, subtract each
  executable's one-page RSS, and record wall time, max RSS, exit code, and
  combined stdout/stderr `diff -u` results.

- [ ] **Step 5: Run fresh committed patch coverage.**

  ```bash
  cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
  scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
  ```

  Require 100% of changed executable flpdf lines. Add tests for uncovered
  defensive branches; do not add a coverage ignore for a reachable path.

- [ ] **Step 6: Re-fetch/rebase and rerun affected evidence.**

  ```bash
  git fetch --prune origin
  GIT_EDITOR=true git rebase origin/main
  git status --short
  cargo test -p flpdf --test writer_streaming_allocation_tests -- --nocapture
  cargo test -p flpdf --lib writer
  ```

  Resolve only branch-local mechanical conflicts with `apply_patch`; preserve
  unrelated worktrees. Re-run coverage from the final committed rebased HEAD.

- [ ] **Step 7: Create Draft PR after clean rebase.**

  ```bash
  git push -u origin refactor/flpdf-ymuj-4
  gh pr create --draft --base main --head refactor/flpdf-ymuj-4 --title "perf(writer): stream non-linearized output" --body-file .superpowers/sdd/2026-09-12-writer-streaming/pr-body.md
  PR_NUMBER=$(gh pr view --head refactor/flpdf-ymuj-4 --json number --jq .number)
  ```

  The body names `flpdf-ymuj.4`, exact qpdf source/citations, all
  non-linearized scope, measurements, and verification. It does not claim a
  merge or add a merge plan.

- [ ] **Step 8: Record Beads evidence and graph state.**

  Append the final commit, numeric PR number, allocation/live-byte and RSS/
  time matrix, test totals, review resolution, and patch coverage with
  `bd update flpdf-ymuj.4 --external-ref gh-$PR_NUMBER --append-notes ...`.
  Read back the issue, run `bd dep cycles`, and run `bd dolt push`; require the
  literal `Push complete.`.

- [ ] **Step 9: Wait for every required check and mark Ready.**

  ```bash
  gh pr checks "$PR_NUMBER" --watch --interval 30
  gh pr view "$PR_NUMBER" --json state,isDraft,headRefOid,baseRefOid,mergeStateStatus,statusCheckRollup
  gh pr ready "$PR_NUMBER"
  ```

  Run `gh pr ready` only when every required check, including patch coverage,
  is successful, the PR head equals the rebased branch, `mergeStateStatus` is
  `CLEAN`, and the worktree is clean. Do not merge. After Ready, return to
  `bd ready` and re-check `flpdf-3yn9.48`; `.48.34` remains another session's
  worktree and must not be touched.
