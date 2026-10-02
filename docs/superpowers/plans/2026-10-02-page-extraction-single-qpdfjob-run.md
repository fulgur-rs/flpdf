# Page Extraction Through One `QPDFJob::run()` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task. The user selected Native inline execution.

**Goal:** Route top-level `--pages`, `rewrite --pages`, and `--empty --pages` through one configured `QPDFJob::run()` per invocation.

**Architecture:** Keep argv parsing and usage preflight in `flpdf-cli`, share the page-operation setters, and let one `QPDFJob` own source opening, page selection, transformations, output, warning completion, and status. Move the linearized content-normalization pass into the Job writer lifecycle so page extraction does not hold an intermediate `Pdf` between create and write.

**Tech Stack:** Rust workspace (`flpdf`, `flpdf-cli`), pinned qpdf 11.9.0, qpdf-zlib-compat for deterministic byte comparisons.

**Spec:** `docs/superpowers/specs/2026-10-02-page-extraction-single-qpdfjob-run-design.md`

## Global Constraints

- qpdf 11.9.0 source and live behavior are authoritative.
- Each extraction invocation configures one `QPDFJob` and calls `run()` exactly once.
- Preserve usage preflight order, status, stdout, stderr, warning order, page-source identity, and writer behavior.
- Keep the no-`--pages` rotation/split behavior and the option parser outside this slice.
- Keep secondary page and overlay sources alive through the same Job's write stage.
- Do not keep a CLI-owned `Pdf` between separate create, transform, and write Jobs.
- Before code, create or reuse and claim a bounded child under `flpdf-3yn9.48.180`; keep the parent open until integrated-main evidence satisfies its remaining acceptance criteria.

## Review Focus

- Same primary spelling, `.` shorthand, repeated paths, and per-spec passwords must keep qpdf's raw filename/source identity behavior.
- `--empty --pages` with repeated sources and collation must preserve source order, password handling, and empty-primary diagnostics.
- Repeated pages and cross-document selection must preserve annotation, AcroForm, page-label, outline/destination, structure-tree, and thread-reference behavior observed from qpdf 11.9.0.
- Overlay/underlay, rotation, image and annotation transforms must run after page selection in qpdf order; provider-backed sources must remain live through output.
- Split, encryption preservation/copy/decrypt, linearized normalization, output-open failures, warning exits, and `--no-warn` must keep qpdf's output and diagnostic ordering.

---

### Task 1: Move linearized page-content normalization into the Job writer lifecycle

**Files:**
- Create: `crates/flpdf/src/job/content_normalization.rs`
- Modify: `crates/flpdf/src/job/mod.rs`
- Modify: `crates/flpdf/src/job/lifecycle.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Test: `crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs`
- Test: `crates/flpdf-cli/tests/cli_tests.rs`
- Test: `crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs`

**Interfaces:**
- `ContentNormalizationWarning { parsed_offset: Option<u64>, last_token_was_bad: bool }` remains private to the Job implementation.
- `normalize_page_contents<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<ContentNormalizationWarning>>` uses the existing `crate::content_normalizer::normalize_content_stream` implementation.
- `configure_rewrite_job` copies explicit `WriterOptions::content_normalization` (`content_normalization_set == true`) into the Job's `normalize_content` state, including explicit `n`.
- `QPDFJob::write_qpdf` owns the pre-write pass when output is configured, `linearize` is true, and `normalize_content == Some(true)`.
- The Job records a warning exit and emits the current warning lines through its logger unless warning delivery is suppressed.

- [ ] **Step 1: Add a failing route contract and page-selection normalization test**

Add `linearized_content_normalization_is_job_owned` to `cli_pages_verbose_diagnostics.rs`. It fails while `main.rs` directly owns `normalize_page_contents` and passes only when the Job writer boundary owns the stage.

Add `pages_linearize_normalize_content_matches_qpdf` to `page_ops_qpdf_matrix.rs`. Use a content stream whose CRLF normalization changes `/Length`; compare qpdf 11.9.0 and flpdf page content, status, linearization validity, and deterministic bytes where applicable.

- [ ] **Step 2: Run the new route contract and observe RED**

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics linearized_content_normalization_is_job_owned`

Expected: FAIL because the normalization pass still runs in the CLI after `create_qpdf`.

- [ ] **Step 3: Move normalization and warning delivery into `QPDFJob::write_qpdf`**

Move the warning type, page walk, stream mutation, and warning formatting from `main.rs` to `job/content_normalization.rs`. Reuse the existing content normalizer and preserve page order, indirect `/Contents` handling, alias de-duplication, parsed offsets, warning order, and `--no-warn` behavior. In `configure_rewrite_job`, copy explicit normalization selection to the Job as well as the writer configuration. Call the private Job stage before writer setup. Remove the three CLI calls to `normalize_page_contents` so no caller normalizes a prepared `Pdf` between Job stages.

- [ ] **Step 4: Run the normalization behavior tests**

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics linearized_content_normalization_is_job_owned`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test cli_tests normalize_content`

Expected: existing top-level and rewrite normalization, warning-exit, and pass-one tests pass with unchanged stderr.

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix pages_linearize_normalize_content_matches_qpdf`

Expected: PASS against qpdf 11.9.0.

- [ ] **Step 5: Commit Task 1**

```bash
git add crates/flpdf/src/job/content_normalization.rs crates/flpdf/src/job/mod.rs crates/flpdf/src/job/lifecycle.rs crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs crates/flpdf-cli/tests/cli_tests.rs crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs
git commit -m "refactor(job): own linearized content normalization"
```

### Task 2: Share page-operation configuration and route top-level `--pages` through `run()`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Test: `crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs`
- Test: `crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs`

**Interfaces:**
- `configure_page_selection_job(job: &mut QPDFJob, page_ops: &PageOpArgs, remove_unref: CliRemoveUnreferencedResources) -> CliResult<()>` parses page specs and applies per-spec passwords, rotations, collation, split pages, keep-open policy, and resource policy.
- `run_page_extraction_job(job: QPDFJob, page_ops: &PageOpArgs, remove_unref: CliRemoveUnreferencedResources) -> CliResult<()>` calls the configuration helper and maps the single `job.run()` result through `finish_job_exit_status`.
- Input/empty-input, JSON update, overlays, other transforms, encryption, writer options, and output remain configured on that same `QPDFJob` by the existing helpers.

- [ ] **Step 1: Add a failing top-level route contract**

Add `top_level_page_extraction_uses_one_job_run` to `cli_pages_verbose_diagnostics.rs`. Scope the assertion to the top-level `--pages` dispatch and reject a caller-owned `create_qpdf()` / `write_qpdf()` pair. The no-`--pages` rotate/split branch remains on its existing path.

- [ ] **Step 2: Run the route test and observe RED**

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics top_level_page_extraction_uses_one_job_run`

Expected: FAIL because the current route directly calls `create_qpdf()` and `write_qpdf()`.

- [ ] **Step 3: Extract the shared page-selection configuration helper**

Move the existing page-spec, per-source password, rotation, collate, split, keep-open, and resource-policy setters into `configure_page_selection_job`. Keep same-file checks, `.`/empty validation, page-range parsing, JSON input/update ordering, and copy-encryption password fallback at the CLI boundary. `run_page_extraction_job` configures and runs that same Job once.

- [ ] **Step 4: Finish the top-level page-operation route through `job.run()`**

Dispatch top-level `--pages` to `run_page_extraction_job`. Keep the existing no-`--pages` rotate/split options and output behavior unchanged.

- [ ] **Step 5: Run the route and qpdf behavior tests**

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics top_level_page_extraction_uses_one_job_run`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix pages_single_range_matches_qpdf_count`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix pages_cross_document_merge_matches_qpdf`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix pages_then_split_pages_combined_matches_qpdf`

Expected: PASS.

- [ ] **Step 6: Commit Task 2**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs
git commit -m "refactor(cli): run top-level page operations in one job"
```

### Task 3: Route rewrite single-source, multi-source, and empty extraction through the shared Job

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf/src/job/lifecycle.rs` only if a qpdf differential test identifies a missing create-stage operation
- Test: `crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs`
- Test: `crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs`
- Test: `crates/flpdf-cli/tests/cli_tests.rs`

**Interfaces:**
- `configure_rewrite_job(input: Option<&Path>, ...) -> CliResult<QPDFJob>` uses `Some(path)` for file input and `None` for an empty primary; the caller sets `empty_input()` before JSON update and page specs. Existing file-backed callers pass `Some(path)`.
- `run_page_extraction_job(job: QPDFJob, page_ops: &PageOpArgs, remove_unref: CliRemoveUnreferencedResources) -> CliResult<()>` is the shared extraction completion boundary for both CLI surfaces.
- `run_page_extraction` and `run_empty_page_extraction` retain their usage preflights and then invoke the shared Job runner once.

- [ ] **Step 1: Add a failing rewrite route contract and cross-document oracle case**

Update `rewrite_pages_ordinary_output_uses_the_canonical_job_writer_route` to require one `run_page_extraction_job`/`job.run()` boundary and reject CLI-owned `create_qpdf()`, `write_qpdf()`, `split_job`, and `write_job` routes.

Add `rewrite_pages_cross_document_merge_matches_qpdf` to `page_ops_qpdf_matrix.rs`, using distinct primary and secondary files. Compare page order, exit status, primary catalog state, and deterministic qpdf-zlib-compat output.

- [ ] **Step 2: Run the route contract and observe RED**

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics rewrite_pages_ordinary_output_uses_the_canonical_job_writer_route`

Expected: FAIL because rewrite page extraction still prepares pages separately from its transform and writer Jobs.

- [ ] **Step 3: Configure rewrite extraction on one Job**

Generalize `configure_rewrite_job` to accept an optional primary path. `None` sets the `empty PDF` input label and leaves input creation to `empty_input()`; the caller sets `empty_input()` before JSON update and page specs. Configure the primary password and copy-encryption donor/password on the same Job before adding page specs, so qpdf's per-source password fallback sees the donor settings. Configure all raw page specs, transformations, overlays, split behavior, and writer settings on that Job.

- [ ] **Step 4: Remove the CLI multi-Job extraction pipeline**

Use `QPDFJob::create_qpdf` page-spec handling as the only page-selection implementation. Remove the source-count split and the CLI `run_page_extraction_after_plan` second `get_all_pages()` / page-tree rebuild / completion pass. Keep the existing single-source completion inside `QPDFJob::prepare_document`; rely on multi-source `handle_page_specs` for multi-source page mutation. The qpdf differential matrix is the acceptance gate for page labels, annotations, outlines/destinations, structure references, and resource pruning; any missing behavior must be implemented at the matching Job create-stage source order, not by restoring a CLI rebuild.

- [ ] **Step 5: Run rewrite page-selection and diagnostic tests**

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics rewrite_empty_pages_repeated_collated_sources_match_qpdf`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics rewrite_pages_split_keeps_write_time_password_notice_after_split_preflight`

Expected: PASS with qpdf warning/status ordering.

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix rewrite_pages_cross_document_merge_matches_qpdf`

Expected: PASS against qpdf 11.9.0.

Run: `cargo test -p flpdf-cli --features qpdf-zlib-compat --test page_ops_qpdf_matrix`

Expected: the complete page-operation matrix passes under qpdf-compatible compression.

Run: `cargo test -p flpdf-cli --test cli_tests rewrite_subcommand_supports_pages`

Expected: PASS.

Run: `cargo test -p flpdf-cli --test cli_tests pages_extraction`

Expected: PASS for the existing outline/destination and resource-pruning page-extraction cases.

Run: `cargo test -p flpdf-cli --test cli_pages_verbose_diagnostics`

Expected: all page-source, empty-primary, warning-order, output-open, and non-UTF8 path diagnostics pass.

Run: `cargo test -p flpdf-cli --features qpdf-zlib-compat --test page_ops_qpdf_matrix`

Expected: the full page-operation differential matrix passes against qpdf 11.9.0.

- [ ] **Step 6: Refresh E-19/E-21 route evidence and commit Task 3**

Update `docs/qpdf-route-matrix/e-job-cli-capi.md` from the resulting source and measured call sites. Keep E-19/E-21 mixed if other CLI consumers remain.

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf/src/job/lifecycle.rs crates/flpdf-cli/tests/cli_pages_verbose_diagnostics.rs crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs crates/flpdf-cli/tests/cli_tests.rs docs/qpdf-route-matrix/e-job-cli-capi.md
git commit -m "refactor(cli): run rewrite page extraction in one job"
```

## Final Verification

Run after the last task:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/check-qpdf-route-matrix.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: all commands exit 0; each page-extraction route contract shows one `QPDFJob::run()` per invocation, and the CLI contains no split create/transform/write path for page extraction. Fresh PR checks must pass at the exact pushed head before marking the PR Ready.
