# PR #672 Warning Failure Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make PR #672 deliver recovery warnings before terminal open failures, propagate logger infrastructure failures through check APIs, and answer the stream-cleanup review with qpdf evidence without changing matching semantics.

**Architecture:** `ResolverWarningOptions` remains the single owner of warning formatting, suppression, and logger delivery. `Pdf::open_with_repair_mode` selects that policy before xref loading and uses it on both successful and terminal-failure paths. The check layer propagates qpdf runtime/logic categories while continuing to convert malformed-input failures into `CheckReport` diagnostics.

**Tech Stack:** Rust workspace, qpdf 11.9.0 pinned source, GitHub GraphQL review threads, Beads.

## Global Constraints

- Pinned qpdf 11.9.0 source and observable behavior are authoritative.
- Do not duplicate warning formatting in `engine.rs`.
- Do not expose `PipelineError` across the core API boundary; use crate `Error` categories.
- Do not change stream-pipeline cleanup after warning delivery failure because qpdf unwinds before the common finish tail.
- Use RED→GREEN TDD for production changes.
- Push only `feature/flpdf-qynx.4-document-warnings`.
- Reply once in each original inline thread; do not resolve, merge, or clean up branches/worktrees.

---

### Task 1: Route repair warnings on terminal open failure

**Files:**
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/engine.rs`
- Test: `crates/flpdf/tests/pdf_logger_tests.rs`

**Interfaces:**
- Consumes: `PdfOpenOptions::{logger,suppress_warnings,description}`, `Error::open_failure()`, `Diagnostics`.
- Produces: `ResolverWarningOptions::replay_warnings(&self, diagnostics: &Diagnostics) -> Result<()>`; the same formatting path is used before and after resolver construction.

- [ ] **Step 1: Add the terminal-repair fixture and failing tests**

Add this fixture builder beside `warnings_only_corrupt_xref_bytes`:

```rust
fn terminal_repair_failure_bytes() -> (Vec<u8>, usize) {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"zref\n0 2\n0000000000 65535 f \n");
    pdf.extend_from_slice(
        format!(
            "traile_\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    (pdf, xref_start)
}
```

Add two tests using the real open path:

```rust
#[test]
fn terminal_open_failure_delivers_accumulated_repair_warnings_first() {
    let (logger, output) = recording_logger();
    let (bytes, xref_start) = terminal_repair_failure_bytes();
    let error = match Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            description: "broken.pdf".to_owned(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("repair must still fail without a trailer keyword"),
        Err(error) => error,
    };

    assert!(error.open_failure().is_some());
    assert_eq!(
        output.lock().unwrap().as_slice(),
        format!(
            "WARNING: broken.pdf: file is damaged\n\
             WARNING: broken.pdf (offset {xref_start}): expected integer\n\
             WARNING: broken.pdf: Attempting to reconstruct cross-reference table\n"
        )
        .as_bytes()
    );
}

#[test]
fn terminal_open_failure_returns_warning_delivery_failure() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let (bytes, _) = terminal_repair_failure_bytes();

    assert!(matches!(
        Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
                description: "broken.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}
```

- [ ] **Step 2: Run the two tests and verify RED**

Run:

```bash
cargo test -p flpdf --test pdf_logger_tests terminal_open_failure -- --nocapture
```

Expected: the recording logger remains empty, and the failing logger returns the original `OpenFailure` instead of `Error::System`.

- [ ] **Step 3: Move reusable replay behavior onto the warning policy**

In `resolver.rs`, add methods to `ResolverWarningOptions` and make the existing resolver replay delegate to the same implementation:

```rust
impl ResolverWarningOptions {
    fn route_warning(
        &self,
        offset: Option<u64>,
        message: &str,
    ) -> Result<()> {
        route_warning(
            &self.logger,
            self.suppress_warnings,
            &self.description,
            offset,
            message,
        )
    }

    pub(crate) fn replay_warnings(&self, diagnostics: &Diagnostics) -> Result<()> {
        for diagnostic in diagnostics.entries() {
            self.route_warning(diagnostic.offset, &diagnostic.message)?;
        }
        Ok(())
    }
}
```

Extract the current formatting body into one private module function:

```rust
fn route_warning(
    logger: &crate::QPDFLogger,
    suppress_warnings: bool,
    description: &str,
    offset: Option<u64>,
    message: &str,
) -> Result<()> {
    if suppress_warnings {
        return Ok(());
    }
    let positive_offset = offset.filter(|offset| *offset > 0);
    let location = match (description.is_empty(), positive_offset) {
        (false, Some(offset)) => format!("{description} (offset {offset})"),
        (false, None) => description.to_owned(),
        (true, Some(offset)) => format!("offset {offset}"),
        (true, None) => String::new(),
    };
    if location.is_empty() {
        logger.warn(format!("WARNING: {message}\n"))
    } else {
        logger.warn(format!("WARNING: {location}: {message}\n"))
    }
}
```

Keep `ResolverHandle::push_warning_with_offset` and
`ResolverHandle::replay_warnings` delegating to this same function; do not add
a second formatter.

- [ ] **Step 4: Select warning policy before xref loading**

In `engine.rs`, build `ResolverWarningOptions` before
`load_xref_state_with_repair` and handle both outcomes:

```rust
let warning_options = ResolverWarningOptions::new(
    options
        .logger
        .clone()
        .unwrap_or_else(crate::QPDFLogger::default_logger),
    options.suppress_warnings,
    options.description.clone(),
);
let loaded_state = match load_xref_state_with_repair(&mut reader, options.repair) {
    Ok(state) => state,
    Err(error) => {
        if let Some((_, diagnostics)) = error.open_failure() {
            warning_options.replay_warnings(diagnostics)?;
        }
        return Err(error);
    }
};
```

Move `warning_options` into `ResolverHandle::new_shared` and keep successful
replay exactly once.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --test pdf_logger_tests
cargo test -p flpdf --test xref_tests
```

Expected: all logger and xref tests pass; the new terminal tests prove delivery ordering and logger-error precedence.

- [ ] **Step 6: Commit Task 1**

```bash
git add crates/flpdf/src/reader/resolver.rs crates/flpdf/src/engine.rs crates/flpdf/tests/pdf_logger_tests.rs
git commit -m "fix(reader): route warnings before terminal open errors"
```

### Task 2: Propagate logger failures through check APIs

**Files:**
- Modify: `crates/flpdf/src/check.rs`
- Test: `crates/flpdf/tests/pdf_logger_tests.rs`

**Interfaces:**
- Consumes: `Pdf::open_with_options` errors after Task 1.
- Produces: repair-enabled check APIs propagate `Error::Encrypted`, `Error::System`, and `Error::Internal`; malformed input remains an invalid report.

- [ ] **Step 1: Add the failing check regression**

Import `check_reader_with_options` and add:

```rust
#[test]
fn check_with_repair_propagates_warning_delivery_failure() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let (bytes, _) = warnings_only_corrupt_xref_bytes();

    assert!(matches!(
        flpdf::check_reader_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                repair: true,
                logger: Some(logger),
                description: "check.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        ),
        Err(Error::System(ref message)) if message == "warning sink failed"
    ));
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf --test pdf_logger_tests check_with_repair_propagates_warning_delivery_failure -- --exact
```

Expected: FAIL because the current function returns `Ok(CheckReport { valid: false, ... })`.

- [ ] **Step 3: Preserve infrastructure error categories**

Change the repair-mode open match in `check_reader_inner_with_options`:

```rust
Err(error @ (Error::Encrypted(_) | Error::System(_) | Error::Internal(_))) => {
    return Err(error);
}
```

Keep the existing fallback arm for `Io`, `Parse`, `Unsupported`, `Missing`,
`OpenFailure`, and other input-facing errors. Update `check_reader`,
`check_reader_with_options`, and limits rustdoc to name runtime/logic logger
delivery failures as propagated errors.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --test pdf_logger_tests
cargo test -p flpdf --test check_tests
```

Expected: all tests pass; existing malformed-input checks still return invalid reports.

- [ ] **Step 5: Commit Task 2**

```bash
git add crates/flpdf/src/check.rs crates/flpdf/tests/pdf_logger_tests.rs
git commit -m "fix(check): propagate warning logger failures"
```

### Task 3: Verify, publish, reply, and read back

**Files:**
- No production files beyond Tasks 1-2.
- Update tracker evidence through `bd`; do not manually edit Beads files.

**Interfaces:**
- Consumes: Task 1 and Task 2 commits plus qpdf source evidence.
- Produces: pushed PR #672 head, green CI, one reply per supplied thread, and thread-aware readback with all threads unresolved.

- [ ] **Step 1: Verify the oracle-mismatch stream behavior without changing it**

Read pinned qpdf:

```bash
qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
sed -n '2494,2538p' "$qpdf_source/libqpdf/QPDF.cc"
sed -n '90,105p' "$qpdf_source/libqpdf/QPDFLogger.cc"
```

Run the existing focused resolver tests:

```bash
cargo test -p flpdf reader::resolver::tests::decoding_warning_sink_failures_propagate_from_each_warning -- --exact
cargo test -p flpdf reader::resolver::tests::a_write_failure_still_finishes_the_sink_once -- --exact
```

Record that ordinary stream failure reaches the cleanup tail, but a logger
failure thrown inside a catch exits before it.

- [ ] **Step 2: Run final local quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test pdf_logger_tests --test check_tests --test xref_tests
cargo test --workspace
python3 scripts/qpdf-module-docs.py --check
scripts/patch-coverage.sh --base feature/flpdf-qynx.4-qpdf-logger
git diff --check
```

Expected: every command exits 0 and changed-line coverage is 100% for every reported component.

- [ ] **Step 3: Push only PR #672 branch and monitor CI**

```bash
git push origin feature/flpdf-qynx.4-document-warnings
gh pr view 672 --json headRefOid,state,url
gh pr checks 672 --watch --interval 10
```

Expected: local, tracking, and PR heads match; all required jobs including Windows pass; PR remains OPEN.

- [ ] **Step 4: Reply once in each original thread**

Use `addPullRequestReviewThreadReply` with these thread IDs:

- `PRRT_kwDOSYPosM6XVMob`: oracle match; terminal-open warning routing fix, qpdf `QPDF.cc:315-318,488-530`, commit and tests.
- `PRRT_kwDOSYPosM6XVMof`: oracle match; check propagation fix, error-category rationale, commit and tests.
- `PRRT_kwDOSYPosM6XVMoj`: oracle mismatch; no semantic change, qpdf `QPDF.cc:2505-2537` and `QPDFLogger.cc:96-101`, focused verification.

Do not call `resolveReviewThread`.

- [ ] **Step 5: Read back GitHub and persist Beads evidence**

Fetch PR #672 `reviewThreads` and confirm each supplied thread contains the
original plus exactly one new reply and remains `isResolved: false`. Append the
final commits, oracle classifications, test/coverage counts, CI status, reply
IDs, and thread states to `flpdf-qynx.4`; keep its CLOSED state unchanged.

```bash
bd update flpdf-qynx.4 --append-notes 'PR #672 warning follow-up: terminal-open warnings and check logger failures fixed; stream cleanup request rejected against qpdf 11.9.0; see PR thread replies and final CI.'
bd dolt push
bd show flpdf-qynx.4
git status --short --branch
```

Expected: Beads push succeeds, the branch is clean and synchronized, and no PR
merge, thread resolution, or cleanup occurred.
