# qpdf Check Object-Warning Location Plan

### Task 1: Audit

- [x] Read pinned qpdf QPDFExc/objectWarning source and run the live check
  differential.
- [x] Confirm current ObjectHandle construction and job/check emission route.
- [x] Create this design and plan.

### Task 2: RED/GREEN

- [x] Add a job-check unit regression for contextless object descriptions.
- [x] Add/strengthen the CLI qpdf differential assertion.
- [x] Implement the smallest emitter classification and run focused GREEN
  tests.

### Task 3: Verify and hand off

- [ ] Run fmt, strict rustdoc, all-features clippy, workspace tests, qpdf-doc
  checks, deviation checks, and fresh patch coverage.
- [ ] Rebase latest origin/main, push, create Draft PR, and mark ready only
  after every CI check is green.
- [ ] Record Beads implementation/PR/verification evidence, run `bd dep cycles`,
  and confirm `bd dolt push` says `Push complete.`.
