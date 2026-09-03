# Outline helper document-identity guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reject live outline access through an `OutlineDocumentHelper` backed by a different `Pdf`, matching qpdf's private document-helper ownership boundary.

**Architecture:** Keep `OutlineItem` as an arena entry with its canonical `ObjectHandle`. Add a crate-private ownership check on the existing `OutlineDocumentHelper` borrow, using `Pdf::unique_id` and `ObjectHandle::belongs_to_pdf`; invoke it before all live outline accessor bodies. Leave named-destination resolution, existing synthetic-handle behavior, and consumers unchanged.

**Tech Stack:** Rust workspace, `flpdf` integration tests, qpdf 11.9.0 source oracle, Cargo, rustdoc, Clippy, llvm-cov, and repository qpdf documentation checks.

---

### Task 1: Add the canonical foreign-helper regression test

**Files:**
- Modify: `crates/flpdf/tests/inspection_tests.rs:1-3` and the outline test/fixture section near lines 25-206

- [ ] **Step 1: Add a named-destination fixture and the failing test**

Import `Error` and add this test before the existing `single_outline_pdf` helper:

```rust
#[test]
fn outline_accessors_reject_a_foreign_helper() {
    let bytes = named_destination_outline_pdf();
    let mut source = Pdf::open(Cursor::new(bytes.clone())).unwrap();
    let mut foreign = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut source_helper = source.outline();
    let tree = source_helper.get_tree().unwrap();
    let item = &tree[tree.roots()[0]];
    let mut foreign_helper = foreign.outline();
    let expected = "ObjectHandle belongs to another Pdf";

    let results = [
        item.get_title(&mut foreign_helper).map(|_| ()),
        item.get_count(&mut foreign_helper).map(|_| ()),
        item.get_dest(&mut foreign_helper).map(|_| ()),
        item.get_dest_page(&mut foreign_helper).map(|_| ()),
    ];
    for result in results {
        assert!(
            matches!(result, Err(Error::Unsupported(message)) if message == expected),
            "foreign outline helper must be rejected, got {result:?}"
        );
    }
}
```

Add the fixture immediately before `finalize_pdf`:

```rust
fn named_destination_outline_pdf() -> Vec<u8> {
    finalize_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R /Dests 5 0 R >>\nendobj\n"
            .to_vec(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [6 0 R] >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Title (Named) /Parent 3 0 R /Dest /chapter >>\nendobj\n".to_vec(),
        b"5 0 obj\n<< /chapter [6 0 R /Fit] >>\nendobj\n".to_vec(),
        b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n"
            .to_vec(),
    ])
}
```

- [ ] **Step 2: Run the focused test and verify the expected RED**

Run:

```bash
cargo test -p flpdf --test inspection_tests outline_accessors_reject_a_foreign_helper -- --exact
```

Expected: the test fails in its ownership assertions because the current
accessors return `Ok(...)` and the named destination is resolved through the
foreign helper. A compilation error or fixture parse error is not an
acceptable RED result; correct those before proceeding.

### Task 2: Add the canonical ownership guard and make the test GREEN

**Files:**
- Modify: `crates/flpdf/src/outline_document_helper.rs` near `resolve_handle`
- Modify: `crates/flpdf/src/outline_object_helper.rs:29-30,124-190`

- [ ] **Step 1: Add the document-helper guard**

Extend the imports in `outline_document_helper.rs` with `Error`, then add this
crate-private method immediately after `resolve_handle`:

```rust
pub(crate) fn ensure_handle_belongs_to_pdf(&self, handle: &ObjectHandle) -> Result<()> {
    if handle.belongs_to_pdf(self.pdf.unique_id()) {
        return Ok(());
    }
    Err(Error::Unsupported(
        "ObjectHandle belongs to another Pdf".to_string(),
    ))
}
```

This reuses the existing canonical identity primitive. It deliberately permits
detached direct handles according to `belongs_to_pdf`'s established semantics;
it does not add a sentinel or a new ownership representation.

- [ ] **Step 2: Call the guard before each live accessor reads the item**

At the start of `OutlineItem::get_title`, `get_count`, and `get_dest`, before
`outline_dict_key` or `try_get_key`, add:

```rust
helper.ensure_handle_belongs_to_pdf(&self.object)?;
```

Leave `get_dest_page` as a call to `get_dest`, so it has one ownership path and
the same error contract.

- [ ] **Step 3: Run the regression test and verify GREEN**

Run the same exact focused command:

```bash
cargo test -p flpdf --test inspection_tests outline_accessors_reject_a_foreign_helper -- --exact
```

Expected: one test passes with no failures. Then run the complete related test
binary:

```bash
cargo test -p flpdf --test inspection_tests
```

Expected: all inspection tests pass, including the existing same-document
outline title, count, explicit-destination, and destination-page cases.

### Task 3: Record the qpdf boundary in documentation

**Files:**
- Modify: `crates/flpdf/src/outline_object_helper.rs:16-23`
- Modify: `docs/qpdf-correspondence.md` in the `QPDFOutlineDocumentHelper / QPDFOutlineObjectHelper` row

- [ ] **Step 1: Update module documentation**

Document that the externally passed helper is the Rust adaptation for live
recomputation and that every accessor first rejects a helper whose `Pdf` does
not own the item handle, corresponding to qpdf's private `m->dh` reference.
Keep the existing note about the synthetic `Pdf::set_object` terminal chase;
do not describe the new guard as a qpdf output deviation.

- [ ] **Step 2: Update the correspondence row**

Add the same ownership-boundary explanation to the existing outline row while
preserving its current live-handle, cache, and bridge descriptions. Do not
change unrelated correspondence rows.

- [ ] **Step 3: Verify documentation and formatting**

Run:

```bash
cargo fmt --all -- --check
git diff --check
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
```

Expected: all commands exit 0; no new deviation marker is required.

### Task 4: Run the complete local quality gates

**Files:**
- Verify the complete worktree; no additional source files are expected beyond Tasks 1-3

- [ ] **Step 1: Run focused library and CLI tests**

```bash
cargo test -p flpdf --test inspection_tests
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_tests
```

Expected: every command exits 0 with zero failed tests.

- [ ] **Step 2: Run strict Rustdoc and all-features Clippy**

```bash
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: both commands exit 0 without warnings promoted to errors.

- [ ] **Step 3: Run workspace tests and changed-line coverage**

```bash
cargo test --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: workspace tests pass and patch coverage reports zero uncovered
changed executable lines. If coverage attributes a line incorrectly, add a
behavioral assertion or restructure the line; do not add `cov:ignore` for a
testable branch.

### Task 5: Publish a Draft PR without merging

**Files:**
- Verify `git` and GitHub state; no further source changes are planned

- [ ] **Step 1: Re-read Beads and verify the branch before publication**

```bash
bd show flpdf-e584
bd dep cycles
git status --short --branch
git diff --check
```

Expected: `flpdf-e584` is `in_progress`, the dependency graph has no cycles,
and the worktree contains only the intended design, test, implementation, and
documentation changes.

- [ ] **Step 2: Rebase onto the latest origin/main and rerun the publication checks**

```bash
git fetch origin main
git rebase origin/main
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: rebase succeeds and all fresh checks exit 0. Resolve any conflict
using qpdf source and the design doc as authority, then rerun the affected
checks.

- [ ] **Step 3: Push and create a Draft PR**

```bash
git push --force-with-lease -u origin feature/flpdf-e584-outline-owner
gh pr create --draft --base main --head feature/flpdf-e584-outline-owner --title "fix: reject foreign outline helpers" --body-file /tmp/flpdf-e584-pr-body.md
```

The PR body must state the qpdf 11.9.0 source locations, the canonical route,
the named-destination regression test, and the local verification results. It
must not mention session merge policy.

- [ ] **Step 4: Wait for every required CI gate and mark Ready only afterward**

```bash
gh pr checks <PR_NUMBER> --watch
gh pr checks <PR_NUMBER>
gh pr ready <PR_NUMBER>
```

Run fresh local checks after any CI-driven change. `gh pr ready` is permitted
only after all required CI checks, including patch coverage, are green.
Do not merge the PR in this implementation session.

- [ ] **Step 5: Record implementation and verification in Beads**

Append the PR number, commit, RED/GREEN result, focused tests, workspace
tests, fmt, strict Rustdoc, all-features Clippy, qpdf module-doc check, and
patch-coverage result to `flpdf-e584`; then read back the issue and run:

```bash
bd dep cycles
bd dolt push
```

Expected: `No dependency cycles detected` followed by `Push complete.`. Leave
the issue open/in progress for the integration session unless the user later
explicitly requests closure after merge evidence.
