# Linearization raw QpdfObjGen follow-ups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every linearization plan, ordering, hint, ObjStm, stream-parameter, and encryption decision preserve qpdf 11.9.0's complete `QPDFObjGen` identity for raw generations such as `5 65536`.

**Architecture:** Keep `QpdfObjGen` as the canonical identity from the resolver through `Optimization`, `LinearizationPlan`, `RenumberMap`, hint inputs, and body emission. Use `ObjectRef` only at explicitly checked public/output boundaries; do not manufacture a synthetic `ObjectRef` for a raw generation. Replace the eight ObjectRef-only projections with raw-aware views that use qpdf's source-object order and translate to output numbers only when a hint field or output reference requires it.

**Tech Stack:** Rust workspace, `ObjectHandle`, `QpdfObjGen`, `Optimization`, linearization plan/renumber/hint writers, qpdf 11.9.0 CLI/source oracle, `cargo llvm-cov`.

**Spec:** Beads issue `flpdf-pwyo2`, qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, and the corresponding rows in `docs/qpdf-correspondence.md` and `docs/qpdf-route-matrix/d-writer.md`.

## Global Constraints

- qpdf 11.9.0 source and live output are authoritative; the pinned source commit is `3b97c9bd266b7c32ea36d3536e22dab77412886d`.
- Raw identities use `QpdfObjGen`; `QpdfObjGen::to_object_ref()` remains a checked projection and never supplies a sentinel identity.
- Preserve qpdf's `QPDF_optimization.cc:261-333` traversal scope and `QPDF_linearization.cc:1228-1402` part/order rules.
- Do not add a qtest-only shim, output rewrite, compatibility bridge, or qpdf-deviation marker for behavior qpdf already defines.
- Every behavior change begins with a real RED test and ends with focused differential coverage.
- Before delivery run fmt, focused tests, workspace all-features tests, strict private rustdoc, all-features clippy, qpdf documentation/deviation/route checks, and clean committed patch coverage against `origin/main`.

---

### Task 1: Establish RED coverage for all raw-only linearization gaps

**Files:**
- Modify: `crates/flpdf/tests/qpdf_obj_gen_header_tests.rs`
- Modify: `crates/flpdf/src/linearization/plan.rs` test module only when a unit-level raw fixture is needed
- Modify: `crates/flpdf/src/linearization/hint_page.rs` and `crates/flpdf/src/linearization/hint_shared.rs` test modules only when a direct plan assertion is needed

**Interfaces:**
- Consumes: existing `matching_out_of_range_*` fixtures and `Pdf::get_object_handle_by_raw_identity`.
- Produces: independent tests that fail against the current eight ObjectRef-only projections and assert qpdf-derived raw order/bytes rather than implementation details.

- [ ] **Step 1: Write the encryption RED test.** Attach a `5 65536` stream marked `/Type /Metadata` to the Catalog, set V=4 encryption with `encrypt_metadata = false`, write linearized output, and assert that the raw metadata payload is not treated as a generic encrypted payload. Add a second raw non-metadata stream in the same document so `None == None` cannot satisfy the assertion accidentally.
- [ ] **Step 2: Write RED tests for raw content normalization and parameter omission.** Give a raw page-content stream a filter/normalization-sensitive dictionary and an indirect parameter child; assert the output drops/rebuilds the same parameter edges as qpdf's `initializeSpecialStreams` plus `QPDF::optimize` path. The tests must inspect the emitted object graph and not merely the presence of a raw object.
- [ ] **Step 3: Write RED tests for raw Part-8, outline, and Part-7 order.** Attach raw objects to two later pages, attach a raw `/Outlines` graph, and attach raw private objects to different pages with intentionally interleaved object numbers. Assert the raw source order of emitted objects and that the outline hint `/O` data is present and covers the correct consecutive output units.
- [ ] **Step 4: Write RED tests for raw ObjStm anchors and page shared identifiers.** Combine at least two eligible ObjStm containers with a raw plain peer and make valid public shared objects have output order different from source order. Assert container placement and shared identifier order against a hand-derived qpdf order.
- [ ] **Step 5: Run the new tests before production changes.** Run `cargo test -p flpdf --test qpdf_obj_gen_header_tests -- --test-threads=1` and the relevant linearization unit targets. Record the expected failure for each test and keep the failures attributable to the eight missing raw routes.
- [ ] **Step 6: Commit the RED tests.** Use `git add` only for the test files and commit with `test(linearization): cover raw identity follow-ups`.

### Task 2: Make linearization graph and stream-policy inputs raw-first

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:116-360,981-1183,1322-1630`
- Modify: `crates/flpdf/src/writer/rewrite_renumber.rs:259-480`
- Test: `crates/flpdf/src/linearization/plan.rs` tests and Task 1 differential tests

**Interfaces:**
- Consumes: `Optimization::raw_object_users`, `QpdfObjGen`, `QpdfObjGen::new_for_raw`-compatible handles, and qpdf's stream policy callback.
- Produces: raw-aware content normalization and skipped-parameter sets, plus raw-aware closure/reachability inputs that preserve edge type and stream identity through plan construction.

- [ ] **Step 1: Replace the ObjectRef-only content normalization collector.** Collect page `/Contents` as canonical `QpdfObjGen` values using the same immediate stream/array inspection as `QPDFWriter::initializeSpecialStreams` (`QPDFWriter.cc:1912-1936`). Preserve direct-stream omission and do not chase flpdf-only holder chains.
- [ ] **Step 2: Thread raw skipped-stream-parameter identities through closure collection.** Change the stream child walker and its callers so a raw stream whose filter probe returns level 2 skips `/Filter` and `/DecodeParms` during reachability and page closure, exactly as `QPDF_optimization.cc:306-333` does. Keep the existing `ObjectRef` adapter only for legacy test plans that cannot carry raw identities.
- [ ] **Step 3: Preserve raw edge classification and null handling.** Ensure direct array edges, dictionary edges, missing/free rows, and raw indirect handles retain their `QpdfObjGen` identity until the plan partitions them. Do not use a numeric sentinel for an absent or unprojectable raw identity.
- [ ] **Step 4: Run the Task 1 stream-policy tests.** Run the focused plan tests and raw header tests; verify the RED failures for normalization/parameter omission become GREEN without changing ordinary valid-generation output.
- [ ] **Step 5: Commit the graph/policy cutover.** Use `git add` for the plan and reachability files plus their tests and commit with `fix(linearization): retain raw stream policy identities`.

### Task 3: Correct raw part ordering, outline metadata, ObjStm anchors, and shared IDs

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:946-1183,2550-2692`
- Modify: `crates/flpdf/src/linearization/renumber.rs:760-1171`
- Modify: `crates/flpdf/src/linearization/writer.rs:1996-2071,3134-3246,3630-3835`
- Modify: `crates/flpdf/src/linearization/hint_page.rs:420-640`
- Modify: `crates/flpdf/src/linearization/hint_shared.rs:180-330`
- Test: `crates/flpdf/tests/qpdf_obj_gen_header_tests.rs`, linearization plan/renumber/hint unit modules

**Interfaces:**
- Consumes: raw plan vectors from Task 2 and output slots from `RenumberMap`.
- Produces: source-order-stable raw part vectors, raw outline hint inputs, raw second-half anchor keys, and shared-identifier lists ordered by qpdf's raw object-user set.

- [ ] **Step 1: Preserve qpdf order while merging raw and checked plan vectors.** Merge raw extras into their owning page/category at the same position implied by `QPDFObjGen` order or page traversal; never append raw Part-8 or Part-7 members after a checked suffix when qpdf's set order interleaves them.
- [ ] **Step 2: Make outline hint calculation raw-aware.** Derive the `/Outlines` source identity from the raw root-key user set, map it through `RenumberMap::new_for_raw`, and count the raw outline units using the same consecutive-unit rule as `pushOutlinesToPart` and `calculateHOutline` (`QPDF_linearization.cc:1406-1432,1614-1631`).
- [ ] **Step 3: Make second-half anchors use raw order keys.** Represent an anchor as a raw `QpdfObjGen` or explicit `BeforeFirst`/`AfterLast`, and have `second_half_container_anchors` inspect raw plain peers as well as checked peers. Preserve the existing source-container behavior for Preserve mode.
- [ ] **Step 4: Restore qpdf raw page-user order in `hint_page`.** For every shared hint entry, order page identifiers from the raw `Optimization` user set and then translate each entry to its hint-table index. Do not switch all entries to output-number sorting merely because one raw entry cannot project.
- [ ] **Step 5: Run focused ordering tests.** Run `cargo test -p flpdf --lib linearization::plan::tests`, `...::renumber::tests`, `...::hint_page::tests`, `...::hint_shared::tests`, and the raw header target. Confirm the new raw tests are GREEN and ordinary qpdf-zlib-compatible ordering remains unchanged.
- [ ] **Step 6: Commit the ordering cutover.** Commit with `fix(linearization): preserve raw part and hint ordering`.

### Task 4: Finish raw encryption identity, documentation, and delivery verification

**Files:**
- Modify: `crates/flpdf/src/writer.rs:2350-2420,2686-2707`
- Modify: `crates/flpdf/src/linearization/writer.rs:644-745,4069-4525`
- Modify: `docs/qpdf-correspondence.md` in the linearization/raw identity section
- Modify: `docs/qpdf-route-matrix/d-writer.md` in D17/D21/D22/D26/D31 annotations
- Test: `crates/flpdf/tests/qpdf_obj_gen_header_tests.rs` and relevant writer tests

**Interfaces:**
- Consumes: raw metadata identity and all raw plan/hint outputs from Tasks 2–3.
- Produces: qpdf-equivalent metadata stream classification and a documented, test-backed raw linearization route.

- [ ] **Step 1: Replace metadata projection comparison.** Preserve the metadata stream's raw identity or use qpdf's stream-dictionary `/Type /Metadata` classification at the same writer boundary (`QPDFWriter.cc:1242-1281,1537-1556`). Ensure only the metadata stream skips encryption when `encrypt_metadata` is false; every other raw stream remains encrypted.
- [ ] **Step 2: Run the complete focused differential matrix.** Run raw-generation tests across Disable/Generate, compact/QDF, stream/non-stream, encryption, `--compress-streams`, outline, Part-7/Part-8, and ObjStm cases. For generated files run `qpdf --check-linearization` and compare qpdf-zlib-compatible bytes where the qpdf API shape is representable.
- [ ] **Step 3: Update and validate correspondence evidence.** Document each raw-first owner and cite the exact qpdf source lines; mark no behavior as a deviation because all eight rules have qpdf counterparts. Run `python3 scripts/qpdf-module-docs.py --check`, `python3 scripts/check-qpdf-deviation-markers.py --check`, and `python3 scripts/check-qpdf-route-matrix.py --check`.
- [ ] **Step 4: Run all quality gates.** Run `cargo fmt --all -- --check`; focused tests; `cargo test --workspace --all-features --quiet`; strict private rustdoc with `cargo +1.97.1 doc`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; and `scripts/patch-coverage.sh --base origin/main` on a clean committed tree.
- [ ] **Step 5: Read back the intended diff and commit.** Confirm only the planned source/tests/docs changed, record the final commit SHA and coverage result in Beads, and commit any final documentation/test adjustments.
- [ ] **Step 6: Rebase, publish, and lifecycle-gate the PR.** Fetch the latest `origin/main`, rebase before push, create a Draft PR, wait for every required CI check including patch coverage, then mark the PR Ready. Immediately close `flpdf-pwyo2` with the PR/CI reason, run `bd dep cycles`, run `bd dolt push` and confirm `Push complete.`, then `git push`. Do not merge in this implementation session.
