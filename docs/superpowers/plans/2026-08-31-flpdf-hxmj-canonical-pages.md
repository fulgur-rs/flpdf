# qpdf QPDFJob Canonical Single-Source Pages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every flpdf CLI `--pages` selection that uses one source document through the qpdf-shaped `QPDFJob::handle_page_specs` boundary, removing the duplicated in-place single-source orchestration.

**Architecture:** Keep `QPDFJob::handle_page_specs` as the sole page-spec owner, matching qpdf 11.9.0's unconditional `handlePageSpecs` call whenever page specifications exist (`libqpdf/QPDFJob.cc:466-470`). The single-source CLI path constructs a source vector containing the already-opened primary and one `PageSpecInput` per occurrence; the job returns an in-place primary result so qdf output retains qpdf's live object identities. Multi-source calls retain the existing fresh `Merged` result. The standalone `CombinedPlan::build_repeated` plus `job::collate` path is removed from the production CLI route; comma-separated collate values belong to the dependent `flpdf-egzr.8.11` slice and are not mixed into this PR.

**Tech Stack:** Rust workspace, `flpdf`/`flpdf-cli`, qpdf 11.9.0 oracle, `assert_cmd`, qpdf CLI differential tests.

**Spec:** `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`

## Global Constraints

- qpdf 11.9.0 pinned source and `/usr/bin/qpdf` 11.9.0 are the semantic and observable-output oracle.
- Do not add a compatibility adapter or preserve the old single-source route solely for API compatibility; pre-v1.0 qpdf responsibility outranks the current split.
- Preserve the existing supported scalar `--collate[=N]` contract in this slice; `flpdf-egzr.8.11` owns per-spec comma-list expansion after this prerequisite.
- Keep the existing job-route warning, source lifetime, encryption-source, page-label, AcroForm, and writer ordering contracts.
- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-hxmj`; never commit implementation changes on `main`.
- Every changed executable line under `crates/flpdf/src` must be covered by fresh patch coverage at 100%.

---

### Task 1: Record the plan and establish the route-lock test

**Files:**
- Create: `docs/superpowers/plans/2026-08-31-flpdf-hxmj-canonical-pages.md`
- Create: `crates/flpdf/tests/page_job_route_cutover_tests.rs`
- Read: `crates/flpdf-cli/src/main.rs`, `crates/flpdf/src/job/page_specs.rs`, `crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs`

**Interfaces:**
- Consumes: the current `run_page_extraction_from_repeated_pdf` single-source path and the existing `QPDFJob::handle_page_specs` multi-source path.
- Produces: a test-only route contract that requires the single-source production function to call `handle_page_specs` and forbids `CombinedPlan::build_repeated` and standalone `collate(&plan, ...)` in that function.

- [x] **Step 1: Write the failing route-lock test**

```rust
#[test]
fn single_source_pages_use_the_qpdf_job_page_specs_route() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let start = source
        .find("fn run_page_extraction_from_single_source(")
        .expect("single-source job route must have a named production function");
    let body = &source[start..];
    assert!(
        body.contains("handle_page_specs("),
        "single-source --pages must use QPDFJob::handle_page_specs"
    );
    assert!(
        !body.contains("CombinedPlan::build_repeated"),
        "single-source --pages must not build a duplicate CombinedPlan route"
    );
    assert!(
        !body.contains("collate(&plan"),
        "single-source --pages must not call the standalone collate bridge"
    );
}
```

- [x] **Step 2: Run the route-lock test and verify the expected RED failure**

Run:

```bash
cargo test -p flpdf --test page_job_route_cutover_tests --quiet
```

Expected: FAIL because the current source has no `run_page_extraction_from_single_source` function and the existing single-source route still calls `CombinedPlan::build_repeated` and `collate(&plan, ...)`. This confirms the test guards the intended route change rather than an unrelated behavior.

- [x] **Step 3: Commit the plan and RED test**

```bash
git add docs/superpowers/plans/2026-08-31-flpdf-hxmj-canonical-pages.md crates/flpdf/tests/page_job_route_cutover_tests.rs
git commit -m "test: require canonical qpdf page job route"
```

### Task 2: Cut the single-source CLI over to `QPDFJob::handle_page_specs`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:17-23,4586-4860`
- Test: `crates/flpdf/tests/page_job_route_cutover_tests.rs`

**Interfaces:**
- Consumes: `InputSpec` ranges, the already-opened `Pdf<R>` from the JSON/file job lifecycle, scalar `parse_collate_n`, and `QPDFJob::handle_page_specs`.
- Produces: `run_page_extraction_from_single_source`, which keeps the source alive while the job result is written and passes the page-job result into the shared post-copy boundary.

- [x] **Step 1: Rename the old route and replace its planning body**

Change `run_page_extraction_from_repeated_pdf` to `run_page_extraction_from_single_source`. Capture `primary_copy_encryption` and `primary_encrypted`, build one `PageSpecInput::new(0, input.range.clone())` per resolved input occurrence, parse the existing scalar collate option, configure a `QPDFJob` logger/message prefix, and call the sole `QPDFJob::handle_page_specs` entry point. Its single-source branch performs the qpdf-shaped in-place page-tree operation and returns the primary live document plus `RebuildResult`; its multi-source branch returns the fresh merged document. The shared post-copy writer boundary consumes either result without reinstating the old planner:

```rust
let mut sources = vec![pdf];
let mut job = QPDFJob::new();
job.set_logger(cli_logger());
job.set_message_prefix(progname());
let before_warnings = job.has_warnings();
let page_output = job.handle_page_specs(
    &mut sources,
    &specs,
    collate,
    remove_unref.into(),
    options.preserve_unreferenced_objects,
)?;
let source_warnings = before_warnings || job.has_warnings();
```

For `InPlace`, project `RebuildResult::new_kids` to the existing `CombinedPage` shape and pass the result/pruning mode into `run_page_extraction_after_plan`; for `Merged`, project the fresh output page tree and pass no prebuilt result. The shared function applies only the remaining rotate/navigation/writer stages when the job already rebuilt the primary. Keep `sources` in scope through the call so provider-backed copied streams retain their source lifetime.

Remove the `CombinedPlan` and standalone `collate` imports if no production use remains. Update both `JobPdf::File` and `JobPdf::Json` dispatch calls to the renamed function.

- [x] **Step 2: Run the route-lock test and focused scalar page-operation tests**

Run:

```bash
cargo test -p flpdf --test page_job_route_cutover_tests --quiet
cargo test -p flpdf-cli --test page_ops_qpdf_matrix --quiet
cargo test -p flpdf --lib page_collate --quiet
```

Expected: the route-lock test passes; all existing scalar page-operation/qpdf matrix tests remain green, demonstrating that the canonical cutover preserves the already-supported `--collate` behavior.

- [x] **Step 3: Run the full relevant CLI tests**

Run:

```bash
cargo test -p flpdf-cli --test cli_tests --quiet
cargo test -p flpdf-cli --test encrypted_rewrite_tests --quiet
```

Expected: zero failures, including JSON-input page selection, repeated same-file selections, warnings, encryption preservation, and split-page combinations.

- [x] **Step 4: Commit the canonical cutover**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf/tests/page_job_route_cutover_tests.rs
git commit -m "refactor: route single-source pages through qpdf job"
```

### Task 3: Run qpdf differential probes and inspect the exact route result

**Files:**
- Modify: `crates/flpdf-cli/tests/page_ops_qpdf_matrix.rs` only if a missing regression assertion is identified by the live probe
- Test: existing qpdf matrix plus the route-lock test

**Interfaces:**
- Consumes: pinned qpdf 11.9.0 and the current `flpdf` binary.
- Produces: same-input qpdf/flpdf evidence for repeated same-source page specs, page labels, AcroForm annotations, warnings, encryption source, and supported scalar collate.

- [x] **Step 1: Run the focused qpdf matrix and binary probes**

Run:

```bash
cargo test -p flpdf-cli --test page_ops_qpdf_matrix --quiet
qpdf --version
```

For the existing repeated-source and form fixtures, compare qpdf and flpdf page counts, page order markers, AcroForm JSON fields, warning exit status, and output readability. Do not accept object-number differences as semantic failures; compare the reachable structure and observable diagnostics.

- [x] **Step 2: Run the full workspace test suite**

Run:

```bash
cargo test --workspace --all-features --quiet
```

Expected: zero failures and no new ignored test introduced for this route.

The latest full qtest run at `e24f7f6d` produced 2,811 parsed subtests,
2,260 ordinary passes, zero allowlist regressions, and zero parity-manifest
validation errors. The initial single-source cutover exposed qtest
`merge-and-split` 20/22 object-identity regressions; the qpdf-shaped in-place
result branch was added and a focused rerun returned both to PASS.

- [x] **Step 3: Commit only evidence-backed test adjustments**

If the live oracle identifies a missing supported scalar regression, add that regression first, run it RED against the pre-change commit, then implement the smallest canonical correction and rerun GREEN. Otherwise do not modify existing test expectations.

### Task 4: Quality gates and patch coverage

**Files:**
- Test: repository quality gates and changed-line coverage

- [x] **Step 1: Run formatting and strict static checks**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [x] **Step 2: Run authoritative changed-line coverage**

```bash
bash scripts/patch-coverage.sh --base origin/main
```

Expected: `flpdf changed ..., uncovered 0 -> PASS (100%)`.

- [x] **Step 3: Inspect the final worktree**

```bash
git diff --check
git status --short --branch
git log --oneline --decorate -5
```

Confirm no generated artifacts or unrelated files are present.

### Task 5: Rebase, Draft PR, CI, and Beads handoff

**Files:**
- Modify: Beads notes for `flpdf-hxmj` and the dependent issue `flpdf-egzr.8.11`
- Create: GitHub Draft PR from `flpdf-hxmj-canonical-pages` to `main`

- [x] **Step 1: Rebase the feature branch onto current origin/main**

```bash
git fetch --prune origin
git rebase origin/main
```

Rerun the focused tests, full workspace tests, strict gates, and patch coverage after the rebase.

- [x] **Step 2: Push and create a Draft PR**

```bash
git push -u origin flpdf-hxmj-canonical-pages
gh pr create --draft --base main --head flpdf-hxmj-canonical-pages --title "refactor: route single-source pages through qpdf job" --body $'Route every single-source --pages selection through QPDFJob::handle_page_specs, matching qpdf 11.9.0 QPDFJob.cc:466-470 and QPDFJob.cc:2360-2632.\n\nThis removes the duplicate CombinedPlan::build_repeated and standalone collate route; per-spec comma-list collate remains in the dependent flpdf-egzr.8.11 slice.\n\nVerification: focused qpdf page-operation matrix, full workspace tests, strict rustdoc, all-features Clippy, qpdf module/deviation checks, and 100% changed-line patch coverage.'
```

The PR body must cite qpdf `QPDFJob.cc:466-470` and `QPDFJob.cc:2360-2632`, explain that the single-source duplicate route was removed, list focused/full verification, and avoid claiming merge or completion beyond the actual checks.

- [x] **Step 3: Wait for every required CI check and address review findings against qpdf**

```bash
gh pr checks flpdf-hxmj-canonical-pages --watch
```

For any review finding, re-check the pinned qpdf source/live behavior before changing code. Reply in the original inline thread with the source and test evidence; resolve only after the finding is addressed and the check is green.

- [x] **Step 4: Mark the PR ready only after all CI is green**

```bash
gh pr ready flpdf-hxmj-canonical-pages
```

Do not merge this PR in the implementation session.

- [x] **Step 5: Record implementation evidence and persist Beads**

```bash
pr_url="$(gh pr view flpdf-hxmj-canonical-pages --json url --jq .url)"
bd update flpdf-hxmj --append-notes "Implementation and PR ${pr_url}: qpdf 11.9.0 QPDFJob.cc:466-470,2360-2632 canonical single-source page route; RED route-lock then GREEN focused/full verification; no merge in this session."
bd update flpdf-egzr.8.11 --append-notes "Prerequisite flpdf-hxmj is implemented and its PR is ready; the comma-list collate slice is now unblocked after the canonical page-job route cutover."
bd close flpdf-hxmj --reason="qpdf canonical single-source page route implemented and verified"
bd dep cycles
bd dolt push
```

Confirm the final output contains `No dependency cycles detected` and `Push complete.`. Leave both the dedicated worktree and open PR available for integration review.
