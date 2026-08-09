# qpdf ID Selector Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ordinary/static `/ID` generation and encryption consume one qpdf 11.9.0-equivalent identifier selection across full-rewrite, incremental, and linearized output, then remove the private duplicate selector routes.

**Architecture:** `writer.rs` owns the supported source `/ID[0]` predicate and a complete ordinary/static two-element ID generator. Full-rewrite computes that array once before explicit encryption context construction and passes the same first element to key derivation and the same complete array to every trailer writer. Linearization stores the same complete array before layout while retaining qpdf's pass-1 zero-width placeholder and deterministic-id path.

**Tech Stack:** Rust workspace, `cargo test`, qpdf 11.9.0 oracle, existing `Pdf`/`WriteOptions`/`LinearizationPlan` test helpers.

---

### Task 1: Add RED coverage for the shared qpdf ID generator

**Files:**
- Modify: `crates/flpdf/src/writer.rs` in the private writer test module near the existing `apply_static_id` and `apply_random_id` tests.

- [ ] **Step 1: Add the empty-source default test**

Add a test that constructs `Object::Array([Object::String(vec![]), Object::String(non_empty)])`, calls the planned `generate_id_array(Some(&source), false)`, extracts the two strings, and asserts:

```rust
assert!(!id0.is_empty());
assert_eq!(id0, id1, "qpdf reuses generated id2 when source id1 is empty");
```

- [ ] **Step 2: Add the empty-source static test**

Use the same source and call `generate_id_array(Some(&source), true)`. Assert both elements equal `QPDF_STATIC_ID`.

- [ ] **Step 3: Add the non-empty-source preservation test**

Use a well-formed two-string source with `b"permanent"` as the first element. Assert ordinary generation preserves that first element and generates a non-empty second element; assert static generation preserves the first element and uses `QPDF_STATIC_ID` as the second.

- [ ] **Step 4: Run the new tests and verify RED**

Run:

```bash
cargo test -p flpdf writer::tests::generate_id_array --lib
```

Expected result: compilation fails because `generate_id_array` does not exist yet. This is the intended feature-missing failure, not a test typo.

### Task 2: Add RED coverage for full-rewrite and linearized output

**Files:**
- Modify: `crates/flpdf/src/writer.rs` private writer tests.
- Modify: `crates/flpdf/src/linearization/writer.rs` private linearization tests near `static_id_linearized_main_trailer_visible_to_reader`.

- [ ] **Step 1: Add full-rewrite empty-ID tests**

Reuse the existing writer fixture builder that accepts a trailer fragment. For `/ID [<> <bbbb...>]` and `/ID [() ()]`, write with ordinary full-rewrite and assert the reader-visible output `/ID[0]` is non-empty and equals `/ID[1]`. Repeat with `static_id: true` and assert both elements equal `QPDF_STATIC_ID`.

- [ ] **Step 2: Add linearized empty-ID tests**

Use the existing `tiny_pdf_with` and `linearize_with` helpers. For both `/ID [<> <bbbb...>]` and `/ID [() ()]`, assert every collected linearized `/ID` array is byte-identical, has non-empty equal elements in default mode, and has two pi elements in static mode. Keep the existing linearization checker assertion.

- [ ] **Step 3: Add encrypted output ID-consistency coverage**

For the supported AES-128 encryption helper already used by the writer and linearization tests, write an empty-ID source and assert:

```rust
let trailer_id = reader.trailer().get("ID").expect("/ID").as_array().unwrap();
assert_eq!(trailer_id[0], trailer_id[1]);
```

Re-open with the empty user password and verify the existing plaintext round-trip assertion. The test must inspect the emitted `/ID[0]`, not only successful decryption, so a mismatched hidden selector cannot pass.

- [ ] **Step 4: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf writer::tests::empty_source_id --lib
cargo test -p flpdf linearization::writer::tests::empty_source_id --lib
```

Expected result: the new behavior assertions fail against the current empty-string-preserving paths.

### Task 3: Implement the qpdf-equivalent source predicate and generator

**Files:**
- Modify: `crates/flpdf/src/writer.rs` around `source_permanent_id`, `apply_static_id`, and `random_id_array`.

- [ ] **Step 1: Extract the one supported source-ID predicate**

Introduce a private-value helper with this exact responsibility:

```rust
fn source_permanent_id_value(source_id: Option<&Object>) -> Option<Vec<u8>>
```

It accepts only the currently supported two-element string array and returns the first string only when it is non-empty. Refactor the existing dictionary wrapper:

```rust
pub(crate) fn source_permanent_id(trailer: &Dictionary) -> Option<Vec<u8>> {
    source_permanent_id_value(trailer.get("ID"))
}
```

Do not expand malformed-array behavior in this task.

- [ ] **Step 2: Implement one complete ordinary/static ID generator**

Add:

```rust
pub(crate) fn generate_id_array(source_id: Option<&Object>, static_id: bool) -> Object
```

Generate the changing identifier once: `QPDF_STATIC_ID.to_vec()` for static mode or `fresh_id_bytes().to_vec()` otherwise. Use the non-empty source value from `source_permanent_id_value`, or clone the generated changing value when the source is absent/empty within the supported shape. Return exactly two `Object::String` elements.

- [ ] **Step 3: Make the generator pass the RED unit tests**

Run:

```bash
cargo test -p flpdf writer::tests::generate_id_array --lib
```

Expected result: all generator tests pass. Do not wire consumers or delete old routes until this minimal helper is green.

### Task 4: Cut over full-rewrite and incremental callers

**Files:**
- Modify: `crates/flpdf/src/writer.rs` at `write_incremental_trailer`, `apply_encrypt_trailer_entries`, and `write_pdf_full_rewrite_inner`.
- Test: `crates/flpdf/src/writer.rs` existing writer tests plus the new empty-ID tests.

- [ ] **Step 1: Replace incremental ordinary/static mutations**

Replace the `if options.static_id { apply_static_id } else { apply_random_id }` branch in `write_incremental_trailer` with one `generate_id_array(trailer.get("ID"), options.static_id)` call and insert the returned array.

- [ ] **Step 2: Compute the full-rewrite ordinary/static array once**

In `write_pdf_full_rewrite_inner`, before explicit encryption context construction, create an `Option<Object>` containing `generate_id_array(pdf.trailer().get("ID"), options.static_id)` when `deterministic_id` is false and `copy_encryption` is absent. Keep it `None` for deterministic-id and donor-copy routes.

- [ ] **Step 3: Feed the selected first element to explicit encryption**

Extract the first string from that complete array and pass it to `build_encryption_context`. Remove `resolve_id0_for_encryption`; the generator is now the sole ordinary/static selector. Preserve the existing deterministic-id rejection and donor-copy branch.

- [ ] **Step 4: Reuse the complete array at every full-rewrite trailer site**

Extend `apply_encrypt_trailer_entries` with an optional selected ordinary/static ID array. For explicit encryption and plaintext ordinary/static output, insert that array unchanged. For copy-encryption, retain the donor context's existing ID behavior. For deterministic-id, retain the existing direct/placeholder path.

- [ ] **Step 5: Run full-rewrite RED tests until GREEN**

Run:

```bash
cargo test -p flpdf writer::tests::empty_source_id --lib
cargo test -p flpdf writer::tests::apply_static_id --lib
cargo test -p flpdf writer::tests::apply_random_id --lib
```

Rename or rewrite old unit tests to assert the new qpdf behavior rather than preserving deleted helper names. All focused writer tests must pass before moving to linearization.

### Task 5: Cut over linearization and remove duplicate selector routes

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` at `finalize_linearized_id` and its tests.
- Modify: `crates/flpdf/src/writer.rs` to remove unused `apply_static_id`, `random_id_array`, `apply_random_id`, and `resolve_id0_for_encryption` definitions and references.

- [ ] **Step 1: Replace ordinary/static linearization branches**

Keep the deterministic-id branch in `finalize_linearized_id`. Replace its static and default branches with:

```rust
crate::writer::generate_id_array(source_trailer.get("ID"), options.static_id)
```

The working trailer remains the file-scoped source for every linearized trailer site, and the existing pass-1 placeholder continues to use `source_permanent_id` for width.

- [ ] **Step 2: Verify linearized encryption consumes the emitted first element**

Keep the existing extraction of the finalized array's first string for `build_encryption_context`. Confirm no second generation is introduced and the final trailer's first string remains the same value.

- [ ] **Step 3: Remove only unreachable private selector routes**

Use `rg` to prove no consumer remains for the old selector functions. Delete the functions and update their tests/docs. Do not remove `source_permanent_id`, deterministic-id logic, pass-1 placeholder logic, or copy-encryption code.

- [ ] **Step 4: Run linearization focused tests**

Run:

```bash
cargo test -p flpdf linearization::writer::tests::empty_source_id --lib
cargo test -p flpdf linearization::writer::tests::static_id_linearized --lib
cargo test -p flpdf linearization::writer::tests::deterministic_id_linearized --lib
```

Expected result: default/static empty-ID tests, existing deterministic tests, and existing linearization tests pass together.

### Task 6: Oracle and quality verification

**Files:**
- Modify: tests only if a focused oracle assertion is still missing after Tasks 1–5.

- [ ] **Step 1: Run the affected crate tests**

```bash
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test compat_matrix_tests
```

The compatibility matrix may skip when qpdf is unavailable; record the actual result.

- [ ] **Step 2: Run qpdf live checks on the empty-ID fixtures**

For hex and literal empty sources, inspect default/static full-rewrite and linearized outputs with qpdf 11.9.0. Require `qpdf --check` and `qpdf --check-linearization` where applicable, and compare static `/ID` arrays to the pi oracle. For encrypted outputs, reopen with the configured password and verify the same selected `/ID[0]` drives decryption.

- [ ] **Step 3: Run formatting, clippy, and workspace tests**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

- [ ] **Step 4: Check the cutover and diff**

```bash
rg -n "apply_static_id|random_id_array|apply_random_id|resolve_id0_for_encryption" crates/flpdf/src
git diff --check
git status --short
git diff --stat
```

The selector names must have no production consumers after removal, and the diff must contain only the spec/plan plus the focused implementation and tests.

- [ ] **Step 5: Update Beads with evidence and persist**

Read back `flpdf-m0q1`, append the verification summary without overwriting prior notes, run `bd dep cycles`, and run `bd dolt push`. Do not close the issue until the implementation and all required evidence are complete.
