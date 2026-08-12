# qpdf StreamDataProvider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan task-by-task. Each stacked layer must remain independently testable.

**Goal:** Port qpdf 11.9.0's deferred `StreamDataProvider` contract and provider-backed stream source into flpdf's canonical `ObjectHandle` graph.

**Architecture:** Keep qpdf's three exclusive stream sources—replaced buffer, provider, and original parsed source—inside the canonical stream value. Add the provider contract and registration boundary first, then connect the provider branch to the already-ported `.3yn9.6` pipeline/count/retry execution boundary. Keep EmbeddedFile migration downstream in `.25kg.4.4`.

**Tech Stack:** Rust workspace, `Rc<dyn StreamDataProvider>`, existing `Pipeline`/`PipelineError`, canonical `ObjectHandle`, qpdf 11.9.0 source and executable oracle.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative: `QPDFObjectHandle.hh:68-127`, `QPDFObjectHandle.cc:48-90,1365-1428`, `QPDF_Stream.cc:571-620,640-685`, `QPDFEFStreamObjectHelper.cc:102-107`.
- Provider registration is lazy and must not invoke or materialize the provider.
- Buffer and provider sources are mutually exclusive; no empty-`Vec` sentinel or direct-stream adapter.
- `None` filter/decode parameters preserve keys; an explicit null handle removes keys through canonical `replace_key`.
- Provider failures use object-layer `Error::Internal`/`Error::System`; do not expose `PipelineResult` as the provider contract and do not panic for qpdf's default virtual errors.
- `.3yn9.6` owns filter construction, `Pl_Count`, retry/length policy, and raw retry handoff; `.25kg.4.4` owns Filespec migration.
- Every stacked PR runs its own RED→GREEN focused tests, `cargo fmt --all -- --check`, strict clippy/rustdoc gates, and fresh changed-line coverage.

## Stacked layer map

1. Design/specification (this PR): qpdf facts, boundaries, API shape, test matrix.
2. Oracle probe and RED tests: pin observable provider behavior before implementation.
3. Contract/storage: provider trait, callback adapters, third source field, replacement and ownership rules.
4. Pipe integration: provider source dispatch through the existing pipeline with count/retry/length behavior.
5. EmbeddedFile consumer: separate `.25kg.4.4` issue and PR stack.

### Task 1: Provider oracle and contract tests

**Files:**
- Create: `tests/oracle/qpdf_stream_data_provider_probe.cc`
- Create: `scripts/qpdf-stream-data-provider-probe.sh`
- Modify: `crates/flpdf/src/object_handle.rs` tests near the existing stream source tests

**Interfaces:**
- Consumes: qpdf `QPDF::newStream`, `QPDFObjectHandle::replaceStreamData`, `StreamDataProvider`, and `QPDF_Stream` source behavior.
- Produces: exact probe output and RED Rust tests for delayed registration, repeated calls, identity forwarding, default-method errors, and source replacement.

- [ ] Write the qpdf probe first. Construct an empty stream, register a provider, verify registration does not call it, pipe it twice, record the same `QPDFObjGen` and bytes, verify provider `/Length` handling, and exercise both legacy and retry-aware forms.
- [ ] Run `scripts/qpdf-stream-data-provider-probe.sh` against the pinned qpdf. Expected initial result is a probe failure only for the not-yet-recorded assertions; do not weaken assertions to accommodate flpdf.
- [ ] Add Rust tests that define providers recording call count and `ObjectRef`, providers returning retry false, providers returning `PipelineError::logic/runtime`, and providers using default trait methods. Assert the qpdf-compatible `Error` category and exact default message.
- [ ] Run the focused tests and record the RED failures before adding implementation.
- [ ] Commit only probe and tests with `test(qpdf): probe StreamDataProvider contract`.

### Task 2: Provider contract and stream storage

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/lib.rs` to re-export the public provider contract
- Test: `crates/flpdf/src/object_handle.rs`

**Interfaces:**
- Consumes: `ObjectRef`, `Pipeline`, existing `replace_filter_data`, and `.3.21`'s owned empty stream factory.
- Produces: `StreamDataProvider`, `ObjectValue::Stream.stream_provider`, `replace_stream_data_provider`, and callback adapters with qpdf delegation semantics.

- [ ] Add the provider trait with `ObjectRef` and `(object_number, generation)` forms, retry-aware forms, `supports_retry`, and default methods that return `Error::Internal` with qpdf's message.
- [ ] Add RED assertions for legacy-to-ObjGen delegation and retry flag forwarding, then implement the minimal default delegation without using panic or `PipelineResult` in the public provider contract.
- [ ] Add `stream_provider: Option<Rc<dyn StreamDataProvider>>` to the stream value and clone it with the rest of the qpdf stream source state.
- [ ] Implement provider replacement so it clears `stream_data`, calls the shared filter/length boundary with zero length, and never calls the provider during registration. Implement buffer replacement so it clears the provider.
- [ ] Use `Some(ObjectHandle::null())` for explicit direct-null key removal and `None` for qpdf uninitialized preservation; route both through canonical dictionary mutation.
- [ ] Implement qpdf's void and retry-aware function-provider adapters without collecting output into a `Vec`.
- [ ] Run contract/storage tests and commit `feat(object-handle): add qpdf stream provider source`.

### Task 3: Provider pipe integration

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/reader/resolver.rs` at the existing `ObjectHandle` pipe handoff for provider source dispatch
- Test: `crates/flpdf/src/object_handle.rs`, `crates/flpdf/src/reader/resolver.rs`

**Interfaces:**
- Consumes: `stream_provider`, the completed `.3yn9.6` filter pipeline, `Pipeline`, `suppress_warnings`, and `will_retry`.
- Produces: qpdf source order `buffer -> provider -> no-data -> original`, counted provider output, length validation/update, retry and warning propagation.

- [ ] Add RED tests for provider output through an unfiltered sink, provider output through the existing decoder chain, repeated calls, existing `/Length` match/mismatch, absent `/Length` update, and retry-aware false/success results.
- [ ] Implement the provider branch after the replaced-buffer branch and before the parsed-offset-zero branch. Pass the stream's `ObjectRef` unchanged; do not look it up through legacy `Object` or `Pdf::resolve`.
- [ ] Reuse the existing `.3yn9.6` `Pipeline`/count/error boundary. Do not create a second filter construction path or collect provider output before piping.
- [ ] Preserve qpdf's branch distinction: a retry-aware `false` clears filtering success and permits the existing raw retry; a provider exception/error remains an object-layer error.
- [ ] Add repeated-write tests proving the provider is not consumed, mutated, or replaced by the first call.
- [ ] Run `cargo test -p flpdf --lib object_handle`, `cargo test -p flpdf --lib reader`, the qpdf probe, and writer tests; commit `feat(stream): pipe qpdf provider-backed sources`.

### Task 4: Verification and handoff

**Files:**
- Modify: `docs/qpdf-correspondence.md` for the provider row and approved internal representation note
- Test: existing qpdf compatibility and writer suites

- [ ] Add a correspondence entry mapping qpdf's provider ownership and source dispatch to the Rust trait/field, recording `Rc` as an internal container substitution only where it preserves qpdf processing order and observable behavior.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo test -p flpdf --lib object_handle` and `cargo test -p flpdf --lib reader` for the provider and resolver coverage.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` and strict private-item rustdoc.
- [ ] Run the relevant writer, qpdf differential, and workspace tests, then fresh per-PR changed-line coverage.
- [ ] Verify `git diff --check`, `bd dep cycles`, `bd dolt push`, and the stacked PR base/head chain before handoff.
