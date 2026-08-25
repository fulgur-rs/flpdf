# qtest xref-streams.test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all five qpdf 11.9.0 `xref-streams.test` subtests pass through the flpdf CLI while preserving qpdf's xref ownership, warning order, output format, and exit status.

**Architecture:** Keep xref parsing and effective-table ownership in `Pdf`, expose the qpdf `QPDF::showXRefTable` consumer as `QPDFJob::show_xref`, and dispatch the top-level `--show-xref` option through the existing inspection lifecycle. The consumer formats only effective uncompressed and compressed entries from `Pdf::get_xref_table`; it does not inspect raw parser state, rewrite qtest fixtures, or add a CLI-only compatibility bridge.

**Tech Stack:** Rust workspace, `flpdf`/`flpdf-cli`, assert_cmd integration tests, qpdf 11.9.0 source and `/home/ubuntu/flpdf-qtest` upstream harness.

---

### Task 1: Add the canonical CLI regression test

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`
- Test: `crates/flpdf-cli/tests/cli_tests.rs`

- [ ] **Step 1: Write the failing test**

Add one integration test that asserts the exact qpdf-formatted effective xref table for both a normal fixture and an object-stream fixture:

```rust
#[test]
fn show_xref_prints_qpdf_effective_table() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-xref", "../../tests/fixtures/compat/one-page.pdf"])
        .assert()
        .success()
        .stdout(concat!(
            "1/0: uncompressed; offset = 61\n",
            "2/0: uncompressed; offset = 92\n",
            "3/0: uncompressed; offset = 199\n",
            "4/0: uncompressed; offset = 392\n",
            "5/0: uncompressed; offset = 460\n",
            "6/0: uncompressed; offset = 721\n",
            "7/0: uncompressed; offset = 780\n",
        ));

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-xref", "../../tests/fixtures/compat/three-page-objstm.pdf"])
        .assert()
        .success()
        .stdout(concat!(
            "1/0: uncompressed; offset = 15\n",
            "2/0: compressed; stream = 1, index = 0\n",
            "3/0: compressed; stream = 1, index = 1\n",
            "4/0: compressed; stream = 1, index = 2\n",
            "5/0: compressed; stream = 1, index = 3\n",
            "6/0: compressed; stream = 1, index = 4\n",
            "7/0: compressed; stream = 1, index = 5\n",
            "8/0: compressed; stream = 1, index = 6\n",
            "9/0: compressed; stream = 1, index = 7\n",
            "10/0: uncompressed; offset = 532\n",
            "11/0: uncompressed; offset = 685\n",
            "12/0: uncompressed; offset = 838\n",
            "13/0: uncompressed; offset = 991\n",
        ));
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p flpdf-cli --test cli_tests show_xref_prints_qpdf_effective_table`

Expected: FAIL because clap reports `unexpected argument '--show-xref'`.

### Task 2: Implement the qpdf-shaped inspection route

**Files:**
- Modify: `crates/flpdf/src/job/inspection.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/src/arg_parser.rs`

- [ ] **Step 1: Add `QPDFJob::show_xref`**

Implement the public inspection method beside `show_npages`/`show_pages`:

```rust
pub fn show_xref<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<JobExitCode> {
    let logger = self.logger();
    self.inspect(pdf, |pdf| {
        for (object_ref, entry) in pdf.get_xref_table() {
            let line = match entry {
                XrefEntry::Uncompressed { offset } => {
                    format!("{}/{}: uncompressed; offset = {offset}\n", object_ref.number, object_ref.generation)
                }
                XrefEntry::Compressed { stream, index } => {
                    format!("{}/{}: compressed; stream = {stream}, index = {index}\n", object_ref.number, object_ref.generation)
                }
                XrefEntry::Free { .. } => continue,
            };
            logger.info(line)?;
        }
        Ok(())
    })
}
```

The method must use the existing `inspect` completion boundary so open-time and operation-time qpdf warnings are emitted before the warning exit status. `Free` rows are not printed because qpdf's `QPDF::showXRefTable` iterates its effective `m->xref_table`, where type-0 entries are not ordinary output rows; no synthetic line or error is invented for them.

- [ ] **Step 2: Register and dispatch `--show-xref`**

Add `show-xref` to the shared qpdf bare-long option list and add a `show_xref: bool` field to the top-level CLI inspection flags. Include it in the same inspection conflicts as `show-pages`, then add the ordered dispatch:

```rust
} else if args.show_xref {
    run_show_xref(args.input, args.repair, &args.password)
```

Add `run_show_xref` beside `run_show_pages`:

```rust
fn run_show_xref(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    finish_job_exit_status(job.show_xref(&mut pdf)?)
}
```

Update JSON-input inspection predicates only if the existing qpdf job path supports this inspection mode; ordinary file-backed qtest invocation must reach the same `run_show_xref` path as the other top-level inspection options.

- [ ] **Step 3: Run the focused Rust test and verify GREEN**

Run: `cargo test -p flpdf-cli --test cli_tests show_xref_prints_qpdf_effective_table`

Expected: PASS.

### Task 3: Verify the upstream five-case behavior

**Files:**
- No qpdf-qtest vendor files are copied into flpdf.
- No expected qtest output is modified.

- [ ] **Step 1: Build the CLI used by the isolated harness**

Run: `cargo build --workspace`

Expected: exit 0.

- [ ] **Step 2: Run only `xref-streams.test` with the real harness**

Use a temporary copy of `/home/ubuntu/flpdf-qtest/vendor/qpdf-qtest`, set `FLPDF_CLI_BIN` to this worktree's `target/debug/flpdf`, set `FLPDF_QTEST_NORMALIZE` to the repository normalization rules, and run `TESTS=xref-streams` through `vendor/qtest/bin/qtest-driver`.

Expected: `Total tests: 5`, `Passes: 5`, `Failures: 0`, exit 0.

- [ ] **Step 3: Run focused and workspace quality gates**

Run, in order:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-cli --test cli_tests show_xref_prints_qpdf_effective_table
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: every command exits 0.

- [ ] **Step 4: Read back the implementation diff and tracker state**

Confirm `git diff --check`, `git status --short --branch`, `bd show flpdf-egzr.10`, and `bd dep cycles` are clean/acyclic. Keep the issue open until the qtest and full quality evidence is recorded; only then append the verification note and close/push according to the repository session protocol.
