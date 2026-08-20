# qpdf Attachment Provider Error Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove the extra attachment preflight open while keeping qpdf-style
path-open failures.

### Task 1: Source-backed design

- [x] Read qpdf 11.9.0 provider/FIFO responsibility and run the bounded live
  connection-count probe.
- [x] Create the design and implementation plan documents.
- [ ] Commit the design baseline after `cargo check -p flpdf`.

### Task 2: RED regressions

- [ ] Add a regression for provider-layer path-open errors retaining qpdf's
  missing-file/permission diagnostics.
- [ ] Add a Unix FIFO regression proving two producer connections suffice.
- [ ] Run the focused tests and record the preflight failure before the fix.

### Task 3: Implement the provider boundary

- [ ] Move qpdf-style open-error mapping into the path provider callback.
- [ ] Remove the job-level preflight `File::open`.
- [ ] Run focused GREEN tests and the bounded FIFO probe.

### Task 4: Verify and hand off

- [ ] Run fmt, strict rustdoc, all-features clippy, relevant/full tests,
  qpdf-doc checks, deviation checks, and fresh patch coverage.
- [ ] Rebase onto latest `origin/main`, rerun focused checks/coverage, push, and
  create a Draft PR without merging.
- [ ] Mark the PR ready only after all CI and patch coverage checks are green.
- [ ] Append implementation/PR/verification evidence to Beads, run
  `bd dep cycles`, and confirm `bd dolt push` says `Push complete.`.
