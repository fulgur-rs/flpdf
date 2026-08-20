# qpdf_time FFI Boundary Plan

### Task 1: Audit

- [x] Read the parent qpdf_time implementation and identify the file-wide
  unsafe-code allowance.
- [x] Confirm the parent source/live behavior remains unchanged.
- [x] Create this design and plan.

### Task 2: Narrow the boundary

- [x] Remove the file-wide allow(unsafe_code).
- [x] Isolate Unix and Windows unsafe acquisition in platform modules.
- [x] Run qpdf_time tests and all-features clippy.

### Task 3: Verify and hand off

- [ ] Run fmt, strict rustdoc, workspace tests, qpdf-doc checks, deviation
  checks, and fresh patch coverage.
- [ ] Rebase the stacked branch onto the current parent PR head, push, and open
  a Draft PR without merging.
- [ ] Mark ready only after CI is green; record Beads evidence and push Dolt.
