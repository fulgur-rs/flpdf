# flpdf-5p6h.1: Xref Tombstone Lifetime Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make flpdf's deleted-object bookkeeping match qpdf 11.9.0 by keeping number-wide free-row suppression inside xref registration/recovery and removing the persistent mutation-time tombstone from both object replacement routes.

**Architecture:** `XrefRegistration` remains the owner of transient `deleted_objects` during xref parsing and each recovery merge. `LoadedXrefState` and `ResolverCore` no longer transport that set across the xref/resolver ownership boundary. `Pdf::set_object`, `Pdf::replace_object_handle`, and object removal update canonical cache/xref state without mutating xref-parser bookkeeping. Rust regression tests and a pinned qpdf C++ probe exercise the same malformed-input, removal, generation-replacement, xref enumeration, and handle-minting observations.

**Tech Stack:** Rust workspace (`flpdf`), qpdf 11.9.0 C++ oracle, shell probe wrapper, cargo test/clippy/doc, `cargo-llvm-cov` changed-line coverage, Beads, and stacked Draft PR workflow.

## Global Constraints

- Use `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` and `/usr/bin/qpdf` 11.9.0 as the behavioral oracle; do not infer a replacement outcome from the Rust design document alone.
- Preserve the approved responsibility boundary in `docs/superpowers/specs/2026-08-14-flpdf-5p6h-1-tombstone-lifetime-design.md`; do not add a persistent-number or generation-specific compatibility bridge.
- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-5p6h-1-tombstone-lifetime`; leave root `main` and its untracked design documents untouched.
- Follow RED-to-GREEN TDD: add or modify the failing regression first, run it and capture the failure, then make the smallest production change that passes it.
- Keep `XrefRegistration`'s existing free-row, exact-generation, `/Size`, compressed-entry, and xref-stream candidate behavior intact; this plan does not change writer behavior or unrelated consumer cutovers.
- The PR stays Draft until required CI checks are green; do not merge it.

---

## Task 1: Establish the qpdf oracle contract and add RED regressions

**Files:** `tests/oracle/qpdf_tombstone_lifetime_probe.cc`, `scripts/qpdf-tombstone-lifetime-probe.sh`, `crates/flpdf/src/xref.rs`, `crates/flpdf/src/reader/resolver.rs`, `crates/flpdf/src/reader.rs`.

- [ ] Add `tests/oracle/qpdf_tombstone_lifetime_probe.cc` using qpdf's public C++ API (`processMemoryFile`, `getXRefTable`, `getAllObjects`, `getObject`, `removeObject`, and `replaceObject`) to print stable, machine-readable observations for:
  - a free xref row followed by another generation of the same object number during one xref registration;
  - removing `3 0`, forcing the damaged-file recovery path, and recording whether the stale body is re-registered;
  - replacing `3 0` with `3 1` and recording the effective xref keys, `getAllObjects` keys, and whether `getObject(3, 1)` is initialized after recovery.
- [ ] Add `scripts/qpdf-tombstone-lifetime-probe.sh` following the existing pinned-probe safeguards: resolve `fetch-qpdf-source.sh --print-path`, verify the exact qpdf commit and clean tracked tree, build only `libqpdf` in a validated `/tmp` directory, compile the probe with the pinned headers/library, validate `ldd` resolves the pinned shared library, and run it with the pinned `LD_LIBRARY_PATH`.
- [ ] Run the probe and `/usr/bin/qpdf` on the same in-memory/file fixture, record the observed qpdf rows in the probe's assertions, and make the probe fail if the source or runtime oracle drifts.
- [ ] Convert `reconstruction_does_not_reintroduce_a_removed_unindexed_object` into a qpdf-shaped regression whose name and assertions describe the observed recovery result; retain the existing fixture `synthetic_mismatch_discovers_unindexed_object_pdf()` and force recovery through the same resolver entrypoint.
- [ ] Add resolver tests named `set_object_generation_replacement_matches_qpdf_tombstone_lifetime` and `replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime`. Each test must cover removal, same-generation replacement, different-generation replacement (`3 0` → `3 1`), recovery, `get_xref_table`, `get_all_objects`, and `get_object_handle`/resolver minting, with assertions taken from the oracle probe.
- [ ] Add the direct xref test `xref_registration_free_object_suppression_is_local_to_registration` beside `xref_registration_free_object_suppresses_later_generations`; assert that the free row suppresses the later row in that registration while a fresh registration has no inherited tombstone.
- [ ] Run the focused RED commands and save their failure evidence before editing production code:
  - `cargo test -p flpdf --lib reconstruction_does_not_reintroduce_a_removed_unindexed_object -- --exact`
  - `cargo test -p flpdf --lib set_object_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact`
  - `cargo test -p flpdf --lib replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact`
  - `cargo test -p flpdf --lib xref_registration_free_object_suppression_is_local_to_registration -- --exact`
  - `scripts/qpdf-tombstone-lifetime-probe.sh`

## Task 2: Remove the persistent tombstone from the canonical resolver boundary

**Files:** `crates/flpdf/src/xref.rs`, `crates/flpdf/src/engine.rs`, `crates/flpdf/src/reader/resolver.rs`.

- [ ] Remove `LoadedXrefState::deleted_objects` as a long-lived resolver-construction field and remove the corresponding `ResolverHandle::new_shared` argument and `ResolverCore::deleted_object_numbers` field; update every test-only constructor call to use the reduced signature.
- [ ] Keep `XrefRegistration::deleted_objects` private and local. Preserve its use in `insert_xref_entry`, `insert_free_xref_entry`, `/Size` warning calculation, xref-stream candidate re-entry, and recovered-entry filtering while the current registration/recovery merge is active.
- [ ] Change `load_xref_state_with_options` and recovery merge helpers so they snapshot their effective entries and diagnostics before clearing the local set, then discard the set at the xref-loader boundary instead of handing it to `engine.rs`.
- [ ] Refactor `reconstruct_xref_and_retry` so it consumes the already-filtered recovery result and never filters recovered entries against resolver mutation history. A later recovery operation must create and consume its own `XrefRegistration` tombstone set.
- [ ] Remove `mark_deleted_object_number`, `clear_deleted_object_number`, and the associated long-lived-set comments if they have no remaining caller; retain `qpdf_removed_refs` only where it represents its separate canonical removal responsibility.
- [ ] Update qpdf correspondence comments at every changed source boundary to cite `QPDF.cc:516-575`, `QPDF.cc:686-708`, and `QPDF.cc:1187-1210`, explicitly distinguishing transient xref registration state from `replaceObject`/`removeObject` cache mutation.
- [ ] Run the four Task 1 focused tests after the resolver refactor; keep the test suite RED until the public mutation routes are changed in Task 3.

## Task 3: Cut both object mutation routes over to canonical cache/xref behavior

**Files:** `crates/flpdf/src/reader.rs`, `crates/flpdf/src/reader/resolver.rs`, `crates/flpdf/src/reader.rs` tests.

- [ ] In `Pdf::set_object`, remove only the call that clears xref-parser tombstone state; preserve legacy payload lifting, canonical handle replacement, source-description cleanup, dirty propagation, and `qpdf_removed_refs` behavior.
- [ ] In `Pdf::replace_object_handle`, remove only the call that clears xref-parser tombstone state; preserve direct/foreign/indirect validation and canonical `ObjectHandle` identity/update behavior.
- [ ] In `remove_object_preserving_handle`, stop inserting the object number into a resolver-wide tombstone; retain source/default xref removal, cache invalidation, outstanding-handle missing state, and canonical removal bookkeeping required by `QPDF::removeObject`'s cache/xref responsibility.
- [ ] Make the Task 1 replacement matrix pass for both routes, including same-generation replacement and different-generation `3 0` → `3 1` replacement followed by recovery and resolver handle creation.
- [ ] Keep existing `get_all_objects_excludes_deleted_objects_from_xref_and_canonical_snapshots` and any `qpdf_removed_refs` assertions green; if an assertion depended specifically on the removed persistent tombstone, rewrite it to assert the canonical xref/cache contract instead of preserving the old hardening policy.
- [ ] Run focused GREEN checks:
  - `cargo test -p flpdf --lib reconstruction_does_not_reintroduce_a_removed_unindexed_object -- --exact`
  - `cargo test -p flpdf --lib set_object_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact`
  - `cargo test -p flpdf --lib replace_object_handle_generation_replacement_matches_qpdf_tombstone_lifetime -- --exact`
  - `cargo test -p flpdf --lib xref_registration_free_object_suppression_is_local_to_registration -- --exact`
  - `cargo test -p flpdf --lib xref_registration_free_object_suppresses_later_generations -- --exact`

## Task 4: Preserve adjacent xref regressions and update correspondence documentation

**Files:** `crates/flpdf/src/xref.rs`, `crates/flpdf/src/reader/resolver.rs`, `crates/flpdf/src/reader.rs`, `docs/qpdf-correspondence.md`.

- [ ] Run and, only when a failure is caused by the ownership-boundary change, adjust the existing free-row, exact-generation, compressed-entry, `/Size`, xref-stream candidate, loaded-free-object, and recovery-merge tests; do not weaken assertions or remove coverage.
- [ ] Add a source-near comment to the xref state types explaining that `deleted_objects` is a per-registration/per-recovery number filter and is intentionally absent from `ResolverCore` after the qpdf clear points.
- [ ] Update the `QPDF.cc` correspondence row in `docs/qpdf-correspondence.md` to describe the new split: xref registration/recovery owns temporary free-row suppression, while `Pdf` replacement/removal owns canonical cache/xref mutation and does not clear or add the xref set.
- [ ] Run `cargo fmt --all`, `cargo fmt --all -- --check`, and the focused xref/resolver/reader tests before committing the implementation.
- [ ] Commit the implementation as `fix(qpdf): align xref tombstone lifetime` after the focused suite is green.

## Task 5: Full verification, coverage, Beads, and Draft stacked PR

**Files:** repository metadata and the implementation branch only; no source scope expansion.

- [ ] Run the complete quality gates from the repository instructions:
  - `cargo test -p flpdf`
  - `cargo test` 
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" cargo doc --workspace --all-features --no-deps --document-private-items`
  - `scripts/qpdf-tombstone-lifetime-probe.sh`
  - the applicable qpdf differential/compatibility checks for the changed reader/xref path.
- [ ] Run `scripts/patch-coverage.sh --base origin/main` and do not hand off until every changed executable line is covered at 100%; add narrowly scoped tests for any uncovered branch introduced by the refactor.
- [ ] Review `git diff --check`, `git status --short`, and the complete diff against `main`; confirm no generated files, unrelated legacy changes, or root worktree files entered the branch.
- [ ] Update Beads issue `flpdf-5p6h.1` with the implementation commit, oracle probe result, focused/full test commands, and coverage result; close it only after the implementation is actually merged, otherwise leave it open/in progress according to the stack state.
- [ ] Run `bd dep cycles` and `bd dolt push`, requiring the exact `Push complete.` result.
- [ ] Check the current stacked ancestry and open-PR count with `gh-stack`/GitHub. Create the next Draft PR on the approved parent branch, include the qpdf source citations and verification summary, and keep it Draft until all required CI checks pass.
- [ ] Push the implementation branch and verify remote branch/PR state; do not merge and stop if the open-PR pause threshold of five is reached.

