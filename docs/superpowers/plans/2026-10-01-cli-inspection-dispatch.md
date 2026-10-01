# CLI Inspection Dispatch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route flpdf's native `check-linearization`, `pages`, and `show-encryption` commands through the existing `QPDFJob::run()` lifecycle while preserving their CLI behavior.

**Architecture:** Keep the current Clap commands and configure their existing `QPDFJobConfig` inspection flags. Each command sets its input/password policy on one job and calls `run()`, which owns document creation, inspection, warning completion, and exit status. Do not change library APIs or combine unrelated multi-Job page, rewrite, or attachment flows.

**Tech Stack:** Rust workspace, `flpdf-cli`, `QPDFJob` / `QPDFJobConfig`, `assert_cmd`, live qpdf 11.9.0.

**Spec:** `docs/superpowers/specs/2026-10-01-cli-inspection-dispatch-design.md`

## Global Constraints

- Preserve CLI command names, flags, positional arguments, usage text, stdout/stderr, warning order, and exit status.
- Use qpdf 11.9.0 as the behavior oracle; compare status, stdout, and stderr for native commands against their qpdf top-level flag equivalents.
- Keep wrong-password `show-encryption` behavior: report the partial document, preserve diagnostics, skip later create-stage transformations, and return the qpdf status.
- Do not change public library APIs or fold unrelated page-selection, extraction, rewrite, attachment, or other multi-Job paths into one new abstraction.
- Retain `get_exit_code()` after `write_qpdf()` and warning-state transfer between distinct page Jobs.

## Review Focus

- A linearized file and a non-linearized file must produce the same stdout, stderr, and exit status through `check-linearization` as through `qpdf --check-linearization` (Task 1).
- `pages --show-npages` and default `pages` must select the matching qpdf report and preserve exact report bytes (Task 2).
- A repairable input must emit warnings in the same order and retain the same warning exit code and summary through the page and encryption reports (Tasks 2 and 3).
- A correct password, a password file, and a wrong password must reach the Job open boundary; wrong-password encryption inspection must still print the partial report with qpdf's status (Task 3).
- A source-level route contract must fail while a CLI handler calls a standalone wrapper and pass only when that handler uses `QPDFJob::run()` with the matching config flag (Tasks 1-3).

---

## File Map

- `crates/flpdf-cli/src/main.rs` — adapt only the three native inspection handlers and their `run_command` call arguments.
- `crates/flpdf-cli/tests/overlay_transform_order_route_tests.rs` — add source-level contracts for the three native handlers, following the existing `standalone_check_uses_the_canonical_job_lifecycle` pattern.
- `crates/flpdf-cli/tests/cli_linearize_qpdf.rs` — exact qpdf differential for `check-linearization`.
- `crates/flpdf-cli/tests/cli_pages_direct_root_qpdf.rs` — exact qpdf differential for both native `pages` reports and a repair-warning case.
- `crates/flpdf-cli/tests/encrypt_cli_tests.rs` — exact qpdf differential for the native `show-encryption` subcommand, including wrong-password and repair-warning cases.
- `docs/qpdf-route-matrix/e-job-cli-capi.md` — remeasure and update only the E-7/E-19/E-21 evidence/classification supported by the final callsite audit.

No `flpdf` library source or public API change is planned; the existing config setters and `QPDFJob::run()` are the implementation surface.

### Task 1: Route `check-linearization` through `QPDFJob::run()`

**Files:**
- Modify: `crates/flpdf-cli/tests/overlay_transform_order_route_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_linearize_qpdf.rs`
- Modify: `crates/flpdf-cli/src/main.rs`

**Interfaces:**
- Consumes: `QPDFJobConfig::check_linearization()`, `QPDFJob::set_input_file`, the existing password/recovery setters, and `QPDFJob::run()`.
- Produces: `run_check_linearization(input: PathBuf, repair: bool, password: &PasswordArgs, no_warn: bool) -> CliResult<()>`; the command retains its syntax and returns `finish_job_exit_status(job.run()?)`.

- [ ] **Step 1: Add the failing route contract**

Add `check_linearization_subcommand_uses_job_run` to
`overlay_transform_order_route_tests.rs`. Extract only the production body of
`run_check_linearization`; assert it configures `check_linearization`, calls
`job.run()`, and does not call `job.check_linearization`, `job.show_linearization`,
`open_with_description`, or `File::open`.

- [ ] **Step 2: Confirm the route contract fails before the change**

Run: `cargo test -p flpdf-cli --test overlay_transform_order_route_tests check_linearization_subcommand_uses_job_run`

Expected: FAIL because the current handler opens a `Pdf` and calls the public
inspection wrapper directly.

- [ ] **Step 3: Add the qpdf behavior lock**

Add `check_linearization_subcommand_matches_qpdf_exact_output` to
`cli_linearize_qpdf.rs`. Run qpdf `--check-linearization` and flpdf
`check-linearization` on `linearized-one-page.pdf` and `one-page.pdf`; set
`FLPDF_PROGNAME=qpdf` for flpdf, and assert exact exit status, stdout, and
stderr. Keep the existing qpdf availability guard; CI uses the repository's
pinned 11.9.0 oracle.

- [ ] **Step 4: Route the handler through the Job**

Simplify the private handler to the interface above. Configure
`job.config().check_linearization()`, copy the existing password/recovery
values onto the Job before setting its input, and return
`finish_job_exit_status(job.run()?)`. Remove the now-unneeded direct-open and
direct-inspection path for this command; do not change the top-level flag
route.

- [ ] **Step 5: Verify and commit Task 1**

Run the route contract and `cargo test -p flpdf-cli --test cli_linearize_qpdf`.
The route contract must pass, and the two qpdf cases must have identical
status/stdout/stderr. Run `cargo fmt --all -- --check`, then commit the test
and handler change.

### Task 2: Route both native `pages` reports through `QPDFJob::run()`

**Files:**
- Modify: `crates/flpdf-cli/tests/overlay_transform_order_route_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_pages_direct_root_qpdf.rs`
- Modify: `crates/flpdf-cli/src/main.rs`

**Interfaces:**
- Consumes: `QPDFJobConfig::show_npages()`, `QPDFJobConfig::show_pages()`, the existing input/password/recovery setters, and `QPDFJob::run()`.
- Produces: `run_show_npages(input: PathBuf, repair: bool, password: &PasswordArgs, suppress_warnings: bool) -> CliResult<()>` and `run_show_pages(input: PathBuf, repair: bool, password: &PasswordArgs, suppress_warnings: bool) -> CliResult<()>`; `pages --show-npages` selects only `show_npages`, and default `pages` selects only `show_pages`.

- [ ] **Step 1: Add the failing pages route contract**

Add `pages_subcommands_use_job_run` to
`overlay_transform_order_route_tests.rs`. Extract the bodies of
`run_show_npages` and `run_show_pages`; assert each configures its matching
report and calls `job.run()`. Forbid direct `job.show_npages`, `job.show_pages`,
`open_pdf_with_suppression`, and `complete_report` calls in those handler bodies.

- [ ] **Step 2: Confirm the route contract fails before the change**

Run: `cargo test -p flpdf-cli --test overlay_transform_order_route_tests pages_subcommands_use_job_run`

Expected: FAIL because both handlers open a `Pdf` before invoking a report
wrapper.

- [ ] **Step 3: Add qpdf output and warning regressions**

Add exact qpdf comparisons in `cli_pages_direct_root_qpdf.rs` for:

1. `pages --show-npages` versus qpdf `--show-npages` on `three-page.pdf`.
2. default `pages` versus qpdf `--show-pages` on `three-page.pdf`.
3. both native reports versus the equivalent qpdf flag on
   `test_driver/repairable_input.pdf`, asserting exit status and exact
   stdout/stderr so repair-warning order and summary remain fixed.

Set `FLPDF_PROGNAME=qpdf` for flpdf invocations and retain the existing exact
qpdf 11.9.0 guard.

- [ ] **Step 4: Configure and run each page report on one Job**

Simplify both private handlers to the interfaces above. Set exactly one
corresponding config flag, copy the existing password and recovery settings
before input open, and let `job.run()` own the open/report/warning-completion
sequence. Update the `run_command` calls and remove only arguments that were
always hard-coded defaults for these native subcommands.

- [ ] **Step 5: Verify and commit Task 2**

Run `cargo test -p flpdf-cli --test overlay_transform_order_route_tests
pages_subcommands_use_job_run` and
`cargo test -p flpdf-cli --test cli_pages_direct_root_qpdf`. Confirm all three
qpdf comparisons match in status/stdout/stderr, run `cargo fmt --all -- --check`,
and commit the route and tests.

### Task 3: Route native `show-encryption` through `QPDFJob::run()`

**Files:**
- Modify: `crates/flpdf-cli/tests/overlay_transform_order_route_tests.rs`
- Modify: `crates/flpdf-cli/tests/encrypt_cli_tests.rs`
- Modify: `crates/flpdf-cli/src/main.rs`

**Interfaces:**
- Consumes: `QPDFJobConfig::show_encryption()`, the existing password/recovery setters, and `QPDFJob::run()`.
- Produces: `run_show_encryption(input: PathBuf, repair: bool, password: &PasswordArgs) -> CliResult<()>`; the native command obtains the report, warning completion, and exit status from the same Job lifecycle as qpdf.

- [ ] **Step 1: Add the failing route contract**

Add `show_encryption_subcommand_uses_job_run` to
`overlay_transform_order_route_tests.rs`. Assert the handler configures
`show_encryption`, sets input/password options before open, calls `job.run()`,
and does not call `job.show_encryption`, `complete_report`, or an independent
open helper.

- [ ] **Step 2: Confirm the route contract fails before the change**

Run: `cargo test -p flpdf-cli --test overlay_transform_order_route_tests show_encryption_subcommand_uses_job_run`

Expected: FAIL because the current handler renders the report and completes the
Job manually.

- [ ] **Step 3: Add qpdf regressions for sensitive open cases**

In `encrypt_cli_tests.rs`, retain the existing correct-password report
comparison and add:

- `show_encryption_subcommand_wrong_password_matches_qpdf` using
  `encrypted/v2-rc4-128-r3.pdf` and a deliberately wrong password; compare
  exit status, stdout, and stderr with qpdf's `--show-encryption` route.
- `show_encryption_subcommand_repair_warnings_match_qpdf` using
  `test_driver/repairable_input.pdf`; compare status, warning order, report
  body, and warning summary.
- `show_encryption_subcommand_password_file_matches_qpdf` using the encrypted
  fixture and a one-line temporary password file; compare status/stdout/stderr.

Use `FLPDF_PROGNAME=qpdf` for flpdf and the file's existing platform newline
normalization convention.

- [ ] **Step 4: Let `QPDFJob::run()` own encryption inspection**

Simplify the private handler to the interface above. Configure
`show_encryption`, set input/password/recovery policy before opening, and call
`job.run()`. Preserve `QPDFJob::create_qpdf`'s partial-document wrong-password
path and remove the now-unused `finish_show_encryption` helper only if there
are no other callers.

- [ ] **Step 5: Verify and commit Task 3**

Run the route contract and
`cargo test -p flpdf-cli --test encrypt_cli_tests`; require exact qpdf status,
stdout, and stderr for correct password, wrong password, repair warnings, and
password-file cases. Run `cargo fmt --all -- --check`, then commit.

### Task 4: Refresh route evidence and run integrated gates

**Files:**
- Modify: `docs/qpdf-route-matrix/e-job-cli-capi.md`

**Interfaces:**
- Consumes: the final production caller inventory and Task 1-3 route contract tests.
- Produces: E-7/E-19/E-21 notes that state only what current source and qpdf evidence support.

- [ ] **Step 1: Recount and inspect the final routes**

Run `python3 scripts/qpdf-route-callers.py --symbol check_linearization
--symbol show_npages --symbol show_pages --symbol show_encryption --symbol
get_exit_code --symbol complete_report --symbol has_warnings` and inspect the
three native handler bodies. Record current-main production counts; do not
count comments, tests, or unrelated same-named methods as CLI callers.

- [ ] **Step 2: Update the route-matrix rows**

Record that these native inspection commands now use `QPDFJob::run()` and its
existing config dispatch. Reclassify E-7/E-19 only if the final caller audit
supports it. Keep E-21 mixed for unrelated multi-Job orchestration still
outside this slice.

- [ ] **Step 3: Run integrated verification and commit**

Run, in order:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/check-qpdf-route-matrix.py
scripts/patch-coverage.sh --base origin/main
```

Commit the matrix update after these local gates pass. Then open a Draft PR,
record the exact head in Beads, and mark it Ready only after every exact-head
GitHub check succeeds and a fresh PR readback is `OPEN`, non-draft, and CLEAN.
