# qtest type-checks QPDFObjectHandle Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all six qpdf 11.9.0 `type-checks.test` subtests pass with the exact qpdf `test_driver 42` warning and fallback behavior.

**Architecture:** Add the missing warning-producing `QPDFObjectHandle` operations to `crates/flpdf/src/object_handle.rs`, retaining canonical handle identity, resolver ownership, and the existing fallible Rust error boundary. Make `flpdf-qtest-tools` call those operations in qpdf source order and drain document diagnostics at each warning boundary; no expected warning is synthesized in the driver.

**Tech Stack:** Rust workspace, `flpdf-qtest-tools`, qpdf 11.9.0 pinned source, qtest Perl harness, exact fixture output comparison, Beads.

---

### Task 1: Add the RED driver regression for test 42

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/driver/test_42_49.rs`
- Test: the source-near `#[cfg(test)]` module in the same file
- Read: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/qpdf/test_driver.cc:1407-1549`

- [x] **Step 1: Expand the Rust fixture to contain the qpdf test-42 shape**

Extend `pdf_with_object_types_qtest()` with a page whose `/Contents` is an indirect stream, a `/QTest/Dictionary` containing `/Key1 /Value1`, `/Key2` with `/Item0` through `/Item2`, and `/Integer` as an indirect integer. Keep this fixture authored in flpdf; do not copy qpdf-qtest fixtures or expected output files into the workspace.

- [x] **Step 2: Require warning output in the existing driver test**

Replace the current `assert!(stderr.is_empty())` in `object_type_and_form_presence_paths_use_canonical_resolution` with:

```rust
assert!(stdout.is_empty());
let warning_text = String::from_utf8(stderr).expect("warnings are UTF-8");
assert!(warning_text.contains("operation for string attempted on object of type"));
assert!(warning_text.contains("returning null for out of bounds array access"));
assert!(warning_text.contains("ignoring attempt to append item"));
assert!(warning_text.contains("test 42 done"));
```

Retain the separate test-43 assertions in the same test function.

- [x] **Step 3: Run RED**

```bash
cargo test -p flpdf-qtest-tools --lib object_type_and_form_presence_paths_use_canonical_resolution
```

Expected: the test executes the current `run_test_42` GAP and fails because its stderr has no type-operation warnings. A fixture or compile failure is not an acceptable RED result.

- [x] **Step 4: Commit the RED test**

```bash
git add crates/flpdf-qtest-tools/src/driver/test_42_49.rs
git commit -m "test(qtest): require type-check warning output"
```

### Task 2: Port warning-producing scalar and container accessors

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: source-near tests in `object_handle.rs`
- Modify: `crates/flpdf/src/lib.rs` only when a new public helper type needs re-exporting

- [x] **Step 1: Add failing core tests for warning context and fallback values**

Add a parsed-document test that invokes the warning-producing family and checks both values and `Pdf::repair_diagnostics()`. The assertions must cover:

```rust
assert_eq!(integer.try_get_bool_value().unwrap(), false);
assert_eq!(integer.try_get_real_value().unwrap(), b"0.0");
assert_eq!(integer.try_get_name().unwrap(), b"/QPDFFakeName");
assert_eq!(integer.try_get_string_value().unwrap(), b"");
assert_eq!(integer.try_get_utf8_value().unwrap(), b"");
assert_eq!(integer.try_get_operator_value().unwrap(), b"QPDFFAKE");
assert_eq!(integer.try_get_inline_image_value().unwrap(), b"");
assert_eq!(dictionary.try_get_int_value().unwrap(), 0);
assert_eq!(dictionary.try_get_numeric_value().unwrap(), 0.0);
```

The test must verify the actual object description, expected type, found type, and warning order. Add a contextless direct-handle case that expects the existing `Error::System` boundary instead of silently printing.

- [x] **Step 2: Implement scalar qpdf accessors once in ObjectHandle**

Reuse `try_dereference`, silent `as_*` inspection, `type_warning`, and `warn_if_possible`. Add these public fallible facades:

```rust
pub fn try_get_bool_value(&self) -> Result<bool>;
pub fn try_get_int_value(&self) -> Result<i64>;
pub fn try_get_real_value(&self) -> Result<Vec<u8>>;
pub fn try_get_numeric_value(&self) -> Result<f64>;
pub fn try_get_name(&self) -> Result<Vec<u8>>;
pub fn try_get_string_value(&self) -> Result<Vec<u8>>;
pub fn try_get_utf8_value(&self) -> Result<Vec<u8>>;
pub fn try_get_operator_value(&self) -> Result<Vec<u8>>;
pub fn try_get_inline_image_value(&self) -> Result<Vec<u8>>;
```

Each wrong-type branch calls `type_warning(expected, fallback_message)` before returning qpdf's zero-like value. Valid real/name values preserve qpdf's spelling and leading-slash conventions.

- [x] **Step 3: Add failing tests for array/dictionary operations**

Cover non-array length/vector/item/mutator calls, negative and oversized indexes, non-dictionary key/map operations, missing-key child descriptions, and `getKeyIfDict` null short-circuit:

```rust
assert_eq!(integer.try_get_array_n_items().unwrap(), 0);
assert!(integer.try_get_array_as_vector().unwrap().is_empty());
assert!(integer.try_get_array_item(-1).unwrap().is_null());
assert!(!integer.try_get_has_key(b"/Potato").unwrap());
assert!(integer.try_get_dict_as_map().unwrap().is_empty());
assert!(integer.try_get_key_if_dict(b"/Potato").unwrap().is_null());
assert!(null.try_get_key_if_dict(b"/Integer").unwrap().is_null());
```

Verify that invalid mutations leave the original value unchanged and that a missing dictionary key gets a child description rather than a non-dictionary warning.

- [x] **Step 4: Implement container accessors and signed-index qpdf faces**

Promote the already qpdf-shaped `type_warning`, `object_warning`, `try_get_key`, `try_get_keys`, `try_has_key`, `try_get_int_value`, and `try_array_len` as public canonical operations. Add:

```rust
pub fn try_get_key_if_dict(&self, key: &[u8]) -> Result<ObjectHandle>;
pub fn try_get_array_n_items(&self) -> Result<usize>;
pub fn try_get_array_item(&self, index: i64) -> Result<ObjectHandle>;
pub fn try_get_array_as_vector(&self) -> Result<Vec<ObjectHandle>>;
pub fn try_get_dict_as_map(&self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>>;
pub fn try_get_has_key(&self, key: &[u8]) -> Result<bool>;
pub fn try_set_array_item_at(&self, index: i64, value: ObjectHandle) -> Result<()>;
pub fn try_insert_array_item_at(&self, index: i64, value: ObjectHandle) -> Result<()>;
pub fn try_erase_array_item_at(&self, index: i64) -> Result<()>;
```

Perform explicit signed bounds checks before conversion to `usize`; do not encode negative indexes or absence as a sentinel. Preserve qpdf's type-warning versus object-warning distinction.

- [x] **Step 5: Run core RED/GREEN and commit**

```bash
cargo test -p flpdf --lib object_handle::qpdf_type_check
cargo test -p flpdf --lib object_handle::qpdf_array
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/lib.rs
git commit -m "feat(object): port qpdf type-check accessors"
```

Expected: the new tests and the pre-existing ObjectHandle tests pass.

### Task 3: Port geometry, iterator, and initialized-state contracts

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: source-near tests in `object_handle.rs`

- [x] **Step 1: Add failing geometry/state/cursor tests**

Cover valid and invalid Rectangle/Matrix arrays, non-numeric children, empty/end cursors, decrement-before-begin, increment-after-end, and a default/uninitialized handle:

```rust
let rectangle = ObjectHandle::new_from_rectangle(Rectangle::new(1.2, 3.4, 5.6, 7.8));
assert!(rectangle.try_is_rectangle().unwrap());
assert_eq!(ObjectHandle::integer(1).try_get_array_as_rectangle().unwrap(), Rectangle::default());

let matrix = ObjectHandle::new_from_matrix(ObjectHandleMatrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3));
assert!(matrix.try_is_matrix().unwrap());
assert_eq!(ObjectHandle::integer(1).try_get_array_as_matrix().unwrap(), ObjectHandleMatrix::default());

let uninitialized = ObjectHandle::uninitialized();
assert!(!uninitialized.is_initialized());
assert!(!uninitialized.try_is_integer().unwrap());
assert!(uninitialized.try_dereference().is_err());
```

- [x] **Step 2: Implement qpdf ObjectHandle geometry**

Add public `ObjectHandleMatrix` with fields `a` through `f`, `new`, and all-zero `Default`. Add:

```rust
pub fn new_from_rectangle(rectangle: Rectangle) -> ObjectHandle;
pub fn try_is_rectangle(&self) -> Result<bool>;
pub fn try_get_array_as_rectangle(&self) -> Result<Rectangle>;
pub fn new_from_matrix(matrix: ObjectHandleMatrix) -> ObjectHandle;
pub fn try_is_matrix(&self) -> Result<bool>;
pub fn try_get_array_as_matrix(&self) -> Result<ObjectHandleMatrix>;
```

Resolve the receiver, inspect exact length and numeric children without warnings, and return qpdf's default geometry for invalid shapes. Do not change `flpdf::Matrix::default()`, which is the affine identity.

- [x] **Step 3: Implement Rust-native reversible cursors**

Add public `ArrayItems`/`ArrayItemCursor` and `DictItems`/`DictItemCursor` owned by ObjectHandle. Provide `begin`, `current`, `next`, `previous`, `is_end`, and initialized-state transitions. Cursors use a stable safe `ivalue` cell that is rebound on movement, and dictionary cursors snapshot qpdf's visible `getKeys()` set so retained values observe qpdf's end transition without unsafe aliasing or raw `Object` materialization.

- [x] **Step 4: Add explicit initialized state**

Add an `initialized: bool` field to `ObjectSlot`, set it for every existing constructor, and add:

```rust
impl Default for ObjectHandle {
    fn default() -> Self {
        Self::uninitialized()
    }
}
pub fn uninitialized() -> ObjectHandle;
pub fn is_initialized(&self) -> bool;
```

`try_dereference` returns the existing qpdf-equivalent `Error::Internal` for uninitialized state. Initialized null, unresolved, reserved, and destroyed states remain distinct.

- [x] **Step 5: Run and commit**

```bash
cargo test -p flpdf --lib object_handle::qpdf_geometry
cargo test -p flpdf --lib object_handle::qpdf_iterator
cargo test -p flpdf --lib object_handle::qpdf_initialized
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/lib.rs
git commit -m "feat(object): port qpdf type-check geometry and cursors"
```

### Task 4: Replace the driver GAP with complete qpdf test 42

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/driver/test_42_49.rs`
- Test: the same file's exact stdout/stderr unit tests

- [x] **Step 1: Port setup and cursors in source order**

Resolve `/QTest`, `/Dictionary`, `/Key2`, `/Integer`, and the first page through canonical handles. Use the new qpdf-facing methods and cursor state assertions.

- [x] **Step 2: Port every warning-producing operation**

Implement qpdf `test_driver.cc:1449-1495` in order:

```rust
integer.try_get_array_item(-1)?;
integer.try_append_array_item(ObjectHandle::null())?;
array.try_erase_array_item_at(-1)?;
array.try_erase_array_item_at(16_059)?;
array.try_insert_array_item_at(42, ObjectHandle::name(b"Dontpanic".to_vec()))?;
array.try_set_array_item_at(42, ObjectHandle::name(b"Dontpanic".to_vec()))?;
integer.try_erase_array_item_at(0)?;
integer.try_insert_array_item_at(0, ObjectHandle::null())?;
integer.try_set_array_items(Vec::new())?;
integer.try_set_array_item_at(0, ObjectHandle::null())?;
```

Call `emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?` immediately after every warning-producing operation. Keep the `One error` and `Two errors` markers at qpdf's exact positions.

- [x] **Step 3: Port nested descriptions, stream dictionary, geometry, and state**

Use the warning-producing key and scalar methods for `/Quack`, invalid array items, and the stream dictionary. Keep all Rectangle/Matrix and uninitialized assertions. Return through the existing outer `test 42 done` path; do not print warning literals.

- [x] **Step 4: Add exact driver assertions and run tests**

Assert the synthetic fixture's exact warning bytes in the unit test, then run:

```bash
cargo test -p flpdf-qtest-tools --all-features
cargo test -p flpdf --lib object_handle
```

Build release binaries and run `type-checks.test` against a writable copy of `vendor/qpdf-qtest`; require Total tests 6, Passes 6, Failures 0, Errors 0.

- [x] **Step 5: Commit the driver cutover**

```bash
git add crates/flpdf-qtest-tools/src/driver/test_42_49.rs
git commit -m "feat(qtest): port test driver type checks"
```

### Task 5: Verify the full objective and update the qtest ledger

**Files:**
- Modify only after evidence: `/home/ubuntu/flpdf-qtest/parity/qtest-11.9.0.jsonl`
- Read: `/home/ubuntu/flpdf-qtest/allowlist.txt`
- Read: paired final `harness.log` and `qtest-results.xml`

- [x] **Step 1: Run implementation-worktree quality gates**

```bash
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
bash scripts/qpdf-test-driver-diff.sh --check
```

- [x] **Step 2: Run full qtest with paired artifacts**

Use a writable copied datadir and `QTEST_FULL=1 ./scripts/run.sh` with all ten release binary variables from the qtest README. Keep the same-run `harness.log` and `qtest-results.xml` pair. Confirm `type-checks 3` and `type-checks 4` are ordinary PASS outcomes before editing the JSONL ledger.

- [x] **Step 3: Validate and record evidence**

```bash
python3 scripts/verify-parity-manifest.py survey/latest/harness.log survey/latest/qtest-results.xml parity/qtest-11.9.0.jsonl
bd dep cycles
bd show flpdf-25kg.2.5.8
```

Only promote the two proven type-check rows to `passing`; leave unrelated rows unchanged. Append the exact commits, focused results, full qtest totals, and quality-gate results to the Beads issue.

- [ ] **Step 4: Final status and synchronization**

```bash
git status --short --branch
bd dolt push
git push
```

Do not claim completion unless the final qtest artifacts, manifest validation, quality gates, Beads readback, and remote synchronization all provide fresh successful evidence.
