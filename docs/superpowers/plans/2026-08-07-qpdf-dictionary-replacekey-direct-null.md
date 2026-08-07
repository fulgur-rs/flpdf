# QPDF Dictionary replaceKey Direct-Null Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ObjectHandle::replace_key` remove a dictionary key for a direct null while preserving indirect null and dangling indirect handles.

**Architecture:** Keep the existing `ObjectHandle` mutation boundary and delegate the direct-null case to `remove_key`, which already updates live containment edges. All other values continue through the existing insertion path, preserving handle identity and indirect boundaries.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source oracle, Cargo tests, cargo-llvm-cov.

## Global Constraints

- Pinned qpdf 11.9.0 `QPDF_Dictionary::replaceKey` and `QPDFObjectHandle::replaceKey` are authoritative.
- Direct null removes an existing key and does not create a missing key.
- Indirect resolved-null and dangling indirect handles remain dictionary values.
- Do not add `checkOwnership`, warning parity, stream-provider, filtering, or Filespec behavior.

---

### Task 1: Port direct-null dictionary removal

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: `crates/flpdf/src/object_handle.rs` (`mutation_tests`)

**Interfaces:**
- Consumes: `ObjectHandle::is_direct()`, `ObjectHandle::is_null()`, and `ObjectHandle::remove_key(&self, key: &[u8])`.
- Produces: qpdf-compatible `ObjectHandle::replace_key(&self, key: &[u8], value: ObjectHandle)` direct-null behavior without changing its signature.

- [x] **Step 1: Add regression tests for the acceptance matrix**

  Add focused tests covering existing and missing keys on direct dictionaries, a resolved indirect dictionary, a non-dictionary handle, a resolved indirect null value, a missing/dangling indirect value, retained handle identity, and containment detachment.

- [x] **Step 2: Run the focused tests and verify RED**

  Run: `cargo test -p flpdf --lib object_handle::mutation_tests::replace_key`

  Expected: direct-null cases fail because the key remains present; indirect-preservation cases remain green.

- [x] **Step 3: Implement the minimal qpdf branch**

  At the start of `replace_key`, check `value.is_direct() && value.is_null()`. Delegate that case to `self.remove_key(key)` and return. Leave the existing insertion and containment logic unchanged for indirect and non-null values.

- [x] **Step 4: Verify GREEN and regressions**

  Run the focused mutation tests, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test`.

- [x] **Step 5: Verify changed-line coverage and qpdf-compatible corpus**

  Generate fresh LCOV coverage for the workspace with `qpdf-zlib-compat`, run `scripts/patch-coverage.sh` against `main`, and run the byte-identical compatibility tests relevant to `flpdf`.

- [ ] **Step 6: Commit and publish**

  Commit only the plan, implementation, and tests; push the feature branch and persist Beads with `bd dolt push` after verification.
