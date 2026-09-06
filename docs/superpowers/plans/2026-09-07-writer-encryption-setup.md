# Shared qpdf Writer Encryption Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task with review checkpoints.

**Goal:** Make explicit/copy/preserve encryption and writer setup one qpdf-shaped state shared by standard and linearized output routes.

**Architecture:** Normalize all writer settings in `prepared_write_options`, then build a `WriterSetupState` once before route dispatch. Split encryption parameter construction from route-specific `/Encrypt` slot assignment, and pass the same immutable parameter state into standard and linearized consumers.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source oracle, `EncryptionContext`, `WriterOptions`, Cargo tests, qpdf differential fixtures, rustdoc, Clippy, patch coverage.

**Spec:** `docs/superpowers/specs/2026-09-07-writer-encryption-setup-design.md`

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- Reuse the existing encryption builders; do not reimplement Standard security handlers.
- Keep qtest exceptions `.48.45` out of scope.
- Preserve route-specific Encrypt slot numbers while sharing parameter state.
- Do not add a legacy bridge or merge the eventual PR.

---

### Task 1: RED tests for one-time setup and parameter/slot separation

**Files:**
- Modify: `crates/flpdf/src/writer.rs` test module
- Modify: `crates/flpdf/src/linearization/writer.rs` focused tests if required for slot evidence
- Test: existing encrypted rewrite and linearization parity fixtures

**Interfaces:**
- Consumes: current `prepared_write_options`, explicit/copy/preserve options, and existing encryption builders.
- Produces: failing tests that require a shared setup object and prove no route rebuilds encryption parameters or ID material.

- [ ] **Step 1: Add a test-only setup API expectation.**

  Add focused tests for explicit encryption, copy encryption, and source-preservation precedence. Assert the prepared option set disables preservation for explicit encryption and for QDF/content-normalization/decode/PCLm modes, while a compatible encrypted source selects copy parameters. Add a test that the setup exposes parameter state without an assigned Encrypt slot and that assigning two route slots preserves the same `/V`, `/R`, `/O`, `/U`, file-key identity, and ID0.

- [ ] **Step 2: Add RED output cases.**

  Extend the existing qpdf-compatible encrypted standard and linearized tests with assertions for the output Encrypt object number and generation order. Use fixed V5 randomness/static AES-IV fixtures where available so the expected state is deterministic.

- [ ] **Step 3: Run focused tests and verify RED.**

  ```bash
  cargo test -p flpdf --lib writer::encryption_setup
  cargo test -p flpdf --test encrypt_cli_tests --features qpdf-zlib-compat
  ```

  Expected: the setup API is absent or the new assertions expose route-local context/slot construction.

- [ ] **Step 4: Commit RED tests.**

  ```bash
  git add crates/flpdf/src/writer.rs crates/flpdf/src/linearization/writer.rs
  git commit -m "test(writer): pin one-time qpdf encryption setup"
  ```

### Task 2: Split parameter state from route-specific Encrypt slots

**Files:**
- Modify: `crates/flpdf/src/writer.rs` encryption state and builders
- Test: focused setup tests from Task 1

**Interfaces:**
- Consumes: existing `build_encryption_context`, `build_copy_encryption_context`, `WriterOptions`, source/donor ID0, metadata resolution.
- Produces: `EncryptionParameters`, `EncryptionContext::with_encrypt_ref`, and `WriterSetupState` with one-time random/key/dictionary construction.

- [ ] **Step 1: Add `EncryptionParameters` without an output slot.**

  Move the existing context fields except `encrypt_ref` into an internal parameter state. Keep the existing Standard and copy builders as the only dictionary/key implementations, changing them to return parameter state. Add an explicit `with_encrypt_ref(ObjectRef)` transition that creates the route-owned context.

- [ ] **Step 2: Build the common setup state.**

  Add a `build_writer_setup` function that receives normalized options and the live PDF, captures the source/donor ID0 once, creates the generated ID material once for non-deterministic writes, resolves cleartext metadata once, and calls the existing explicit/copy builder once. Preserve the existing deterministic-ID-plus-encryption error before any encryption state is constructed.

- [ ] **Step 3: Run the focused setup tests and verify GREEN.**

  ```bash
  cargo test -p flpdf --lib writer::encryption_setup
  cargo test -p flpdf --lib writer::encrypted_strings
  ```

- [ ] **Step 4: Commit the state split.**

  ```bash
  git add crates/flpdf/src/writer.rs
  git commit -m "refactor(writer): split encryption parameters from output slot"
  ```

### Task 3: Move setup to the common PdfWriter lifecycle and migrate standard output

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Test: `crates/flpdf/src/writer.rs` lifecycle/setup tests and encrypted standard fixtures

**Interfaces:**
- Consumes: `WriterSetupState` and `EncryptionParameters` from Task 2.
- Produces: `PdfWriter::write` setup-before-dispatch and standard full-rewrite context allocation without a second builder call.

- [ ] **Step 1: Add the setup state to `PdfWriter::write`.**

  Build it after `prepared_write_options` and before `initialize_special_streams`/`prepare_file_for_write`. Pass it into the standard and linearized route entrypoints. Keep the existing one-shot write and progress ordering.

- [ ] **Step 2: Replace standard route-local builder calls.**

  Remove the standard route's direct `build_encryption_context`/copy-builder calls. After standard body/renumber planning, call `setup.encryption.with_encrypt_ref` using the existing qpdf standard output slot. Use the setup's generated ID state when building the trailer; do not draw a second random ID.

- [ ] **Step 3: Run standard RED/GREEN parity.**

  ```bash
  cargo test -p flpdf --test encrypt_cli_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test encrypted_rewrite_tests --features qpdf-zlib-compat
  cargo test -p flpdf --lib writer::encryption_setup
  ```

- [ ] **Step 4: Commit standard cutover.**

  ```bash
  git add crates/flpdf/src/writer.rs
  git commit -m "refactor(writer): use shared encryption setup in standard route"
  ```

### Task 4: Migrate linearized route to the same setup state

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`
- Modify: `crates/flpdf/src/writer.rs` route handoff types
- Test: linearized encrypted/copy-encryption tests and qpdf byte/structural fixtures

**Interfaces:**
- Consumes: common `WriterSetupState`, finalized ID policy, and `EncryptionParameters::with_encrypt_ref`.
- Produces: linearized output that reserves its qpdf-specific Encrypt slot from the shared parameters without rebuilding the dictionary/key or overwriting a second context.

- [ ] **Step 1: Use common generated ID inputs.**

  Keep deterministic linearized pass-1 placeholder/digest behavior, but make the non-deterministic/static/copy path consume the setup's one generated ID material. Preserve the existing two-pass byte layout and hint behavior.

- [ ] **Step 2: Replace linearized route-local builder calls.**

  Remove the second `build_encryption_context`/copy-builder call. After linearized local renumbering determines the qpdf slot, create the route context from the shared parameters and thread it through both passes and the Encrypt dictionary emission.

- [ ] **Step 3: Run linearized RED/GREEN parity.**

  ```bash
  cargo test -p flpdf --test cli_byte_identical --features qpdf-zlib-compat
  cargo test -p flpdf --test encrypt_cli_tests --features qpdf-zlib-compat
  cargo test -p flpdf --lib linearization::writer
  ```

- [ ] **Step 4: Commit linearized cutover.**

  ```bash
  git add crates/flpdf/src/writer.rs crates/flpdf/src/linearization/writer.rs
  git commit -m "refactor(writer): share encryption setup with linearization"
  ```

### Task 5: Document D1/D16 ownership and complete gates

**Files:**
- Modify: `docs/qpdf-route-matrix/d-writer.md`
- Modify: `docs/qpdf-correspondence.md`

- [ ] **Step 1: Update D1/D16.**

  Record the common setup owner, one-time parameter construction, route-specific slot allocation, exact qpdf citations, and remaining out-of-scope qtest exceptions. Do not claim the old builders are duplicated after the cutover.

- [ ] **Step 2: Run documentation checks.**

  ```bash
  python3 scripts/check-qpdf-route-matrix.py
  python3 scripts/check-qpdf-deviation-markers.py --check
  ```

- [ ] **Step 3: Run full verification.**

  ```bash
  cargo fmt --all -- --check
  RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace
  cargo test --workspace --features qpdf-zlib-compat
  scripts/patch-coverage.sh --base origin/main
  ```

- [ ] **Step 4: Rebase, push, Draft PR, CI, Ready, Beads.**

  Rebase onto latest `origin/main`, rerun all gates, push, create a Draft PR, wait for every required CI check including patch coverage, mark Ready only after all pass, then append PR/head/tests to Beads, run `bd dep cycles`, `bd dolt push` with `Push complete.`, and push Git state. Do not merge.

