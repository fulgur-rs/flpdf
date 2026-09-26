# JSON Overlay Fx0 Stream Length Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Match qpdf 11.9.0 JSON output by materializing the destination page Form XObject `/Fx0` before serialization, so dictionary-only JSON includes its actual `/Length`.

**Architecture:** Keep the qpdf responsibility boundary in `job/overlay.rs`: after creating the destination Form XObject, read its raw bytes and replace its provider with the resulting buffer before registering `/Fx0`. Reuse `ObjectHandle` stream primitives; leave source `/Fx1` Forms and JSON serialization behavior unchanged.

**Tech Stack:** Rust workspace, `flpdf-cli` differential integration tests, qpdf 11.9.0, `serde_json`.

**Spec:** Beads issue `flpdf-60b3y`; pinned qpdf 11.9.0 at `3b97c9bd266b7c32ea36d3536e22dab77412886`; `docs/qpdf-correspondence.md` rows for `QPDFJob.cc`, `QPDFPageObjectHelper.cc`, and `QPDF_Stream::writeStreamJSON`.

## Global Constraints

- Pinned qpdf 11.9.0 source and verified live output define behavior.
- Only the destination page `/Fx0` is materialized; imported provider-backed source `/Fx1` retains qpdf's missing `/Length` behavior.
- Dictionary-only JSON retains `/Length`; inline/file JSON output removes `/Length` from its emitted dictionary as qpdf does.
- Do not synthesize `/Length` in the JSON serializer or change the shared Form XObject helper.
- Work in `.worktrees/flpdf-60b3y`; use test-first RED→GREEN.
- PR handoff requires latest-main rebase, all required CI including patch coverage green, Beads implementation record, and issue closure after Ready.

## Review Focus

- Non-empty destination content produces the exact `/Fx0 /Length` value in `--json=2` and `--json-stream-data=none` output.
- Overlay and underlay share the same destination materialization boundary.
- Provider-backed `/Fx1` remains without `/Length`.
- Default inline JSON and no-overlay JSON remain byte-identical to qpdf.

---

### Task 1: Pin and fix overlay destination Form JSON length

**Files:**
- Create: `docs/superpowers/plans/2026-09-27-flpdf-60b3y-overlay-fx0-length.md`
- Modify: `crates/flpdf-cli/tests/cli_qpdf_conflict_matrix.rs`
- Modify: `crates/flpdf/src/job/overlay.rs`

**Interfaces:**
- Consume `ObjectHandle::get_raw_stream_data() -> Result<Rc<Vec<u8>>>`.
- Consume `ObjectHandle::replace_stream_data(Rc<Vec<u8>>, None, None)` to switch `/Fx0` from provider-backed to buffer-backed and update `/Length`.
- Produce no new production API.

- [x] **Step 1: Add `top_level_json_overlay_fx0_length_matches_qpdf`.**

Use `fxo-red.pdf` as the source and non-empty `one-page.pdf` as the destination. For both `--overlay` and `--underlay`, compare full `--json=2` output and `--json-output=2 --json-stream-data=none` output against qpdf. Parse the dictionary-only JSON by following the page's `/Resources /XObject` references; assert the referenced `/Fx0` stream has `/Length 89` and `/Fx1` has no `/Length`. Also compare default inline output and no-overlay `--json=2` output against qpdf.

- [x] **Step 2: Run the new test and verify RED.**

Run: `cargo test -p flpdf-cli --test cli_qpdf_conflict_matrix top_level_json_overlay_fx0_length_matches_qpdf -- --exact`

Expected: fail because qpdf reports `/Fx0 /Length` 89 while flpdf omits it; `/Fx1` remains without `/Length` on both sides.

- [x] **Step 3: Materialize `/Fx0` in the overlay consumer.**

In `apply_overlays_to_page`, immediately after `get_form_xobject_for_page`, obtain the destination-owned handle, call `get_raw_stream_data`, then `replace_stream_data(data, None, None)` before adding `/Fx0` to the resource entries. Propagate the raw-read error with `?`.

- [x] **Step 4: Run the new test and verify GREEN.**

Run: `cargo test -p flpdf-cli --test cli_qpdf_conflict_matrix top_level_json_overlay_fx0_length_matches_qpdf -- --exact`

Expected: pass for overlay/underlay, dictionary-only and inline modes, plus the no-overlay baseline.

- [x] **Step 5: Run focused overlay/JSON regressions and commit.**

Run: `cargo test -p flpdf-cli --test cli_qpdf_conflict_matrix`

Expected: all tests in the conflict matrix pass against qpdf 11.9.0.

Commit: `fix(cli): preserve qpdf overlay Fx0 stream length in JSON`

### Task 2: Run quality gates and hand off the PR

**Files:** no additional source files; Beads and GitHub metadata only after verification.

- [ ] **Step 1: Run required local gates.**

Run each gate:

- `cargo fmt --all -- --check`
- `cargo test -p flpdf-cli --test cli_qpdf_conflict_matrix`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items`
- `python3 -m unittest scripts/tests/test_qpdf_module_docs.py`
- `python3 scripts/qpdf-module-docs.py --check`
- `python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py`
- `python3 scripts/check-qpdf-deviation-markers.py --check`
- `python3 -m unittest scripts/tests/test_check_qpdf_route_matrix.py`
- `python3 scripts/check-qpdf-route-matrix.py --check --no-qpdf`
- `python3 -m unittest scripts/tests/test_qpdf_route_callers.py`
- `scripts/patch-coverage.sh --base origin/main`

Expected: every gate exits successfully and changed executable lines have 100% patch coverage.

- [ ] **Step 2: Rebase onto current `origin/main` and rerun affected gates.**

Expected: clean rebase; no source or test behavior changes outside the reviewed issue scope. Rerun the focused CLI matrix, `cargo fmt --all -- --check`, `cargo test --workspace`, all-feature Clippy, strict Rustdoc, qpdf module/deviation/route checks, and fresh patch coverage on the rebased head.

- [ ] **Step 3: Push and create a Draft PR.**

Expected: PR targets `main`, describes the qpdf `/Fx0` materialization behavior and mode-specific regression coverage, and is Draft until every required check for the exact pushed head succeeds.

- [ ] **Step 4: Wait for all current-head checks, mark Ready, and record handoff.**

Expected: all required CI checks, including Coverage and codecov/patch, are green; `gh pr ready` succeeds; Beads contains commit, PR, and verification results; `flpdf-60b3y` is closed after handoff; `bd dep cycles` reports no cycles; `bd dolt push` prints `Push complete.`
