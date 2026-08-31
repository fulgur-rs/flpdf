# qpdf Comma-Separated Collate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`[ ]` syntax for tracking.

**Goal:** Port qpdf 11.9.0's `--collate` vector syntax through the shared flpdf job boundary so JSON and CLI page specifications accept comma-separated and repeated values, preserve qpdf's per-spec order/cardinality rules, and allow qpdf's valid zero-page result.

**Architecture:** `QPDFJob` owns one parser for a qpdf `Config::collate` parameter and stores the result as `Option<Vec<usize>>`; the JSON initializer and CLI both call that parser. `QPDFJob::handle_page_specs` receives a borrowed view of that vector, validates qpdf's vector cardinality, and applies the per-spec round-robin order in both the in-place single-source and merged multi-source paths. The page-tree rebuild primitive will accept an empty selection because qpdf `--collate=0` can intentionally produce `/Count 0` and `/Kids [ ]`.

**Tech Stack:** Rust workspace (`flpdf`, `flpdf-cli`), qpdf 11.9.0 pinned source and `/usr/bin/qpdf` oracle, `assert_cmd`, existing PDF/page helpers, flpdf-qtest.

**Spec:** `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`

## Global Constraints

- qpdf 11.9.0 source and live qpdf 11.9.0 probes are the semantic and observable-output oracle.
- `QPDFJob::Config::collate` at `libqpdf/QPDFJob_config.cc:95-125` is the parser model; `QUtil::string_to_ull` at `libqpdf/QUtil.cc:396-425` supplies unsigned decimal-prefix and error behavior.
- `QPDFJob.cc:2474-2506` is the page-order/cardinality model: zero is valid, one value is padded to each specification, and a vector with more than one value must have one value per specification.
- JSON and CLI must call the same production parser; no CLI-only comma parser or compatibility adapter is allowed.
- `--collate` without `--pages` remains a no-op for output, but its parameter is still parsed and invalid parameters still fail as qpdf does.
- Do not edit qpdf or flpdf-qtest source/fixtures in this worktree; qtest is run against the release binary after the library and CLI changes are complete.
- Keep all implementation commits on `flpdf-egzr-8-11-collate`, whose parent is `origin/flpdf-hxmj-canonical-pages`; do not commit implementation changes on `main` or the existing hxmj worktree.
- Every changed executable line under `crates/flpdf/src` must be covered by fresh patch coverage at 100% before completion claims.

---

### Task 1: Add the qpdf-shaped collate parameter parser

**Files:**
- Modify: `crates/flpdf/src/job/lifecycle.rs:JobConfiguration`, parser helpers near `parse_positive_usize`, JSON initialization near the `collate` handler, and lifecycle unit tests.
- Modify: `crates/flpdf/src/job/mod.rs` only if a public job export is needed by the CLI.
- Test: `crates/flpdf/src/job/lifecycle.rs` unit tests.

**Interfaces:**
- Produces `QPDFJob::parse_collate(value: &str) -> flpdf::Result<Vec<usize>>` as the single parser used by JSON and CLI.
- Produces `JobConfiguration.collate: Option<Vec<usize>>`, where `None` means the option was absent and an explicit empty parameter becomes `Some(vec![1])`.
- Numeric conversion returns qpdf-style `Error::System` for unsigned underflow/overflow and narrowing errors, and `Error::Usage` for qpdf's trailing-comma usage error.

- [ ] **Step 1: Write the failing parser tests.** Replace the old scalar-only assertions with tests that name and assert each qpdf behavior:

```
assert_eq!(QPDFJob::parse_collate("").unwrap(), vec![1]);
assert_eq!(QPDFJob::parse_collate("2,3").unwrap(), vec![2, 3]);
assert_eq!(QPDFJob::parse_collate("0").unwrap(), vec![0]);
assert_eq!(QPDFJob::parse_collate("2abc").unwrap(), vec![2]);
assert_eq!(QPDFJob::parse_collate("abc").unwrap(), vec![0]);
assert_eq!(QPDFJob::parse_collate(" +2").unwrap(), vec![2]);
assert!(QPDFJob::parse_collate(",2").is_err());
assert!(QPDFJob::parse_collate("2,").is_err());
assert!(QPDFJob::parse_collate("-1").is_err());
assert!(QPDFJob::parse_collate("18446744073709551616").is_err());
```

- [ ] **Step 2: Run the parser test to verify the expected RED result.**

Run: `cargo test -p flpdf --lib job::lifecycle::tests::job_json_private_parsers_cover_encryption_and_writer_choices -- --exact`

Expected: FAIL because `QPDFJob::parse_collate` does not exist and the current `parse_positive_usize` rejects zero and comma values.

- [ ] **Step 3: Implement the minimal qpdf parser.** Add the associated public job entry point and a private byte-oriented helper. The helper must:

1. Return `[1]` for an empty parameter.
2. Locate commas by byte offset and reproduce the literal qpdf `parameter.substr(pos, end)` count argument, so `2,,3` yields the same vector `[2, 0, 3]` rather than inventing a new middle-empty error.
3. Reproduce `QUtil::string_to_ull`: skip only qpdf's six ASCII whitespace bytes, reject a leading `-` after whitespace, accept `+`, parse a base-10 digit prefix, return zero when no digit is present, stop at non-digit/NUL, and report u64 overflow with qpdf's message.
4. Apply qpdf's `QIntC::to_uint` narrowing limit (`u32::MAX`) before converting to `usize`.

Store `QPDFJob::parse_collate(&String::from_utf8_lossy(&value))?` in JSON initialization, replacing `parse_positive_usize` and retaining the existing absent-versus-empty JSON distinction.

- [ ] **Step 4: Run the parser test to verify GREEN and preserve the existing lifecycle suite.**

Run: `cargo test -p flpdf --lib job::lifecycle::tests::job_json_private_parsers_cover_encryption_and_writer_choices -- --exact`

Expected: PASS, with the parser assertions and the existing full-handler assertions passing.

- [ ] **Step 5: Commit the parser slice.**

```
git add crates/flpdf/src/job/lifecycle.rs
git commit -m "feat: parse qpdf collate value lists in QPDFJob"
```

### Task 2: Permit the qpdf-valid empty page selection

**Files:**
- Modify: `crates/flpdf/src/pages/tree_rebuild.rs:379-425,876-880`.
- Test: `crates/flpdf/src/pages/tree_rebuild.rs` unit tests.

**Interfaces:**
- `rebuild_page_tree` and `rebuild_page_tree_with_max_depth` accept `selected.is_empty()` and produce a rebuilt root with `/Kids [ ]`, `/Count 0`, an empty `RebuildResult::new_kids`, and all original page refs in `removed_pages`.
- Existing missing-root, malformed-root, selected-page, duplicate-page, and inheritance behavior remains unchanged.

- [ ] **Step 1: Change the existing empty-selection test into a qpdf-shaped failing expectation.** Rename it to `empty_selection_rebuilds_an_empty_page_tree`, assert `new_kids.is_empty()`, `removed_pages` contains the original leaves, and resolve the rebuilt `/Pages` root to assert `/Count` is zero and `/Kids` is an empty array.

- [ ] **Step 2: Run the test to verify the expected RED result.**

Run: `cargo test -p flpdf --lib pages::tree_rebuild::tests::empty_selection_rebuilds_an_empty_page_tree -- --exact`

Expected: FAIL because the current primitive returns `Error::Missing("page-tree rebuild: empty selection")`.

- [ ] **Step 3: Remove only the obsolete empty-selection rejection and update the public error documentation.** Leave the existing root/page-tree preparation and root rewrite path intact so the empty vector follows exactly the same qpdf-shaped rebuild lifecycle as non-empty selections.

- [ ] **Step 4: Run the empty-selection test and all tree-rebuild tests.**

Run: `cargo test -p flpdf --lib pages::tree_rebuild::tests::empty_selection_rebuilds_an_empty_page_tree -- --exact`

Run: `cargo test -p flpdf --lib pages::tree_rebuild --quiet`

Expected: both commands pass with zero failures.

- [ ] **Step 5: Commit the page-tree primitive.**

```
git add crates/flpdf/src/pages/tree_rebuild.rs
git commit -m "feat: allow empty qpdf page-tree selections"
```

### Task 3: Apply vector cardinality and per-spec order in `handle_page_specs`

**Files:**
- Modify: `crates/flpdf/src/job/page_specs.rs:68-125,159-201,486-575,767-800`.
- Test: `crates/flpdf/src/job/page_specs.rs` unit tests.

**Interfaces:**
- Change the page-job collate argument from `Option<usize>` to `Option<&[usize]>` at the single-source helper, merged helper, and public `QPDFJob::handle_page_specs` boundary.
- Add one private helper that validates `n_collate == 0 || n_collate == 1 || n_collate == n_specs`, pads a single value across multiple specs, and returns the per-spec values without rejecting zero.
- Single-spec jobs do not enter qpdf's collate loop (`n_specs > 1` is required); `Some(&[0])` therefore selects the page normally for one specification.
- Multi-spec jobs with zero values can return no pages; no old `selected.is_empty()` job error may reject that qpdf result.

- [ ] **Step 1: Write failing page-job tests.** Add tests for `select_single_source_pages` and `handle_page_specs` covering:

```
// Two specifications, explicit per-spec groups: [1, 2] then [1, 3]...
// A single value is padded to every specification.
assert_eq!(selected_indices(&mut source, &two_specs, Some(&[2, 1])), vec![1, 2, 1, 3, 2, 3]);
assert_eq!(selected_indices(&mut source, &two_specs, Some(&[2])), vec![1, 2, 1, 2, 3, 3]);

// Zero is valid and can produce an empty page result.
assert!(selected_indices(&mut source, &two_specs, Some(&[0])).is_empty());
assert_eq!(selected_indices(&mut source, &two_specs, Some(&[0, 1])), vec![1, 2, 3]);

// A vector with more than one value must have one value per specification.
assert!(select_single_source_pages(&mut source, &two_specs, Some(&[1, 2, 3])).is_err());
```

The merged route test must assert that a two-source `[2, 3]` vector produces the same page order and count as qpdf, and that `[0, 1]` produces only the second specification's pages.

- [ ] **Step 2: Run the new page-job tests to verify RED.**

Run: `cargo test -p flpdf --lib job::page_specs::tests -- --nocapture`

Expected: FAIL to compile until call sites use the vector signature, and then fail on the scalar implementation's zero rejection/order/cardinality behavior. This is the required failure proving the tests exercise the missing feature.

- [ ] **Step 3: Implement qpdf's vector logic.** Remove scalar zero guards, call the shared cardinality helper after plans are built, use `values[spec_index]` for each round, and run the round-robin only when there is more than one specification. Remove the merged helper's unconditional empty-selection error. Keep source grouping, label capture, AcroForm replay, warning attribution, and `PageSpecJobOutput` unchanged.

- [ ] **Step 4: Update existing scalar tests and lifecycle callers to pass borrowed slices.** Use `Some(&[1])` for the previous scalar behavior and `configuration.collate.as_deref()` in the JSON job path. Ordinary `None` calls remain unchanged.

- [ ] **Step 5: Run page-job tests to verify GREEN.**

Run: `cargo test -p flpdf --lib job::page_specs::tests -- --nocapture`

Run: `cargo test -p flpdf --lib job::lifecycle::tests --quiet`

Expected: PASS with zero failures.

- [ ] **Step 6: Commit the canonical page-job vector route.**

```
git add crates/flpdf/src/job/page_specs.rs crates/flpdf/src/job/lifecycle.rs
git commit -m "feat: apply qpdf collate groups per page specification"
```

### Task 4: Route CLI and JSON through the shared parser

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:PageOpArgs`, page-operation parsing, top-level dispatch, `run_command`, and both page extraction callers.
- Modify: `crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs`.
- Modify: `crates/flpdf-cli/tests/cli_job_json.rs`.
- Test: the two CLI integration tests above.

**Interfaces:**
- `PageOpArgs.collate` becomes an appendable `Vec<String>` so repeated `--collate` occurrences preserve qpdf's repeated `Config::collate(parameter)` appends.
- Add `parse_collate_values(&[String]) -> CliResult<Option<Vec<usize>>>`, which concatenates `QPDFJob::parse_collate` results in argument order.
- Validate top-level collate values before dispatch, and validate `rewrite` subcommand values before its page-op/no-page-op branch, so malformed collate values fail even when collate has no output effect.
- Pass `collate.as_deref()` into `QPDFJob::handle_page_specs`; no CLI page planner or standalone `flpdf::collate` call is introduced.

- [ ] **Step 1: Add failing qpdf differential CLI tests.** In `page_ops_qpdf_matrix.rs`, add tests that run the same command through qpdf and flpdf and compare page order using the existing `media_boxes_of` helper:

```
--pages . 1-3 secondary.pdf 1-4 -- --collate=2,3 output.pdf
```

Also add a zero-value test for `--collate=0` that asserts both commands succeed and both outputs have zero pages. Add a repeated-value test for `--collate=2 --collate=3` and compare it to the equivalent comma list. In `cli_job_json.rs`, add a `--job-json-file` test with two page specifications and `"collate":"2,1"` plus a zero/one case, comparing qpdf and flpdf page counts and order.

- [ ] **Step 2: Run the new integration tests to verify RED.**

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix collate_comma -- --nocapture`

Run: `cargo test -p flpdf-cli --test cli_job_json job_json_file_collate -- --nocapture`

Expected: FAIL because the current CLI rejects commas and zero, the JSON path parses a scalar positive integer, and repeated `--collate` is not appendable.

- [ ] **Step 3: Implement shared CLI plumbing.** Change clap metadata/help to `N[,M,...]`, use append action, call `parse_collate_values`, and pass the resulting vector to both page extraction functions. Parse once at each dispatch boundary for validation of no-page operations; return the underlying `Error::Usage`/`Error::System` so main's existing qpdf-shaped error renderer handles the result.

- [ ] **Step 4: Run focused CLI tests to verify GREEN.**

Run: `cargo test -p flpdf-cli --test page_ops_qpdf_matrix --quiet`

Run: `cargo test -p flpdf-cli --test cli_job_json --quiet`

Expected: PASS with qpdf/flpdf page order, zero-page output, JSON behavior, and existing page-operation cells all green.

- [ ] **Step 5: Commit the CLI/JSON integration.**

```
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs crates/flpdf-cli/tests/cli_job_json.rs
git commit -m "feat: expose qpdf collate groups in CLI and job JSON"
```

### Task 5: Remove stale scalar collate bridge and update correspondence

**Files:**
- Delete: `crates/flpdf/src/job/page_collate.rs` after its only production consumer is confirmed absent.
- Modify: `crates/flpdf/src/job/mod.rs` and `crates/flpdf/src/lib.rs` to remove the standalone `collate` module/export.
- Modify: `crates/flpdf/src/job/page_combine.rs` to remove the now-unreachable `CombinedPlan::build_repeated` method and its tests/doc text.
- Modify: `docs/qpdf-correspondence.md` Job/CLI rows to describe `QPDFJob::parse_collate`, `Option<Vec<usize>>`, and `QPDFJob::handle_page_specs` as the only collate route; remove stale `CombinedPlan::build_repeated` and `job/page_collate.rs (--collate[=N])` claims.

**Interfaces:**
- The canonical public page-job API is `QPDFJob::handle_page_specs`; no public free function claims ownership of qpdf `--collate`.
- `CombinedPlan` remains only for its separate generic page-plan/merge API; it no longer exposes a repeated-selection or collate route that can diverge from qpdf's per-spec semantics.

- [ ] **Step 1: Verify callers before deletion.**

Run: `rg -n '\bcollate\\(|page_collate|build_repeated' crates/flpdf/src crates/flpdf-cli/src crates/flpdf/tests crates/flpdf-cli/tests`

Expected: only the standalone module/tests, stale method/tests, and correspondence references remain; no production CLI/library route calls them.

- [ ] **Step 2: Delete the unreachable bridge and update exports/docs.** Preserve `CombinedPlan::build`/`from_specs` where their distinct generic responsibilities remain used, and do not add a replacement free collate function.

- [ ] **Step 3: Run the route-lock and documentation/deviation checks.**

Run: `cargo test -p flpdf --test page_job_route_cutover_tests --quiet`

Run: `python3 scripts/check-qpdf-deviation-markers.py --check`

Run: `python3 scripts/qpdf-module-docs.py --check`

Expected: PASS with no new deviation marker and no stale collate route.

- [ ] **Step 4: Commit the cleanup.**

```
git add crates/flpdf/src/job/mod.rs crates/flpdf/src/lib.rs crates/flpdf/src/job/page_combine.rs docs/qpdf-correspondence.md
git rm crates/flpdf/src/job/page_collate.rs
git commit -m "refactor: remove the obsolete standalone collate route"
```

### Task 6: Full verification, qtest, stack publication, and Beads handoff

**Files:**
- Modify: Beads notes for `flpdf-egzr.8.11` only after the implementation evidence exists.
- No qtest source/fixture changes; use the existing `/home/ubuntu/flpdf-qtest` harness and release binary.

- [ ] **Step 1: Run the local quality gates from the feature worktree.**

```
cargo fmt --all -- --check
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf job::check::tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test page_ops_qpdf_matrix
cargo test -p flpdf-cli --test cli_job_json
cargo test --workspace
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 2: Run qpdf compatibility and same-run qtest evidence.** Build the release binary, run the literal collate target in `/home/ubuntu/flpdf-qtest`, preserve `harness.log` and `qtest-results.xml`, and report target testcase outcomes separately from any pre-existing global manifest failures. The acceptance record must contain zero allowlist regressions and zero manifest validation errors for the same run.

- [ ] **Step 3: Run fresh changed-line coverage and inspect the diff.** Use the repository's full workspace coverage command with `--features qpdf-zlib-compat` and `scripts/patch-coverage.sh --base origin/flpdf-hxmj-canonical-pages --lcov`; inspect `git diff --check`, `git status --short`, and all untracked files before committing or publishing.

- [ ] **Step 4: Rebase only if the parent PR moved, then push the feature branch.** Confirm PR #1386's live head/base first. If its base remains `main` and the feature branch is based on the current remote hxmj head, push with `git push -u origin flpdf-egzr-8-11-collate`. Do not touch the dirty/local hxmj worktree.

- [ ] **Step 5: Create or update the stacked Draft PR and read it back.** Use non-interactive `gh stack link`/`gh stack view --json` or an explicit `gh pr create --draft --base flpdf-hxmj-canonical-pages --head flpdf-egzr-8-11-collate`, then verify the exact base/head, body, checks, and stack dependency. Do not claim Ready until required CI and review gates are green.

- [ ] **Step 6: Append implementation evidence, close the completed Beads issue, and push Dolt.** Read the issue before mutation; append the source citations, commit/PR, focused/full tests, qtest target results, patch coverage, and any known unrelated global failures. Then run `bd close flpdf-egzr.8.11 --reason="qpdf comma-separated collate implemented and verified"`, read it back, run `bd dep cycles`, and finish with `bd dolt push` reporting `Push complete.`.
