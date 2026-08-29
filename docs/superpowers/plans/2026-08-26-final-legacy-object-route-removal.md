# Final Legacy Object Route Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove flpdf's raw `Object` resolution/materialization route after all consumers have moved to the qpdf-shaped `ObjectHandle` graph, with no compatibility alias or wrapper remaining.

**Architecture:** `Pdf::get_object_handle`, `Pdf::resolve`, `Pdf::get_all_objects`, and `Pdf::trailer` remain the document-level access boundaries. Consumers use live handle accessors and handle-native mutation/writer APIs. The legacy resolver methods, materialization memo, and public `Object` projection are deleted; no qpdf-incompatible bridge is added.

**Tech Stack:** Rust workspace, cargo test/clippy/rustdoc, qpdf 11.9.0 oracle, Beads, GitHub stacked PRs, `cargo llvm-cov` patch coverage.

---

### Task 1: Migrate the signatures consumer slice (RED)

**Files:**
- Modify: `crates/flpdf/tests/legacy_route_cutover_tests.rs`
- Read: `crates/flpdf/src/signatures.rs`, `crates/flpdf/tests/signature_tests.rs`, `crates/flpdf/tests/sig_flags_tests.rs`

- [ ] **Step 1: Write the failing source contract test**

Add `signatures_production_uses_the_canonical_handle_route`. Read `signatures.rs` with `include_str!` and reject raw resolver calls, `Object::`, raw `Dictionary` imports, `set_object`, and `materialize`; require `ObjectHandle`, a local one-hop `resolve_handle`, and `mark_object_handle_dirty`.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p flpdf --test legacy_route_cutover_tests signatures_production_uses_the_canonical_handle_route -- --exact
```

Expected: an assertion failure naming a remaining signatures route marker, not a compile error.

- [ ] **Step 3: Commit the contract**

```bash
git add crates/flpdf/tests/legacy_route_cutover_tests.rs
git commit -m "test: guard signature legacy route removal"
```

### Task 2: Remove the reader-owned materialization bridge

**Files:**
- Modify: `crates/flpdf/src/reader.rs`, `pdf.rs`, `engine.rs`, `object_handle.rs`, `lib.rs`
- Test: existing reader/engine tests for lazy resolution, dangling objects, replacement, streams, and JSON ordering

- [ ] **Step 1: Classify every core bridge caller**

```bash
rg -n 'resolve_borrowed|resolve_object|resolve_to_cache|legacy_materialized|\\.materialize\\(\\)' crates/flpdf/src/reader.rs crates/flpdf/src/pdf.rs crates/flpdf/src/engine.rs
```

For each production caller, acquire a live handle with `get_object_handle`, call `Pdf::resolve`, and use the corresponding `try_get_key`, `try_as_dictionary`, `try_array_item`, stream, or typed accessor. Use `set_object_handle` for test fixtures.

- [ ] **Step 2: Delete the memo and old methods**

Remove `legacy_materialized_memo`, `legacy_materialized_replacement_refs`, `materialize_canonical_compatibility_value`, `materialize_handle_for_legacy`, `reconcile_legacy_materialized_memos`, `resolve_to_cache`, `resolve_object`, and `resolve_borrowed`, plus their invalidation calls.

- [ ] **Step 3: Delete the public raw projections**

Remove `ObjectHandle::materialize` and the `Object` re-export. Keep `ObjectRef` only as an identity key where the qpdf contract requires it. Update docs to handle accessors; do not add an alias.

- [ ] **Step 4: Verify GREEN**

```bash
cargo test -p flpdf reader:: --lib
cargo test -p flpdf engine:: --lib
cargo test -p flpdf --test legacy_route_cutover_tests
```

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/reader.rs crates/flpdf/src/pdf.rs crates/flpdf/src/engine.rs crates/flpdf/src/object_handle.rs crates/flpdf/src/lib.rs crates/flpdf/tests
git commit -m "refactor: remove reader legacy object materialization bridge"
```

### Task 3: Migrate remaining production consumers

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs`, `embedded_files.rs`, `filespec_helper/mod.rs`, `job/overlay.rs`, `job/page_merge.rs`, `json_inspect.rs`
- Modify: `linearization/{check,plan,show,writer}.rs`, `nntree.rs`, `pages/repair.rs`, `pages/tree_rebuild.rs`, `resources.rs`, `signatures.rs`, `struct_tree_pg.rs`, `job/page_subset.rs`, `writer/reachability.rs`
- Modify: `object_copy.rs`, `job/outline_dest_remap.rs`, `page_label_document_helper.rs`, `ref_chain.rs`, `writer.rs`, `xref.rs`, `reader/resolver.rs`, `writer/{pclm,rewrite_renumber}.rs`, `writer/plain/{body,plan}.rs`, `writer/object_streams/mod.rs`, `crates/flpdf-cli/src/main.rs`
- Test: the corresponding module and route-contract tests

- [ ] **Step 1: Migrate by qpdf responsibility**

Before each file change, read its pinned qpdf owner. Replace raw resolver calls with the owner helper's live handle API, preserve qpdf one-hop dereference/null/error timing, and delete flpdf-only bare-reference chasing where qpdf has no counterpart. Do not add `materialize`, `lift`, or snapshot adapters.

- [ ] **Step 2: Add production-region route guards**

Each guard scans only the production region and rejects `Object::`, raw resolver calls, `materialize`, `lift_object_to_handle`, and `set_object` where the qpdf owner requires handles; it also asserts the canonical helper or mutation method.

- [ ] **Step 3: Run focused GREEN tests after each group**

```bash
cargo test -p flpdf --test legacy_route_cutover_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
```

- [ ] **Step 4: Commit each independently green responsibility group**

Use qpdf-responsibility commit subjects and leave the workspace compiling after every commit.

### Task 4: Migrate tests and external consumers

**Files:**
- Modify: remaining test modules under `crates/flpdf/src`
- Modify: `crates/flpdf/tests/**/*.rs`, `crates/flpdf-cli/tests/**/*.rs`, and `crates/flpdf-qtest-tools/src/**/*.rs`

- [ ] **Step 1: Replace raw assertions and fixtures**

Use live handle typed accessors instead of snapshot equality, `ObjectHandle::dictionary/array/name/integer/string/null` for fixtures, and `set_object_handle` plus dirty marking for mutation. Delete tests whose only behavior is the flpdf-specific bare-reference redirect or clone/materialization route.

- [ ] **Step 2: Prove the route inventory is clean**

```bash
rg -n 'resolve_borrowed|resolve_object|resolve_to_cache|legacy_materialized|\\.materialize\\(\\)|pub use object::.*Object|\\bObject::' crates/flpdf crates/flpdf-cli crates/flpdf-qtest-tools --glob '*.rs'
```

Remaining matches must be an explicitly qpdf-owned internal serialization/value boundary, not a public consumer route or compatibility bridge.

- [ ] **Step 3: Run the full test suite**

```bash
cargo test --workspace --all-features
```

- [ ] **Step 4: Commit the test/API cutover**

```bash
git add crates/flpdf crates/flpdf-cli crates/flpdf-qtest-tools
git commit -m "test: migrate consumers off raw object snapshots"
```

### Task 5: Verify, rebase, open the PR, and synchronize Beads

**Files:**
- Modify: `docs/qpdf-correspondence.md` only if the final current-state row needs updating
- Modify: Beads notes for `flpdf-egzr.3.2.8`

- [ ] **Step 1: Run all local gates**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
bash scripts/qpdf-test-driver-diff.sh --check
cargo test --workspace --all-features
```

- [ ] **Step 2: Run fresh patch coverage**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
bash scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: `uncovered 0` and `PASS (100%)` for changed executable lines.

- [ ] **Step 3: Rebase and push**

```bash
git fetch origin --prune
git rebase origin/main
git push --force-with-lease --set-upstream origin feature/flpdf-egzr-3-2-8-final-legacy-removal
```

- [ ] **Step 4: Create a Draft PR**

Include qpdf source paths, live probes, before/after route inventory, focused tests, all gates, and patch coverage. Use real Markdown and omit prohibited merge wording.

- [ ] **Step 5: Ready only after all CI is green**

Read back `gh pr checks`, `gh pr view`, current head/base, and the rendered body. Run `gh pr ready` only after Coverage/Codecov, Fuzz, all OS tests, Quality, Analyze, and release gates pass. Do not merge.

- [ ] **Step 6: Synchronize Beads**

Append implementation/PR/verification evidence, run `bd dep cycles`, read back the issue, and run `bd dolt push` until it reports exactly `Push complete.`
