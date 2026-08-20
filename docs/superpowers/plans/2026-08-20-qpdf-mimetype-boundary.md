# qpdf Attachment Mimetype Boundary Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move attachment mimetype validation to the qpdf-shaped CLI parser
boundary while keeping the library attachment consumer raw and composable.

### Task 1: Establish the source-backed design

- [x] Read qpdf 11.9.0 source and run the invalid-mimetype live probe.
- [x] Create the design and implementation plan documents.
- [x] Commit the design baseline after `cargo check -p flpdf`.

### Task 2: Write RED tests

- [x] Add a direct library test proving `textplain` reaches the embedded-file
  subtype setter instead of being rejected by `add_attachments`.
- [x] Add a CLI parser test proving `--mimetype=textplain` is rejected with
  qpdf's exact primary diagnostic.
- [x] Run the focused tests and record the expected failure before the fix.

### Task 3: Implement the boundary correction

- [x] Validate mimetype slash presence in `parse_add_attachment_segment`.
- [x] Remove mimetype validation from `QPDFJob::add_attachments`.
- [x] Update the direct-library regression test and retain CLI error coverage.
- [x] Run focused GREEN tests and the invalid-mimetype live probe.

### Task 4: Verify, rebase, and hand off

- [ ] Run fmt, strict rustdoc, all-features clippy, relevant/full tests,
  qpdf-doc checks, deviation checks, and fresh patch coverage.
- [ ] Rebase onto latest `origin/main`, rerun focused checks/coverage, push, and
  create a Draft PR without merging.
- [ ] Mark the PR ready only after all CI and patch coverage checks are green.
- [ ] Append implementation/PR/verification evidence to Beads, run
  `bd dep cycles`, and confirm `bd dolt push` says `Push complete.`.
