# qtest content-preservation.test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every one of the 361 qtest `content-preservation.test` cases pass against the pinned qpdf 11.9.0 observable contract.

**Architecture:** Keep PDF behavior in the canonical `Pdf`/`QPDFJob` reader and writer boundaries. Add the missing qpdf `showEncryption` report to the job-owned check lifecycle and implement only the portable observations of qpdf `qpdf-ctest` test01 at the separate qtest-tools process boundary. The upper branch depends on the lower test01 adapter branch so the final qtest run exercises both layers together.

**Tech Stack:** Rust workspace (`flpdf`, `flpdf-cli`, `flpdf-qtest-tools`), qtest Perl harness, qpdf 11.9.0 source and fixtures, Cargo tests, qtest `qtest-results.xml`.

**Spec:** `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`

## Global Constraints

- qpdf 11.9.0 source and observed output are the semantic authority.
- Do not change qtest `.test` files or expected output files.
- Do not add C or C++ ABI support; test01 is a Rust-native portable observable adapter.
- Keep `main` untouched and use the two dedicated worktrees for the stacked layers.
- Every production behavior change has a RED test that fails for the expected reason before implementation.
- Keep qtest `harness.log` and `qtest-results.xml` from the same invocation when accounting results.

### Task 1: Implement the portable qpdf-ctest test01 adapter

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs`
- Test: `crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs`

**Interfaces:**
- Consumes: public `Pdf::version`, `Pdf::adobe_extension_level`, `Pdf::is_linearized`, `Pdf::is_encrypted`, `Pdf::encryption_info`, and `Permissions` projections.
- Produces: `qpdf-ctest 1 infile password outfile` stdout matching qpdf `test01`, while retaining existing test19 and `--version` behavior.

- [ ] **Step 1: Add RED tests for test01 plain and encrypted observations**

Add subprocess tests that invoke `qpdf-ctest 1` with the existing minimal PDF and a generated encrypted PDF, and assert the complete stdout including the final `C test 1 done` line. Assert that the test01 invocation does not create or require the output path. Keep the existing test19 and version assertions unchanged.

- [ ] **Step 2: Run the focused qpdf-ctest tests and verify the expected RED**

```bash
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli
```

Expected: the new test01 cases fail with the current test19-only usage error; existing test19 and version cases pass.

- [ ] **Step 3: Implement test01 from qpdf `qpdf-ctest.c:135-158`**

Dispatch argument `1` to a reader-only path. Emit `version`, the optional extension-level line, `linearized`, and `encrypted` in qpdf order. For encrypted documents emit the trimmed user password and the nine numeric permission projections using the revision-dependent qpdf bit rules. Finish with `C test 1 done`; leave unknown test numbers rejected and do not route them to test19.

- [ ] **Step 4: Run the focused tests and verify GREEN**

```bash
cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli
```

Expected: all qpdf-ctest CLI tests pass with no new warnings or output drift.

- [ ] **Step 5: Commit the lower layer**

```bash
git add crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs
git commit -m "feat(qtest): implement portable qpdf-ctest test01"
```

### Task 2: Route qpdf encryption reporting through QPDFJob check

**Files:**
- Modify: `crates/flpdf/src/job/check.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Test: `crates/flpdf/src/job/check.rs` tests and the existing CLI encryption-report test surface

**Interfaces:**
- Consumes: `Pdf::encryption_info`, `Permissions`, `QPDFLogger`, and the existing job warning/completion lifecycle.
- Produces: one canonical qpdf `showEncryption` report renderer reused by `QPDFJob::check` and CLI `--show-encryption`.

- [ ] **Step 1: Add RED coverage for encrypted QPDFJob check output**

Extend the job check capture test with a valid R=3 encrypted document and assert qpdf’s exact sequence: `R`, `P`, `User password`, password-match lines, all nine permission lines, the linearization line, and the normal check footer. Preserve the existing plaintext expected output assertion.

- [ ] **Step 2: Run the focused check test and verify the expected RED**

```bash
cargo test -p flpdf --lib job::check::tests
```

Expected: the new encrypted assertion fails because the current check output contains only `File is encrypted`.

- [ ] **Step 3: Extract the qpdf report into the job-owned canonical renderer**

Move the byte-oriented report construction currently duplicated in `flpdf-cli` to the job/inspection boundary. Preserve qpdf’s report order from `QPDFJob.cc:700-742`, use qpdf-shaped individual `Pdf` encryption projections and revision-dependent permission logic, and make `QPDFJob::check` invoke it immediately after the encryption state line and before linearization inspection as in `QPDFJob.cc:744-765`. Keep warning suppression and completion outside the report body.

- [ ] **Step 4: Make CLI show-encryption delegate to the shared renderer**

Replace the CLI-local duplicate formatting with the job-owned renderer while retaining the CLI-only incorrect-password prefix and final completion behavior. Verify plaintext, authenticated encrypted input, and wrong-password inspection behavior against existing tests.

- [ ] **Step 5: Run focused tests and verify GREEN**

```bash
cargo test -p flpdf --lib job::check::tests
cargo test -p flpdf-cli --test cli_tests
```

Expected: encrypted and plaintext check/report tests pass, with no change to unrelated inspection output.

- [ ] **Step 6: Commit the upper layer**

```bash
git add crates/flpdf/src/job/check.rs crates/flpdf-cli/src/main.rs
git commit -m "fix(job): emit qpdf encryption report during check"
```

### Task 3: Verify the complete qtest contract

**Files:**
- Verify only: `vendor/qpdf-qtest/content-preservation.test` in a disposable qtest datadir
- Verify only: `qtest-results.xml` and `harness.log` from the same run
- Follow-up ledger: `flpdf-qtest/parity/qtest-11.9.0.jsonl` only after the result is proven

- [ ] **Step 1: Build all qtest helper binaries from the upper branch**

```bash
cargo build --release --bin flpdf --bin flpdf-test-compare --bin flpdf-test-driver --bin qpdfjob-ctest --bin qpdf-ctest --bin flpdf-test-pdf-doc-encoding --bin flpdf-test-pdf-unicode --bin flpdf-test-unicode-filenames --bin test_xref --bin test_parsedoffset
```

- [ ] **Step 2: Run only `content-preservation.test` in an isolated qtest datadir**

Use `TESTS=content-preservation` with the qtest driver, the complete shim PATH, and all ten Rust helper binaries. Count the XML testcases and require 361 ordinary passes, zero failures, and exit status 0.

- [ ] **Step 3: Verify the target against qpdf expected artifacts**

Confirm that all 24 encrypted `check status` rows and all 120 `check with C API` rows pass, while the 217 baseline passing rows remain passing. Do not treat a manifest exclusion or a missing result as a pass.

- [ ] **Step 4: Run repository quality gates on the upper branch**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check HEAD~2..HEAD
```

- [ ] **Step 5: Reconcile only proven qtest manifest rows**

After a same-run 361/361 result, update only the content-preservation rows whose outcomes changed from `blocked` or `excluded` to `passing`, retaining the qtest repository’s exact XML identity and no unrelated suite rows.

- [ ] **Step 6: Read back Beads, Git, qtest artifacts, and PR/CI state before completion**

Read both claimed issues, verify dependencies and cycles, inspect `git status`, retain the paired qtest artifacts, and report any remaining external CI or manifest dependency explicitly.
