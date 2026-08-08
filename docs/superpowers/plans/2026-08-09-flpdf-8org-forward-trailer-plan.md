# flpdf-8org Forward Trailer Recovery

> For agentic workers: REQUIRED SUB-SKILL: Use TDD and verify behavior against qpdf 11.9.0.

**Goal:** Make damaged-file trailer recovery match qpdf's forward, first-valid-dictionary-wins scan, including continuing past malformed or non-dictionary `trailer` candidates.

**Architecture:** Unify reconstructed object-entry and trailer discovery in one bounded line scan. Preserve the existing `fallback_trailer` gate and xref-stream candidate fallback; only the first successfully parsed dictionary from the scan becomes the recovered trailer. Return both results from the scanner, while the resolver consumes only its entries.

**Tech Stack:** Rust workspace, `flpdf` integration tests, qpdf 11.9.0 source and executable as behavioral oracle.

---

### Task 1: Add regression coverage

**Files:** `crates/flpdf/tests/xref_tests.rs`

- Add a fixture with a valid trailer followed by an invalid/non-dictionary trailer and assert the first valid trailer and recovered object entry are retained.
- Add the inverse malformed-first/valid-later case and assert scanning continues to the later valid dictionary.
- Run the focused tests before implementation and record the expected RED failure.

### Task 2: Replace the independent trailer search

**Files:** `crates/flpdf/src/xref.rs`

- Return the reconstructed entries together with the first valid trailer discovered during the same forward line scan.
- Ignore malformed or non-dictionary candidates and continue scanning, while preserving the existing fallback-trailer and xref-stream-candidate precedence.
- Update the resolver consumer to pass the trailer-capture flag explicitly and use only the returned entries; remove the raw last-substring trailer search.

### Task 3: Verify and hand off

- Run focused xref tests, the `flpdf` crate tests, formatting, workspace tests, and applicable lint/doc checks.
- Recheck the behavior against qpdf 11.9.0, commit the focused branch, push Beads state and the feature branch, and report exact verification results.
