# library-version.test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the real qpdf 11.9.0 `library-version.test` pass all three subtests through Rust-owned canonical routes.

**Architecture:** Keep the qpdf library version as a core flpdf primitive distinct from the Cargo package version. Render qpdf's `--version` and `--copyright` at the flpdf CLI boundary, and make the Rust `qpdf-ctest` process adapter consume the same core qpdf version primitive. Update the qtest manifest only after a paired authoritative run proves all three outcomes.

**Tech Stack:** Rust workspace, clap qpdf-argv preprocessing, assert_cmd integration tests, flpdf-qtest Perl qtest-driver, qpdf 11.9.0 pinned source and `/usr/bin/qpdf` oracle, JSONL parity manifest.

---

### Task 1: Establish RED tests and the shared qpdf version contract

**Files:**
- Create: `crates/flpdf-cli/tests/library_version.rs`
- Modify: `crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs`
- Modify: `crates/flpdf/src/lib.rs`

- [ ] **Step 1: Add process-level CLI expectations before implementation**

  Add tests that run the real `flpdf` binary and assert qpdf's pinned output contracts:

  ```rust
  Command::cargo_bin("flpdf")
      .unwrap()
      .arg("--version")
      .assert()
      .success()
      .stdout("qpdf version 11.9.0\nRun qpdf --copyright to see copyright and license information.\n")
      .stderr("");
  ```

  Add a second test for `--copyright` asserting the complete qpdf 11.9.0 text from `QPDFJob_argv.cc:108-130`, including the version line, Jay Berkenbilt copyright line, Apache license line, and empty stderr.

- [ ] **Step 2: Add the qpdf-ctest version expectation before implementation**

  Extend `qpdf_ctest_cli.rs` with an `assert_cmd` test requiring:

  ```rust
  Command::cargo_bin("qpdf-ctest")
      .unwrap()
      .arg("--version")
      .assert()
      .success()
      .stdout("qpdf-ctest version 11.9.0\n")
      .stderr("");
  ```

- [ ] **Step 3: Run the focused RED tests**

  Run `cargo test -p flpdf-cli --test library_version` and `cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli`. They must fail because `--version`/`--copyright` are rejected and qpdf-ctest currently reports the Cargo version `0.4.0`.

- [ ] **Step 4: Add the core qpdf version primitive**

  Add a documented `flpdf::qpdf_version() -> &'static str` returning the pinned qpdf version `11.9.0`, explicitly separate from the existing `flpdf::version()` Cargo package version. The doc must cite `QPDF::QPDFVersion` (`libqpdf/QPDF.cc:178-181`) and state that qtest-facing qpdf compatibility output uses this primitive.

- [ ] **Step 5: Run the focused tests again and commit the contract/test slice**

  The tests remain RED until the consumers are implemented. Commit the shared primitive and tests only after confirming the expected failure messages are the missing consumer behavior, not a test typo.

### Task 2: Port qpdf CLI version and copyright ownership

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Test: `crates/flpdf-cli/tests/library_version.rs`

- [ ] **Step 1: Register qpdf-shaped top-level flags**

  Add boolean `--version` and `--copyright` fields to `Cli`, with no clap auto-version text. They must survive the existing qpdf argv preprocessing and be handled before PDF input opening, writer dispatch, static-id warnings, or ordinary usage requirements.

- [ ] **Step 2: Implement the qpdf ArgParser output boundary**

  Add CLI-owned output functions that print to stdout exactly as qpdf `ArgParser::argVersion` and `argCopyright` do (`libqpdf/QPDFJob_argv.cc:103-130`), using `flpdf::qpdf_version()` for the version. `--version` and `--copyright` exit successfully without reading input and write no stderr.

- [ ] **Step 3: Verify GREEN and preserve ordinary CLI behavior**

  Run the two library-version CLI tests plus `cargo test -p flpdf-cli --test cli_tests`. Then run `cargo run --bin flpdf -- --version` and `cargo run --bin flpdf -- --copyright` against the implementation binary and compare stdout/stderr/exit status with qpdf.

- [ ] **Step 4: Commit the canonical CLI slice**

  Commit the core primitive and CLI implementation/tests with a focused message after the focused tests are GREEN.

### Task 3: Align the qpdf-ctest process adapter

**Files:**
- Modify: `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs`
- Test: `crates/flpdf-qtest-tools/tests/qpdf_ctest_cli.rs`

- [ ] **Step 1: Replace Cargo-version output with the shared qpdf version**

  Change only the `--version` process branch to print `qpdf-ctest version {flpdf::qpdf_version()}`. Keep test19's existing writer responsibility and argument/exit behavior unchanged; qpdf's C API version query is represented at this process boundary, not by adding a C ABI.

- [ ] **Step 2: Run qpdf-ctest focused tests**

  Run `cargo test -p flpdf-qtest-tools --test qpdf_ctest_cli` and the full qtest-tools test suite. Confirm the existing deterministic-ID test19 output remains `C test 19 done`.

### Task 4: Prove the real qtest suite and update its manifest

**Files:**
- Modify in qtest worktree: `parity/qtest-11.9.0.jsonl`

- [ ] **Step 1: Build all qtest helper binaries from the flpdf feature branch**

  Build the ten binaries used by `flpdf-qtest/scripts/run.sh`, then run the real vendored `library-version.test` with isolated datadir and shim PATH. Capture `harness.log` and `qtest-results.xml` from the same invocation.

- [ ] **Step 2: Confirm the target suite is 3/3 before editing the ledger**

  Require qtest-driver exit 0, `library-version` Total 3, Passes 3, Failures 0, Missing 0, and Extra 0. Do not use the manifest or a partial stderr match as proof of the target outcome.

- [ ] **Step 3: Promote only the three library-version rows**

  Update manifest rows `library-version 1`, `library-version 2`, and `library-version 3` to `state: passing` and clear `rationale`, `owner`, `bead`, and `replacement_ref`. Preserve all other JSONL lines and field order byte-for-byte.

- [ ] **Step 4: Run manifest validation and qtest tests**

  Run `python3 scripts/verify-parity-manifest.py harness.log qtest-results.xml parity/qtest-11.9.0.jsonl`, the full qtest Python suite, and `git diff --check`. If a full survey exposes unrelated stale rows, record them separately and do not rewrite them as part of this target task.

- [ ] **Step 5: Commit and push the qtest manifest slice**

  Commit the three-row manifest update on the dedicated qtest branch and push it for CI.

### Task 5: Completion audit and handoff

- [ ] **Step 1: Run relevant flpdf quality gates**

  Run `cargo fmt --all -- --check`, focused CLI/qpdf-ctest tests, workspace tests, all-features clippy, and strict private rustdoc for changed flpdf code.

- [ ] **Step 2: Verify PR/CI and paired qtest evidence live**

  Re-query PR state, commit SHAs, qtest job conclusion, and the same-run `harness.log`/`qtest-results.xml` pair. Confirm no manifest validation errors and all three target rows pass.

- [ ] **Step 3: Update and close Beads only after the audit**

  Append the evidence to `flpdf-n9t0.14`, run `bd dep cycles`, close the issue, and require `bd dolt push` to print `Push complete.` before handoff.
