# Linearization Raw QpdfObjGen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make linearized output preserve qpdf's raw `QPDFObjGen` identity for reachable objects whose generation cannot be projected to `ObjectRef`, while retaining qpdf's intentional generation-discard check for linearization-only hint maps.

**Architecture:** `QpdfObjGen` is the canonical key through the linearization optimization map, reachability walk, plan, renumber map, and two-pass serializer. Existing public `ObjectRef` fields and APIs remain explicit checked projection views for callers that require PDF `N G R` references; the writer consumes private raw plan views and never converts a raw identity per emitted object. A single raw removed-generation set is constructed at the writer boundary and borrowed by both passes.

**Tech Stack:** Rust workspace, `ObjectHandle`, `QpdfObjGen`, `BTreeMap`/`BTreeSet`, qpdf 11.9.0 source oracle, `cargo test`, `cargo llvm-cov`, and `qpdf-zlib-compat` differential fixtures.

**Spec:** `docs/qpdf-route-matrix/d-writer.md` (D-6, D-18, D-20, D-21, D-22, D-31) and Beads issue `flpdf-474u8`.

## Global Constraints

- qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, pinned HEAD `3b97c9bd266b7c32ea36d3536e22dab77412886d` is authoritative.
- `QPDFWriter::obj_renumber` remains keyed by raw `QPDFObjGen`; `discardGeneration` remains a single checked conversion only for qpdf's object-number-only linearization hint routines.
- Do not add synthetic `ObjectRef` identities, sentinel object numbers for raw source objects, legacy bridges, or output rewrites that inline raw indirect children.
- Write each test before its production change, observe the expected RED failure, then make the smallest qpdf-shaped change that turns it GREEN.
- Preserve the existing public `ObjectRef` projection error contract for page identities and preserve all existing qpdf differential byte tests.

---

### Task 1: Add raw-generation linearization RED coverage

**Files:**
- Modify: `crates/flpdf/tests/qpdf_obj_gen_header_tests.rs`

**Interfaces:**
- Consumes: existing raw-generation fixture helpers and `PdfWriter` API.
- Produces: a real one-page PDF with `5 65536 obj`, a Catalog `/RawChild` edge to the raw handle, and an assertion that linearized output succeeds, keeps `/RawChild` as a reference, and emits `45` as an object body.

- [x] **Step 1: Write the failing test**

  Add `matching_out_of_range_one_page_pdf` with explicit xref offsets and add `linearized_renumber_keeps_a_raw_generation_child_as_a_reference` using `ObjectStreamMode::Disable` and a static ID.

- [x] **Step 2: Run test to verify it fails**

  Run:

  ```bash
  cargo test -p flpdf --test qpdf_obj_gen_header_tests linearized_renumber_keeps_a_raw_generation_child_as_a_reference -- --nocapture
  ```

  Expected: `Unsupported("qpdf raw object identity 5 65536 cannot be used by an ObjectRef map")` from the linearization writer.

- [x] **Step 3: Keep the test as the first implementation gate**

  Do not weaken the assertions to accept an inlined `45` or a missing Catalog key.

### Task 2: Move optimization and reachability identity to raw keys

**Files:**
- Modify: `crates/flpdf/src/optimization.rs`
- Modify: `crates/flpdf/src/linearization/plan.rs`
- Modify: `crates/flpdf/src/writer/rewrite_renumber.rs`
- Modify: `crates/flpdf/src/reader.rs` when a raw canonical cache enumeration is required

**Interfaces:**
- Consumes: `ObjectHandle::qpdf_obj_gen()`, `ResolverCore::object_cache`, and valid page references from `PageDocumentHelper`.
- Produces: raw-keyed `Optimization` user/object maps, raw reachability and null-resurrection sets, and raw linearization plan partitions. Existing public plan fields are populated only through checked projections; private raw fields are the writer source of truth.

- [x] **Step 1: Write focused raw-plan tests**

  Extend the raw-generation test or a plan unit test to assert that the raw Catalog child is present in the plan's internal part-4 universe and receives a `RenumberMap` slot, without manufacturing an `ObjectRef` for `5 65536`.

- [x] **Step 2: Run the focused RED tests**

  Run the raw header test and the relevant linearization plan tests. Expected failure is either the existing ObjectRef conversion error or an absent raw plan slot, not a panic.

- [x] **Step 3: Implement raw user/object maps**

  Change `Optimization` identity maps and callback stream identities to `QpdfObjGen`. Convert valid page roots to raw keys at the boundary, and record every indirect child from `qpdf_obj_gen()` regardless of `to_object_ref()`.

- [x] **Step 4: Implement raw reachability**

  Change the linearization reachability and resurrectable-null walks to carry `QpdfObjGen`; keep object number zero non-indirect and keep stream `/Length` omission and array-vs-dictionary null visibility unchanged.

- [x] **Step 5: Build raw plan partitions and checked public projections**

  Keep the qpdf part ordering and outline/open-document precedence identical. Store raw part vectors, raw root/pages/info identities, raw page-private lists, raw shared hints, raw content-normalization set, and raw removed set. Populate existing `ObjectRef` views with `filter_map(QpdfObjGen::to_object_ref)` only at the documented public projection boundary.

- [x] **Step 6: Run GREEN tests and existing plan suites**

  Run:

  ```bash
  cargo test -p flpdf --test qpdf_obj_gen_header_tests
  cargo test -p flpdf --lib linearization::plan::tests
  cargo test -p flpdf --test cmp_linearize_tests
  ```

### Task 3: Make `RenumberMap` and ObjStm routing raw-keyed

**Files:**
- Modify: `crates/flpdf/src/linearization/renumber.rs`
- Modify: `crates/flpdf/src/linearization/plan.rs`
- Modify: `crates/flpdf/src/linearization/hint_page.rs`
- Modify: `crates/flpdf/src/linearization/hint_shared.rs`
- Modify: `crates/flpdf/src/writer/object_streams` only where raw plan members cross the gen-0 ObjStm eligibility boundary

**Interfaces:**
- Consumes: raw plan partitions and raw optimization user maps from Task 2.
- Produces: `RenumberMap` keyed by `QpdfObjGen` with output `ObjectRef` values, raw lookup methods for writer/hint consumers, and an explicit `ObjectRef` lookup adapter only for existing public/test projection callers.

- [x] **Step 1: Write raw renumber and raw removed-set tests**

  Add tests that map `QpdfObjGen::new(5, 65536)` to an output generation-zero reference, reject an absent raw key, and verify a raw removed identity is written as `null` without converting the set per object.

- [x] **Step 2: Run RED**

  Run the new renumber tests and the linearized raw-child test. Expected failure is the missing raw lookup or ObjectRef-map conversion error.

- [x] **Step 3: Implement raw `RenumberMap` lookup and partition placement**

  Replace internal `BTreeMap<ObjectRef, ObjectRef>` identity keys with `BTreeMap<QpdfObjGen, ObjectRef>`. Keep the qpdf duplicate-object-number error when `discardGeneration` would collapse two generations. Ensure gen-0 ObjStm members are projected only where the qpdf ObjStm contract requires gen 0.

- [x] **Step 4: Update hint builders**

  Make page/shared hint consumers query raw plan identities through raw renumber lookups; retain physical output object numbers and existing container sentinel representation only for generated hint-table entries, never as source identities.

- [x] **Step 5: Run GREEN renumber and hint suites**

  Run:

  ```bash
  cargo test -p flpdf --lib linearization::renumber::tests
  cargo test -p flpdf --lib linearization::hint_page::tests
  cargo test -p flpdf --lib linearization::hint_shared::tests
  cargo test -p flpdf --test linearize_objstm_generate_tests
  ```

### Task 4: Route both linearization passes through one raw serializer map/set

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`
- Modify: `crates/flpdf/src/writer/object.rs`
- Modify: `crates/flpdf/src/writer/encrypted_strings.rs`
- Modify: `crates/flpdf/src/writer.rs` only if encryption metadata identity must become raw

**Interfaces:**
- Consumes: raw `RenumberMap`, raw plan partitions, raw removed set, and `ObjectHandle` lookup by raw identity.
- Produces: `append_object`/`append_body_object` paths accepting `Fn(QpdfObjGen) -> Result<ObjectRef>` and `&BTreeSet<QpdfObjGen>`, with one raw removed set borrowed by pass 1, hint construction, and pass 2.

- [x] **Step 1: Write encrypted and stream raw-child coverage**

  Extend the raw-child fixture to cover a raw stream child under linearization and an encrypted linearized write. Add a focused test for the raw removed set to assert dictionary-key omission and array-position null behavior.

- [x] **Step 2: Run RED**

  Run the focused raw header and serializer tests. Expected failure is the ObjectRef conversion error or a missing raw serializer method.

- [x] **Step 3: Add/use raw writer emission methods**

  Use the existing qpdf-keyed writer walkers where available and add only the missing non-QDF/encrypted raw wrapper. Remove linearization calls to `qpdf_obj_gen_map_from_object_ref_map` and `qpdf_obj_gen_set_from_object_ref_set`; those helpers remain for non-linearization callers that explicitly accept an ObjectRef boundary.

- [x] **Step 4: Construct removed state once**

  Build the raw removed set once in `write_linearized_for_pdf_writer`, pass it by reference through `do_write_pass` and both body/trailer serializers, and clear no per-object temporary set. Preserve qpdf's single `discardGeneration` check and pass-2 reuse.

- [x] **Step 5: Run GREEN linearization coverage**

  Run:

  ```bash
  cargo test -p flpdf --test qpdf_obj_gen_header_tests
  cargo test -p flpdf --test cmp_linearize_tests
  cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
  cargo test -p flpdf --test linearize_objstm_generate_tests
  ```

### Task 5: Documentation, full verification, and delivery preparation

**Files:**
- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/qpdf-route-matrix/d-writer.md`
- Modify: `docs/qpdf-route-matrix/README.md` only if generated counts or current-main reconciliation requires it

**Interfaces:**
- Consumes: final raw implementation and qpdf source evidence.
- Produces: source-faithful correspondence describing raw linearization identity and the one-time removed-set boundary.

- [x] **Step 1: Update correspondence and route matrix**

  Cite `QPDFWriter.hh:668`, `QPDFWriter.cc:1057-1157,2510-2654,2858`, `QPDF_optimization.cc:57-118,264-381`, and `QPDF_linearization.cc:963-1064,1173-1449`. State that `ObjectRef` is projection-only and that `discardGeneration` is intentional qpdf logic, not a raw identity store.

- [ ] **Step 2: Run every required local gate**

  ```bash
  cargo fmt --all -- --check
  cargo test --workspace --all-features --quiet
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo +1.97.1 doc --workspace --no-deps --document-private-items
  python3 scripts/qpdf-module-docs.py --check
  python3 scripts/check-qpdf-deviation-markers.py --check
  python3 scripts/check-qpdf-route-matrix.py --check
  scripts/patch-coverage.sh --base origin/main
  ```

- [ ] **Step 3: Commit, rebase, and push**

  Commit the implementation and documentation after all tests are green, fetch the latest `origin/main`, rebase this branch, rerun the authoritative coverage on the rebased commit, and push the dedicated branch.

- [ ] **Step 4: Create and deliver the Draft PR**

  Create the PR as Draft with the qpdf source evidence and verification summary. Inspect exact base/head SHA and every required CI check; only after all checks are successful and merge state is CLEAN mark it Ready. Immediately close `flpdf-474u8`, run `bd dep cycles`, `bd dolt push` and verify `Push complete.`, then push git. Do not merge.
