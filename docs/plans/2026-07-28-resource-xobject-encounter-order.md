# XObject Encounter-Order Preservation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve first-seen content-stream order while deduplicating Form XObject traversal, so a later incomplete decode cannot hide an earlier structural error.

**Architecture:** `ResourceCallbacks` will retain one owned copy of each valid XObject name in a `BTreeMap<Vec<u8>, usize>`, where the value is the first `Do` operator offset. `collect_from_stream` will sort borrowed map entries by that offset before recursion, preserving encounter order without reintroducing repeated-name cloning.

**Tech Stack:** Rust 1.87, standard-library `BTreeMap`, Cargo integration tests, qpdf 11.9.0 differential oracle, `cargo llvm-cov`.

## Global Constraints

- qpdf 11.9.0 behavior and the existing structural-error contract are the compatibility oracle.
- Allocate one owned name per distinct valid XObject; repeated `Do` operators must not clone the name again.
- Add no dependency.
- Keep inline-image and resource-finder behavior unchanged.
- Finish with fresh 100% changed executable-line coverage.

---

### Task 1: Reproduce and fix encounter-order loss

**Files:**
- Modify: `crates/flpdf/tests/resource_pruning_tests.rs`
- Modify: `crates/flpdf/src/resources.rs:675-820`

**Interfaces:**
- Consumes: `remove_unreferenced_resources(&mut Pdf<R>, RemoveUnreferencedResources) -> flpdf::Result<()>`
- Produces: `ResourceCallbacks::valid_xobjects: BTreeMap<Vec<u8>, usize>`, mapping each distinct name to its first valid `Do` operator offset.

- [ ] **Step 1: Add the failing integration regression**

Add this test after `test_form_decode_failure_retains_page` in
`crates/flpdf/tests/resource_pruning_tests.rs`:

```rust
#[test]
fn earlier_xobject_resolution_error_is_not_hidden_by_later_incomplete_form() {
    let bad_form_body = b"this is not valid flate data";
    let bad_form = {
        let mut bytes = format!(
            "7 0 obj\n<< /Subtype /Form /Filter /FlateDecode /Length {} >>\nstream\n",
            bad_form_body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(bad_form_body);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes
    };
    let extra = vec![
        (4u32, stream_obj(4, b"/Z Do /A Do")),
        (
            5,
            obj_bytes(5, "<< /XObject << /Z 6 0 R /A 7 0 R >> >>"),
        ),
        (6, obj_bytes(6, "<0g>")),
        (7, bad_form),
    ];
    let pdf_bytes = build_pdf(&["/Contents 4 0 R /Resources 5 0 R"], &extra);
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("open");

    let error =
        remove_unreferenced_resources(&mut pdf, RemoveUnreferencedResources::Yes)
            .expect_err("the first-seen structural error must propagate");

    assert!(matches!(error, flpdf::Error::Parse { .. }), "{error:?}");
}
```

This catches the real regression: replacing encounter-ordered storage with a
name-sorted set makes `/A` return `Ok(false)` before `/Z` can return `Err`.

- [ ] **Step 2: Run the regression and verify RED**

Run:

```bash
cargo test -p flpdf --test resource_pruning_tests \
  earlier_xobject_resolution_error_is_not_hidden_by_later_incomplete_form -- --exact
```

Expected: FAIL at `expect_err` because the current `BTreeSet` visits `A` before
`Z` and `remove_unreferenced_resources` returns `Ok(())`.

- [ ] **Step 3: Store first-seen offsets without repeated clones**

In `crates/flpdf/src/resources.rs`, change the callback field and insertion:

```rust
struct ResourceCallbacks {
    finder: ResourceFinder,
    inline_header: Option<Vec<Object>>,
    valid_xobjects: BTreeMap<Vec<u8>, usize>,
    complete: bool,
}
```

```rust
if operator == b"Do" && self.complete {
    if let Some(name) = self.finder.last_name() {
        if !self.valid_xobjects.contains_key(name) {
            self.valid_xobjects.insert(name.to_vec(), offset);
        }
    }
}
```

Initialize it with `BTreeMap::new()` in `collect_from_stream`.

- [ ] **Step 4: Traverse distinct names in first-seen offset order**

Replace the name-sorted traversal in `collect_from_stream` with:

```rust
let mut valid_xobjects = callbacks.valid_xobjects.iter().collect::<Vec<_>>();
valid_xobjects.sort_unstable_by_key(|(_, offset)| *offset);
for (name, _) in valid_xobjects {
    if !recurse_form_xobject(ctx, name, scope, depth)? {
        complete = false;
        break;
    }
}
```

The map already deduplicates names, so remove the redundant `traversed`
`BTreeSet`.

Update `resource_callbacks_deduplicate_repeated_xobject_names_before_traversal`
to read its single name through
`callbacks.valid_xobjects.keys().next().unwrap()`.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf --test resource_pruning_tests \
  earlier_xobject_resolution_error_is_not_hidden_by_later_incomplete_form -- --exact
cargo test -p flpdf --lib \
  resources::tests::resource_callbacks_deduplicate_repeated_xobject_names_before_traversal \
  -- --exact
cargo test -p flpdf --test resource_pruning_tests
```

Expected: all commands PASS with zero failures.

- [ ] **Step 6: Run formatting, lint, full tests, and qpdf parity**

Run:

```bash
cargo fmt -- --check
cargo clippy -p flpdf --lib --tests -- -D warnings
cargo test -p flpdf
bash scripts/qpdf-tokenizer-diff.sh
git diff --check
```

Expected: every command exits 0; all five qpdf 11.9.0 differential tests pass.

- [ ] **Step 7: Commit the implementation**

Run:

```bash
git add crates/flpdf/src/resources.rs crates/flpdf/tests/resource_pruning_tests.rs
git commit -m "fix(resources): preserve XObject encounter order"
```

Expected: one commit containing only the regression test and ordered-dedup fix.

### Task 2: Validate, publish, and close the review thread

**Files:**
- Verify only: `crates/flpdf/src/resources.rs`
- Verify only: `crates/flpdf/tests/resource_pruning_tests.rs`

**Interfaces:**
- Consumes: the committed Task 1 implementation.
- Produces: a pushed PR #578 head, a verified inline reply, and resolved thread `PRRT_kwDOSYPosM6USESQ`.

- [ ] **Step 1: Run fresh changed-line coverage**

Run:

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: `flpdf` reports `PASS (100%)` with zero uncovered changed executable
lines.

- [ ] **Step 2: Synchronize Beads and push the branch**

Run:

```bash
bd dolt push
git fetch origin feature/flpdf-qynx-3-resource-cutover
git status --short --branch
git push origin feature/flpdf-qynx-3-resource-cutover
```

Expected: the worktree is clean before push, and the remote feature branch
advances to the implementation commit.

- [ ] **Step 3: Reply in the original review thread**

Use `addPullRequestReviewThreadReply` for thread
`PRRT_kwDOSYPosM6USESQ`. State that the callback now deduplicates by name while
storing the first `Do` offset, traversal sorts by that offset, and the new
`/Z Do /A Do` regression proves the earlier structural error propagates.
Include the implementation commit and these verification commands:

```text
cargo test -p flpdf
bash scripts/qpdf-tokenizer-diff.sh
scripts/patch-coverage.sh --base origin/main
```

Expected: the mutation returns the inline reply URL.

- [ ] **Step 4: Resolve and verify the thread**

Use `resolveReviewThread` for `PRRT_kwDOSYPosM6USESQ`, then query the thread and
PR head again.

Expected:

```text
thread.isResolved = true
pullRequest.headRefOid = local HEAD
```

- [ ] **Step 5: Confirm CI and final repository state**

Run:

```bash
gh pr checks 578 --watch --interval 10
git status --short --branch
```

Expected: every required PR check passes; the local and remote branch are
synchronized and the worktree is clean.
