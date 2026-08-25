# qtest `--check-linearization` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0's top-level `--check-linearization` inspection route into the shared Rust `QPDFJob` lifecycle and make every corresponding `linearization.test` invocation pass.

**Architecture:** Keep linearization detection and hint validation in `crates/flpdf/src/linearization/check.rs`, add the missing qpdf-owned inspection consumer to `crates/flpdf/src/job/check.rs`, and make both the qpdf-shaped top-level CLI option and the existing flpdf subcommand call that consumer. The CLI opens one document with one configured logger, while qtest remains an external acceptance harness.

**Tech Stack:** Rust workspace, clap, assert_cmd, qpdf 11.9.0, Perl qtest-driver, Beads.

---

### Task 1: Add the CLI RED differential tests

**Files:**
- Create: `crates/flpdf-cli/tests/cli_check_linearization.rs`

- [ ] **Step 1: Write the failing tests**

Create a focused integration test file with a qpdf runner and three real
behaviors. The qpdf runner must use `/usr/bin/qpdf` when available and skip
the differential tests when it is not installed. The clean and non-linearized
tests compare status, stdout, and stderr from qpdf 11.9.0 with the top-level
flpdf command. The warning test copies
`tests/fixtures/compat/linearized-one-page.pdf`, replaces the equal-width
`/O 6 /E` bytes with `/O 7 /E`, and compares the warning result. Include a
subcommand test asserting that `flpdf check-linearization INPUT` uses the same
output as the top-level option.

The core helper shape is:

```rust
fn run(program: &str, args: &[&str]) -> std::process::Output {
    std::process::Command::new(program)
        .args(args)
        .env("FLPDF_PROGNAME", "qpdf")
        .output()
        .expect("inspection command should spawn")
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
```

Each test must invoke `Command::cargo_bin("flpdf")` for the Rust command and
assert the complete `Output` tuple against the qpdf result. The warning test
must additionally assert status 3, the exact `WARNING: <path>: first page
object (/O) mismatch` line, and `qpdf: operation succeeded with warnings`.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test cli_check_linearization
```

Expected: the tests compile, then fail because the production CLI reports
`unexpected argument '--check-linearization' found`; no production code is
changed before this failure is observed.

- [ ] **Step 3: Commit the RED tests**

```bash
git add crates/flpdf-cli/tests/cli_check_linearization.rs
git commit -m "test: cover qpdf check-linearization CLI route"
```

### Task 2: Add the canonical `QPDFJob` linearization consumer

**Files:**
- Modify: `crates/flpdf/src/job/check.rs`
- Test: `crates/flpdf-cli/tests/cli_check_linearization.rs`

- [ ] **Step 1: Add the job-level qpdf contract**

Implement `QPDFJob::check_linearization<R: Read + Seek + 'static>` in
`job/check.rs`. It must:

1. clone the job logger and input name, then install the logger on `pdf`;
2. call `pdf.is_linearized()` once for the qpdf detection branch;
3. print `<input> is not linearized` and complete success when detection is
   false;
4. read `pdf.source_bytes()` from the same document and invoke
   `check_linearization_warnings(pdf, &source_bytes, false)`;
5. emit every returned warning with the existing `emit_warning` helper in
   order, or wrap an `InvalidParam`/I/O checker result with qpdf's
   `error encountered while checking linearization data:` warning text;
6. record the warning state, print `<input>: no linearization errors` only
   when no warning was emitted, and finish through `complete(false)`.

Logger failures must return the existing `crate::Error` through the job
boundary. A malformed PDF must become qpdf's warning/exit-3 path; it must not
panic and must not be converted into a CLI-only `exit(1)` error.

- [ ] **Step 2: Run the RED tests after only the library consumer is added**

Run:

```bash
cargo test -p flpdf-cli --test cli_check_linearization
```

Expected: the tests still fail at argument parsing, proving the library
consumer did not accidentally make the unsupported top-level option pass.

- [ ] **Step 3: Commit the job consumer**

```bash
git add crates/flpdf/src/job/check.rs
git commit -m "feat: add qpdf-shaped linearization check job"
```

### Task 3: Wire the top-level option and subcommand to the job route

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/src/arg_parser.rs`
- Test: `crates/flpdf-cli/tests/cli_check_linearization.rs`

- [ ] **Step 1: Add the qpdf-shaped top-level flag**

Add `check_linearization: bool` to `Cli` with clap's inspection conflicts so
it cannot be combined with output, rewrite, JSON, page, encryption, or the
other inspection modes. Keep `arg_parser.rs`'s bare-option normalization and
add a unit assertion that `--check-linearization=ignored` becomes the bare
flag, matching qpdf's `addBare` behavior.

- [ ] **Step 2: Add one open-and-dispatch function**

Add `run_check_linearization(input, repair, password)` beside `run_check`.
Open the input with a `QPDFJob` configured with `cli_logger()` and
`progname()`, pass the job's logger through `job.open`, call
`job.check_linearization(&mut pdf)`, and map the returned `JobExitCode` with
`finish_job_exit_status`. Do not call `check_linearization_path`, reopen the
file, or construct a second logger.

Dispatch the top-level bool before the generic `--check` branch. Change the
existing `Commands::CheckLinearization` arm to call this same function with
its input instead of printing `linearization OK` from the old standalone
wrapper.

- [ ] **Step 3: Run the focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test cli_check_linearization
cargo test -p flpdf-cli --test cli_check_exitcodes
cargo test -p flpdf --lib job::check::tests
```

Expected: all new clean/non-linearized/malformed/subcommand tests pass, the
existing generic check exit-code tests remain green, and no qpdf warning is
printed twice.

- [ ] **Step 4: Commit the CLI cutover**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/src/arg_parser.rs crates/flpdf-cli/tests/cli_check_linearization.rs
git commit -m "feat: wire top-level check-linearization"
```

### Task 4: Verify the qtest target and update only evidence-backed attribution

**Files:**
- Create outside the repository: paired temporary qtest artifacts under a
  disposable directory
- Potentially modify in the separate `/home/ubuntu/flpdf-qtest` repository:
  `parity/qtest-11.9.0.jsonl` only for rows proven passing by the fresh run

- [ ] **Step 1: Build the release binaries used by qtest**

Run:

```bash
cargo build --release --workspace
```

- [ ] **Step 2: Run only `linearization.test` through qtest-driver**

Use a disposable copy of `vendor/qpdf-qtest`, set `TESTS=linearization`, and
route every helper through the existing `/home/ubuntu/flpdf-qtest/shim` with
the release binaries from this worktree. Preserve the same-run `harness.log`
and `qtest-results.xml`; do not patch the vendored `.test` file or the shim.

The authoritative target check is that every qtest row whose command is
`qpdf --check-linearization a.pdf` has `PASSED`, including all three
`object-streams` modes for every input in `@to_linearize`. Other pre-existing
linearization failures must be reported separately rather than counted as
target passes.

- [ ] **Step 3: Promote qtest manifest rows only after the target is green**

If the separate repository's manifest workflow requires attribution, update
only the exact `linearization` check-linearization rows whose fresh qtest
results are passing. Preserve unrelated failing/blocked rows, run the
repository's manifest verifier, and keep the change in a separate qtest
worktree/branch if it is needed. Do not edit qpdf-qtest vendor sources.

- [ ] **Step 4: Run quality gates and read back state**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test --workspace
git diff --check
bd show flpdf-25kg.6.17
bd dep cycles
bd dolt push
```

The full workspace result must distinguish the known unrelated baseline
failure if it remains. The final goal is not complete until the focused qtest
target rows are all passing and the fresh paired artifacts are inspected.

- [ ] **Step 5: Commit or push only after fresh verification**

Read `git status`, the full diff, and all focused command exit codes. Push the
feature branch only after the verification output supports the claim; do not
close the Beads issue until the requested qtest evidence and quality gates are
complete.
