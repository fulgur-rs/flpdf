# ObjectHandle Lazy Inspection Primitives Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the qpdf 11.9.0 `QPDFObjectHandle` inspection primitives needed by `QPDF::decryptStream` without materializing legacy `Object` trees or snapshotting whole containers.

**Architecture:** Add crate-private fallible methods directly to `ObjectHandle`, because qpdf owns these operations on `QPDFObjectHandle`. Each method resolves only the handle it is currently inspecting, releases every `RefCell` borrow before resolving a child, and represents qpdf names/keys as flpdf's existing decoded byte strings without the leading slash.

**Tech Stack:** Rust 2021, `Rc<RefCell<_>>`, flpdf's `ObjectHandle`/`DocumentResolver`, Cargo tests, `cargo-llvm-cov`, pinned qpdf 11.9.0 source.

## Global Constraints

- Treat pinned qpdf 11.9.0 as the semantic oracle: `libqpdf/QPDFObjectHandle.cc:456-466`, `:759-785`, and `:1027-1039`.
- Keep all new behavior in `crates/flpdf/src/object_handle.rs`; do not add reader-local helpers, legacy `Object` materialization, `resolve_borrowed`, sentinel values, or whole-array/dictionary snapshots.
- Use flpdf's established name/key representation: decoded bytes without qpdf's leading `/`.
- A holder borrow must end before any child method can re-enter `DocumentResolver`.
- `try_array_item` covers the valid-index contract used by `QPDF::decryptStream`; non-array and out-of-bounds inputs return `None`, leaving qpdf's warning/null-invalid-access behavior to a future diagnostic-bearing boundary.
- Run RED to GREEN for every production method and finish with 100 percent changed executable-line coverage without new `cov:ignore` annotations.

---

### Task 1: Name equality predicate

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:630-680`
- Test: `crates/flpdf/src/object_handle.rs:3147-3600`

**Interfaces:**
- Consumes: `ObjectHandle::try_dereference() -> Result<()>`, `ObjectHandle::with_value(...)`.
- Produces: `pub(crate) fn try_is_name_and_equals(&self, name: &[u8]) -> Result<bool>`.

- [ ] **Step 1: Write failing name-equality tests**

  Add focused tests proving direct and indirect name equality, wrong-name false, wrong-type false, resolver-error propagation, and dropped-document error propagation. Each assertion uses literal byte strings such as `b"Crypt"`.

- [ ] **Step 2: Run the focused tests and verify RED**

  Run: `cargo test -p flpdf --lib try_is_name -- --nocapture`

  Expected: compilation fails because `try_is_name_and_equals` does not exist.

- [ ] **Step 3: Implement the minimal name predicate**

  Implement `try_is_name_and_equals` as holder dereference followed by an in-place `ObjectValue::Name` byte comparison.

- [ ] **Step 4: Run the focused tests and verify GREEN**

  Run: `cargo test -p flpdf --lib try_is_name -- --nocapture`

  Expected: all matching name-equality tests pass with no warnings.

- [ ] **Step 5: Commit the name-predicate slice**

  ```bash
  git add crates/flpdf/src/object_handle.rs
  git commit -m "feat(object-handle): add lazy name equality"
  ```

### Task 2: Valid array-item access

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:645-680`
- Test: `crates/flpdf/src/object_handle.rs:3500-3600`

**Interfaces:**
- Consumes: `ObjectHandle::try_dereference() -> Result<()>`, `ObjectHandle::with_value(...)`.
- Produces: `pub(crate) fn try_array_item(&self, index: usize) -> Result<Option<ObjectHandle>>`.

- [ ] **Step 1: Write failing array-item tests**

  Add tests that fetch the first, middle, and last element by identity; return `None` for a non-array and out-of-bounds index; resolve an indirect holder exactly once; and propagate a dropped-document error. The identity assertion must use `ptr_eq` so the test catches accidental whole-subtree materialization.

- [ ] **Step 2: Run the focused tests and verify RED**

  Run: `cargo test -p flpdf --lib try_array_item -- --nocapture`

  Expected: compilation fails because `try_array_item` does not exist.

- [ ] **Step 3: Implement minimal valid-item access**

  Dereference the holder, inspect `ObjectValue::Array`, and clone only `children.get(index)`. Return `None` for every invalid-domain case; do not resolve the returned child.

- [ ] **Step 4: Run the focused tests and verify GREEN**

  Run: `cargo test -p flpdf --lib try_array_item -- --nocapture`

  Expected: all array-item tests pass and the fetched handle shares identity with the stored child.

- [ ] **Step 5: Commit the array-item slice**

  ```bash
  git add crates/flpdf/src/object_handle.rs
  git commit -m "feat(object-handle): add lazy array item access"
  ```

### Task 3: Composite name and dictionary predicates

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:615-710`
- Test: `crates/flpdf/src/object_handle.rs:3390-3600`

**Interfaces:**
- Consumes: `ObjectHandle::try_dereference()`, `ObjectHandle::try_get_key`, `ObjectHandle::try_is_name_and_equals`.
- Consumes: `ObjectHandle::try_array_len`, `ObjectHandle::try_array_item`.
- Produces: `pub(crate) fn try_is_or_has_name(&self, name: &[u8]) -> Result<bool>` and `pub(crate) fn try_is_dictionary_of_type(&self, type_name: &[u8], subtype_name: &[u8]) -> Result<bool>`.

- [ ] **Step 1: Write failing composite-predicate tests**

  Add `try_is_or_has_name` tests for a direct name, array match, non-name/non-array false, short-circuiting before an erroring later child, indirect child resolution, and dropped-document propagation. Add dictionary tests for direct and indirect dictionaries, matching `/Type`, matching `/Type` plus `/Subtype`, empty constraints, missing keys, wrong-typed keys, non-dictionaries, indirect `/Type` children, resolver errors, and dropped-document holders. Use decoded literals `b"Type"`, `b"Subtype"`, and values such as `b"CryptFilterDecodeParms"` without a slash.

- [ ] **Step 2: Run the focused tests and verify RED**

  Run: `cargo test -p flpdf --lib try_is_or_has_name -- --nocapture && cargo test -p flpdf --lib try_is_dictionary_of_type -- --nocapture`

  Expected: compilation fails because `try_is_or_has_name` and `try_is_dictionary_of_type` do not exist.

- [ ] **Step 3: Implement both qpdf branch orders**

  Implement `try_is_or_has_name` by testing the holder as a name first, then obtaining an array length and testing one fetched child at a time. Implement `try_is_dictionary_of_type` by dereferencing and testing dictionary shape first; if `type_name` is non-empty, fetch only `Type` and test it as a name; only after that succeeds, apply the corresponding `Subtype` check when `subtype_name` is non-empty. Ensure no container borrow survives a child predicate call.

- [ ] **Step 4: Run the focused tests and verify GREEN**

  Run: `cargo test -p flpdf --lib try_is_or_has_name -- --nocapture && cargo test -p flpdf --lib try_is_dictionary_of_type -- --nocapture`

  Expected: all composite-predicate tests pass with qpdf's short-circuit order.

- [ ] **Step 5: Commit the dictionary-predicate slice**

  ```bash
  git add crates/flpdf/src/object_handle.rs
  git commit -m "feat(object-handle): add composite lazy predicates"
  ```

### Task 4: Correspondence, integration, and quality gates

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:1-16`
- Modify: `docs/qpdf-correspondence.md:121`
- Modify: `docs/qpdf-correspondence.md:432-442`

**Interfaces:**
- Consumes: all primitives from Tasks 1-3.
- Produces: qpdf source citations and an explicit record of the `QPDF_Array`/canonical-name representation substitutions.

- [ ] **Step 1: Document source and representation correspondence**

  Extend the module docs and correspondence table with the exact qpdf lines, the `Vec<ObjectHandle>` single-item `Rc` clone in place of `QPDF_Array::at`, and the leading-slash-free decoded name representation. State that the substitutions perform no output and introduce no diagnostic timing because the methods only inspect live graph values and `try_array_item` excludes invalid accesses.

- [ ] **Step 2: Format and run focused tests**

  Run: `cargo fmt --all -- --check && cargo test -p flpdf --lib identity_tests -- --nocapture`

  Expected: formatting is clean and all `identity_tests` pass.

- [ ] **Step 3: Run workspace verification**

  Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`

  Expected: both commands exit 0.

- [ ] **Step 4: Regenerate coverage and verify changed executable lines**

  Run: `cargo llvm-cov --workspace --features qpdf-zlib-compat --lcov --output-path target/llvm-cov/lcov.info && scripts/patch-coverage.sh --base origin/main --lcov target/llvm-cov/lcov.info`

  Expected: changed executable-line coverage is 100 percent and no new `cov:ignore` appears in the diff.

- [ ] **Step 5: Commit documentation and any test-only coverage refinements**

  ```bash
  git add crates/flpdf/src/object_handle.rs docs/qpdf-correspondence.md docs/superpowers/plans/2026-08-05-objecthandle-lazy-inspection-primitives.md
  git commit -m "docs: record object handle inspection parity"
  ```

- [ ] **Step 6: Persist and push**

  Run: `bd dolt pull && bd dolt push && git push -u origin feature/flpdf-25kg.3.14-objecthandle-inspection`

  Expected: Beads and the feature branch are both present on their remotes.
