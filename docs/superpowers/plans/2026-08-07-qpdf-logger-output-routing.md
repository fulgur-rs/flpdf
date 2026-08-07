# QPDFLogger Output Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0's shared `QPDFLogger` pipeline router, attach document warnings to it without losing diagnostics, and route supported qpdf-equivalent CLI text and binary output through the logger.

**Architecture:** A cloneable `PipelineHandle` wraps a mutex-protected owned pipeline and preserves shared-pointer identity. A cloneable `QPDFLogger` owns the standard terminals plus mutable info/warn/error/save routes. `PdfOpenOptions` installs that logger and warning policy in the resolver, whose warning operation appends first and emits second. One private logger per CLI invocation supplies every document and consumer, including binary stdout through the save route.

**Tech Stack:** Rust workspace, existing `Pipeline` API, `Arc<Mutex<_>>`, `OnceLock`, Cargo integration tests, qpdf 11.9.0 source and executable oracle, cargo-llvm-cov, Beads, stacked git branches/PRs.

## Global constraints

- qpdf 11.9.0 `QPDFLogger.cc`, `QPDF.cc:487-494`, `QPDFJob.cc`, and observed qpdf output are authoritative.
- Logger contract errors are `crate::Error::Internal`; only raw `PipelineHandle::write` and `finish` return `PipelineResult`.
- Do not expose `PipelineError::Logic` as the logger's public contract.
- Preserve shared pipeline identity; do not replace handles with copied writers, enums, or route names.
- A custom sink remains caller-owned and is never implicitly finished by `QPDFLogger`.
- Preserve the diagnostic collection and append-before-emit order, including when warning output is suppressed or fails.
- Migrate warning integration and delete the corresponding CLI replay in one commit so no stack point duplicates warnings.
- Configure binary stdout before any info write for JSON, stream, attachment, and PDF-to-`-` routes.
- Leave flpdf-only inspection routes without a qpdf `QPDFJob` counterpart unchanged and inventory them explicitly.
- Stop before editing `docs/qpdf-correspondence.md` if the active `flpdf-h8mv` worktree still overlaps that file and cannot be synchronized cleanly.
- Every production behavior starts with a failing test and finishes with fresh 100% changed executable-line coverage.

---

## Stack layer 1: logger core

### Task 1: Make `PlOStream` own its writer generically

**Files:**
- Modify: `crates/flpdf/src/pipeline/ostream.rs`
- Modify/Test: `crates/flpdf/tests/pipeline_public_api.rs`

**Interfaces:**
- Consumes: any `W: Write`, including `&mut W`, `Stdout`, and `Stderr`.
- Produces: `PlOStream<W>` with unchanged sticky write/flush semantics and no drop-time finish.

- [ ] **Step 1: Add a failing owned-writer public API test**

  Construct `PlOStream::new("owned", Cursor::new(Vec::new()))`, call it through `Pipeline`, and assert that it accepts writes and finish. Keep the existing borrowed-writer tests unchanged.

- [ ] **Step 2: Verify RED**

  ```bash
  cargo test -p flpdf --test pipeline_public_api ostream_can_own_a_writer
  ```

  Expected: type mismatch because `PlOStream` currently requires `&mut dyn Write`.

- [ ] **Step 3: Implement the generic ownership boundary**

  Change the struct and impls to `PlOStream<W: Write> { writer: W, ... }`. Keep sticky failures, repeated finish behavior, and lack of a `Drop` impl byte-for-byte equivalent.

- [ ] **Step 4: Verify GREEN and existing behavior**

  ```bash
  cargo test -p flpdf --test pipeline_public_api ostream
  cargo test -p flpdf --lib pipeline::ostream::tests
  ```

### Task 2: Add the shared public `PipelineHandle`

**Files:**
- Modify: `crates/flpdf/src/pipeline.rs`
- Modify/Test: `crates/flpdf/tests/pipeline_public_api.rs`

**Interfaces:**
- Produces: cloneable `PipelineHandle` over `Arc<Mutex<Box<dyn Pipeline + Send>>>`.
- Public methods: constructor from an owned pipeline, `identifier`, `write`, `finish`, and identity comparison used by the logger.

- [ ] **Step 1: Add failing handle tests**

  Cover shared writes from two clones, pointer identity for clones versus distinct handles, downstream logic/runtime error preservation, and mutex-poison recovery without panic.

- [ ] **Step 2: Verify RED**

  ```bash
  cargo test -p flpdf --test pipeline_public_api pipeline_handle
  ```

  Expected: unresolved `PipelineHandle`.

- [ ] **Step 3: Implement the minimal handle**

  Lock only around one pipeline operation. Recover a poisoned mutex with `into_inner()`. Keep `write`/`finish` in `PipelineResult`; do not translate errors here.

- [ ] **Step 4: Verify GREEN**

  ```bash
  cargo test -p flpdf --test pipeline_public_api pipeline_handle
  cargo test -p flpdf --lib pipeline::tests
  ```

### Task 3: Port `QPDFLogger` routes and standard terminals

**Files:**
- Create: `crates/flpdf/src/logger.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create/Test: `crates/flpdf/tests/qpdf_logger_tests.rs`

**Interfaces:**
- Produces: public cloneable `QPDFLogger`, `create`, `default_logger`, route getters/setters, standard terminal getters, `info`/`warn`/`error`, optional save, `save_to_standard_output`, and `set_output_streams`.
- Consumes: `PipelineHandle`, `PlOStream`, and `Discard`.

- [ ] **Step 1: Port the default-route and unset-save RED scenario**

  Use in-memory shared test sinks to assert info→stdout, warn→stderr, error→stderr, and `get_save()` returning `Error::Internal` with the qpdf message while `get_save_if_set()` returns `None`.

- [ ] **Step 2: Run the focused test and verify RED**

  ```bash
  cargo test -p flpdf --test qpdf_logger_tests default_routes
  ```

- [ ] **Step 3: Implement logger state, built-ins, default logger, and delivery**

  Use `Arc<Mutex<LoggerState>>` for cloned logger identity and `OnceLock<QPDFLogger>` for the process-global default. Convert downstream `PipelineError` through the existing `From` implementation. Avoid holding the logger-state mutex during a pipeline write.

- [ ] **Step 4: Port reset/following behavior RED tests**

  Cover discard/reset, warn following error only while unset, independently assigned warn, `set_output_streams`, and same-handle equality.

- [ ] **Step 5: Implement route reset and following semantics; verify GREEN**

  ```bash
  cargo test -p flpdf --test qpdf_logger_tests reset
  cargo test -p flpdf --test qpdf_logger_tests warn_follows_error
  ```

### Task 4: Port tracked stdout and save collision behavior

**Files:**
- Modify: `crates/flpdf/src/logger.rs`
- Modify/Test: `crates/flpdf/tests/qpdf_logger_tests.rs`

- [ ] **Step 1: Add RED tests for qpdf `logger 2` and `logger 3` scenarios**

  Cover stdout used before save, same-save-handle no-op, `only_if_not_set`, save-first info rerouting to stderr, reset while save is stdout, restoration after clearing save, and binary-stdout selection.

- [ ] **Step 2: Verify RED**

  ```bash
  cargo test -p flpdf --test qpdf_logger_tests stdout
  cargo test -p flpdf --test qpdf_logger_tests save
  ```

- [ ] **Step 3: Implement private `PlTrack` and save state machine**

  Mark stdout used on every non-empty or empty write exactly as qpdf does. Check tracked state before assigning stdout. Map collision errors to `Error::Internal`. Reroute info based on pipeline identity, not identifier text.

- [ ] **Step 4: Add custom-sink lifecycle and failure RED tests**

  Assert logger drop does not finish a custom sink; explicit handle finish still works; custom runtime/logic errors become `Error::System`/`Error::Internal` with unchanged text.

- [ ] **Step 5: Verify all layer-1 tests GREEN**

  ```bash
  cargo test -p flpdf --test qpdf_logger_tests
  cargo test -p flpdf --test pipeline_public_api
  ```

### Task 5: Gate and publish stack layer 1

- [ ] **Step 1: Run layer-1 quality gates**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test -p flpdf --test qpdf_logger_tests
  cargo test -p flpdf
  git diff --check
  ```

- [ ] **Step 2: Measure layer-1 changed-line coverage**

  Generate fresh LCOV and run `scripts/patch-coverage.sh --base main`; add tests until no changed executable line is uncovered.

- [ ] **Step 3: Commit and establish the first stack branch**

  Commit only layer-1 implementation/tests. Push `feature/flpdf-qynx.4-qpdf-logger` and create the first draft PR. Create the layer-2 branch from that commit using the repository's stacked-PR flow.

---

## Stack layer 2: document warnings and CLI warning cutover

### Task 6: Add logger policy to `PdfOpenOptions` and resolver state

**Files:**
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/engine.rs`
- Create/Test: `crates/flpdf/tests/pdf_logger_tests.rs`

**Interfaces:**
- Add `logger: Option<QPDFLogger>`, `suppress_warnings: bool`, and `description: String` to `PdfOpenOptions`.
- Store the selected logger, suppression flag, and description in `ResolverCore`.

- [ ] **Step 1: Add RED option/default and identity tests**

  Assert default logger selection, default suppression false, empty description, clone/equality behavior, and an explicitly supplied logger surviving document construction.

- [ ] **Step 2: Verify RED**

  ```bash
  cargo test -p flpdf --test pdf_logger_tests open_options
  ```

- [ ] **Step 3: Thread the fields through construction**

  Update `ResolverHandle::new_shared` and `Pdf::open_with_repair_mode`. Select `QPDFLogger::default_logger()` only when the option is `None`.

- [ ] **Step 4: Verify GREEN and struct-literal fallout**

  ```bash
  cargo test -p flpdf --test pdf_logger_tests open_options
  cargo check --workspace --all-targets
  ```

### Task 7: Make every document warning append then route immediately

**Files:**
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify/Test: `crates/flpdf/tests/pdf_logger_tests.rs`

- [ ] **Step 1: Add RED tests for initial and lazy warning delivery**

  Use a repair fixture and a lazy-resolution fixture. Assert exact `WARNING: <description>[ (offset N)]: <message>\n` bytes, original append order, and unchanged `repair_diagnostics()` snapshots/counts.

- [ ] **Step 2: Add RED tests for suppression and delivery failure**

  Suppression must retain diagnostics while emitting nothing. A failing warn sink must still leave the newly appended diagnostic observable and return the translated error where the calling API can return one.

- [ ] **Step 3: Verify RED**

  ```bash
  cargo test -p flpdf --test pdf_logger_tests warning
  ```

- [ ] **Step 4: Implement append/release-borrow/emit**

  Factor one resolver warning helper taking optional offset. Append under a short core borrow, clone logger/policy/description, release the borrow, format once, then write. Replay initial xref diagnostics exactly once after resolver construction.

- [ ] **Step 5: Verify GREEN and warning-heavy resolver tests**

  ```bash
  cargo test -p flpdf --test pdf_logger_tests warning
  cargo test -p flpdf --lib reader::resolver
  cargo test -p flpdf --test reader_tests
  cargo test -p flpdf --test xref_tests
  ```

### Task 8: Add live document logger and suppression controls

**Files:**
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify/Test: `crates/flpdf/tests/pdf_logger_tests.rs`

- [ ] **Step 1: Add RED live-replacement tests**

  Replace the logger after open, trigger a lazy warning, and assert only the replacement receives it. Toggle suppression around later warnings and assert the collection remains complete.

- [ ] **Step 2: Implement resolver and `Pdf` getters/setters**

  Expose `logger`, `set_logger`, `suppress_warnings`, and `set_suppress_warnings` without leaking resolver borrows across writes.

- [ ] **Step 3: Verify GREEN**

  ```bash
  cargo test -p flpdf --test pdf_logger_tests live
  ```

### Task 9: Install one CLI logger and cut over warning output atomically

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify/Test: `crates/flpdf-cli/tests/cli_tests.rs`
- Modify/Test: warning-focused CLI integration tests that currently assert qpdf warning bytes

**Interfaces:**
- One private `QPDFLogger::create()` per invocation.
- Every qpdf-equivalent document open receives the logger and path description.
- `emit_warnings_since` no longer prints document warnings already routed by the logger.

- [ ] **Step 1: Add RED CLI tests for open and lazy warning uniqueness**

  Assert exact warning stderr bytes occur once, success-with-warnings text and exit status are unchanged, and a clean run emits no warning.

- [ ] **Step 2: Verify RED against the intended immediate-delivery behavior**

  ```bash
  cargo test -p flpdf-cli --test cli_tests warning_logger
  ```

- [ ] **Step 3: Thread a logger through qpdf-equivalent command dispatch and open helpers**

  Build `PdfOpenOptions` with `logger: Some(logger.clone())` and `description` from the input path. Keep flpdf-only inspection commands on their current routes.

- [ ] **Step 4: Remove old warning replay in the same change**

  Retain diagnostic length checks for counts/exit status, but delete direct printing of document warnings from `emit_warnings_since`, `finish_lazy_warnings`, `finish_rewrite_warnings`, JSON error paths, and equivalent qpdf routes.

- [ ] **Step 5: Verify GREEN and no duplicate warnings**

  ```bash
  cargo test -p flpdf-cli --test cli_tests warning
  cargo test -p flpdf-cli --test cli_overlay warning
  cargo test -p flpdf-cli --test cli_qdf warning
  ```

### Task 10: Gate and publish stack layer 2

- [ ] **Step 1: Run focused and crate gates**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test -p flpdf --test pdf_logger_tests
  cargo test -p flpdf
  cargo test -p flpdf-cli --test cli_tests
  cargo test -p flpdf-cli
  git diff --check
  ```

- [ ] **Step 2: Require fresh 100% changed executable-line coverage**

  Measure against the layer-1 branch as base and fill every uncovered branch/line.

- [ ] **Step 3: Commit and publish the second stack layer**

  Push the layer-2 branch, open its dependent draft PR, and create the layer-3 branch from it.

---

## Stack layer 3: CLI info/error/save cutover

### Task 11: Route binary stdout consumers through logger save

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Create/Test: `crates/flpdf-cli/tests/cli_logger_routing.rs`
- Modify/Test: `crates/flpdf-cli/tests/cli_attachment_lifecycle.rs`

**Consumers:** JSON stdout, raw/filtered stream stdout, attachment stdout, and PDF output path `-` where supported.

- [ ] **Step 1: Add RED binary-output tests**

  For every consumer, assert exact stdout bytes, empty/nonempty stderr as appropriate, and exit status. Add a collision case where info is attempted first and must become `Error::Internal` rather than corrupting binary output.

- [ ] **Step 2: Verify RED**

  ```bash
  cargo test -p flpdf-cli --test cli_logger_routing binary
  ```

- [ ] **Step 3: Configure save before document/info work**

  Call `save_to_standard_output(true)` before opening/processing, obtain the save handle, and adapt writers so the consumer writes through that handle. Do not wrap a second independent stdout terminal.

- [ ] **Step 4: Replace direct binary stdout writes**

  Remove scoped `stdout().lock().write_all`, `stdout().write_all(&stream.data)`, attachment `write_all`, and PDF-to-`-` writes only for the declared qpdf-equivalent consumers.

- [ ] **Step 5: Verify GREEN**

  ```bash
  cargo test -p flpdf-cli --test cli_logger_routing binary
  cargo test -p flpdf-cli --test cli_attachment_lifecycle
  cargo test -p flpdf-cli --test cli_tests json
  cargo test -p flpdf-cli --test cli_tests stream
  ```

### Task 12: Route qpdf-equivalent human output and fatal text

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify/Test: `crates/flpdf-cli/tests/cli_logger_routing.rs`
- Modify/Test: existing check/show/rewrite/verbose CLI tests

- [ ] **Step 1: Inventory and classify direct output sites**

  Record every `println!`, `eprintln!`, and direct stdout write as migrated qpdf info/warn/error/save or explicitly retained flpdf-only inspection. Use qpdf `QPDFJob.cc` call sites to settle ambiguous ownership.

- [ ] **Step 2: Add RED clean/warning/error/verbose tests**

  Cover check success text, check warning/error text, rewrite verbose text, attachment listing text, and supported show output. Assert stdout, stderr, ordering, and exit status.

- [ ] **Step 3: Add small logger write helpers and migrate sites**

  Route human stdout through `info`, warning bytes through `warn`, and fatal qpdf-equivalent diagnostics through `error`. Preserve exact prefixes/newlines; do not normalize byte strings through lossy UTF-8 when an existing route is byte-oriented.

- [ ] **Step 4: Verify GREEN for focused consumers**

  ```bash
  cargo test -p flpdf-cli --test cli_logger_routing text
  cargo test -p flpdf-cli --test cli_tests check
  cargo test -p flpdf-cli --test cli_tests verbose
  cargo test -p flpdf-cli --test cli_attachment_lifecycle list
  ```

### Task 13: Add qpdf differential coverage

**Files:**
- Modify/Test: `crates/flpdf-cli/tests/cli_logger_routing.rs`
- Modify/Test: existing qpdf compatibility tests where reuse is clearer

- [ ] **Step 1: Add an oracle runner that captures all three observables**

  Run pinned/system qpdf 11.9.0 and flpdf with matched arguments and compare raw stdout, raw stderr, and numeric exit status.

- [ ] **Step 2: Cover the accepted matrix**

  Include clean/warning/error completion, JSON stdout, raw/filtered stream, attachment, save-first info rerouting, and file/custom output not marking standard stdout used.

- [ ] **Step 3: Run the differential target**

  ```bash
  cargo test -p flpdf-cli --test cli_logger_routing qpdf_differential -- --nocapture
  ```

  Classify each mismatch as oracle mismatch, fixture/precondition mismatch, or intentionally out-of-scope flpdf-only behavior before changing code.

### Task 14: Update correspondence only after overlap resolution

**Files:**
- Modify: `docs/qpdf-correspondence.md`
- Modify: design/implementation inventory only if the final retained-route list differs from the approved scope

- [ ] **Step 1: Recheck active worktree overlap**

  ```bash
  git worktree list --porcelain
  git -C /home/ubuntu/flpdf/.worktrees/flpdf-h8mv-decodeparms-null-keys status --short
  ```

  If `docs/qpdf-correspondence.md` remains modified there, synchronize its landed commit or stop and report the overlap. Never overwrite it.

- [ ] **Step 2: Update component correspondence and retained-route inventory**

  Mark `QPDFLogger.cc` as the logger module plus warning/CLI consumers, cite exact source ranges, and list flpdf-only direct-output sites intentionally retained.

- [ ] **Step 3: Run documentation/source checks**

  ```bash
  cargo test -p flpdf --doc
  git diff --check
  ```

### Task 15: Final verification, review, Beads closure, and publication

- [ ] **Step 1: Run all repository quality gates fresh**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test -p flpdf --test qpdf_logger_tests
  cargo test -p flpdf --test pdf_logger_tests
  cargo test -p flpdf-cli --test cli_logger_routing
  cargo test --workspace
  git diff --check
  ```

- [ ] **Step 2: Prove changed-line coverage**

  Generate fresh workspace LCOV and run `scripts/patch-coverage.sh --base <layer-2-branch>` for layer 3 and `--base main` for the complete stack. Require zero uncovered changed executable lines.

- [ ] **Step 3: Inspect the complete stack and request review**

  Review every `main...HEAD` diff, direct-output inventory, test fixture, and qpdf citation. Use `superpowers:requesting-code-review`; address findings with source/oracle evidence.

- [ ] **Step 4: Commit and push the third layer**

  Commit only the approved scope, push the dependent branch, open/update the final draft PR, and verify all remote checks are green. Do not merge unless the user explicitly asks.

- [ ] **Step 5: Persist and close Beads only when acceptance is demonstrated**

  Attach focused tests, differential evidence, workspace gates, coverage, and PR links to `flpdf-qynx.4`; close it when the implementation is complete and dependencies remain satisfied. Run `bd dolt push`, then confirm git and Beads pushes succeeded before handoff.
