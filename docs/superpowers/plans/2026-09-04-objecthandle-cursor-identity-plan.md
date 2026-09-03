# ObjectHandle cursor identity and end-state parity Implementation Plan

> For agentic workers: use superpowers:executing-plans or superpowers:subagent-driven-development to execute this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make Rust ObjectHandle cursors return canonical child handles and distinguish dictionary missing-key nulls from the qpdf iterator end sentinel.

**Architecture:** Keep the existing safe value-returning current() API and the dictionary visible-key snapshot. Remove the persistent cursor proxy so each call returns the actual child handle clone; return a new uninitialized handle only at the position sentinel. Use the existing ObjectHandle::get_key for every non-end dictionary position, and update the qtest consumer to account for qpdf's borrowed auto& reference versus Rust's value return.

**Tech Stack:** Rust workspace, flpdf and flpdf-qtest-tools, pinned qpdf 11.9.0 source and live C++ probe, separate flpdf-qtest harness, Cargo, rustdoc, Clippy, llvm-cov, and repository parity/documentation scripts.

---

### Task 1: Add source-derived RED tests

**Files:**
- Modify: crates/flpdf/src/object_handle.rs cursor tests near qpdf_cursors_return_live_children_and_uninitialized_end_values
- Modify: crates/flpdf-qtest-tools/src/driver/test_42_49.rs run_test_42 cursor assertions

- [ ] Step 1: Add the array identity and value-stability test

Add this test before the existing cursor test:

    #[test]
    fn cursor_current_returns_child_identity_without_rebinding_held_value() {
        let array = ObjectHandle::array(vec![
            ObjectHandle::name(b"Item0".to_vec()),
            ObjectHandle::name(b"Item1".to_vec()),
        ]);
        let items = array.try_array_items().unwrap();
        let mut cursor = items.begin();
        let held = cursor.current();

        assert!(held.is_same_object_as(&array.try_get_array_item(0).unwrap()));
        cursor.next();
        assert_eq!(held.try_get_name().unwrap(), b"/Item0");
        assert_eq!(cursor.current().try_get_name().unwrap(), b"/Item1");
        cursor.next();
        assert!(cursor.is_end());
        assert!(!cursor.current().is_initialized());
        assert!(held.is_initialized());
        assert_eq!(held.try_get_name().unwrap(), b"/Item0");
    }

- [ ] Step 2: Add the dictionary missing-key/null test

Add a two-entry test:

    #[test]
    fn dict_cursor_returns_identity_and_initialized_null_for_removed_key() {
        let dictionary = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::name(b"ValueA".to_vec())),
            (b"B".to_vec(), ObjectHandle::name(b"ValueB".to_vec())),
        ]);
        let items = dictionary.try_dict_items().unwrap();
        let mut cursor = items.begin();
        let first = cursor.current();
        assert_eq!(first.key, b"/A");
        assert!(first.value.is_same_object_as(&dictionary.get_key(b"/A")));

        dictionary.remove_key(b"/A");
        let removed = cursor.current();
        assert_eq!(removed.key, b"/A");
        assert!(!cursor.is_end());
        assert!(removed.value.is_initialized());
        assert!(removed.value.is_null());

        cursor.next();
        assert_eq!(cursor.current().key, b"/B");
        cursor.next();
        assert!(cursor.is_end());
        assert!(!cursor.current().value.is_initialized());
    }

Change the existing non-dictionary live-container test to assert that its
non-end /A value is initialized and null after the dictionary becomes a scalar.

- [ ] Step 3: Correct qtest test_42 assertions

In run_test_42, keep the qpdf source-order operations but make the copied
i_value and entry.value stable. At the end position, inspect a fresh
cursor.current() for the uninitialized sentinel. Remove comments that claim
copied Rust values follow later cursor transitions.

- [ ] Step 4: Run the focused tests and verify behavioral RED

    cargo test -p flpdf --lib cursor_current_returns_child_identity_without_rebinding_held_value
    cargo test -p flpdf --lib dict_cursor_returns_identity_and_initialized_null_for_removed_key
    cargo test -p flpdf --lib dict_item_cursor_falls_back_to_uninitialized_when_the_container_stops_being_a_dictionary

Expected: the array test fails at identity or held-value stability, the dictionary
test fails at identity or removed-key null state, and the non-dictionary test
fails because the proxy returns uninitialized. These must be behavioral assertion
failures, not compilation or fixture errors. Do not change production code until
the RED failures are observed.

### Task 2: Remove the persistent proxy

**Files:**
- Modify: crates/flpdf/src/object_handle.rs:1311-1503 and the cursor-only helper near 1524-1620

- [ ] Step 1: Remove current state and constructor rebinding

Remove current: ObjectHandle from ArrayItemCursor and DictItemCursor. Their
constructors retain only the live container, visible key snapshot where
applicable, and index. Remove constructor and movement calls to update_current.

- [ ] Step 2: Return the actual array child handle

Implement ArrayItemCursor::current with this direct lookup:

    pub fn current(&mut self) -> ObjectHandle {
        self.array
            .with_value(|value| match value {
                Some(ObjectValue::Array(children)) => children.get(self.index).cloned(),
                _ => None,
            })
            .unwrap_or_else(ObjectHandle::uninitialized)
    }

The cloned child must be the same outer ObjectHandle allocation as the array
element. Keep next and previous responsible only for index movement.

- [ ] Step 3: Return dictionary values through get_key

Implement DictItemCursor::current with a separate end branch:

    pub fn current(&mut self) -> DictItem {
        let Some(key) = self.keys.get(self.index) else {
            return DictItem {
                key: Vec::new(),
                value: ObjectHandle::uninitialized(),
            };
        };
        DictItem {
            key: key.clone(),
            value: self.dictionary.get_key(key),
        }
    }

This preserves present-child identity and returns an initialized contextual null
for a removed key or a live non-dictionary receiver. Only the snapshot end
returns an uninitialized handle.

- [ ] Step 4: Delete rebind_cursor_value and update cursor docs

Run rg -n rebind_cursor_value and confirm the helper has no unrelated caller.
Delete it without removing shared ownership helpers used by other canonical
paths. Document that current() returns a value clone sharing selected-child
identity but not later cursor position; only a fresh end current() is
uninitialized.

- [ ] Step 5: Run GREEN tests

    cargo test -p flpdf --lib cursor_current_returns_child_identity_without_rebinding_held_value
    cargo test -p flpdf --lib dict_cursor_returns_identity_and_initialized_null_for_removed_key
    cargo test -p flpdf --lib dict_item_cursor_falls_back_to_uninitialized_when_the_container_stops_being_a_dictionary
    cargo test -p flpdf --lib qpdf_cursors_return_live_children_and_uninitialized_end_values
    cargo test -p flpdf --lib object_handle

Expected: all commands exit 0 with zero failures.

### Task 3: Correct qpdf correspondence and historical type-check docs

**Files:**
- Modify: crates/flpdf/src/object_handle.rs cursor documentation
- Modify: crates/flpdf-qtest-tools/src/driver/test_42_49.rs cursor comments
- Modify: docs/superpowers/specs/2026-08-30-qtest-type-checks-design.md iterator section
- Modify: docs/superpowers/plans/2026-08-30-qtest-type-checks.md iterator task
- Modify: docs/qpdf-correspondence.md ObjectHandle/array iterator annotation

- [ ] Step 1: Record the exact value/reference boundary

State that qpdf auto& binds to the iterator's internal ivalue, while a copied
QPDFObjectHandle shares the selected child object but remains a copy of that
handle after movement. flpdf current() returns by value and follows the latter
behavior.

- [ ] Step 2: Record dictionary missing-key behavior

State that the visible key snapshot remains qpdf-compatible, get_key supplies
initialized null for a non-end missing key or non-dictionary receiver, and only
the position after the snapshot receives an uninitialized value. Preserve pinned
qpdf citations and the live probe result. Add no deviation marker.

- [ ] Step 3: Verify documentation and formatting

    cargo fmt --all -- --check
    git diff --check
    python3 scripts/qpdf-module-docs.py --check
    python3 scripts/check-qpdf-deviation-markers.py --check

Expected: all commands exit 0.

### Task 4: Run qtest and complete local quality gates

**Files:**
- Verify the full worktree; no additional production modules are expected.

- [ ] Step 1: Build release binaries with qpdf-zlib-compat for qtest

    cargo build --release --workspace --features qpdf-zlib-compat

Expected: the worktree release binaries used by flpdf-qtest are built from the
current commit.

- [ ] Step 2: Run the complete type-check qtest suite

From /home/ubuntu/flpdf-qtest, run:

    FLPDF_DIR=/home/ubuntu/flpdf/.worktrees/flpdf-syrr QTEST_FULL=1 ./scripts/run.sh

Inspect survey/latest/harness.log and survey/latest/qtest-results.xml from this
same invocation. Require type-checks 1-6 to pass with no unexpected result.
The script invokes scripts/verify-allowlist.py and
scripts/verify-parity-manifest.py against that artifact pair and writes their
summaries under survey/latest; inspect both verdicts.
Do not copy qpdf-qtest fixtures into flpdf.

- [ ] Step 3: Run focused, crate, CLI, and workspace tests

    cargo test -p flpdf --lib object_handle
    cargo test -p flpdf-qtest-tools
    cargo test -p flpdf
    cargo test -p flpdf-cli --test cli_tests
    cargo test --workspace

Expected: every command exits 0 with zero failed tests.

- [ ] Step 4: Run strict Rustdoc, Clippy, formatting, and qpdf checks

    cargo fmt --all -- --check
    RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    python3 scripts/qpdf-module-docs.py --check
    python3 scripts/check-qpdf-deviation-markers.py --check
    git diff --check

- [ ] Step 5: Generate current-head LCOV and pass patch coverage

    cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
    scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov

Expected: flpdf reports changed executable lines with uncovered 0 and PASS.
Do not use cov:ignore for reachable cursor behavior.

### Task 5: Review, publish, and persist without merging

**Files:**
- Verify git, GitHub, and Beads state; include only the intended cursor/doc/test changes.

- [ ] Step 1: Request independent review

Review origin/main..HEAD against the pinned qpdf source and live probe. Check
canonical outer identity, stable copied values, initialized null for non-end
dictionary misses, qtest assertions, stale docs, and absence of writer/NameTree
or legacy changes. Resolve valid findings with new RED tests and fresh gates.

- [ ] Step 2: Rebase before publication

    git fetch origin main
    git rebase origin/main
    cargo fmt --all -- --check
    cargo test --workspace
    cargo clippy --workspace --all-targets --all-features -- -D warnings

Expected: rebase succeeds and all fresh checks pass.

- [ ] Step 3: Push and create a Draft PR

Push feature/flpdf-syrr-cursor-identity and create a Draft PR against main.
Include qpdf source/probe evidence, corrected Rust value semantics, the same-run
qtest artifact pair, local gates, and patch coverage. Do not add a compatibility
bridge or modify flpdf-qtest fixtures.

- [ ] Step 4: Wait for all CI and mark Ready

Monitor Quality, Coverage, codecov/patch, Fuzz, Release, Analyze, label, and
every platform test. Verify all required checks are green, then verify PR
base/head/body and run gh pr ready with the actual PR number. Do not merge.

- [ ] Step 5: Append complete evidence to Beads

Read back flpdf-syrr, run bd dep cycles, append the commit/PR, RED/GREEN,
same-run qtest artifact pair, local gate results, CI results, and patch
coverage, then run:

    bd dep cycles
    bd dolt push

Require No dependency cycles detected and Push complete. Keep the issue in
progress for integration.
