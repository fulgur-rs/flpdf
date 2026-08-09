# ObjStm Eligibility QPDF Convergence Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make flpdf object-stream membership match qpdf 11.9.0 by removing the input-linearization heuristic from base eligibility and applying only output-mode exclusions.

**Architecture:** Keep qpdf's graph traversal and base object eligibility independent of input linearization. Preserve encryption identity as the only context-dependent base exclusion. Apply the output-linearized and output-encrypted exclusions after planning, at the writer boundary that knows the output mode.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source and qpdf executable as semantic oracle, unit/integration tests, cargo fmt/test/clippy.

## Global constraints

- Work only in the dedicated worktree for flpdf-25kg.6.4; preserve main and unrelated user files.
- Use qpdf 11.9.0 source and observed output as the authority.
- Follow RED then GREEN: add a focused failing regression before changing production code.
- Do not implement QPDF::isLinearized in this issue. That work is already tracked by flpdf-25kg.3.29.
- Do not add sentinel object-number rules or retain the input linearization probe under another name.
- Do not alter the separate resolver-entrypoint ownership boundary.

## Task 1: Add a regression for unreachable malformed object 1

- [x] Read the current object-stream test helpers and planner call path immediately before editing.
- [x] Add a test-only PDF builder variant that allows a trailer root other than object 1 while retaining explicit xref offsets.
- [x] Add a RED test named eligibility_context_ignores_unreachable_malformed_object_one. The fixture contains malformed object 1 and a valid reachable catalog/page tree.
- [x] Run the exact focused test:

    cargo test -p flpdf --lib writer::object_streams::tests::eligibility_context_ignores_unreachable_malformed_object_one -- --exact

- [x] Record the expected failure: current eligibility construction eagerly probes the input linearization hint and attempts to parse unreachable object 1.

## Task 2: Remove the input-linearization probe from base eligibility

- [x] Reduce EligibilityContext to the encryption identity required by qpdf's base eligibility logic.
- [x] Remove linearized_hint_ref construction and the corresponding eligibility branch.
- [x] Update every context literal and test helper.
- [x] Remove the test that treats the linearization parameter dictionary as intrinsically ineligible; retain coverage for encryption identity and ordinary objects.
- [x] Remove body-validation logic that classifies a linearization parameter dictionary separately.
- [x] Run the RED regression and the focused object-stream tests until they pass.

## Task 3: Apply output-mode exclusions at the writer boundary

- [x] Add focused tests for a planned batch containing catalog, page dictionary, and an ordinary child:
  - non-linearized and non-encrypted output removes neither;
  - encrypted output removes catalog only;
  - linearized output removes page dictionaries and catalog;
  - empty batches are discarded.
- [x] Implement a small output-mode post-filter that:
  - enumerates page dictionaries only when the output is linearized;
  - removes the root catalog when output is linearized or encrypted;
  - preserves the order of remaining members;
  - does not inspect input linearization state.
- [x] Apply the filter to the legacy full-rewrite plan after planning and before object-stream container allocation, passing output_linearized false and output_encrypted from the output encryption option.
- [x] Verify the existing linearized planner/writer exclusions remain driven by output linearization mode and do not reintroduce an input probe.
- [x] Run object-stream, plain-writer, and linearization-focused tests.

## Task 4: Exercise the end-to-end writer and qpdf oracle

- [x] Add or extend a writer regression that rewrites the malformed-object-1 fixture with generated object streams and static IDs, then verifies the output is produced and structurally readable.
- [x] Run qpdf --check against generated plain, encrypted, and linearized outputs.
- [x] Compare representative object-stream membership against qpdf 11.9.0 for reachable ordinary objects, page dictionaries, catalog dictionaries, encryption dictionaries, and malformed unreachable object 1.
- [x] Confirm the expected output-mode matrix with qpdf-derived assertions rather than relying only on internal helper tests.

## Task 5: Documentation, verification, and handoff

- [x] Update the qpdf correspondence documentation if it still describes the removed input-linearization probe as part of eligibility.
- [x] Run cargo fmt --all -- --check.
- [x] Run focused tests for object streams, plain writing, linearization, writer integration, and xref behavior.
- [x] Run the full flpdf library and integration test suites.
- [ ] Run the full workspace test suite and the repository's required clippy/doc/coverage checks as applicable.
- [ ] Inspect the diff for scope, oracle citations, and absence of unrelated changes.
- [ ] Commit the implementation and tests on the dedicated branch.
- [ ] Read back flpdf-25kg.6.4 and its dependency graph, run bd dep cycles, and push Beads state with bd dolt push.
- [ ] Push the implementation branch and report the exact branch, commit, verification results, and that flpdf-25kg.3.29 was reused rather than duplicated.
