# QPDFWriter Data-Key State Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a qpdf-faithful writer-owned emitted-object data-key lifecycle without migrating production consumers.

**Architecture:** A focused `writer::encryption_state` module owns qpdf's encryption configuration and current key. It delegates V<5 derivation to the existing Algorithm 3.1 primitive, uses the V>=5 file key directly, and wraps top-level emission callbacks with set/clear while omitting per-member keys inside ObjStm containers.

**Tech Stack:** Rust workspace, existing `security::standard::per_object_key`, Cargo tests, llvm-cov, qpdf 11.9.0 source oracle.

## Global Constraints

- Use emitted/renumbered object number and generation 0, never the source `ObjectRef`.
- Do not mutate/materialize legacy `Object`, add encrypted-string tags, use sentinels, or panic.
- Do not add string/stream crypto, metadata exemption, Encrypt dictionary construction, production cutover, or linearization layout.
- Keep qpdf-incompatible key validation out of `set_data_key`.
- Record the output-neutral failure cleanup substitution in the module doc and correspondence table.

---

### Task 1: Writer encryption state and lifecycle

**Files:**
- Create: `crates/flpdf/src/writer/encryption_state.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Test: `crates/flpdf/src/writer/encryption_state.rs`

**Interfaces:**
- Consumes: `crate::security::standard::{per_object_key, ObjectKeyAlg}`
- Produces: `WriterEncryptionState::{new,current_data_key,with_object_data_key}` and configuration accessors for later string/stream consumers.

- [ ] **Step 1: Write failing tests for key derivation**

Add tests that construct writer state for RC4, AES-128, V5 direct-key, and disabled encryption, then assert the key visible inside a top-level callback is derived from the emitted number with generation 0.

- [ ] **Step 2: Run the derivation tests and verify RED**

Run: `cargo test -p flpdf writer::encryption_state::tests::`

Expected: compilation fails because `writer::encryption_state` and `WriterEncryptionState` do not exist.

- [ ] **Step 3: Implement minimal state and derivation**

Add `WriterEncryptionState` with exact qpdf field meanings, `Option<Vec<u8>>` current-key presence, `set_data_key`, accessors, and the V<5/V>=5 branches.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `cargo test -p flpdf writer::encryption_state::tests::`

Expected: all derivation tests pass.

- [ ] **Step 5: Write failing lifecycle tests**

Add tests for top-level success cleanup, top-level error cleanup with exact error propagation, ObjStm member omission, emitted-number/source-number distinction, and deferred invalid V5 key length.

- [ ] **Step 6: Run lifecycle tests and verify RED**

Run: `cargo test -p flpdf writer::encryption_state::tests::`

Expected: lifecycle tests fail because `with_object_data_key` is absent.

- [ ] **Step 7: Implement minimal lifecycle wrapper**

Add `with_object_data_key<T, E>` using `Option<u32>` for qpdf's `object_stream_index == -1` sentinel and always clear after the callback returns.

- [ ] **Step 8: Run focused tests and verify GREEN**

Run: `cargo test -p flpdf writer::encryption_state::tests::`

Expected: all focused tests pass.

- [ ] **Step 9: Commit the primitive**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/encryption_state.rs
git commit -m "feat(writer): add emitted-object data key state"
```

### Task 2: Correspondence and verification

**Files:**
- Modify: `docs/qpdf-correspondence.md`
- Modify: generated module-doc index if required by `scripts/qpdf-module-docs.py`

**Interfaces:**
- Consumes: the completed `writer::encryption_state` module.
- Produces: durable qpdf source mapping and verification evidence.

- [ ] **Step 1: Update correspondence documentation**

Record the writer fields, `setDataKey`, top-level/ObjStm lifecycle, and the output-neutral Rust error cleanup substitution in the `QPDFWriter.cc` row.

- [ ] **Step 2: Regenerate and check module docs**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

Expected: all checks pass.

- [ ] **Step 3: Run formatting and focused verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf writer::encryption_state::tests::
```

Expected: all checks pass.

- [ ] **Step 4: Run workspace quality gates**

Run:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

Expected: all checks pass.

- [ ] **Step 5: Run fresh changed-line coverage**

Generate fresh LCOV and run `scripts/patch-coverage.sh` against `origin/main`; expected result is 100% changed executable-line coverage.

- [ ] **Step 6: Commit documentation**

```bash
git add docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git commit -m "docs(writer): map QPDFWriter data key state"
```

- [ ] **Step 7: Close and publish**

Read back the diff and tests, close `flpdf-3yn9.11`, run `bd dolt push`, rebase if needed, and push `feature/flpdf-3yn9.11-set-data-key`.
