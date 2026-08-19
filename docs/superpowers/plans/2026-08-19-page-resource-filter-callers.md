# Page and Resource Filter Callers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the three production page/resource filter callers assigned to `flpdf-egzr.3.2.6` onto the canonical `ObjectHandle` filter route without changing pruning, coalescing, or write-back behavior.

**Architecture:** Keep `resources.rs` and `pages.rs` responsible for traversal and mutation, but obtain each stream's canonical `ObjectHandle`, resolve its live stream dictionary, and read raw bytes through the handle before calling `decode_stream_data_from_handle`. The existing `Object`/`Stream` snapshots remain only at the surrounding compatibility/write-back boundary; no new materialization bridge is introduced.

**Tech Stack:** Rust workspace, `ObjectHandle`, qpdf 11.9.0 filter pipeline, focused unit tests, qpdf-zlib-compat differential fixtures, `cargo llvm-cov` patch coverage.

**Spec:** `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`; Beads `flpdf-egzr.3.2.6.1` under `flpdf-egzr.3.2.6`.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative; cite `QPDFPageObjectHelper.cc:313-345,474-476,539-649`, `QPDFObjectHandle.cc:1289-1292,1300-1341,1550-1572,1710-1737`, and `QPDF_Stream.cc:379-569` in the touched route.
- Preserve the existing qpdf-derived failure timing: decode failure remains an error for coalescing and an incomplete/conservative result for resource pruning.
- Do not add an `Object`/`ObjectHandle` adapter, clone-based resolution, or a second filter implementation.
- Do not change legacy mutation/write-back or unrelated page walkers in this slice.
- Every changed executable line must be covered by tests; do not add a coverage ignore for behavior.

### Task 1: Pin the canonical filter-caller contract with RED tests

**Files:**
- Modify: `crates/flpdf/src/pages.rs:826-1380` (existing page tests)
- Modify: `crates/flpdf/src/resources.rs:1936-end` (existing resource tests)

**Interfaces:**
- Consumes: the existing `Pdf`, page/content fixtures, indirect stream dictionary handles, and `DecodeLimits::default()`.
- Produces: regression tests that require indirect `/Filter` and `/DecodeParms` to be resolved through the owning document handle, while preserving the existing coalescing and resource-pruning results.

- [x] **Step 1: Write the failing coalescing test**

Add a page fixture whose `/Contents` array contains two indirect stream objects. Put `/Filter 7 0 R` and `/DecodeParms 8 0 R` in the first stream, where object 7 is `/FlateDecode` and object 8 carries the predictor parameters. Call `coalesce_page_contents` and assert the replacement stream contains the decoded payload. The old `&Dictionary` route must fail before the implementation change because it treats the indirect filter holder as a non-name filter.

- [x] **Step 2: Run the focused test and verify the expected RED failure**

Run:

```bash
cargo test -p flpdf --lib pages::tests::coalesce_page_contents_resolves_indirect_filter_and_decode_parms -- --exact
```

Expected: FAIL in the filter decoder with the legacy indirect `/Filter` shape, not a compile error or fixture-construction error.

- [x] **Step 3: Write the failing resource-pruning test**

Add a Form XObject fixture with an indirect `/Filter` and `/DecodeParms`, plus used and unused `/Font` entries. Call `remove_unreferenced_resources_on_page` and assert the used font remains and the unused font is pruned. The old `&Dictionary` route must fail on the indirect filter holder before the implementation change.

- [x] **Step 4: Run the focused resource test and verify the expected RED failure**

Run:

```bash
cargo test -p flpdf --lib resources::tests::remove_unreferenced_resources_resolves_indirect_form_filter -- --exact
```

Expected: FAIL at the legacy filter-shape decoder, with the resource graph otherwise valid.

### Task 2: Migrate the three production callers

**Files:**
- Modify: `crates/flpdf/src/pages.rs:437-567`
- Modify: `crates/flpdf/src/resources.rs:239-303`
- Modify: `crates/flpdf/src/resources.rs:1496-1707`

**Interfaces:**
- Consumes: `Pdf::get_object_handle`, `Pdf::resolve_object_handle_to_terminal_ref`, `ObjectHandle::as_stream_dict`, `ObjectHandle::get_raw_stream_data`, and `filters::decode_stream_data_from_handle`.
- Produces: the same decoded bytes, conservative resource-pruning decisions, error text, and legacy write-back as before, with filter metadata resolved by the canonical handle.

- [x] **Step 1: Migrate the Form-XObject pre-pass caller**

For `remove_unreferenced_resources_in_form_xobjects`, retain the existing legacy `Stream` only for the surrounding resource mutation. In the decode branch, obtain and resolve `pdf.get_object_handle(form_ref)`, obtain its `as_stream_dict`, read `get_raw_stream_data`, and call `decode_stream_data_from_handle` with `DecodeLimits::default()`. Preserve the existing `Err` branch, child discovery, and subsequent `pdf.set_object` behavior.

- [x] **Step 2: Migrate the recursive Form caller**

For `recurse_form_xobject`, use the canonical handle for the same `xobj_ref` and pass its live stream dictionary plus raw bytes to `decode_stream_data_from_handle`. Keep `parse_resource_content`, unresolved-name handling, child discovery, and resource pruning unchanged. The handle must be resolved before `as_stream_dict` and raw data are read so indirect dictionary entries follow qpdf's accessor contract.

- [x] **Step 3: Migrate page coalescing**

In `coalesce_page_contents`, resolve the page's `/Contents` array as canonical handles, follow each holder chain with `resolve_object_handle_to_terminal_ref`, validate each terminal as a stream, and call `get_raw_stream_data` plus `decode_stream_data_from_handle`. Preserve the first stream's non-filter dictionary keys, newline placement, newly allocated stream number, and legacy page write-back exactly.

- [x] **Step 4: Run the RED tests and focused page/resource suites as GREEN**

Run:

```bash
cargo test -p flpdf --lib pages::tests::coalesce_page_contents_resolves_indirect_filter_and_decode_parms -- --exact
cargo test -p flpdf --lib resources::tests::remove_unreferenced_resources_resolves_indirect_form_filter -- --exact
cargo test -p flpdf --lib pages::tests --quiet
cargo test -p flpdf --lib resources::tests --quiet
```

Expected: both new tests and all existing page/resource tests pass with no new warnings or golden changes.

### Task 3: Verify parity and prepare the PR

**Files:**
- Modify: `docs/qpdf-correspondence.md` only if the current row needs the exact caller ownership citation; otherwise leave it unchanged.
- Test: existing page/resource differential and compatibility suites.

**Interfaces:**
- Consumes: the green implementation from Task 2 and the parent branch `origin/main`.
- Produces: a reviewed, rebased, 100%-patch-covered PR for `flpdf-egzr.3.2.6.1`, marked Ready only after GitHub CI is fully green.

- [x] **Step 1: Run local quality gates**

Run `cargo fmt --all -- --check`, the strict workspace rustdoc command from `.github/workflows/ci.yml`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the focused page/resource and qpdf-zlib-compat suites, `cargo test --workspace --quiet`, and the authoritative patch-coverage command against `origin/main`.

- [x] **Step 2: Review the diff against qpdf**

Confirm that every production filter call in the scoped functions uses the handle route, no `decode_stream_data(&legacy_dict, ...)` remains there, no new bridge is introduced, and existing qpdf differential fixtures have no byte changes.

- [x] **Step 3: Commit, push, create the stacked PR, and request review**

Commit the plan, tests, and implementation as one bounded change, push `feature/flpdf-egzr-3-2-6-page-resources`, create a PR based on `main`, and include Beads `flpdf-egzr.3.2.6.1`, qpdf citations, the RED/GREEN evidence, and exact verification commands in the body.

- [x] **Step 4: Wait for all required checks and mark Ready**

Inspect `gh pr checks <number> --required`; wait for Coverage, Codecov patch, Quality, Fuzz, Analyze, Ubuntu x86/ARM, macOS, Windows, and approval to pass. Address actionable review comments with qpdf source/probe evidence, then call `gh pr ready <number>` only after all required checks are green.

- [x] **Step 5: Close the child Beads issue only after the Ready gate**

Add the final PR URL, head SHA, check summary, patch-coverage result, and review-thread state to the child issue. Close `flpdf-egzr.3.2.6.1` and push Beads with `bd dolt push`; leave the parent `.3.2.6` open for the next page consumer slice.
