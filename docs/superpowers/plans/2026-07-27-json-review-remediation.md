# JSON Stacked PR Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every actionable review finding across JSON stacked PRs #559-#562 while preserving qpdf 11.9.0 behavior and 100% per-PR patch coverage.

**Architecture:** Keep qpdf's generic `Json` map semantics separate from the raw-PDF-name ordered inspection sink. Apply each change on its owning stacked branch, then rebase all dependent branches and verify each diff against its immediate parent.

**Tech Stack:** Rust, qpdf 11.9.0 oracle source and executable, cargo-llvm-cov, gh-stack, Beads.

## Global Constraints

- qpdf 11.9.0 source and observed output are the semantic oracle.
- Every changed PR must have 100% patch coverage against its immediate parent.
- Preserve partial JSON already accepted by the sink on fatal errors.
- Do not reply to or resolve GitHub review threads without separate authorization.
- Keep the branch order core → parser → validation → integration.

---

### Task 1: Batch Base64 writes and refresh the fuzz lockfile

**Files:**
- Modify: `crates/flpdf/src/json/writer.rs`
- Test: `crates/flpdf/src/json/writer.rs`
- Modify: `fuzz/Cargo.lock`

**Interfaces:**
- Consumes: `Json::make_blob` callbacks writing arbitrary fragment sizes.
- Produces: identical Base64 bytes with bounded downstream write calls.

- [ ] **Step 1: Write the failing batching test**

Add a real `Write` sink that records accepted chunks. Feed a blob larger than
two encoder chunks and assert the emitted JSON equals the hand-derived
`base64::engine::general_purpose::STANDARD` result while the sink receives
bounded multi-byte chunks rather than one four-byte write per input triple.

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p flpdf json::writer::tests::blob_batches_base64_output -- --exact
```

Expected: failure because the existing writer emits a four-byte downstream
write for every three source bytes.

- [ ] **Step 3: Implement bounded chunking**

Buffer complete three-byte groups, encode them with
`STANDARD.encode_slice`, write the encoded chunk once, and retain only the
zero-to-two-byte remainder. Finish by encoding the remainder with qpdf-compatible
padding.

- [ ] **Step 4: Verify behavior and the standalone fuzz workspace**

Run:

```bash
cargo test -p flpdf json::writer -- --nocapture
cargo check --manifest-path fuzz/Cargo.toml
cargo check --manifest-path fuzz/Cargo.toml --locked
```

Expected: all commands pass and `fuzz/Cargo.lock` contains the workspace's
`base64` dependency.

- [ ] **Step 5: Verify and commit the #559 layer**

Run formatting, focused tests, and direct-parent patch coverage against
`origin/main`; commit only core and fuzz-lock changes on
`feature/flpdf-qxba-6-1-json-core`.

### Task 2: Preserve schema error order with keyed lookups

**Files:**
- Modify: `crates/flpdf/src/json/schema.rs`
- Test: `crates/flpdf/tests/json_schema_tests.rs`

**Interfaces:**
- Consumes: qpdf-style shared `Json` dictionaries.
- Produces: identical boolean result and diagnostic ordering with ordered-map
  membership lookups.

- [ ] **Step 1: Add a large disjoint-dictionary regression**

Build schema and value dictionaries with thousands of distinct literal keys.
Assert the first and last missing-key diagnostics, the first and last
extra-key diagnostics, and the total count. This catches skipped entries and
error-order drift while exercising the lookup-heavy path.

- [ ] **Step 2: Run the focused regression**

Run:

```bash
cargo test -p flpdf --test json_schema_tests large_disjoint_dictionaries_preserve_qpdf_error_order -- --exact
```

Expected before optimization: semantic PASS; retain this as a characterization
gate because complexity is not safely asserted with wall-clock timing.

- [ ] **Step 3: Replace vector scans with ordered maps**

Return `BTreeMap<Vec<u8>, Json>` snapshots from `dictionary_items`. In the
schema pass use `values.get(key)`; in the value pass use
`schema_items.contains_key(key)`. Keep the two loops and their order unchanged.

- [ ] **Step 4: Verify and commit the #561 layer**

Run the focused schema suites and direct-parent patch coverage against the
rebased parser branch; commit only validation changes on
`feature/flpdf-qxba-6-3-json-validation`.

### Task 3: Make the exact-output API boundary explicit

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Modify: `CHANGELOG.md` or public rustdoc migration notes as appropriate
- Test: `crates/flpdf/tests/` public API and qpdf exact-output tests

**Interfaces:**
- Consumes: materialized inspection helpers and the public incremental sink.
- Produces: one public qpdf-exact output path,
  `write_qpdf_json_v2_selected_objects_with_options`.

- [ ] **Step 1: Add a public replacement smoke test**

From an external integration test, call
`write_qpdf_json_v2_selected_objects_with_options` on the minimal fixture and
assert its metadata order and a raw-name ordering case against literal/qpdf
bytes.

- [ ] **Step 2: Restrict non-exact materialized helpers**

Change materialized builders whose `Json` return type loses raw-name or fixed
metadata ordering to `pub(crate)` or private visibility. Keep internal
structural consumers intact. Add migration rustdoc pointing callers to the
incremental sink.

- [ ] **Step 3: Verify qpdf parity**

Run the public replacement test, existing live-oracle JSON tests, and direct
CLI comparisons for regular output, `/dev/null`, inline streams, and raw-name
dictionaries.

- [ ] **Step 4: Commit as a breaking change**

Commit on `feature/flpdf-qxba-6-4-json-integration` with a `!` subject and a
`BREAKING CHANGE:` body that lists removed materialized APIs and their sink
replacement.

### Task 4: Restack and verify every PR

**Files:**
- No new source files.

**Interfaces:**
- Consumes: the three modified layer commits.
- Produces: four adjacent, remotely published stacked PRs.

- [ ] **Step 1: Rebase dependent branches**

Use `gh stack rebase`/Git rebase in bottom-up order, resolving only replay
conflicts caused by the lower-layer fixes.

- [ ] **Step 2: Run unchanged-finding regressions**

Rerun parser interrupted-read tests, qpdf-absent guard tests, raw-order tests,
and non-regular output smoke tests. Confirm the outdated handler-reset scenario
remains inapplicable to the public API.

- [ ] **Step 3: Run full quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

- [ ] **Step 4: Measure direct-parent patch coverage**

For every changed branch, generate a fresh LCOV report and run
`scripts/patch-coverage.sh` against its immediate parent. Expected: zero
uncovered changed executable lines on each PR.

- [ ] **Step 5: Publish and verify**

Run `bd dolt push`, submit the stack to `origin`, verify local/remote OIDs,
adjacent PR bases, mergeability, and all GitHub checks. Do not post review
replies or resolve threads.
