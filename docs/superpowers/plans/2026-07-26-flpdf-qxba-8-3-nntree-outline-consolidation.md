# NNTree Outline Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete `flpdf-qxba.8.3` by routing outline named-destination lookup and repair through the shared qpdf-compatible `NameTree`, deleting the second NNTree implementation from `outline_document_helper.rs`, and preserving qpdf 11.9.0 warnings, mutation shape, and byte behavior.

**Architecture:** `crates/flpdf/src/nntree.rs` remains the sole owner of NNTree traversal, targeted lookup, direct-kid preflight, structural repair, allocation, splitting, and warning behavior. `outline_document_helper.rs` keeps only outline-specific catalog selection, PDF-string-to-UTF-8 lookup preparation, direct-root writeback to `/Catalog /Names /Dests`, and destination-object resolution. A source-boundary integration test prevents the private NNTree implementation from returning.

**Tech Stack:** Rust 2021 workspace; existing `Pdf`, `Object`, `NameTree`, and reference-chain APIs; qpdf 11.9.0 as source and live behavioral oracle; `qpdf-zlib-compat` byte gates; Beads; Git worktrees; Cargo tests, Clippy, strict private-item rustdoc, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 is the behavior oracle. Resolve its read-only source with `scripts/fetch-qpdf-source.sh --print-path`; do not clone or edit another qpdf tree.
- Base the work on `origin/main`, which already contains PR #556 (`flpdf-qxba.8.1`), PR #557 (`flpdf-qxba.8.2`), and PR #558's split-invariant follow-up.
- Use branch `feature/flpdf-qxba-8-3-outline` in a worktree created with `superpowers:using-git-worktrees`.
- Do not add a depth cap to outline named-destination lookup. The shared `NameTree` must retain its default unlimited traversal for this consumer.
- Preserve silent `Object::Null` results for missing, scalar, or non-dictionary `/Names /Dests` stores.
- Preserve qpdf's exact targeted-search visit order, warning order and wording, one-repair retry, direct-kid indirecting, holder chains, 16/17 split order, and object-number allocation order.
- A repaired direct `/Dests` root must be written back into the same direct `/Names` dictionary or terminal indirect `/Names` holder. An indirect `/Dests` root retains its original reference chain and is updated by `NameTree`.
- Do not change the public `NameTree` API or add an outline-specific NNTree adapter type. The shared API added by `flpdf-qxba.8.2` is sufficient.
- Do not change destination precedence: explicit `/Dest` still suppresses `/A`; only `/S /GoTo /D` contributes from an action dictionary; modern name-tree destinations still win over legacy `/Catalog /Dests`.
- Keep existing malformed-tree and live-qpdf tests as the observable behavior contract. Do not re-bless expected warnings or object shapes during this consolidation.
- Oracle correction (2026-07-26): live qpdf 11.9.0 exits 2 when the first
  `/Names` key is invalid; repair reinserts that first pair and then fails
  rather than warning, skipping it, and succeeding. Replace only the
  contradicted `invalid_first_name_tree_key_does_not_enable_the_lower_bound_shortcut`
  expectation with this observed fatal behavior. Keep every other existing
  warning and object-shape expectation unchanged unless a dedicated live-qpdf
  fixture proves another contradiction.
- Warning-context correction (2026-07-26): `NNTree.cc::warn` always supplies
  `get_description(node)`, including direct roots and repair iteration.
  Dedicated qpdf runs confirmed outer direct-root context with inner indirect
  child context, and outer/inner indirect contexts for an indirect root.
  Preserve those node contexts in flpdf NNTree diagnostics; replace raw-message
  expectations only where the qpdf source and these live fixtures prove the
  previous outline-private formatting was incomplete.
- The committed branch must have 100% patch coverage for changed executable lines against `origin/main`.
- Required final gates are formatting, workspace Clippy with all targets/features, focused tests, workspace tests, strict private-item rustdoc, live qpdf 11.9.0 oracle tests, and the relevant byte-parity suites.

## File Map

- Modify `crates/flpdf/src/outline_document_helper.rs`
  - Keep outline traversal and destination-store selection.
  - Replace the private NNTree lookup/repair path with `crate::NameTree`.
  - Keep the direct `/Dests` root catalog writeback adapter.
  - Delete private lookup, binary search, preflight, enumeration, repair, split, and NNTree-only unit tests.
- Modify `crates/flpdf/tests/outline_document_helper_tests.rs`
  - Add the source-boundary contract proving that outline lookup constructs `NameTree` and contains no second NNTree algorithm.
  - Retain the existing behavioral and live-qpdf matrices unchanged.
- Modify `docs/qpdf-correspondence.md`
  - Mark `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` complete and point to the shared engine plus compatibility wrappers.
- Do not modify `crates/flpdf/src/nntree.rs`
  - Its `NameTree` API and repair tests already cover the required outline behavior.

## Interfaces

- Consumes:
  - `crate::NameTree::new(root: Object, auto_repair: bool) -> NameTree`
  - `NameTree::find_object<R, K>(&mut self, pdf: &mut Pdf<R>, key: K) -> Result<Option<Object>>`
  - `NameTree::root(&self) -> &Object`
  - `NameTree::into_root(self) -> Object`
  - `crate::json_inspect::qpdf_utf8_value(bytes: &[u8]) -> Vec<u8>`
  - `crate::json_inspect::qpdf_new_unicode_utf8_value(bytes: &[u8]) -> Vec<u8>`
  - `crate::ref_chain::resolve_ref_chain(pdf, value) -> Result<(Object, Option<ObjectRef>)>`
- Preserves:
  - `OutlineDocumentHelper::resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<Object>`
  - `resolve_terminal_object(pdf, value) -> Result<Object>`
- Produces:
  - One consumer call to `crate::NameTree::new(dests_root, true)`.
  - Direct-root writeback only when `tree.root() != &original_root`.
  - No private NNTree enum, cursor, binary search, traversal, repair, or split implementation in `outline_document_helper.rs`.

---

### Task 1: Cut outline lookup over to the shared `NameTree`

**Files:**

- Read: `crates/flpdf/src/nntree.rs`
- Modify: `crates/flpdf/tests/outline_document_helper_tests.rs`
- Modify: `crates/flpdf/src/outline_document_helper.rs:46-50`
- Modify: `crates/flpdf/src/outline_document_helper.rs:363-406`
- Delete private algorithm block: `crates/flpdf/src/outline_document_helper.rs:449-1218`
- Retain and rename catalog adapter: `crates/flpdf/src/outline_document_helper.rs:1220-1252`
- Delete NNTree-only unit tests: `crates/flpdf/src/outline_document_helper.rs:1327-1415`

**Interfaces:**

- Consumes: merged `NameTree` implementation on `origin/main`.
- Consumes:
  - `NameTree::new(Object, true)`
  - `NameTree::find_object(&mut Pdf<R>, &[u8])`
  - `NameTree::root()` and `NameTree::into_root()`
- Produces:
  - Clean worktree `feature/flpdf-qxba-8-3-outline` with claimed Bead `flpdf-qxba.8.3`.
  - `resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<Object>` with unchanged caller-visible behavior.
  - `write_back_direct_dests_root(pdf, Dictionary) -> Result<()>` as catalog wiring only.

- [ ] **Step 1: Refresh and verify the merged dependency**

Run from `/home/ubuntu/flpdf`:

```bash
git fetch origin
git merge-base --is-ancestor 8675dd9d origin/main
git status --short --branch
bd show flpdf-qxba.8.3
```

Expected: the ancestry command exits 0, the current checkout is clean apart from the separately saved plan file, and Beads reports `.8.3` open with `.8.2` closed.

- [ ] **Step 2: Create the worktree with the required skill**

Invoke `superpowers:using-git-worktrees` and have it create:

```text
worktree: /home/ubuntu/flpdf/.worktrees/flpdf-qxba-8-3-outline
branch: feature/flpdf-qxba-8-3-outline
base: origin/main
```

Then enter the worktree and verify it:

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-qxba-8-3-outline
git branch --show-current
git status --short
git log -1 --oneline
```

Expected: branch is `feature/flpdf-qxba-8-3-outline`, status is clean, and HEAD is the current `origin/main`.

- [ ] **Step 3: Claim the Bead**

```bash
bd update flpdf-qxba.8.3 --claim
bd show flpdf-qxba.8.3
```

Expected: `flpdf-qxba.8.3` is `IN_PROGRESS` and assigned to the current owner.

- [ ] **Step 4: Confirm the oracle and source mapping**

```bash
qpdf --version
QPDF_ORACLE="$(scripts/fetch-qpdf-source.sh --print-path)"
rg -n 'NNTreeImpl::(find|findInternal|repair)|NNTreeIterator::(deepen|split)' "$QPDF_ORACLE/libqpdf/NNTree.cc"
```

Expected: qpdf reports `11.9.0`; the source matches `NNTreeImpl::repair` around line 807, `find` around line 820, and `findInternal` around line 837.

- [ ] **Step 5: Run the existing characterization baseline**

```bash
cargo test -p flpdf --test nntree_tests
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf-cli --test cli_tests json_key_outlines_and_qpdf_repairs_before_raw_object_projection -- --nocapture
```

Expected: all three commands pass before refactoring. These tests pin the shared engine, the existing outline behavior, and JSON's repair-before-raw-object projection.

---

- [ ] **Step 6: Add the failing source-boundary test**

Add this test immediately before `named_destination_lookup_handles_qpdf_node_shapes` in `crates/flpdf/tests/outline_document_helper_tests.rs`:

```rust
#[test]
fn outline_named_destination_lookup_uses_only_shared_nntree_engine() {
    const SOURCE: &str = include_str!("../src/outline_document_helper.rs");

    assert!(
        SOURCE.contains("crate::NameTree::new("),
        "outline named-destination lookup must construct the shared NameTree"
    );

    for private_algorithm in [
        "enum NameTreeLookup",
        "struct NameTreeStructuralError",
        "fn find_name_tree_value<",
        "fn name_tree_begin_preflight<",
        "fn name_tree_node<",
        "fn find_name_tree_leaf_value(",
        "fn select_name_tree_kid<",
        "fn qpdf_name_tree_binary_search<",
        "fn name_tree_kid_ordering<",
        "fn enumerate_name_tree_entries<",
        "fn repair_name_tree<",
        "fn build_repaired_name_tree_root<",
        "enum RepairedNameTreeNodeKind",
        "fn split_repaired_name_tree_node(",
        "fn repaired_name_tree_dictionary(",
        "fn repaired_name_tree_limit(",
    ] {
        assert!(
            !SOURCE.contains(private_algorithm),
            "outline_document_helper.rs still owns private NNTree algorithm: {private_algorithm}"
        );
    }
}
```

- [ ] **Step 7: Run the contract and verify RED**

```bash
cargo test -p flpdf --test outline_document_helper_tests outline_named_destination_lookup_uses_only_shared_nntree_engine -- --nocapture
```

Expected: FAIL with `outline named-destination lookup must construct the shared NameTree`.

- [ ] **Step 8: Replace the outline-specific lookup/repair call**

Replace `resolve_name_tree_node_dest` with:

```rust
fn resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<Object> {
    let lookup = crate::json_inspect::qpdf_new_unicode_utf8_value(
        &crate::json_inspect::qpdf_utf8_value(bytes),
    );
    let Some(Object::Dictionary(mut names)) = self.catalog_value_terminal("Names")? else {
        return Ok(Object::Null);
    };
    let Some(dests_root) = names.remove("Dests") else {
        return Ok(Object::Null);
    };
    match &dests_root {
        Object::Dictionary(_) => {}
        Object::Reference(_) => {
            if !matches!(
                crate::ref_chain::resolve_ref_chain(self.pdf, &dests_root)?.0,
                Object::Dictionary(_)
            ) {
                return Ok(Object::Null);
            }
        }
        _ => return Ok(Object::Null),
    }

    let original_root = dests_root.clone();
    let mut tree = crate::NameTree::new(dests_root, true);
    let found = tree.find_object(self.pdf, lookup.as_slice());
    if tree.root() != &original_root {
        if let Object::Dictionary(repaired_root) = tree.into_root() {
            write_back_direct_dests_root(self.pdf, repaired_root)?;
        }
    }

    match found? {
        Some(value) => resolve_terminal_object(self.pdf, value),
        None => Ok(Object::Null),
    }
}
```

Do not call `set_max_depth`; outline lookup has no hidden `/Kids` depth limit. Normalize the PDF-string-decoded bytes once before `find_object`: lookup compares the supplied UTF-8 key directly, while `NameKey::to_object` performs new-Unicode-string normalization only for inserted keys. Keep the `Result` unwrapped until after direct-root writeback so qpdf's direct-kid conversion is retained even when a short pair makes the lookup fatal.

- [ ] **Step 9: Keep only the direct-root catalog adapter**

Rename `replace_direct_dests_root` to `write_back_direct_dests_root` and retain this exact consumer-specific implementation:

```rust
fn write_back_direct_dests_root<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    repaired_root: Dictionary,
) -> Result<()> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let Object::Dictionary(mut catalog) = pdf.resolve(catalog_ref)? else {
        return Ok(());
    };
    let Some(names_value) = catalog.get("Names").cloned() else {
        return Ok(());
    };

    match names_value {
        Object::Dictionary(mut names) => {
            names.insert("Dests", Object::Dictionary(repaired_root));
            catalog.insert("Names", Object::Dictionary(names));
            pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        }
        value @ Object::Reference(_) => {
            let (terminal, terminal_ref) =
                crate::ref_chain::resolve_ref_chain(pdf, &value)?;
            let Some(mut names) = terminal.into_dict() else {
                return Ok(());
            };
            let Some(terminal_ref) = terminal_ref else {
                return Ok(());
            };
            names.insert("Dests", Object::Dictionary(repaired_root));
            pdf.set_object(terminal_ref, Object::Dictionary(names));
        }
        _ => {}
    }
    Ok(())
}
```

This adapter is not an NNTree algorithm. It performs the representation substitution required because a direct `Object::Dictionary` root is owned by `NameTree`, while qpdf's `QPDFObjectHandle` mutates shared object identity.

- [ ] **Step 10: Delete the private NNTree implementation**

Delete these definitions from `outline_document_helper.rs`:

```text
NameTreeLookup
NameTreeStructuralError
name_tree_iterator_warning
NameTreeKidSelection
NameTreeKidOrdering
NameTreeBinarySearch
NameTreeFirstBoundary
find_name_tree_value
name_tree_begin_preflight
name_tree_node
find_name_tree_leaf_value
select_name_tree_kid
qpdf_name_tree_binary_search
name_tree_kid_ordering
enumerate_name_tree_entries
repair_name_tree
build_repaired_name_tree_root
RepairedNameTreeNodeKind
RepairedNameTreeNode
repaired_name_tree_node_overflows
split_repaired_name_tree_node
repaired_name_tree_dictionary
repaired_name_tree_limit
qpdf_utf8_tests
```

Reduce the imports to the values still used by outline traversal and direct-root writeback:

```rust
use crate::outline::{OutlineId, OutlineItem, OutlineTree};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};
```

The shared engine retains key-normalization tests in `nntree.rs`; do not move the deleted outline-only tests into another production module.

- [ ] **Step 11: Run the ownership contract and focused behavior suite**

```bash
cargo fmt --all
cargo test -p flpdf --test outline_document_helper_tests outline_named_destination_lookup_uses_only_shared_nntree_engine -- --nocapture
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test nntree_tests
```

Expected: GREEN. The outline suite must preserve all existing warning strings, repaired object graphs, direct-root updates, holder chains, targeted binary-search order, and split-order assertions unchanged.

- [ ] **Step 12: Mechanically verify the production boundary**

```bash
rg -n 'NameTreeLookup|NameTreeStructuralError|name_tree_begin_preflight|qpdf_name_tree_binary_search|enumerate_name_tree_entries|repair_name_tree|RepairedNameTreeNode|split_repaired_name_tree_node' crates/flpdf/src/outline_document_helper.rs
rg -n 'crate::NameTree::new|write_back_direct_dests_root' crates/flpdf/src/outline_document_helper.rs
```

Expected: the first command has no matches. The second finds one `NameTree` construction, one direct-root writeback call, and one adapter definition.

- [ ] **Step 13: Commit the production consolidation**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "refactor(nntree): consolidate outline destination lookup"
```

Expected: the commit removes the private NNTree implementation and keeps the complete focused suite green.

---

### Task 2: Run the live-oracle and byte matrices, then mark correspondence complete

**Files:**

- Modify: `docs/qpdf-correspondence.md:145`
- Test unchanged: `crates/flpdf/tests/outline_document_helper_tests.rs`
- Test unchanged: `crates/flpdf-cli/tests/cli_tests.rs`
- Test unchanged: `crates/flpdf-cli/tests/cli_outline_pagelabels_qpdf.rs`
- Test unchanged: `crates/flpdf/tests/page_extract_outline_nullout_tests.rs`
- Test unchanged: `crates/flpdf/tests/cmp_linearize_tests.rs`
- Test unchanged: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Interfaces:**

- Consumes: qpdf 11.9.0 executable and the consolidated outline consumer.
- Produces: oracle evidence for malformed trees and byte behavior, plus a complete correspondence row.

- [ ] **Step 1: Run every ignored live-qpdf outline oracle**

```bash
qpdf --version
cargo test -p flpdf --test outline_document_helper_tests qpdf_ -- --ignored --nocapture
```

Expected: qpdf reports `11.9.0` and all 12 ignored `qpdf_` oracle tests pass, including direct-child conversion, short pairs, scalar `/Dests`, search order, empty roots, NUL repair, structural repair, missing limits, 33-pair splitting, destination shape, and malformed explicit UTF-8.

If any live-oracle test fails, stop before changing expectations: capture the exact flpdf/qpdf object or warning difference and revise this plan against `libqpdf/NNTree.cc`.

- [ ] **Step 2: Run JSON and page-rebuild integration gates**

```bash
cargo test -p flpdf-cli --test cli_tests json_key_outlines_and_qpdf_repairs_before_raw_object_projection -- --nocapture
cargo test -p flpdf-cli --test cli_outline_pagelabels_qpdf
cargo test -p flpdf --test page_extract_outline_nullout_tests
```

Expected: all tests pass; the CLI JSON path repairs before projecting raw qpdf objects, and page extraction keeps outline/named-destination null-out parity.

- [ ] **Step 3: Run the outline byte-parity filters**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests outlines_
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests outlines_
```

Expected: every matching classic-xref and object-stream outline comparison passes byte-for-byte against its existing qpdf golden.

- [ ] **Step 4: Update the correspondence row**

Replace the `T2-2` row in `docs/qpdf-correspondence.md` with:

```markdown
| `QPDFNameTreeObjectHelper` / `QPDFNumberTreeObjectHelper` / `NNTree.cc` | 1394 | `nntree.rs`(3909) + `name_number_tree.rs`(838: compatibility wrapper) + consumer adapters | ✅ |
```

This marks the component complete only after ordinary consumers and outline repair all share `nntree.rs`.

- [ ] **Step 5: Verify the documentation diff and commit**

```bash
git diff --check
git diff -- docs/qpdf-correspondence.md
git add docs/qpdf-correspondence.md
git commit -m "docs(nntree): mark qpdf component correspondence complete"
```

Expected: one table row changes from `🔀 → T2-2` to `✅`; no unrelated correspondence entries change.

---

### Task 3: Run final quality gates, coverage, review, and publish

**Files:**

- Verify: all changed files from Tasks 2-3.
- Update tracker: `flpdf-qxba.8.3`.

**Interfaces:**

- Consumes: two committed implementation/documentation changes from Tasks 1-2 on `feature/flpdf-qxba-8-3-outline`.
- Produces: fully verified pushed branch, closed Bead, and draft PR targeting `main`.

- [ ] **Step 1: Run formatting and static analysis**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: both commands exit 0 with no warnings.

- [ ] **Step 2: Run focused and full test gates**

```bash
cargo test -p flpdf --test nntree_tests
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf-cli --test cli_tests json_key_outlines_and_qpdf_repairs_before_raw_object_projection -- --nocapture
cargo test -p flpdf-cli --test cli_outline_pagelabels_qpdf
cargo test -p flpdf
cargo test
```

Expected: all focused, crate, and workspace tests pass.

- [ ] **Step 3: Run the strict private-item rustdoc gate**

```bash
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

Expected: documentation builds with all three rustdoc lints denied.

- [ ] **Step 4: Run authoritative patch coverage**

The working tree must be clean because `scripts/patch-coverage.sh` compares committed `HEAD`:

```bash
git status --short
scripts/patch-coverage.sh --base origin/main
```

Expected: status is empty and patch coverage reports 100% for every changed executable line under `crates/flpdf/src`.

- [ ] **Step 5: Review the final diff against the issue**

Invoke `superpowers:requesting-code-review` and require the review to check:

```text
- outline_document_helper.rs contains no second NNTree algorithm
- NameTree lookup retains unlimited outline depth
- direct and indirect /Dests roots preserve holder shape
- warning order and one-repair retry remain qpdf 11.9.0-compatible
- no expected oracle output was re-blessed
- docs/qpdf-correspondence.md marks completion only after all consumers migrated
```

Address any validated finding, rerun the affected focused test, and repeat Steps 1-4 before publishing.

- [ ] **Step 6: Close and push Beads state**

```bash
bd close flpdf-qxba.8.3 --reason "Outline named-destination lookup and repair now use the shared NameTree; duplicate NNTree algorithms removed; qpdf oracle, byte, quality, and 100% patch-coverage gates passed"
bd dolt push
```

Expected: `.8.3` is closed and the Dolt push completes.

- [ ] **Step 7: Push the Git branch**

```bash
git status --short --branch
git push -u origin feature/flpdf-qxba-8-3-outline
```

Expected: the worktree is clean and the remote branch is created successfully.

- [ ] **Step 8: Open the dependent-layer draft PR**

```bash
gh pr create --draft \
  --base main \
  --head feature/flpdf-qxba-8-3-outline \
  --title "refactor(nntree): consolidate outline destination lookup" \
  --body "Closes flpdf-qxba.8.3

Routes outline named-destination lookup and auto-repair through the shared qpdf-compatible NameTree, removes the private outline NNTree implementation, and marks the qpdf component correspondence complete.

Verified with the malformed-tree and live qpdf 11.9.0 oracle matrices, outline byte gates, workspace fmt/clippy/tests, strict private-item rustdoc, and 100% patch coverage against origin/main."
```

Expected: GitHub creates one draft PR from `feature/flpdf-qxba-8-3-outline` to `main`.

- [ ] **Step 9: Record final state**

```bash
gh pr view --json number,url,state,isDraft,baseRefName,headRefName
git status --short --branch
bd show flpdf-qxba.8.3
```

Expected: the PR is open and draft with base `main`; the branch is clean and tracking origin; Beads reports `.8.3` closed.
