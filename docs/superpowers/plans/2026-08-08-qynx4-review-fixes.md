# qynx.4 Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the two qpdf-confirmed PR #677 output-routing gaps while preserving qpdf's nonfatal standard-output terminal behavior, then reply to every unresolved review thread with oracle evidence.

**Architecture:** Page inspection writes each logical line directly to an injected `QPDFLogger`. Linearization keeps its existing public entry point and adds a writer-owned sibling that emits qpdf-shaped pass-1 bytes to a requested path before the final document is returned to the CLI.

**Tech Stack:** Rust workspace, `QPDFLogger`/`Pipeline`, flpdf linearization writer, `assert_cmd`, pinned qpdf 11.9.0, GitHub REST/GraphQL through `gh`.

## Global Constraints

- Pinned qpdf 11.9.0 source and `/usr/bin/qpdf` are the semantic oracle.
- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-qynx.4-qpdf-logger` on `feature/flpdf-qynx.4-cli-output-routing`.
- Preserve the existing `write_linearized` API and avoid extra pass-1 work for callers that do not request it.
- Do not propagate `PlOStream` terminal write failures: qpdf `Pl_OStream.cc:22-34` and the `/dev/full` probe both establish exit 0.
- Do not implement pass 1 as a CLI copy of the final PDF and do not reject pass1 plus stdout.
- Use RED -> GREEN TDD for each production behavior.
- Reply to all three PR #677 inline threads and read replies back; do not resolve them.

---

### Task 1: Stream page descriptions through `QPDFLogger`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:5023-5049`
- Test: `crates/flpdf-cli/src/main.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Pdf<R>`, `QPDFLogger::info`, existing `object_to_pdf` formatting.
- Produces: `fn write_page_descriptions<R: Read + Seek>(pdf: &mut Pdf<R>, logger: &QPDFLogger) -> CliResult<()>`.

- [ ] **Step 1: Write the failing incremental-output test**

Add a test-only pipeline that stores each write as a separate chunk, then add:

```rust
#[test]
fn show_pages_writes_each_logical_line_incrementally() {
    let chunks = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(ChunkRecordingSink {
        chunks: Arc::clone(&chunks),
    })));
    let mut pdf = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    )
    .unwrap();

    write_page_descriptions(&mut pdf, &logger).unwrap();

    let chunks = chunks.lock().unwrap();
    assert_eq!(chunks.len(), 5);
    assert_eq!(
        chunks.concat(),
        b"page 1: 3 0 R\n\
          media-box: [ 0 0 612 792 ]\n\
          resources: << /Font 1 0 R /ProcSet [ /PDF /Text /ImageB /ImageC /ImageI ] >>\n\
          contents: 7 0 R\n\
          rotate: 0\n"
    );
}
```

`ChunkRecordingSink::write` must push `data.to_vec()` and its `finish` must return `Ok(())`.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf-cli --bin flpdf tests::show_pages_writes_each_logical_line_incrementally -- --exact
```

Expected: compilation fails because `write_page_descriptions` does not exist.

- [ ] **Step 3: Implement incremental logger writes**

Move the existing loop into the declared helper. Replace every `output.push_str(&format!(...))` with `logger.info(format!(...))?`. The wrapper becomes:

```rust
fn run_show_pages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    write_page_descriptions(&mut pdf, &cli_logger())
}
```

No complete-document `String` may remain.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --bin flpdf tests::show_pages_writes_each_logical_line_incrementally -- --exact
cargo test -p flpdf-cli --test cli_logger_routing
```

Expected: both commands pass and the `pages` output bytes remain unchanged.

- [ ] **Step 5: Commit Task 1**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "fix(cli): stream page descriptions through logger"
```

---

### Task 2: Add writer-owned linearization pass-1 output

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs:3005-4370`
- Modify: `crates/flpdf/src/linearization/mod.rs:39`
- Test: `crates/flpdf/src/linearization/writer.rs` (`tests` module)

**Interfaces:**
- Consumes: the existing `do_write_pass(..., pass1_digest = true, ...)` representation and final hint-stream length.
- Produces: `pub fn write_linearized_with_pass1_file<R: Read + Seek>(plan: &LinearizationPlan, renumber: &RenumberMap, pdf: &mut Pdf<R>, options: &WriteOptions, pass1_path: &Path) -> Result<LinearizedDocument>`.
- Preserves: the exact existing `write_linearized(...) -> Result<LinearizedDocument>` signature.

- [ ] **Step 1: Write the failing core pass-1 tests**

Add a test that builds a plan from `open_tiny_pdf`, calls the missing sibling API, back-patches the final document, and asserts:

```rust
let pass1 = std::fs::read(&pass1_path).unwrap();
assert!(pass1.starts_with(b"%PDF-"));
assert_ne!(pass1, document.bytes);
assert!(pass1.windows(b"% hint_offset=".len()).any(|w| w == b"% hint_offset="));
assert!(pass1.windows(b"% hint_length=".len()).any(|w| w == b"% hint_length="));
assert!(pass1.ends_with(b"% second_xref_end=0\n"));
```

Run the same helper with `ObjectStreamMode::Generate` and assert the parsed `second_xref_end` comment is positive, covering both qpdf branches.

- [ ] **Step 2: Run the core tests and verify RED**

Run:

```bash
cargo test -p flpdf linearization::writer::tests::write_linearized_with_pass1_file
```

Expected: compilation fails because `write_linearized_with_pass1_file` is not defined.

- [ ] **Step 3: Add the public sibling API and shared implementation**

Export the new function from `linearization/mod.rs`. Keep `write_linearized` as a wrapper and move its current body into a private implementation:

```rust
pub fn write_linearized<R: Read + Seek>(...) -> Result<LinearizedDocument> {
    write_linearized_impl(plan, renumber, pdf, options, None)
}

pub fn write_linearized_with_pass1_file<R: Read + Seek>(
    ...,
    pass1_path: &Path,
) -> Result<LinearizedDocument> {
    write_linearized_impl(plan, renumber, pdf, options, Some(pass1_path))
}
```

Import `std::path::Path` in `writer.rs` for the new public parameter.

Generate the first-pass tuple once when either deterministic ID computation or a pass-1 file needs it. Reuse its bytes for deterministic-ID hashing instead of invoking `do_write_pass` twice.

- [ ] **Step 4: Serialize the qpdf-shaped pass-1 file**

After final convergence, but before returning `LinearizedDocument`, append these lines to the cached pass-1 bytes and write them with `std::fs::write(pass1_path, bytes)?`:

```text
% hint_offset={pass1 hint-stream slot offset}
% hint_length={final hint-stream object length}
% second_xref_offset={pass1 main-xref offset}
% second_xref_end={0 for classic xref; byte offset of trailing startxref for xref stream}
```

The pass-1 tuple already returns the hint slot as item 3 and main-xref offset as item 6. For the xref-stream case, find the final `startxref\n` marker in the pass-1 bytes; its index is qpdf's `second_xref_end`. Treat absence as an internal invariant error. File I/O propagates as `crate::Error::Io` before the document reaches the CLI.

- [ ] **Step 5: Run the core tests and verify GREEN**

Run:

```bash
cargo test -p flpdf linearization::writer::tests::write_linearized_with_pass1_file
cargo test -p flpdf linearization::writer::tests
```

Expected: classic and generated-object-stream pass-1 tests pass; existing linearization tests remain green.

- [ ] **Step 6: Commit Task 2**

```bash
git add crates/flpdf/src/linearization/writer.rs crates/flpdf/src/linearization/mod.rs
git commit -m "feat(linearization): expose writer-owned pass1 output"
```

---

### Task 3: Route CLI `--linearize-pass1` through the core writer

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:30-38,362-367,1735-1780,3054-3170`
- Modify: `crates/flpdf-cli/tests/cli_logger_routing.rs`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs:800-830,930-970`

**Interfaces:**
- Consumes: `write_linearized_with_pass1_file(..., pass1_path)` from Task 2.
- Produces: successful `--linearize --linearize-pass1=PATH INPUT -` with final PDF on stdout and an independently serialized pass-1 file.

- [ ] **Step 1: Write the failing stdout plus pass-1 CLI regression**

Add to `cli_logger_routing.rs`:

```rust
#[test]
fn binary_linearized_pdf_dash_writes_pass1_independently() {
    let directory = tempfile::tempdir().unwrap();
    let pass1 = directory.path().join("pass1.pdf");
    let output = flpdf()
        .args([
            "--linearize",
            &format!("--linearize-pass1={}", pass1.display()),
            ONE_PAGE,
            "-",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
    let pass1_bytes = std::fs::read(pass1).unwrap();
    assert!(pass1_bytes.starts_with(b"%PDF-"));
    assert_ne!(pass1_bytes, output.stdout);
    assert!(pass1_bytes.windows(b"% hint_offset=".len()).any(|w| w == b"% hint_offset="));
}
```

Use a local `String` variable for the formatted argument if Rust rejects the temporary reference in the array.

- [ ] **Step 2: Run the CLI regression and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test cli_logger_routing binary_linearized_pdf_dash_writes_pass1_independently -- --exact
```

Expected: FAIL with exit 2 and `failed to write --linearize-pass1 file: No such file or directory`.

- [ ] **Step 3: Wire the pass-1 path into `run_rewrite`**

Import `write_linearized_with_pass1_file`. Add `linearize_pass1: Option<&Path>` next to the `linearize` argument of `run_rewrite`; pass `args.linearize_pass1.as_deref()` only from the top-level linearize call and `None` from the other two callers. In the linearize branch select:

```rust
let mut doc = match linearize_pass1 {
    Some(path) => write_linearized_with_pass1_file(&plan, &renumber, &mut pdf2, &options, path)?,
    None => write_linearized(&plan, &renumber, &mut pdf2, &options)?,
};
```

Delete the post-`run_rewrite` `std::fs::copy(output, pass1)` block and update the flag documentation so it no longer claims the final file is copied.

- [ ] **Step 4: Update stale pass-1 assertions**

Change `top_level_linearize_normalize_content_warning_writes_pass1_copy` to assert that warning exit 3 retains both outputs, pass 1 starts with `%PDF-`, contains `% hint_offset=`, and differs from the final output. Keep the existing upstream-shaped static-ID test and add the same independence assertion there.

- [ ] **Step 5: Run CLI tests and qpdf probe for GREEN**

Run:

```bash
cargo test -p flpdf-cli --test cli_logger_routing
cargo test -p flpdf-cli --test cli_tests top_level_linearize
```

Then run qpdf and flpdf on `tests/fixtures/compat/one-page.pdf` with `--linearize-pass1=PATH ... -`. Record exit status 0, empty stderr, a `%PDF-` stdout prefix, and a distinct pass-1 file for both implementations.

- [ ] **Step 6: Commit Task 3**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_logger_routing.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "fix(cli): preserve linearization pass1 with stdout"
```

---

### Task 4: Verify, publish, and reply to review threads

**Files:**
- Modify only if verification reveals a scoped defect.
- Beads: `flpdf-qynx.4` implementation notes.
- GitHub: PR #677 inline replies only.

**Interfaces:**
- Consumes: Tasks 1-3 commits and the three existing inline comment IDs.
- Produces: pushed Layer 3 head, verified CI, three read-back inline replies, unresolved threads.

- [ ] **Step 1: Run local quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test qpdf_logger_tests --test pdf_logger_tests
cargo test -p flpdf-cli --test cli_logger_routing --test cli_tests
cargo test --workspace
python3 scripts/qpdf-module-docs.py --check
git diff --check
scripts/patch-coverage.sh --base feature/flpdf-qynx.4-document-warnings
scripts/patch-coverage.sh --base main --lcov target/patch-cov.lcov
```

Expected: all commands pass and both changed-line coverage views report 100%.

- [ ] **Step 2: Push and verify GitHub checks**

```bash
git push origin feature/flpdf-qynx.4-cli-output-routing
gh pr view 677 --json headRefOid,url,state
gh pr checks 677 --watch --interval 10
```

Do not merge. Confirm the remote head equals local HEAD and Windows passes.

- [ ] **Step 3: Reply to all three inline threads**

Fetch REST comment IDs and map them to node IDs `PRRC_kwDOSYPosM7ewkBQ`, `PRRC_kwDOSYPosM7ewkBZ`, and `PRRC_kwDOSYPosM7ewkBh`. POST replies to `repos/fulgur-rs/flpdf/pulls/677/comments/{numeric_id}/replies`:

- stdout failure: classify oracle mismatch; cite `QPDFLogger.cc:43-51`, `Pl_OStream.cc:22-34`, and qpdf/flpdf `/dev/full` exit 0; state no semantic change.
- pages: classify oracle match; cite `QPDFJob.cc:843-874`; state that each logical line now reaches the info pipeline incrementally; include focused test results.
- pass1: classify oracle match; cite `QPDFJob.cc:2907-2909,3039-3054` and `QPDFWriter.cc:2661-2668,2886-2900`; state that core owns real pass-1 output and stdout succeeds; include qpdf/flpdf probe and test results.

- [ ] **Step 4: Read replies and thread state back**

Run the bundled `fetch_comments.py` and confirm each thread contains the new reply, remains `isResolved: false`, and is not accidentally duplicated.

- [ ] **Step 5: Update and close Beads after green CI**

Append the final commit, PR URLs, oracle classifications, test counts, qpdf probes, coverage, CI, and review reply evidence to `flpdf-qynx.4`. Then:

```bash
bd close flpdf-qynx.4 --reason="Implemented qpdf-compatible logger routing and verified the stacked PRs"
bd dolt push
bd show flpdf-qynx.4
git status --short --branch
```

Keep the worktree and branches for review; do not resolve GitHub threads or merge PRs.
