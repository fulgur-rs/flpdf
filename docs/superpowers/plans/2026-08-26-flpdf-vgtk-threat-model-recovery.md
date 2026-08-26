# flpdf-vgtk recovery documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct the threat-model recovery inventory to describe the already-implemented qpdf-compatible default recovery and `--suppress-recovery` opt-out.

**Architecture:** Modify one security documentation file. The qpdf source/live behavior and flpdf code remain unchanged; the document records the existing single document-wide recovery permission and the CLI policy boundary.

**Tech Stack:** Markdown, pinned qpdf 11.9.0 source/live probes, Rust workspace tests, cargo fmt/clippy/rustdoc, and repository documentation checks.

---

### Task 1: Update the threat-model recovery inventory

**Files:**
- Modify: `docs/threat-model.md:3-4,25-32,208-225`
- Reference: `docs/superpowers/specs/2026-08-26-flpdf-vgtk-threat-model-recovery-design.md`

- [ ] **Step 1: Replace the stale opening recovery wording**

Set the review date to `2026-08-26`. State that default document opening uses
qpdf-style recovery, and that the attack surface includes the same untrusted
bytes whether the caller uses a default open, an explicit options open, or the
strict opt-out.

- [ ] **Step 2: Replace the Appendix A opening rows**

Use one row for all public `Pdf::open*`/`open_mem*` paths with recovery enabled
by default. Add a recovery-policy note that `--suppress-recovery` maps to
`PdfOpenOptions::repair=false`, while flpdf's retained `--repair` flag does not
change the default-enabled policy and is not a qpdf option.

- [ ] **Step 3: Confirm the change boundary**

Run `git diff -- docs/threat-model.md` and verify that no file under
`crates/` changed and no claim of a new CLI/parser implementation was added.

- [ ] **Step 4: Commit the documentation change**

```bash
git add docs/threat-model.md
git commit -m "docs: align threat model with recovery defaults"
```

### Task 2: Verify the documented behavior and repository gates

**Files:**
- Test: `crates/flpdf-cli/tests/cli_tests.rs`
- Check: `scripts/qpdf-module-docs.py`, `scripts/check-qpdf-deviation-markers.py`

- [ ] **Step 1: Run recovery-focused tests**

```bash
cargo test -p flpdf-cli --test cli_tests suppress_recovery
```

Expected: the recovery/suppression tests pass; no production code changes are
required.

- [ ] **Step 2: Run documentation and quality checks**

```bash
cargo fmt --all -- --check
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
python3 scripts/check-qpdf-deviation-markers.py --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

- [ ] **Step 3: Run the fresh patch-coverage gate**

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: no changed executable lines under `crates/flpdf/src`; the report
passes without adding a coverage-ignore marker.

### Task 3: Deliver the documentation PR

- [ ] **Step 1: Fetch and rebase**

```bash
git fetch --prune origin
git rebase origin/main
```

- [ ] **Step 2: Push and create a Draft PR**

Push `feature/flpdf-vgtk-threat-model-recovery`, create a Draft PR referencing
`flpdf-vgtk`, and include the qpdf citations and verification results.

- [ ] **Step 3: Wait for CI and review**

Re-query all checks and review threads. Run `gh pr ready` only after every
required check is green; never merge from this workflow.

- [ ] **Step 4: Close Beads after external merge**

After the merge is independently verified, append the merge commit and
post-merge check result, close `flpdf-vgtk`, run `bd dep cycles`, and run
`bd dolt push` until the output contains `Push complete.`.
