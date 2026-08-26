# Resource Callback ObjectHandle Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the production Form-XObject resource-pruning and ResourceReplacer callbacks from the legacy raw `Object` parser boundary to the existing qpdf-shaped `ObjectHandle` parser boundary, deleting the qpdf-unowned inline-header scanner.

**Architecture:** Keep `content_stream::ParserCallbacks` and its unrelated raw consumers unchanged. Make `resources.rs` call `parse_content_stream_handles` directly with the canonical `ResourceFinder`, and make `resource_replacer.rs` use the same handle parser. Remove the custom inline-header scanner, `ResourceFinder`'s raw callback adapter, and the now-unused raw recovering helper; the qpdf-deviation terminal chase remains untouched.

**Tech Stack:** Rust workspace, `ObjectHandle`, `ObjectHandleParserCallbacks`, qpdf 11.9.0 pinned source/live probe, Cargo tests, rustdoc, Clippy, `cargo llvm-cov`, and repository qpdf/coverage scripts.

---

### Task 1: Add the production route contract and observe RED

**Files:**
- Modify: `crates/flpdf/tests/legacy_route_cutover_tests.rs`
- Test: `crates/flpdf/tests/legacy_route_cutover_tests.rs`

- [ ] **Step 1: Add a focused source-contract test.**

Append a test that isolates the Form pre-pass and ResourceFinder production
sections rather than the test fixture section:

```rust
#[test]
fn resource_pruning_callbacks_use_only_the_handle_parser_route() {
    let resources = include_str!("../src/resources.rs");
    let resource_callbacks = resources
        .split_once("fn collect_used_names_for_form")
        .expect("resources has the Form pre-pass")
        .1
        .split_once("#[cfg(test)]")
        .expect("resources has a test module")
        .0;
    for legacy in [
        "parse_content_stream_data",
        "impl ParserCallbacks for ResourceCallbacks",
        "use crate::content_stream::{parse_content_stream_data",
        "Vec<Object>",
        "object: Object,",
        "Object::Operator",
        "Object::InlineImage",
        "struct ResourceCallbacks",
        "finish_inline_header",
        "is_builtin_inline_image_cs",
    ] {
        assert!(
            !resource_callbacks.contains(legacy),
            "resources Form callback still contains the raw parser marker {legacy:?}"
        );
    }
    for canonical in [
        "parse_content_stream_handles",
        "ResourceFinder::default()",
        "has_pending_operands",
    ] {
        assert!(
            resource_callbacks.contains(canonical),
            "resources Form callback must contain the handle parser marker {canonical:?}"
        );
    }

    let finder = include_str!("../src/resource_finder.rs");
    let finder_production = finder
        .split_once("#[cfg(test)]")
        .expect("resource_finder has a test module")
        .0;
    for legacy in [
        "handle_object_borrowed",
        "impl ParserCallbacks for ResourceFinder",
        "use crate::{Object, Result}",
        "last_operator_started_at_boundary",
        "record_resource_name",
    ] {
        assert!(
            !finder_production.contains(legacy),
            "ResourceFinder still contains the raw parser marker {legacy:?}"
        );
    }
    assert!(
        finder_production.contains("impl ObjectHandleParserCallbacks for ResourceFinder")
    );

    let replacer = include_str!("../src/resource_replacer.rs");
    let replacer_production = replacer
        .split_once("#[cfg(test)]")
        .expect("resource_replacer has a test module")
        .0;
    for legacy in [
        "parse_content_stream_data",
        "parse_content_stream_data_recovering_inline_image_eof",
    ] {
        assert!(
            !replacer_production.contains(legacy),
            "ResourceReplacer still contains the raw parser marker {legacy:?}"
        );
    }
    assert!(
        replacer_production.contains("parse_content_stream_handles"),
        "ResourceReplacer must use the handle parser"
    );

    let content_stream = include_str!("../src/content_stream.rs");
    assert!(
        !content_stream.contains("parse_content_stream_data_recovering_inline_image_eof"),
        "the raw recovering parser helper must not remain without a production caller"
    );
}
```

- [ ] **Step 2: Run only the new contract test and verify the expected RED.**

Run:

```bash
cargo test -p flpdf --test legacy_route_cutover_tests resource_pruning_callbacks_use_only_the_handle_parser_route
```

Expected: FAIL because the current Form callback and ResourceReplacer contain
the raw parser route, and the recovering helper still exists. The failure must
identify one of those markers, not a compilation error or a test typo.

### Task 2: Route the Form pre-pass through ResourceFinder

**Files:**
- Modify: `crates/flpdf/src/resources.rs`
- Test: `crates/flpdf/src/resources.rs`

- [ ] **Step 1: Route decoded Form bytes through the canonical parser.**

Instantiate the existing `ResourceFinder` directly and parse the decoded Form
bytes through the qpdf-shaped callback boundary:

```rust
let mut finder = ResourceFinder::default();
let complete = parse_content_stream_handles(stream_bytes, None, &mut finder).is_ok()
    && !finder.had_diagnostics()
    && !finder.has_pending_operands();
```

The decoded Form byte buffer has no document-owned indirect references, so the
parser context remains `None`. Keep `record_direct_names` and the `Some`/`None`
result behavior unchanged. Delete `ResourceCallbacks`, its inline-header state,
and `is_builtin_inline_image_cs`; qpdf's `ResourceFinder` does not classify
inline-image header names as resource-operator references.

- [ ] **Step 2: Add the qpdf-derived malformed inline-header regression.**

Keep a focused Form-scan test for `BI 1 /Foo ID payload EI`. The live qpdf
11.9.0 pruning probe removes unused `/Font` entries and leaves the category
dictionary present, so this malformed inline header must not create a
flpdf-only veto. The existing malformed ordinary-content test continues to
cover parser failures that do abort Form pruning.

- [ ] **Step 3: Run the route contract and Form resource tests.**

Run:

```bash
cargo test -p flpdf --test legacy_route_cutover_tests resource_pruning_callbacks_use_only_the_handle_parser_route
cargo test -p flpdf --lib resources
```

Expected: both the route contract and every resource pruning unit test pass.

### Task 3: Remove the unused ResourceFinder raw adapter and migrate tests

**Files:**
- Modify: `crates/flpdf/src/resource_finder.rs:7-190,225-328`
- Modify: `crates/flpdf/src/resource_replacer.rs:5,88-112`
- Modify: `crates/flpdf/src/content_stream.rs:181-345`

- [ ] **Step 1: Make ResourceFinder and ResourceReplacer use handle parsing.**

Import `parse_content_stream_handles` and `ObjectHandleParserCallbacks`, then
make `find` call:

```rust
fn find(input: &[u8]) -> Result<ResourceFinder> {
    let mut finder = ResourceFinder::default();
    parse_content_stream_handles(input, None, &mut finder)?;
    Ok(finder)
}
```

Make `dump_flpdf_resource_finder` use the same handle parser so its output is
compared with the pinned qpdf probe through the canonical route.

Change `resource_replacer.rs::replace_resource_names` to call
`parse_content_stream_handles(input, None, &mut finder)`, preserving its
warning-only inline-image EOF behavior and downstream token-filter offsets.

- [ ] **Step 2: Delete only raw-route implementation and tests.**

Remove `handle_object_borrowed`, the `ParserCallbacks` import, and
`impl ParserCallbacks for ResourceFinder`. Delete the
`borrowed_large_operand_remains_owned_by_the_caller` test and the
`borrowed_name_is_retained_for_the_following_operator` test because their only
contract is the qpdf-incompatible raw callback boundary. Keep the operator,
offset, duplicate, diagnostic, and live ResourceFinder tests. Delete the
now-unused `parse_content_stream_data_recovering_inline_image_eof` helper and
its dedicated callback-error test; the handle parser owns qpdf's warning-only
inline-image EOF behavior.

- [ ] **Step 3: Run the ResourceFinder differential probe.**

Run the repository's exact pinned qpdf probe workflow:

```bash
scripts/qpdf-tokenizer-diff.sh
```

Expected: the five ignored differential tests, including
`resource_finder::tests::qpdf_resource_finder_differential`, pass against the
freshly built qpdf 11.9.0 probe.

### Task 4: Verify the complete affected behavior and documentation

**Files:**
- Modify: `crates/flpdf/src/resources.rs` module documentation if it still describes the raw Form parser
- Modify: `docs/qpdf-correspondence.md` only if the ResourceFinder row still claims a raw production callback

- [ ] **Step 1: Run all affected focused tests.**

```bash
cargo test -p flpdf --lib resources
cargo test -p flpdf --lib resource_finder
cargo test -p flpdf --lib resource_replacer
cargo test -p flpdf --test legacy_route_cutover_tests
cargo test -p flpdf --test page_object_helper_tests
cargo test -p flpdf --test page_extract_tests
```

Keep the existing qpdf-deviation terminal-chase comments and tests unchanged;
the route contract must not silently expand to that separate bridge. The Form
resource suite includes the qpdf-derived malformed inline-header regression;
it does not treat inline-image `/CS` names as `/Resources/ColorSpace` uses.

- [ ] **Step 2: Confirm the route census and docs.**

Run:

```bash
rg -n 'parse_content_stream_data|impl ParserCallbacks|handle_object_borrowed|Vec<Object>|Object::Operator|Object::InlineImage|parse_content_stream_data_recovering_inline_image_eof' crates/flpdf/src/resources.rs crates/flpdf/src/resource_finder.rs crates/flpdf/src/resource_replacer.rs crates/flpdf/src/content_stream.rs
git diff --check
```

Expected: no matches in the production callback sections; raw parser matches
may remain only in unrelated test modules or the shared legacy parser.

### Task 5: Run all local quality gates

**Files:**
- No additional files unless a verification failure identifies a real regression.

- [ ] **Step 1: Run formatting, focused tests, docs, and Clippy.**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 2: Run workspace and repository checks.**

```bash
cargo test --workspace
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [ ] **Step 3: Run fresh parent-relative patch coverage.**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: `flpdf` reports `PASS (100%)` with zero uncovered changed
executable lines.

### Task 6: Rebase, publish Draft PR, and verify CI without merging

- [ ] **Step 1: Fetch and rebase immediately before publishing.**

```bash
git fetch origin main
git rebase origin/main
git status --short --branch
```

Rerun the affected tests and fresh patch coverage after any rebase. Preserve
the existing NNTree PR and all unrelated dirty worktrees.

- [ ] **Step 2: Push and create the Draft PR.**

```bash
git push --set-upstream origin feature/flpdf-egzr-3-2-6-36-resource-callback-handles
gh pr create --draft --base main --head feature/flpdf-egzr-3-2-6-36-resource-callback-handles \
  --title "refactor: migrate Form resource callbacks to ObjectHandle" \
  --body-file /tmp/flpdf-egzr-3-2-6-36-resource-callback-handles-pr.md
```

The body must include the qpdf source/probe, canonical route, removed raw
route, focused/full tests, and patch coverage. It must not contain an
instruction to merge or block integration.

- [ ] **Step 3: Wait for every CI check and re-query review data.**

```bash
PR_NUMBER="$(gh pr view --json number --jq '.number')"
gh pr checks "$PR_NUMBER"
gh pr view "$PR_NUMBER" --json state,isDraft,mergeStateStatus,reviewDecision,statusCheckRollup
gh api "repos/fulgur-rs/flpdf/pulls/${PR_NUMBER}/reviews"
gh api "repos/fulgur-rs/flpdf/pulls/${PR_NUMBER}/comments"
```

Do not mark pending or missing checks green. Revalidate any review finding
against the pinned qpdf source before changing code.

- [ ] **Step 4: Mark ready only after all checks, including patch coverage, pass.**

```bash
gh pr ready "$PR_NUMBER"
```

Do not merge the PR.

### Task 7: Persist evidence and finish the handoff

- [ ] **Step 1: Append implementation and PR evidence to Beads.**

Append the final worktree, commits, qpdf citations/probe, RED→GREEN result,
focused/full verification, CI result, PR number, and remaining aggregate scope
to `flpdf-egzr.3.2.6.36` without overwriting its readiness note.

- [ ] **Step 2: Verify graph and push Beads.**

```bash
bd show flpdf-egzr.3.2.6.36 --short
bd dep cycles
bd dolt push
```

Expected: no dependency cycles and output containing `Push complete.` Keep the
child and its page-group aggregate open unless their complete acceptance
criteria are independently proven.

- [ ] **Step 3: Perform final readback.**

```bash
git status --short --branch
git worktree list --porcelain
gh pr view "$PR_NUMBER" --json state,isDraft,mergeStateStatus,headRefOid
```

The feature worktree must be clean, main and unrelated worktrees untouched, and
the PR open and unmerged.
