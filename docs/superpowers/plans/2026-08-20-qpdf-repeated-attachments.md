# qpdf Repeated Attachment Segment Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Preserve repeated qpdf attachment segments and route them through
one flpdf batch attachment job.

### Task 1: Source-backed design

- [x] Read qpdf 11.9.0 repeated attachment source paths and run the live probe.
- [x] Create the design and implementation plan documents.
- [ ] Commit the design baseline after `cargo check -p flpdf-cli`.

### Task 2: RED regression

- [ ] Add an end-to-end CLI test with two terminated attachment segments and
  distinct keys.
- [ ] Add raw-segment extraction tests for one and two groups.
- [ ] Run the focused tests and record the current flattening failure.

### Task 3: Implement batch routing

- [ ] Extract repeated attachment groups from raw argv while retaining a valid
  clap dispatch marker.
- [ ] Parse every group and call one `QPDFJob::add_attachments` batch.
- [ ] Preserve singular syntax, ordering, duplicate aggregation, and output
  warning behavior.
- [ ] Run focused GREEN tests and the repeated-segment live probe.

### Task 4: Verify and hand off

- [ ] Run fmt, strict rustdoc, all-features clippy, relevant/full tests,
  qpdf-doc checks, deviation checks, and fresh patch coverage.
- [ ] Rebase onto latest `origin/main`, rerun focused checks/coverage, push, and
  create a Draft PR without merging.
- [ ] Mark the PR ready only after all CI and patch coverage checks are green.
- [ ] Append implementation/PR/verification evidence to Beads, run
  `bd dep cycles`, and confirm `bd dolt push` says `Push complete.`.
