# QDF/Normalize Generate Live Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move non-linearized, non-PCLm, unencrypted QDF and content-normalization Generate output from the planned writer to the qpdf-shaped specialized live queue without changing remaining routes.

**Architecture:** Reuse the setup snapshot and Generated ObjStm groups already consumed by plain Generate. Extend the existing LiveQueue/WriteObject body to accept those groups, use the QDF-specific ObjStm body serializer when QDF is enabled, and keep ordinary QDF length holders and normalization page-context state at their existing emission boundaries.

**Tech Stack:** Rust workspace, `flpdf`/`flpdf-cli`, qpdf 11.9.0 pinned source and binary oracle, `qpdf-zlib-compat`, cargo test/clippy/rustdoc/llvm-cov, Beads.

**Spec:** Beads issue `flpdf-3yn9.48.88` — `writer: QDF/normalize Generate を generated live queue へ cutover`.

## Global Constraints

- qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`, is the behavior and byte-output oracle.
- Preserve setup order: `initializeSpecialStreams`, Generate membership, and fresh placeholders precede `getObjectCount`, progress setup, `prepareFileForWrite`, and body emission.
- Keep Generate candidate order in qpdf DFS order through even splitting; apply `writer_object_order_key` only within each generated group after splitting.
- Keep QDF/normalize Preserve with source ObjStm, explicit/copy encryption, encrypted input, linearized, PCLm, and force-version-suppressed Generate on their current consumers.
- Keep qdf-specific formatting in the writer owner: QDF ObjStm pair tables/markers/direct container length and ordinary-stream output-only length holders are separate contracts.
- Use TDD: every production change follows a test observed failing for the intended reason.
- Do not add qtest-only shims, output post-processing, or qpdf-deviation markers for behavior qpdf already defines.

---

### Task 1: Pin the route boundary with RED tests

**Files:**
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/tests/writer_object_emission_tests.rs`

**Interfaces:**
- Consumes: `PlainRoute::classify_plain_route`, `WriterOptions`, and setup Generate state.
- Produces: route tests proving QDF Generate and normalize Generate leave the planned plain consumer, plus real callback-child behavior tests.

- [x] **Step 1: Write the failing route tests.** Add QDF Generate and normalize Generate assertions expecting `PlainRoute::OutsidePlain` and `!is_plain_consumer()`. Add one progress callback test for each mode that adds an indirect child to the live root and verifies it after reopening the output.

- [x] **Step 2: Run the exact tests and verify RED.**

```bash
cargo test -p flpdf writer::plain::tests --lib
cargo test -p flpdf --test writer_object_emission_tests qdf_and_normalize_generate_progress_callbacks_use_live_queue -- --exact
```

Expected: route assertions fail because both modes are still planned, and the callback test does not exercise a live Generated ObjStm queue.

- [x] **Step 3: Commit only the RED tests.**

```bash
git add crates/flpdf/src/writer/plain/mod.rs crates/flpdf/tests/writer_object_emission_tests.rs
git commit -m "test: pin QDF Generate live route"
```

### Task 2: Select the QDF/normalize Generate live consumer

**Files:**
- Modify: `crates/flpdf/src/writer/plain/mod.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Test: `crates/flpdf/src/writer/plain/mod.rs`

**Interfaces:**
- Consumes: existing `QdfOrNormalizeLive`, `OutsidePlain`, and `emit_specialized_standard_live_with_page_context`.
- Produces: an `OutsidePlain` classification for only the Generate + QDF/normalize cohort, with an explicit outer-dispatch guard so it cannot fall through to `write_plain` or the legacy planned coordinator.

- [x] **Step 1: Add the route classification and predicate.** Return `OutsidePlain` for eligible Generate after the outer writer has normalized force-version suppression; QDF and content-normalization formatting are selected inside the specialized coordinator. Preserve the planned route when the requested/effective mode no longer matches, including force-version-suppressed Generate.

- [x] **Step 2: Connect outer dispatch.** In `emit_canonical_pdf_inner`, route eligible Generate to `emit_specialized_standard_live_with_page_context`. Pass setup-derived page/content context and `options.qdf`; leave encryption, PCLm, and linearized guards unchanged.

- [x] **Step 3: Run route tests and confirm the next isolated failure.**

```bash
cargo test -p flpdf writer::plain::tests --lib
cargo test -p flpdf --test writer_object_emission_tests qdf_and_normalize_generate_progress_callbacks_use_live_queue -- --exact
```

Expected: route assertions pass; the callback test still fails because the live coordinator has not yet been given Generated groups or QDF ObjStm serialization.

### Task 3: Pass Generated groups into the live body

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/writer/plain/body.rs`
- Test: `crates/flpdf/tests/writer_object_emission_tests.rs`

**Interfaces:**
- Consumes: `plan_object_streams_with_reachability_and_source_membership`, `generated_compressible`, `generated_object_stream_sources`, and `LiveQueue::register_object_streams`.
- Produces: specialized live body input carrying the same Generated group identities and member order as plain Generate.

- [x] **Step 1: Preserve the setup snapshot.** Keep the candidate vector in qpdf DFS order through `even_split_into_streams_with_cap`; map each retained batch to its setup-created placeholder; apply output-sensitive root exclusion before queue registration.

- [x] **Step 2: Pass the groups and two-pass flag.** Reuse the specialized coordinator's existing Generate handoff, which already builds `ObjectStreamGroup::Generated` from the setup snapshot and passes it to `emit_live_specialized_standard_with_page_context`. The live body keeps `two_pass_object_streams=true` for this coordinator, so QDF and normalize Generate use the same qpdf progress and offset timing.

- [x] **Step 3: Run body tests.**

```bash
cargo test -p flpdf --test writer_object_emission_tests
cargo test -p flpdf --test cmp_generate_objstm_tests --features qpdf-zlib-compat
```

Expected: ordinary and non-QDF Generate tests remain green; QDF Generate fails only at QDF-specific ObjStm framing.

### Task 4: Implement QDF-aware live ObjStm emission

**Files:**
- Modify: `crates/flpdf/src/writer/plain/body.rs`
- Test: `crates/flpdf/tests/writer_object_emission_tests.rs`
- Test: `crates/flpdf-cli/tests/cli_pages_objstm_order_qpdf.rs`

**Interfaces:**
- Consumes: `emit_objstm_body_from_handles_with_writer_qdf`, `WriteObject`, `QdfObjectInfo`, `LiveQueue`, and `LiveObjectEmitter::unparse_qdf_object`.
- Produces: QDF pair table, object-stream markers, direct container length, `/N`, `/First`, and two-pass member mutation behavior.

- [x] **Step 1: Write QDF RED assertions.** Use the existing qpdf byte-parity gate as the RED oracle after route cutover, then add explicit live regressions for callback child discovery and first-pass ObjStm offsets. The CLI fixture asserts the QDF marker/pair-table/direct-container shape through byte equality with qpdf 11.9.0.

- [x] **Step 2: Add the QDF branch.** Use the QDF body helper for both passes. The member callback applies QDF object-info, dynamic queue mapping, original-object-ID suppression, page/content comments, and exactly one member newline. The generated container uses direct length and does not receive an ordinary member holder.

- [x] **Step 3: Test live mutation timing.** The existing specialized member mutation test asserts second-pass visibility; the QDF-specific regression mutates the first member between passes and asserts that the final pair table still uses qpdf's first-pass offset. The route callback test covers queue discovery before the static QDF map is read.

- [x] **Step 4: Verify focused tests.**

```bash
cargo test -p flpdf --test writer_object_emission_tests
cargo test -p flpdf-cli --test cli_pages_objstm_order_qpdf --features qpdf-zlib-compat
```

### Task 5: Differential matrix and negative routes

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_pages_objstm_order_qpdf.rs`
- Modify: `crates/flpdf/tests/cmp_generate_objstm_tests.rs` only if a library-only case is required

- [x] **Step 1: Add qpdf differential cases.** Cover QDF Generate and normalize Generate for one/two/three-page, source ObjStm, multi-source pages, more than 100 eligible objects, missing/dangling references, indirect stream lengths, and preserve-unreferenced. Use same-run qpdf 11.9.0 status/stderr/bytes comparisons.

- [x] **Step 2: Protect excluded routes.** Verify QDF/normalize Preserve with source ObjStm, explicit encryption, encrypted input, linearized Generate, PCLm, and force-version-below-1.5 Generate retain their existing route and parity tests.

- [x] **Step 3: Run differential suites.**

```bash
cargo test -p flpdf --test cmp_generate_objstm_tests --features qpdf-zlib-compat
cargo test -p flpdf --test cmp_diff_zero_tests --features qpdf-zlib-compat
cargo test -p flpdf-cli --test cli_pages_objstm_order_qpdf --features qpdf-zlib-compat
cargo test -p flpdf-cli --test cli_qdf --features qpdf-zlib-compat
```

- [x] **Step 4: Run the full crate suites.**

```bash
cargo test -p flpdf --features qpdf-zlib-compat
cargo test -p flpdf-cli --features qpdf-zlib-compat
```

### Task 6: Route documentation and Beads evidence

**Files:**
- Modify: `docs/qpdf-route-matrix/d-writer.md`
- Modify: `docs/qpdf-correspondence.md` only where current status is stale
- Update: Beads issue `flpdf-3yn9.48.88`

- [x] **Step 1: Update D2/D3/D5/D7/D8/D11/D26/D28.** Record QDF/normalize Generate as specialized live after this slice, and retain source-backed Preserve QDF/normalize, encryption, linearization, and PCLm as separate consumers. State DFS-before-split and within-group provenance ordering.

- [x] **Step 2: Record qpdf citations and tests.** Include setup snapshot, callback discovery, QDF markers/holders, excluded routes, qpdf source lines `QPDFWriter.cc:1621-1809,1970-2006,2038-2140,2907-3031` and `QPDF.cc:2393-2474`.

- [x] **Step 3: Run documentation checks.**

```bash
python3 scripts/check-qpdf-route-matrix.py --check
python3 scripts/qpdf-module-docs.py --check
```

- [x] **Step 4: Add Astra's read-only Go review and the full test evidence to Beads. Do not close the issue before integrated-main readback.**

### Task 7: Quality gates and delivery

**Files:**
- Modify only the implementation, test, documentation, and plan files listed above.

- [x] **Step 1: Run local quality gates.**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

- [x] **Step 2: Re-read the Beads issue, inspect the clean diff, and commit.**

```bash
git diff --check
git status --short
git add crates/flpdf/src/writer.rs crates/flpdf/src/writer/plain crates/flpdf/src/writer/object_streams crates/flpdf/tests crates/flpdf-cli/tests docs/qpdf-route-matrix docs/qpdf-correspondence.md docs/superpowers/plans/2026-09-13-qdf-normalize-generate-live.md
git commit -m "refactor: move QDF Generate to live queue"
```

- [ ] **Step 3: Push, create a Draft PR, and mark it Ready only after all required CI checks pass. Do not merge from this implementation session.**

- [ ] **Step 4: After external merge, fetch `origin/main`, rerun focused integrated-main tests, close `flpdf-3yn9.48.88` with the exact merge SHA, update the parent/Beads remote, and keep the parent epic open for remaining mixed/bridge rows.**
