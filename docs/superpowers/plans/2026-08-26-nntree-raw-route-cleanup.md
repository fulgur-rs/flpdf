# NNTree Raw Route Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the remaining generic raw `Object` compatibility route from `nntree.rs` while preserving qpdf 11.9.0 handle-native NameTree and NumberTree behavior.

**Architecture:** Keep one generic `NNTree<K>` engine whose root, nodes, cursor values, and mutations are `ObjectHandle` based. Remove only the raw snapshot/projection layer and its test-only API; public `NameTree` and `NumberTree` wrappers remain the sole consumer surface. Canonical tests use direct `ObjectHandle` construction or parser-owned PDF fixtures, so no conversion adapter is introduced.

**Tech Stack:** Rust workspace, `ObjectHandle`, qpdf 11.9.0 source/live probes, Cargo tests, rustdoc, Clippy, `cargo llvm-cov`, and repository qpdf/coverage scripts.

---

## Task 1: Add the canonical route contract and observe RED

**Files:**
- Modify: `crates/flpdf/tests/name_number_tree_route_cutover_tests.rs`

- [ ] **Step 1: Add a production-route assertion for the generic engine.**

Append a test that reads `src/nntree.rs` and rejects the raw route markers that
the implementation must remove:

```rust
#[test]
fn generic_nntree_uses_only_the_canonical_handle_route() {
    let source = include_str!("../src/nntree.rs");
    for forbidden in [
        "fn from_object(",
        "fn to_object(",
        "pub(crate) fn new(root: Object,",
        "materialize_cursor_value",
        "legacy_root_snapshot",
        "legacy_projection",
        "sync_legacy_root",
        "finish_mutation",
        "lift_value",
        "cursor.raw",
        "cursor.current",
        "Result<Option<Object>>",
    ] {
        assert!(
            !source.contains(forbidden),
            "nntree.rs still contains the raw route marker {forbidden:?}"
        );
    }
    for canonical in ["ObjectHandle", "cloned_current", "set_array_items"] {
        assert!(
            source.contains(canonical),
            "nntree.rs must retain the canonical route marker {canonical:?}"
        );
    }
}
```

- [ ] **Step 2: Run the new contract test and verify the expected failure.**

Run:

```bash
cargo test -p flpdf --test name_number_tree_route_cutover_tests generic_nntree_uses_only_the_canonical_handle_route
```

Expected: FAIL because the current source still contains `from_object`,
`materialize_cursor_value`, and legacy projection fields. This confirms the test
is detecting the missing cleanup rather than a typo.

## Task 2: Remove raw key and cursor projection state

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`

- [ ] **Step 1: Remove raw-only imports and `TreeKey` methods.**

Remove the raw `Dictionary`/`Object` imports and the `pdf_string` imports that
exist only for raw key conversion. Change `TreeKey` to expose only:

```rust
pub(crate) trait TreeKey {
    type Key: Clone + Debug + Eq + Ord;
    const ITEMS_KEY: &'static str;

    fn from_handle(handle: &ObjectHandle) -> Option<Self::Key>;
    fn to_handle(key: &Self::Key) -> ObjectHandle;

    fn compare(left: &Self::Key, right: &Self::Key) -> Ordering {
        left.cmp(right)
    }
}
```

Delete `NameKey::from_object`, `NameKey::to_object`, `NumberKey::from_object`,
and `NumberKey::to_object`. Keep the handle codecs and their canonical UTF-8
normalization behavior.

- [ ] **Step 2: Remove the raw cursor fields and accessors.**

Change `NNTreeCursor<K>` so it contains `path`, `leaf`, `item_number`,
`current`, `pdf_id`, and `marker` only. Delete `raw` and all raw projection
fields and assignments. Keep `positioned`, `cloned_current`, `current_key`, `clear_position`,
`same_position`, and `Clone` behavior for canonical cursors.

- [ ] **Step 3: Remove `materialize_cursor_value` and simplify `update_current`.**

`update_current` must clear and set only `current`:

```rust
cursor.current = None;
// load and validate the current key/value pair
let key = resolved_key::<K, _>(pdf, &raw_key)?;
cursor.current = key.map(|key| (key, raw_value));
```

Preserve the existing `allow_invalid` error and warning behavior. Do not turn
an invalid key into a sentinel `ObjectHandle` or a raw `Object`.

## Task 3: Make `NNTree` canonical-only

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`

- [ ] **Step 1: Replace the raw-backed struct and constructor.**

Change the struct to:

```rust
pub(crate) struct NNTree<K: TreeKey> {
    root: ObjectHandle,
    root_pdf_id: Option<u64>,
    auto_repair: bool,
    split_threshold: usize,
    max_depth: Option<usize>,
    marker: PhantomData<K>,
}
```

Make `NNTree::new` construct this value directly. Keep the public
`NameTree::new` and `NumberTree::new` wrappers as the only constructors.

- [ ] **Step 2: Simplify root ownership and mutation completion.**

Keep the root ownership claim in `ensure_root`, including
the full descendant ownership check before claiming a direct root. Return the
canonical root for the same PDF without comparing or rebuilding a raw snapshot.
Delete `legacy_root_snapshot`, `legacy_projection`, `sync_legacy_root`, and
`finish_mutation`; callers return their canonical result directly.

- [ ] **Step 3: Delete raw mutation wrappers and use handle mutation paths.**

Delete the raw `insert`, `insert_after`, `remove`, `remove_at`,
`remove_at_inner`, and `lift_value` methods. Retain the canonical versions used
by the public wrappers. Remove the `raw` suffix from helper methods whose name
referred only to the old API, then update all internal callers.

- [ ] **Step 4: Keep qpdf live node handling and remove compatibility storage.**

Retain `NodeHandle` live-handle traversal, direct-kid diagnostic paths,
`resolved_array`, `LiveDictionary`, `load_node`, `load_anchor`, `repair`,
`split_node_live`, allocator checks, and all canonical error/warning paths.
Keep the canonical `NodeHandle::root`, `NodeHandle::indirect`, and
`NodeHandle::direct_kid` constructors needed by live direct-kid traversal.
Remove test-only `NodeReplacement` and `store_node` with the raw tests.

- [ ] **Step 5: Run a compile-focused test after the production cleanup.**

Run:

```bash
cargo test -p flpdf --test name_number_tree_route_cutover_tests generic_nntree_uses_only_the_canonical_handle_route
```

Expected: the route test passes and no private raw-test module remains. Do not
add a compatibility API to make deleted raw tests compile.

## Task 4: Remove in-module raw tests without losing canonical coverage

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Modify: `crates/flpdf/tests/nntree_tests.rs` when an external canonical case is missing

- [ ] **Step 1: Delete tests whose assertion is exclusively raw-route behavior.**

Delete the codec tests that call `from_object`/`to_object`, raw root snapshot
tests (`raw_store_node_replaces_a_live_dictionary`, root synchronization and
materialization-failure cases), bare-reference legacy-terminal tests, and
`cursor_raw_is_restored_cleared_for_empty_and_cleared_after_last_remove`.
Delete fixture builders and helpers that become unused after those tests are
removed, including raw `make_indirect`, `number_tree_shape` if it has no
canonical caller, and raw `NodeHandle` constructors. In this slice the entire
former in-module suite is raw-route coupled, so no private test module remains.

- [ ] **Step 2: Confirm canonical coverage in external tests.**

The external `NameTree`/`NumberTree` suite now constructs roots with
`ObjectHandle::dictionary`, `ObjectHandle::array`, `ObjectHandle::string`, and
`ObjectHandle::integer`, and uses typed public cursor `current()` accessors.
Inspect direct values through `ObjectHandle` accessors, never `Object` pattern
matching in the canonical route.

- [ ] **Step 3: Preserve malformed and repair coverage through parser-owned handles.**

Where a test needs indirect object numbers, create the object with
`pdf.make_indirect_from_object_handle` and pass the returned handle to the
canonical tree. Where a test needs a parsed malformed tree, keep a small PDF
fixture and open it with `Pdf::open`; do not construct a raw `Object` and lift
it through the removed route. Keep assertions for qpdf warning order, cycle
termination, `/Limits`, split allocation, ownership rejection, and failure
atomicity.

- [ ] **Step 4: Run focused tests and verify GREEN.**

Run:

```bash
cargo test -p flpdf --test nntree_tests
cargo test -p flpdf --test name_number_tree_route_cutover_tests
```

Expected: the external canonical tests pass, the route contract passes, and no
test refers to the deleted generic raw API.

## Task 5: Update qpdf documentation and inspect the complete diff

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Modify: `docs/qpdf-correspondence.md:454`
- Modify: `docs/qpdf-module-doc-index.md` only if the module description still names the deleted raw route

- [ ] **Step 1: Rewrite module documentation.**

State that the shared NNTree engine, public NameTree/NumberTree wrappers, and
typed cursors are entirely live `ObjectHandle` based. Remove claims that generic
raw fixture helpers or legacy projection remain. Keep the pinned qpdf source
citations for iterator, mutation, split, repair, and ownership behavior.

- [ ] **Step 2: Update the correspondence row.**

Remove the phrase saying generic raw fixture helpers remain and preserve the
existing qpdf responsibility mapping. Do not promote the row to `Mirrors`
unless its existing D1-D5 evidence is still true after the cleanup.

- [ ] **Step 3: Review the complete diff and route census.**

Run:

```bash
git diff --check
rg -n 'from_object|to_object|materialize_cursor_value|legacy_root_snapshot|legacy_projection|sync_legacy_root|finish_mutation|lift_value|cursor\.raw|Result<Option<Object>>' crates/flpdf/src/nntree.rs crates/flpdf/tests/nntree_tests.rs
git status --short
```

Expected: no raw generic route marker remains in the implementation or tests;
unrelated worktree files remain untouched.

## Task 6: Run all local quality gates and obtain review

**Files:**
- No additional files unless a verification failure identifies a real regression

- [ ] **Step 1: Run formatting and focused tests.**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib nntree
cargo test -p flpdf --test nntree_tests
cargo test -p flpdf --test name_number_tree_route_cutover_tests
```

- [ ] **Step 2: Run strict rustdoc and all-features Clippy.**

```bash
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Run workspace tests and repository qpdf checks.**

```bash
cargo test --workspace
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
```

If the module-doc script has a different documented invocation, use the exact
repository command after reading its `--help`; record the command and exit code.

- [ ] **Step 4: Run fresh qpdf-compatible patch coverage.**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: patch coverage reports `PASS (100%)` with zero uncovered changed
executable lines.

- [ ] **Step 5: Request an independent code review.**

Use the review workflow with base `origin/main`, the implementation HEAD, the
design/plan requirements above, and the qpdf source citations. Fix every
Critical or Important finding, re-run the affected tests and coverage, and
record any technically rejected finding with qpdf evidence.

## Task 7: Rebase, publish Draft PR, and verify CI without merging

- [ ] **Step 1: Re-read remote state and rebase onto latest origin/main.**

```bash
git fetch origin main
git rebase origin/main
git status --short --branch
```

Resolve only actual conflicts in this branch, then rerun the focused tests and
patch coverage against the rebased parent.

- [ ] **Step 2: Push the feature branch and create a Draft PR.**

```bash
git push --set-upstream origin feature/flpdf-egzr-3-2-nntree-raw-cleanup
gh pr create --draft --base main --head feature/flpdf-egzr-3-2-nntree-raw-cleanup \
  --title "refactor: remove NNTree raw compatibility route" \
  --body-file /tmp/flpdf-egzr-3-2-nntree-raw-cleanup-pr.md
```

The PR body must state qpdf source/probe evidence, the canonical route, the
deleted raw route, focused tests, full gates, and patch coverage. It must not
contain an instruction to merge or otherwise block the integration session.

- [ ] **Step 3: Wait for and re-query every CI check.**

```bash
PR_NUMBER="$(gh pr view --json number --jq '.number')"
gh pr checks "$PR_NUMBER"
gh pr view "$PR_NUMBER" --json state,isDraft,mergeStateStatus,reviewDecision,statusCheckRollup
```

Do not treat pending or missing results as green. Re-run failed checks only
after diagnosing whether the failure is code, stale-base, or infrastructure.

- [ ] **Step 4: Mark ready only after all checks, including patch coverage, are green.**

```bash
gh pr ready "$PR_NUMBER"
```

Do not merge the PR.

## Task 8: Persist implementation evidence and close the handoff

- [ ] **Step 1: Append implementation and PR evidence to Beads.**

Append, without overwriting prior notes, the exact commit, worktree, PR,
qpdf citations/probe, RED→GREEN result, focused/full verification, review
outcome, and remaining aggregate `.3.2` routes. Keep `flpdf-egzr.3.2` open.

- [ ] **Step 2: Verify dependencies and push Beads state.**

```bash
bd show flpdf-egzr.3.2 --short
bd dep cycles
bd dolt push
```

Expected: no dependency cycles and the output contains `Push complete.`

- [ ] **Step 3: Perform final repository readback.**

```bash
git status --short --branch
git worktree list --porcelain
gh pr view "$PR_NUMBER" --json state,isDraft,mergeStateStatus,headRefOid
```

The main worktree and unrelated worktrees must remain untouched, the PR must be
open and non-merged, and the Beads note must accurately describe the bounded
slice rather than aggregate completion.
