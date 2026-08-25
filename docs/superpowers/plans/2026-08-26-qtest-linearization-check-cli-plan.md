# qtest linearization check CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make qtest `linearization.test` rows 16, 17, and 309 pass through qpdf-shaped CLI and warning ownership.

**Architecture:** Keep qtest command identity at the shim boundary through `FLPDF_PROGNAME=qpdf`. Parse `--no-warn` on the top-level CLI and pass it through the existing `QPDFJob` inspection route so the document collects warnings, suppresses delivery, and still returns qpdf status 3. Keep qpdf's fixed clean-check note in the canonical check consumer.

**Tech Stack:** Rust workspace (`flpdf`, `flpdf-cli`), clap, assert_cmd, Python qtest shim tests, qpdf 11.9.0 source and live CLI probes.

---

### Task 1: Add the Rust RED coverage

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`
- Modify: `crates/flpdf/src/job/check.rs` tests

- [ ] **Step 1: Add a focused `--no-warn` integration assertion.**

Write `warnings_only_corrupt_xref_bytes()` to a `NamedTempFile`, run
`flpdf --no-warn --check --repair <file>`, and assert status 3, a checking
banner, and no `WARNING:` in stderr.

- [ ] **Step 2: Add the qpdf fixed-note assertion.**

Require the exact string `No syntax or stream encoding errors found; the file
may still contain\nerrors that qpdf cannot detect\n` in the clean-check test.

- [ ] **Step 3: Run the focused tests and verify RED.**

Run:

```bash
cargo test -p flpdf-cli --test cli_check_exitcodes check_clean_pdf_emits_qpdf_block -- --exact
cargo test -p flpdf-cli --test cli_check_exitcodes check_no_warn_suppresses_warning_delivery_but_keeps_exit_3 -- --exact
```

Expected: the clean-note assertion fails on `flpdf`, and the no-warn test
fails because clap rejects `--no-warn`. Do not edit production code before
observing these failures.

### Task 2: Add qpdf-shaped top-level `--no-warn`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf/src/job/check.rs`
- Test: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`

- [ ] **Step 1: Add the parsed option and route it through canonical check.**

Add a boolean top-level `no_warn` field to `Cli`. Pass it to `run_check`, set
`options.suppress_warnings = true`, and call
`job.set_suppress_warnings(true)` before opening the PDF. Do not add a second
check implementation.

- [ ] **Step 2: Honor document suppression in check diagnostics.**

In `check_document` and its linearization warning helpers, retain
`warnings = true` but skip `logger.warn` delivery when
`pdf.suppress_warnings()` is true. Keep errors and the check information block
visible. This preserves qpdf's distinction between warning state and output.

- [ ] **Step 3: Use qpdf's fixed clean-note string.**

Replace the interpolated message prefix in the no-warning completion note with
the literal `qpdf`, matching `QPDFJob.cc:800-801`. Do not change native error
prefix behavior elsewhere.

- [ ] **Step 4: Run the RED tests until GREEN.**

Run:

```bash
cargo test -p flpdf-cli --test cli_check_exitcodes check_clean_pdf_emits_qpdf_block -- --exact
cargo test -p flpdf-cli --test cli_check_exitcodes check_no_warn_suppresses_warning_delivery_but_keeps_exit_3 -- --exact
cargo test -p flpdf --lib job::check::tests
```

### Task 3: Make the qtest shim preserve qpdf command identity

**Files:**
- Modify: `/home/ubuntu/flpdf-qtest/shim/qpdf`
- Test: `/home/ubuntu/flpdf-qtest/scripts/tests/test_qpdf_shim.py`

- [ ] **Step 1: Add a shim contract test.**

Extend the fake target to print its `FLPDF_PROGNAME` value, invoke
`shim/qpdf`, and assert `FLPDF_PROGNAME=qpdf` while retaining the existing
merged stdout/stderr normalization assertion.

- [ ] **Step 2: Export the identity in the qpdf shim.**

Immediately before delegating to `FLPDF_CLI_BIN`, set
`FLPDF_PROGNAME=qpdf` unless an explicit caller value is already supplied.
Preserve the current pipeline, `PIPESTATUS`, and exit-code behavior.

- [ ] **Step 3: Run the shim tests.**

Run:

```bash
python3 -m unittest scripts.tests.test_qpdf_shim
```

### Task 4: Verify the complete CLI slice

- [ ] **Step 1: Run Rust quality checks.**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-cli --test cli_check_exitcodes
cargo test -p flpdf --lib job::check::tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 2: Run isolated `linearization.test`.**

Use current flpdf release binaries and an isolated copy of
`vendor/qpdf-qtest`, then run `TESTS=linearization qtest-driver`. The paired
artifacts must report rows 16, 17, and 309 passing, with only rows 23, 29, and
35 still failing before the writer slice.

- [ ] **Step 3: Inspect and commit the bounded slice.**

Run:

```bash
git diff --check
git status --short --branch
git diff --stat origin/main...HEAD
```

Commit the flpdf and flpdf-qtest changes in their respective repositories;
keep vendored qtest sources, goldens, and manifest attribution unchanged.
