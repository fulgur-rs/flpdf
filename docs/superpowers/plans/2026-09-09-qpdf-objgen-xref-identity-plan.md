# qpdf QPDFObjGen xref identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0's raw xref object/generation identity boundary so `decode-parameters.test` case 1 no longer fails on a `65536` free-row generation.

**Architecture:** Add an internal `QpdfObjGen` with qpdf's signed integer object/generation fields for raw xref parsing and registration. Convert to the existing valid indirect `ObjectRef` only at the qpdf parser boundary; keep linearization's synthetic identity separate. The effective public/resolver xref surface continues to expose only valid live indirect rows.

**Tech Stack:** Rust workspace, `flpdf` unit/integration tests, pinned qpdf 11.9.0 source and binary, `flpdf-qtest` qtest-driver.

**Spec:** `docs/superpowers/specs/2026-09-09-qpdf-objgen-xref-identity-design.md`

## Global Constraints

- qpdf 11.9.0 source and observed output are the semantic oracle.
- Do not vendor or edit qpdf-qtest fixtures in the flpdf repository.
- Do not widen `ObjectRef` with a replacement sentinel or add a parser-only `65536` special case.
- Production code is written only after a failing regression test has been observed.
- `QTEST_FULL=1` results require the same-run `harness.log` and `qtest-results.xml` pair.
- Keep `/home/ubuntu/flpdf` main clean; all code changes use this isolated worktree.

---

### Task 1: Add the qpdf-shaped raw identity primitive

**Files:**

- Create: `crates/flpdf/src/qpdf_obj_gen.rs`
- Modify: `crates/flpdf/src/lib.rs` to register the crate-private module
- Test: `crates/flpdf/src/qpdf_obj_gen.rs` unit tests

**Interfaces:**

- Consumes: `crate::ObjectRef` and qpdf `QPDFObjGen` rules from
  `include/qpdf/QPDFObjGen.hh:29-86`.
- Produces: `pub(crate) struct QpdfObjGen { object: i32, generation: i32 }`,
  `QpdfObjGen::new`, `QpdfObjGen::is_indirect`, and
  `QpdfObjGen::to_object_ref() -> Option<ObjectRef>`.

- [ ] **Step 1: Write the failing tests**

  Add tests proving that:

  ```rust
  assert!(QpdfObjGen::new(0, 65536).is_indirect() == false);
  assert_eq!(QpdfObjGen::new(7, 0).to_object_ref(), Some(ObjectRef::new(7, 0)));
  assert_eq!(QpdfObjGen::new(7, 65534).to_object_ref(), Some(ObjectRef::new(7, 65534)));
  assert_eq!(QpdfObjGen::new(7, 65535).to_object_ref(), None);
  assert_eq!(QpdfObjGen::new(0, 65536).to_object_ref(), None);
  ```

  Also assert qpdf ordering: object number first, then generation.

- [ ] **Step 2: Run the focused test and verify RED**

  Run:

  ```bash
  cargo test -p flpdf --lib qpdf_obj_gen
  ```

  Expected: compilation failure because the new primitive does not yet exist.

- [ ] **Step 3: Implement the primitive**

  Define the crate-private type with derived `Copy`, `Eq`, `Ord`, and `Hash`.
  Keep fields signed to match qpdf. `to_object_ref` must reject object numbers
  below 1 and generations outside `0..65535`; it must not interpret a sentinel.
  Document each method with the qpdf source citation.

- [ ] **Step 4: Run the focused test and verify GREEN**

  Run the same command and confirm all raw identity tests pass.

- [ ] **Step 5: Commit the primitive**

  ```bash
  git add crates/flpdf/src/lib.rs crates/flpdf/src/qpdf_obj_gen.rs
  git commit -m "feat: add qpdf raw xref object identity"
  ```

### Task 2: Add the canonical classic-xref regression before changing the reader

**Files:**

- Create: `crates/flpdf/tests/qpdf_obj_gen_xref_tests.rs`
- Read/Reuse: `crates/flpdf/src/engine.rs` `Pdf::open_mem_owned_with_options`
  and existing reader test fixture helpers

**Interfaces:**

- Consumes: `Pdf::open_mem_owned_with_options`, `Pdf::repair_diagnostics`,
  and the raw PDF fixture builder.
- Produces: a public integration regression that exercises the canonical open
  path, not a private parser-only helper.

- [ ] **Step 1: Write the failing classic-xref test**

  Build a minimal PDF in the test with a catalog object and a classic xref
  subsection whose object-0 free row is exactly `0000000000 65536 f`. Open it
  through `Pdf::open_mem_owned_with_options` with default repair behavior and
  assert that the root resolves and `repair_diagnostics().entries()` is empty.
  The test must not use qtest vendor data.

- [ ] **Step 2: Run the new test and verify RED**

  ```bash
  cargo test -p flpdf --test qpdf_obj_gen_xref_tests classic_free_row_generation_65536_is_accepted -- --nocapture
  ```

  Expected: failure with the current `invalid fixed-width u16` path or its
  recovery diagnostics.

- [ ] **Step 3: Commit the RED test**

  ```bash
  git add crates/flpdf/tests/qpdf_obj_gen_xref_tests.rs
  git commit -m "test: reproduce qpdf raw xref generation boundary"
  ```

### Task 3: Migrate xref parsing and registration to `QpdfObjGen`

**Files:**

- Modify: `crates/flpdf/src/xref.rs` `ParsedXrefEntry`, `XrefRegistration`,
  `XrefEntryLookup`, `parse_xref_table`, `parse_xref_stream_entries`, and
  registration snapshots
- Modify: `crates/flpdf/src/reader/resolver.rs` only where the canonical owner
  receives effective valid `ObjectRef` entries
- Modify: `crates/flpdf/src/parser.rs` only if the explicit invalid-generation
  reference test requires a shared conversion helper
- Test: `crates/flpdf/tests/qpdf_obj_gen_xref_tests.rs` and existing xref unit
  tests

**Interfaces:**

- Consumes: `QpdfObjGen::to_object_ref`, qpdf raw registration semantics, and
  existing `XrefEntry` values.
- Produces: raw xref registration keyed by `QpdfObjGen`; effective snapshots
  keyed by valid `ObjectRef`; object-number-wide free tombstones.

- [ ] **Step 1: Extend RED coverage for invalid live generations**

  Add a test for a raw live xref row with generation `65535` and a separate
  indirect object header/reference at generation `65535`. Assert that the
  raw row does not become a canonical indirect handle and that the parser
  reference follows qpdf's null boundary. Run the focused test and confirm it
  fails with the current u16/sentinel route.

- [ ] **Step 2: Replace classic fixed-u16 parsing**

  Change the classic xref row parser to read the five-byte generation as a
  signed integer into `QpdfObjGen`. Preserve qpdf's row syntax checks and keep
  the offset field as the existing fixed-width unsigned offset.

- [ ] **Step 3: Convert `XrefRegistration` to raw qpdf identity**

  Change registration and parsed-entry keys to `QpdfObjGen`. Implement:

  ```rust
  fn insert_xref_entry(&mut self, key: QpdfObjGen, entry: XrefEntry);
  fn insert_free_xref_entry(&mut self, key: QpdfObjGen);
  fn effective_snapshot(&self) -> BTreeMap<ObjectRef, XrefEntry>;
  ```

  `insert_xref_entry` must be first-wins on an exact raw key and suppress rows
  whose object number is in the deleted-number set. `insert_free_xref_entry`
  must record only the object number when no exact live row exists. The
  effective snapshot must omit free rows and raw generations that cannot cross
  qpdf's indirect-reference gate.

- [ ] **Step 4: Adapt lookups without adding an adapter route**

  Make `XrefEntryLookup` query raw registration by converting its valid
  `ObjectRef` query to `QpdfObjGen(number as i32, generation as i32)`. Keep
  recovery maps that already represent only valid live indirect objects on
  `ObjectRef`; do not create a second public xref API or reintroduce the old
  mixed key.

- [ ] **Step 5: Run the focused RED tests and verify GREEN**

  ```bash
  cargo test -p flpdf --lib xref
  cargo test -p flpdf --test qpdf_obj_gen_xref_tests -- --nocapture
  ```

  Confirm the new classic `65536 f` test passes without recovery diagnostics,
  the invalid indirect-generation test matches qpdf, and all existing xref
  tests remain green.

- [ ] **Step 6: Commit the canonical cutover**

  ```bash
  git add crates/flpdf/src/xref.rs crates/flpdf/src/reader/resolver.rs crates/flpdf/src/parser.rs crates/flpdf/tests/qpdf_obj_gen_xref_tests.rs
  git commit -m "fix: separate qpdf raw xref identity from ObjectRef"
  ```

### Task 4: Update correspondence documentation and run qpdf differential gates

**Files:**

- Modify: `docs/qpdf-correspondence.md` QPDFObjGen/QPDF.cc rows
- Modify: `crates/flpdf/src/qpdf_obj_gen.rs` module documentation if the final
  implementation changes the ownership boundary
- No changes: `/home/ubuntu/flpdf-qtest/vendor/qpdf-qtest`

**Interfaces:**

- Consumes: the committed raw xref cutover and the pinned qpdf probe.
- Produces: source-faithful correspondence and paired qtest evidence.

- [ ] **Step 1: Update correspondence rows**

  Replace the stale `QPDFObjGen.cc -> ObjectRef -> ✅` claim with the actual
  split: `QpdfObjGen` owns raw xref identity and `ObjectRef` owns valid indirect
  references. Record that the linearization synthetic identity is not part of
  the raw xref primitive.

- [ ] **Step 2: Run focused qpdf probes**

  ```bash
  /usr/bin/qpdf --version
  /usr/bin/qpdf --check /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/fax-decode-parms.pdf
  cargo run --release --bin flpdf -- --check /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/fax-decode-parms.pdf
  ```

  Expected: both check commands exit successfully without the spurious
  recovery-warning sequence.

- [ ] **Step 3: Run focused Rust and formatting gates**

  ```bash
  cargo fmt --all -- --check
  cargo test -p flpdf --lib xref
  cargo test -p flpdf --test qpdf_obj_gen_xref_tests
  cargo test -p flpdf --test reader_tests
  ```

- [ ] **Step 4: Run focused qtest and inspect the paired artifacts**

  Run `decode-parameters` through qtest-driver with the current release
  binaries, then verify the same-run XML reports six passes and retain the
  matching harness log. Do not tee into qtest's own `qtest.log`.

- [ ] **Step 5: Run the full qtest corpus**

  ```bash
  QTEST_FULL=1 FLPDF_DIR=/home/ubuntu/flpdf/.worktrees/flpdf-3yn9-48-74 \
    /home/ubuntu/flpdf-qtest/scripts/run.sh
  ```

  Verify the authoritative `qtest-results.xml` and `harness.log` pair, then
  classify every changed manifest row under the existing flpdf-qtest rules.

- [ ] **Step 6: Run workspace quality gates and commit docs**

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
  git add docs/qpdf-correspondence.md crates/flpdf/src/qpdf_obj_gen.rs
  git commit -m "docs: record qpdf raw xref identity correspondence"
  ```

## Plan self-review

- Spec coverage: raw identity, free tombstones, valid-reference conversion,
  parser boundary, linearization separation, RED/GREEN tests, qtest paired
  artifacts, correspondence, and quality gates each have an explicit task.
- Completeness scan: no unfinished-marker or unspecified implementation steps remain.
- Type consistency: all raw xref steps use `QpdfObjGen`; all effective resolver
  and public steps use `ObjectRef`; `XrefEntry` remains the value type.
- The plan deliberately leaves the separate linearization sentinel issue out of
  scope, as required by the spec and the existing Beads ownership boundary.
