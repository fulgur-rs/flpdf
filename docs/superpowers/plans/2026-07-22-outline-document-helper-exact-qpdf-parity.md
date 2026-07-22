# OutlineDocumentHelper Exact qpdf Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's outline-specific policy with the observable `QPDFOutlineDocumentHelper`/`QPDFOutlineObjectHelper` behavior of qpdf 11.9.0, while keeping an idiomatic arena-backed Rust API.

**Architecture:** `outline.rs` becomes the owned arena model (`OutlineTree`, `OutlineId`, `OutlineItem`, preorder iterator, lazy page index). `outline_document_helper.rs` becomes the qpdf-compatible document adapter and tree builder: it resolves only the current indirect cursor, preserves direct values, materializes the node at depth 51 without expanding it, and computes qpdf-compatible title/count/destination fields. `json_inspect.rs` projects the materialized tree to qpdf JSON v2 instead of walking raw outline links independently.

**Tech Stack:** Rust 2021, existing `Pdf`/`Object` reader and writer, `OnceLock<BTreeMap<Option<ObjectRef>, Vec<OutlineId>>>` for the lazy page index, qpdf 11.9.0 in `/tmp/qpdf-1190` and on `PATH` as the behavioral oracle, Beads (`bd`), stacked Git branches, and `cargo llvm-cov` through `scripts/patch-coverage.sh`.

## Fixed Decisions

- qpdf 11.9.0 is the oracle. Do not retain a flpdf behavior because it is more validating, more standards-oriented, or more convenient.
- This is a pre-1.0 breaking change. Remove `Dest`, normalized named-destination enumeration, outline diagnostics, `/SE` pruning, configurable outline depth, the legacy flat `outline_items` API, and the recursive `OutlineNode` model.
- Preserve raw PDF data. Removing typed `/SE` and checker/pruner APIs must not delete `/SE`, `/A`, unknown keys, name trees, or legacy `/Dests` during an ordinary read/write round trip.
- Do not add holder-chain traversal. An indirect cursor is resolved once to inspect its value; a resolved bare reference is not recursively followed.
- Direct outline items use `source_ref: None`. Never expose `0 0 R` as a fake `ObjectRef` or an arena identity.
- Only indirect source refs participate in the outline seen sets. Direct objects are admitted on each occurrence.
- Top-level depth is 1. Depth 51 is materialized with no kids. Depth 52 is not materialized. Depth overflow is not an error.
- The final JSON item key order is exactly `dest`, `destpageposfrom1`, `kids`, `object`, `open`, `title`.
- Every production behavior change starts with a test that is observed failing. Do not write implementation code before the RED command in that task.
- Every stack branch gets focused tests, full workspace tests, Clippy, and 100% changed-line patch coverage before its issue is closed.

## Stack Layout

All branches live in the existing worktree and form one dependent stack. Use the `gh-stack` skill during execution so each branch and PR has the correct parent.

| Layer | Beads | Branch | Parent |
|---|---|---|---|
| Design base | `flpdf-9hc.38` | `epic/flpdf-9hc-38-outline-exact-parity` | `main` |
| Remove local policy | `flpdf-9hc.38.1` | `stack/flpdf-9hc-38-1-remove-outline-policy` | design base |
| Arena/direct values | `flpdf-x5yi` | `stack/flpdf-x5yi-direct-outline-values` | remove-policy |
| Depth boundary | `flpdf-3g9k` | `stack/flpdf-3g9k-outline-depth-50` | direct-values |
| Title/count | `flpdf-guru` | `stack/flpdf-guru-outline-scalars` | depth |
| Page index | `flpdf-7nu4` | `stack/flpdf-7nu4-outline-page-index` | scalars |
| JSON v2 | `flpdf-9hc.38.2` | `stack/flpdf-9hc-38-2-outline-json-v2` | page-index |

Before starting the first layer, verify the base:

```bash
git status --short --branch
git log -1 --oneline
bd show flpdf-9hc.38
qpdf --version
```

Expected: clean `epic/flpdf-9hc-38-outline-exact-parity`, design commit `a905bb0f`, open epic, and `qpdf version 11.9.0`.

---

### Task 1: Remove qpdf-incompatible outline policy (`flpdf-9hc.38.1`)

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/tests/outline_document_helper_tests.rs`
- Modify: `crates/flpdf/tests/outline_pagelabels_e2e_tests.rs`
- Modify: `crates/flpdf/tests/page_merge_tests.rs`

**Remove now:**

- `Dest` and its recursive normalizer/resolver used by enumeration.
- `legacy_dests`, `name_tree_dests`, `check_legacy_dests`, `check_name_tree_dests`.
- `check_links`, `check_links_with_max_depth`, `check_outline_links`.
- `prune_se`, `prune_se_with_max_depth`, `prune_outline_se`, `prune_outline_se_with_max_depth`.
- `OutlineNode::se`.
- public `MAX_OUTLINE_WALK_DEPTH` and `get_root_with_max_depth`.
- `Diagnostic`, `Diagnostics`, `Severity`, and `BTreeSet` imports that exist only for deleted policy.

Keep the qpdf-compatible node destination resolver added under `flpdf-nm2o`; it is not the normalized `Dest` API being removed.

- [ ] **Step 1: Create and claim the stack layer**

```bash
git switch -c stack/flpdf-9hc-38-1-remove-outline-policy
bd update flpdf-9hc.38.1 --claim
```

Expected: the new branch points at `a905bb0f`; Beads reports `flpdf-9hc.38.1` as `IN_PROGRESS`.

- [ ] **Step 2: Add compile-fail contracts before deleting public names**

At the end of the module-level documentation in `outline_document_helper.rs`, add separate blocks so every removed group independently proves it no longer compiles:

```rust
//! qpdf-incompatible outline policy APIs were removed before flpdf 1.0.
//!
//! ```compile_fail
//! use flpdf::Dest;
//! ```
//!
//! ```compile_fail
//! use flpdf::{check_legacy_dests, check_name_tree_dests, check_outline_links};
//! ```
//!
//! ```compile_fail
//! use flpdf::{prune_outline_se, prune_outline_se_with_max_depth};
//! ```
//!
//! ```compile_fail
//! # use flpdf::Pdf;
//! # use std::io::Cursor;
//! # let mut pdf = Pdf::open(Cursor::new(Vec::<u8>::new())).unwrap();
//! let _ = pdf.outline().get_root_with_max_depth(10);
//! ```
```

Run:

```bash
cargo test -p flpdf --doc
```

Expected RED: all four new `compile_fail` examples fail because the named APIs still compile.

- [ ] **Step 3: Replace typed-policy preservation assertions with raw round-trip assertions**

In `outline_pagelabels_e2e_tests.rs`, replace `.se`, `legacy_dests()`, and `name_tree_dests()` assertions with raw object checks. Add this helper:

```rust
fn dict_value(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef, key: &str) -> Object {
    let Object::Dictionary(dict) = pdf.resolve(object_ref).unwrap() else {
        panic!("{object_ref} must resolve to a dictionary");
    };
    dict.get(key).cloned().unwrap_or(Object::Null)
}
```

For the `/SE` round trip, assert the underlying key rather than a typed field:

```rust
assert_eq!(
    dict_value(&mut reopened, ObjectRef::new(20, 0), "SE"),
    Object::Reference(ObjectRef::new(30, 0)),
    "ordinary rewriting must preserve raw /SE even though no outline /SE policy API remains"
);
```

For both destination stores, resolve the catalog and compare the raw `/Dests` and `/Names` values before and after writing. Do not normalize either store into `Dest`.

Run the new raw-preservation test by exact test name before deleting code:

```bash
cargo test -p flpdf --test outline_pagelabels_e2e_tests raw_outline_policy_keys_survive_write_round_trip -- --nocapture
```

Expected GREEN: raw `/SE`, `/Dests`, and `/Names` values survive with the existing writer.

- [ ] **Step 4: Delete policy code and migrate tests**

Delete the symbols in **Remove now** from `outline_document_helper.rs` and their re-exports from `lib.rs`. Keep a private temporary construction cap for the current recursive-value model until Task 3, but do not expose it or return a caller-selected depth error:

```rust
const TEMPORARY_OUTLINE_BUILD_DEPTH: usize = 5_000;

pub fn get_root(&mut self) -> Result<Vec<OutlineNode>> {
    let Some(first) = self.outline_root_first()? else {
        return Ok(Vec::new());
    };
    let mut visited = BTreeSet::new();
    self.build_siblings(first, 0, None, &mut visited, TEMPORARY_OUTLINE_BUILD_DEPTH)
}
```

This constant is deliberately private and disappears in Task 3. It only keeps the intermediate stack branch compiling; it is not a final behavior commitment.

In tests:

- delete tests whose only subject is normalization, diagnostics, pruning, or a caller-selected depth error;
- keep destination accessor tests from `flpdf-nm2o`;
- replace `/SE` typed checks with the raw checks from Step 3;
- delete `check_name_tree_dests_flags_dest_nulled_by_merge` from `page_merge_tests.rs`; keep the preceding raw merge assertions that already prove null-out behavior;
- update comments that claim flpdf validates or prunes these values.

Verify no removed API remains:

```bash
rg -n 'Dest\b|legacy_dests|name_tree_dests|check_outline_links|check_legacy_dests|check_name_tree_dests|prune_outline_se|get_root_with_max_depth|MAX_OUTLINE_WALK_DEPTH|\.se\b' crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/lib.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/tests/outline_pagelabels_e2e_tests.rs crates/flpdf/tests/page_merge_tests.rs
```

Expected: no matches except PDF dictionary string literals such as `"Dest"` and the intentional compile-fail documentation.

- [ ] **Step 5: Verify GREEN and quality gates**

```bash
cargo test -p flpdf --doc
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf --test page_merge_tests
cargo fmt --all -- --check
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base epic/flpdf-9hc-38-outline-exact-parity
```

Expected: all tests and Clippy pass; patch coverage reports 100%.

- [ ] **Step 6: Commit, review, close, and push the layer**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/lib.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/tests/outline_pagelabels_e2e_tests.rs crates/flpdf/tests/page_merge_tests.rs
git commit -m "refactor(flpdf-9hc.38.1): remove outline-specific policy"
bd close flpdf-9hc.38.1 --reason "Removed qpdf-incompatible outline normalization, diagnostics, pruning, and configurable-depth API; raw keys remain preserved"
bd dolt push
git push -u origin stack/flpdf-9hc-38-1-remove-outline-policy
```

Request a code review for this branch and address findings before starting Task 2.

---

### Task 2: Introduce the arena and direct/mixed value traversal (`flpdf-x5yi`)

**Files:**

- Replace: `crates/flpdf/src/outline.rs`
- Rewrite core construction in: `crates/flpdf/src/outline_document_helper.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf/examples/inspect.rs`
- Modify: `crates/flpdf/tests/inspection_tests.rs`
- Modify: `crates/flpdf/tests/outline_document_helper_tests.rs`
- Modify: `crates/flpdf/tests/outline_pagelabels_e2e_tests.rs`

**Final public interfaces introduced in this task:**

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlineId(usize);

#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    pub source_ref: Option<ObjectRef>,
    pub parent: Option<OutlineId>,
    pub kids: Vec<OutlineId>,
    pub object: Object,
    pub title: String,
    pub count: i32,
    pub dest: Object,
}

#[derive(Debug)]
pub struct OutlineTree {
    pub(crate) items: Vec<OutlineItem>,
    pub(crate) roots: Vec<OutlineId>,
}

impl OutlineTree {
    pub fn roots(&self) -> &[OutlineId];
    pub fn get(&self, id: OutlineId) -> Option<&OutlineItem>;
    pub fn preorder(&self) -> OutlineTreeIter<'_>;
}

impl std::ops::Index<OutlineId> for OutlineTree {
    type Output = OutlineItem;
}

pub struct OutlineTreeIter<'a>;

impl<'a> Iterator for OutlineTreeIter<'a> {
    type Item = (usize, OutlineId, &'a OutlineItem);
}

impl<'a, R: Read + Seek> OutlineDocumentHelper<'a, R> {
    pub fn get_tree(&mut self) -> Result<OutlineTree>;
}
```

- [ ] **Step 1: Create and claim the next stack layer**

```bash
git switch -c stack/flpdf-x5yi-direct-outline-values
bd update flpdf-x5yi --claim
```

Expected: branch parent is `stack/flpdf-9hc-38-1-remove-outline-policy`; issue is `IN_PROGRESS`.

- [ ] **Step 2: Add RED coverage for the removed flat API and new direct behavior**

Replace `outline.rs` module docs with this compile-fail contract before deleting the old functions:

```rust
//! The pre-1.0 flat, configurable-depth outline API was removed in favor of
//! qpdf-compatible [`OutlineTree`] materialization.
//!
//! ```compile_fail
//! use flpdf::outline::{outline_items, outline_items_with_max_depth};
//! ```
```

Add these integration tests to `outline_document_helper_tests.rs`:

```rust
#[test]
fn direct_outlines_first_and_next_are_materialized() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines << /First << /Title (A) /Next << /Title (B) >> >> >> >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert!(pdf.outline().has_outlines().unwrap());
    let tree = pdf.outline().get_tree().unwrap();
    assert_eq!(tree.roots().len(), 2);
    assert_eq!(tree[tree.roots()[0]].source_ref, None);
    assert_eq!(tree[tree.roots()[0]].title, "A");
    assert_eq!(tree[tree.roots()[1]].source_ref, None);
    assert_eq!(tree[tree.roots()[1]].title, "B");
}

#[test]
fn mixed_direct_and_indirect_items_keep_identity_and_parent_ids() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 5 0 R >> >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Title (Parent) /First << /Title (Direct child) /Next 6 0 R >> >>"),
            (6, "<< /Title (Indirect child) >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let parent = tree.roots()[0];
    let direct = tree[parent].kids[0];
    let indirect = tree[parent].kids[1];

    assert_eq!(tree[parent].source_ref, Some(ObjectRef::new(5, 0)));
    assert_eq!(tree[direct].source_ref, None);
    assert_eq!(tree[indirect].source_ref, Some(ObjectRef::new(6, 0)));
    assert_eq!(tree[direct].parent, Some(parent));
    assert_eq!(tree[indirect].parent, Some(parent));
}

#[test]
fn non_dictionary_first_is_still_an_outline_item_with_default_accessors() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines << /First 42 >> >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let tree = pdf.outline().get_tree().unwrap();
    let id = tree.roots()[0];

    assert_eq!(tree[id].object, Object::Integer(42));
    assert_eq!(tree[id].title, "");
    assert_eq!(tree[id].count, 0);
    assert_eq!(tree[id].dest, Object::Null);
    assert!(tree[id].kids.is_empty());
}
```

Also add oracle-pinned finite cycle cases:

- top-level indirect `/Next` cycle stops before a duplicate root;
- a child `/First` pointing back to an already-seen indirect ancestor materializes one repeated node but does not expand its kids;
- the same direct dictionary value encountered in two separate direct positions is materialized twice.

Run:

```bash
cargo test -p flpdf --doc
cargo test -p flpdf --test outline_document_helper_tests direct_outlines_first_and_next_are_materialized -- --nocapture
```

Expected RED: the compile-fail block compiles, and the direct-value test cannot find `get_tree`/`OutlineTree`.

- [ ] **Step 3: Replace the flat module with the arena model**

Implement `OutlineTree`, `OutlineId`, `OutlineItem`, indexing, and preorder traversal in `outline.rs`. The iterator is a lossless view only:

```rust
use crate::{Object, ObjectRef};
use std::ops::Index;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlineId(pub(crate) usize);

#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    pub source_ref: Option<ObjectRef>,
    pub parent: Option<OutlineId>,
    pub kids: Vec<OutlineId>,
    pub object: Object,
    pub title: String,
    pub count: i32,
    pub dest: Object,
}

impl OutlineItem {
    pub fn dest_page(&self) -> Object {
        match &self.dest {
            Object::Array(items) if !items.is_empty() => items[0].clone(),
            _ => Object::Null,
        }
    }
}

#[derive(Debug)]
pub struct OutlineTree {
    pub(crate) items: Vec<OutlineItem>,
    pub(crate) roots: Vec<OutlineId>,
}

impl OutlineTree {
    pub(crate) fn new() -> Self {
        Self { items: Vec::new(), roots: Vec::new() }
    }

    pub fn roots(&self) -> &[OutlineId] {
        &self.roots
    }

    pub fn get(&self, id: OutlineId) -> Option<&OutlineItem> {
        self.items.get(id.0)
    }

    pub fn preorder(&self) -> OutlineTreeIter<'_> {
        OutlineTreeIter {
            tree: self,
            stack: self.roots.iter().rev().map(|&id| (1, id)).collect(),
        }
    }
}

impl Index<OutlineId> for OutlineTree {
    type Output = OutlineItem;

    fn index(&self, id: OutlineId) -> &Self::Output {
        &self.items[id.0]
    }
}

pub struct OutlineTreeIter<'a> {
    tree: &'a OutlineTree,
    stack: Vec<(usize, OutlineId)>,
}

impl<'a> Iterator for OutlineTreeIter<'a> {
    type Item = (usize, OutlineId, &'a OutlineItem);

    fn next(&mut self) -> Option<Self::Item> {
        let (depth, id) = self.stack.pop()?;
        let item = &self.tree[id];
        self.stack
            .extend(item.kids.iter().rev().map(|&kid| (depth + 1, kid)));
        Some((depth, id, item))
    }
}
```

- [ ] **Step 4: Implement a one-resolution cursor and qpdf seen placement**

Replace `outline_root`, `outline_root_first`, `get_root`, `build_siblings`, `OutlineNode`, and the old recursive-value tree with `get_tree` and these private building blocks in `outline_document_helper.rs`:

```rust
#[derive(Clone)]
enum OutlineCursor {
    Direct(Object),
    Indirect(ObjectRef),
}

impl OutlineCursor {
    fn from_object(object: Object) -> Option<Self> {
        match object {
            Object::Null => None,
            Object::Reference(reference) => Some(Self::Indirect(reference)),
            direct => Some(Self::Direct(direct)),
        }
    }

    fn source_ref(&self) -> Option<ObjectRef> {
        match self {
            Self::Direct(_) => None,
            Self::Indirect(reference) => Some(*reference),
        }
    }
}

fn object_key(object: &Object, key: &str) -> Object {
    match object {
        Object::Dictionary(dict) => dict.get(key).cloned().unwrap_or(Object::Null),
        _ => Object::Null,
    }
}
```

Add methods with the following exact responsibilities:

```rust
fn resolve_cursor(&mut self, cursor: &OutlineCursor) -> Result<Object> {
    match cursor {
        OutlineCursor::Direct(object) => Ok(object.clone()),
        OutlineCursor::Indirect(reference) => self.pdf.resolve(*reference),
    }
}

fn catalog_outlines(&mut self) -> Result<Option<Object>> {
    let Some(catalog_ref) = self.pdf.root_ref() else {
        return Ok(None);
    };
    let Object::Dictionary(catalog) = self.pdf.resolve(catalog_ref)? else {
        return Ok(None);
    };
    Ok(catalog.get("Outlines").cloned())
}
```

`get_tree` must:

1. resolve Catalog `/Outlines` once;
2. require its resolved value to be a dictionary with a present `/First` key;
3. iterate `/First` then `/Next`;
4. use a top-level `BTreeSet<ObjectRef>` that rejects repeated indirect roots but always admits direct roots;
5. call `build_item(cursor, 1, None, &mut tree, &mut constructor_seen)` for each root.

`has_outlines` checks only the resolved Catalog `/Outlines` dictionary and whether its `/First` value is non-null. It must not materialize scalar fields or duplicate count/title warnings:

```rust
pub fn has_outlines(&mut self) -> Result<bool> {
    let Some(outlines) = self.catalog_outlines()? else {
        return Ok(false);
    };
    let Some(cursor) = OutlineCursor::from_object(outlines) else {
        return Ok(false);
    };
    let resolved = self.resolve_cursor(&cursor)?;
    Ok(matches!(
        resolved,
        Object::Dictionary(ref dict)
            if !matches!(dict.get("First"), None | Some(Object::Null))
    ))
}
```

`build_item` must:

1. resolve the cursor once and immediately push one `OutlineItem` containing the raw object;
2. preserve `source_ref` and arena `parent`;
3. compute the existing qpdf destination, temporary title, and temporary `i32` count;
4. if the constructor seen-set says not to expand, return the already-created id;
5. otherwise follow raw `/First` then raw `/Next`, append each returned id to `kids`, and never add a direct cursor to either seen set.

For this intermediate branch only, retain Task 1's private 5,000-level error guard before recursing. Task 3 replaces it with qpdf's depth-51 materialization rule; do not expose the guard in any public signature.

Do not call `resolve_terminal_object` on `OutlineCursor` itself. That helper remains valid only for destination/action accessors.

- [ ] **Step 5: Migrate callers from recursive nodes and the flat module**

Update exports to:

```rust
pub use outline::{OutlineId, OutlineItem, OutlineTree, OutlineTreeIter};
pub use outline_document_helper::OutlineDocumentHelper;
```

Migrate `run_show_outline`, the `inspect` example, and inspection tests to `get_tree().preorder()`:

```rust
let tree = pdf.outline().get_tree()?;
for (index, (depth, _id, item)) in tree.preorder().enumerate() {
    println!("{}{}: {}", "  ".repeat(depth - 1), index + 1, item.title);
}
```

Migrate recursive-node tests by indexing ids:

```rust
let tree = pdf.outline().get_tree().unwrap();
let root = tree.roots()[0];
let first_child = tree[root].kids[0];
assert_eq!(tree[first_child].parent, Some(root));
```

Delete `outline_items`, `outline_items_with_max_depth`, their old `OutlineItem`, and `DEFAULT_MAX_OUTLINE_DEPTH` from `outline.rs`.

- [ ] **Step 6: Verify qpdf direct-object JSON evidence remains pinned for Task 6**

Run the already-established oracle fixture:

```bash
qpdf --json=2 --json-key=outlines /tmp/direct-outline-fixture.pdf
```

Expected evidence:

- two roots, both direct;
- first `object` is a direct JSON dictionary containing `/Count`, `/Dest`, `/Next`, and `/Title`;
- second `object` is `{"/Title":"u:Direct B"}`;
- no `0 0 R` string appears.

Record this expectation in a comment beside the direct-object test; do not implement JSON in this task.

- [ ] **Step 7: Verify GREEN and quality gates**

```bash
cargo test -p flpdf --doc
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test inspection_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf-cli --test cli_tests
cargo run -p flpdf --example inspect -- tests/fixtures/minimal.pdf
cargo fmt --all -- --check
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base stack/flpdf-9hc-38-1-remove-outline-policy
```

Expected: direct/mixed/non-dictionary tests pass, old flat imports fail only in compile-fail docs, workspace checks pass, patch coverage is 100%.

- [ ] **Step 8: Commit, review, close, and push the layer**

```bash
git add crates/flpdf/src/outline.rs crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/lib.rs crates/flpdf-cli/src/main.rs crates/flpdf/examples/inspect.rs crates/flpdf/tests/inspection_tests.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/tests/outline_pagelabels_e2e_tests.rs
git commit -m "feat(flpdf-x5yi): support direct outline values"
bd close flpdf-x5yi --reason "Arena model now matches qpdf for direct, indirect, mixed, repeated, cyclic, and non-dictionary outline values"
bd dolt push
git push -u origin stack/flpdf-x5yi-direct-outline-values
```

Request review and resolve it before Task 3.

---

### Task 3: Match qpdf's silent depth-50 construction boundary (`flpdf-3g9k`)

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Modify: `crates/flpdf/tests/outline_document_helper_tests.rs`
- Modify: `crates/flpdf/tests/outline_pagelabels_e2e_tests.rs`

- [ ] **Step 1: Create and claim the layer**

```bash
git switch -c stack/flpdf-3g9k-outline-depth-50
bd update flpdf-3g9k --claim
```

- [ ] **Step 2: Add exact RED boundary tests**

Reuse the existing deep-chain fixture and add:

```rust
#[test]
fn qpdf_depth_50_boundary_materializes_depth_51_without_expanding_it() {
    for (input_levels, expected_levels) in [(50, 50), (51, 51), (52, 51)] {
        let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(input_levels))).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        let visits: Vec<_> = tree.preorder().collect();

        assert_eq!(visits.len(), expected_levels);
        assert_eq!(visits.first().unwrap().0, 1);
        assert_eq!(visits.last().unwrap().0, expected_levels);
        if input_levels == 52 {
            assert!(visits.last().unwrap().2.kids.is_empty());
        }
    }
}
```

Add a test that confirms neither `get_tree` nor `has_outlines` returns an `Unsupported` depth error for a 52-level input.

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests qpdf_depth_50_boundary_materializes_depth_51_without_expanding_it -- --nocapture
```

Expected RED: the 52-level case materializes 52 nodes or still follows the temporary cap instead of qpdf's 51-node view.

- [ ] **Step 3: Put the depth check in qpdf's exact constructor position**

Add:

```rust
const QPDF_MAX_EXPANDED_OUTLINE_DEPTH: usize = 50;
```

In `build_item`, create and populate the current item first, then use:

```rust
if depth > QPDF_MAX_EXPANDED_OUTLINE_DEPTH {
    return Ok(id);
}

if let Some(reference) = source_ref {
    if !seen.insert(reference) {
        return Ok(id);
    }
}
```

Only after these checks may the method read `/First`. Remove the Task 1 temporary cap and every depth-error helper.

- [ ] **Step 4: Preserve the raw 150-level document while truncating only the helper view**

Update `deep_outline_round_trip_through_write_pdf`:

- after reopening, assert `tree.preorder().count() == 51`;
- independently follow raw indirect `/First` links through `Pdf::resolve` and assert all 150 dictionaries remain;
- assert item 51 still has its raw `/First` key even though `tree[item_51].kids` is empty.

This proves the helper truncates its view without mutating serialized PDF data.

- [ ] **Step 5: Verify and finish the layer**

```bash
cargo test -p flpdf --test outline_document_helper_tests qpdf_depth_50_boundary -- --nocapture
cargo test -p flpdf --test outline_pagelabels_e2e_tests deep_outline_round_trip_through_write_pdf -- --nocapture
cargo fmt --all -- --check
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base stack/flpdf-x5yi-direct-outline-values
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/tests/outline_pagelabels_e2e_tests.rs
git commit -m "fix(flpdf-3g9k): match qpdf outline depth boundary"
bd close flpdf-3g9k --reason "Outline construction now materializes depth 51, silently omits depth 52+, and preserves raw deeper data"
bd dolt push
git push -u origin stack/flpdf-3g9k-outline-depth-50
```

Expected: all gates pass and patch coverage is 100%. Request review before Task 4.

---

### Task 4: Match qpdf title and count accessors (`flpdf-guru`)

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Reuse: `crates/flpdf/src/json_inspect.rs` (`qpdf_utf8_value`)
- Modify: `crates/flpdf/tests/outline_document_helper_tests.rs`

- [ ] **Step 1: Create and claim the layer**

```bash
git switch -c stack/flpdf-guru-outline-scalars
bd update flpdf-guru --claim
```

- [ ] **Step 2: Add RED scalar matrices**

Add a table-driven title test covering:

```rust
#[test]
fn titles_match_qpdf_get_utf8_value() {
    let cases: &[(&str, &str)] = &[
        ("(plain)", "plain"),
        ("<95>", "Ł"),
        ("<FEFF540D524D>", "名前"),
        ("<FFFE0D544D52>", "名前"),
        ("<EFBBBFE5908D>", "名"),
        ("<EFBBBFFF>", "�"),
        ("<FEFF0041D800>", "A"),
        ("42", ""),
    ];

    for &(title_object, expected) in cases {
        let mut pdf = Pdf::open(Cursor::new(single_outline_with_title(title_object))).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        assert_eq!(tree[tree.roots()[0]].title, expected, "{title_object}");
    }
}
```

The two malformed-string rows use qpdf 11.9.0 results already measured from `/tmp/malformed-title-fixture.pdf`: the explicit UTF-8 BOM case replaces invalid byte `FF` at the Rust `String` boundary, while malformed UTF-16BE keeps `A` and drops the trailing unpaired high surrogate, matching qpdf's `QUtil::utf16_to_utf8` output.

Add the absent-title case separately:

```rust
let mut pdf = Pdf::open(Cursor::new(single_outline_without_title())).unwrap();
let tree = pdf.outline().get_tree().unwrap();
assert_eq!(tree[tree.roots()[0]].title, "");
```

Add count coverage:

```rust
#[test]
fn counts_match_qpdf_get_int_value_as_int() {
    let cases = [
        ("-2147483649", i32::MIN),
        ("-2147483648", i32::MIN),
        ("7", 7),
        ("2147483647", i32::MAX),
        ("2147483648", i32::MAX),
        ("(wrong type)", 0),
    ];

    for (count_object, expected) in cases {
        let mut pdf = Pdf::open(Cursor::new(single_outline_with_count(count_object))).unwrap();
        let tree = pdf.outline().get_tree().unwrap();
        assert_eq!(tree[tree.roots()[0]].count, expected, "{count_object}");
    }
}
```

For the two out-of-range cases, assert `repair_diagnostics()` contains respectively:

- `requested value of integer is too small; returning INT_MIN`;
- `requested value of integer is too big; returning INT_MAX`.

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests titles_match_qpdf_get_utf8_value -- --nocapture
cargo test -p flpdf --test outline_document_helper_tests counts_match_qpdf_get_int_value_as_int -- --nocapture
```

Expected RED: PDFDocEncoding/UTF-16 title cases or out-of-range count cases differ from qpdf.

- [ ] **Step 3: Implement qpdf scalar accessors**

Use the existing one-object terminal resolution for an indirect `/Title` or `/Count`, then implement:

```rust
fn qpdf_title(value: Object) -> String {
    match value {
        Object::String(bytes) => {
            String::from_utf8_lossy(&crate::json_inspect::qpdf_utf8_value(&bytes)).into_owned()
        }
        _ => String::new(),
    }
}

fn qpdf_count<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> i32 {
    let Object::Integer(value) = value else {
        return 0;
    };
    if value < i64::from(i32::MIN) {
        pdf.push_warning("requested value of integer is too small; returning INT_MIN");
        i32::MIN
    } else if value > i64::from(i32::MAX) {
        pdf.push_warning("requested value of integer is too big; returning INT_MAX");
        i32::MAX
    } else {
        value as i32
    }
}
```

Absent values pass `Object::Null` and produce empty title/zero count. Preserve qpdf's default values for present wrong types; do not serialize a non-string title as flpdf previously did.

- [ ] **Step 4: Verify and finish the layer**

```bash
cargo test -p flpdf --test outline_document_helper_tests titles_match_qpdf_get_utf8_value -- --nocapture
cargo test -p flpdf --test outline_document_helper_tests counts_match_qpdf_get_int_value_as_int -- --nocapture
cargo fmt --all -- --check
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base stack/flpdf-3g9k-outline-depth-50
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "fix(flpdf-guru): match qpdf outline scalar accessors"
bd close flpdf-guru --reason "Outline titles and 32-bit counts now match qpdf decoding, fallbacks, clamping, and warnings"
bd dolt push
git push -u origin stack/flpdf-guru-outline-scalars
```

Expected: all scalar matrices and quality gates pass with 100% patch coverage. Request review before Task 5.

---

### Task 5: Add the lazy qpdf-compatible page index (`flpdf-7nu4`)

**Files:**

- Modify: `crates/flpdf/src/outline.rs`
- Modify: `crates/flpdf/tests/outline_document_helper_tests.rs`

**Public interface:**

```rust
impl OutlineTree {
    pub fn outlines_for_page(
        &self,
        page: Option<ObjectRef>,
    ) -> impl Iterator<Item = (OutlineId, &OutlineItem)>;
}
```

- [ ] **Step 1: Create and claim the layer**

```bash
git switch -c stack/flpdf-7nu4-outline-page-index
bd update flpdf-7nu4 --claim
```

- [ ] **Step 2: Add RED breadth-first and null-bucket tests**

Add this exact fixture with roots `A`, `B`, `No dest`, `Integer dest`, and `Direct page operand`; children `A1`, `A2`, and `B1`; explicit and both kinds of named destination:

```rust
fn page_index_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /Dests << /same [3 0 R /Fit] >> /Names << /Dests 20 0 R >> >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Outlines /First 6 0 R /Last 14 0 R >>"),
            (6, "<< /Title (A) /Dest [3 0 R /Fit] /First 8 0 R /Next 7 0 R >>"),
            (7, "<< /Title (B) /Dest /same /First 10 0 R /Next 12 0 R >>"),
            (8, "<< /Title (A1) /Dest [3 0 R /Fit] /Next 9 0 R >>"),
            (9, "<< /Title (A2) /Dest [4 0 R /Fit] >>"),
            (10, "<< /Title (B1) /Dest (modern) >>"),
            (12, "<< /Title (No dest) /Next 13 0 R >>"),
            (13, "<< /Title (Integer dest) /Dest 42 /Next 14 0 R >>"),
            (14, "<< /Title (Direct page operand) /Dest [<< /Type /Page >> /Fit] >>"),
            (20, "<< /Names [(modern) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}
```

Assert:

```rust
#[test]
fn outlines_for_page_uses_qpdf_breadth_first_order() {
    let mut pdf = Pdf::open(Cursor::new(page_index_outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    let titles: Vec<_> = tree
        .outlines_for_page(Some(ObjectRef::new(3, 0)))
        .map(|(_id, item)| item.title.as_str())
        .collect();

    assert_eq!(titles, ["A", "B", "A1", "B1"]);
}

#[test]
fn outlines_for_page_none_matches_qpdf_objgen_zero_bucket() {
    let mut pdf = Pdf::open(Cursor::new(page_index_outline_pdf())).unwrap();
    let tree = pdf.outline().get_tree().unwrap();

    let titles: Vec<_> = tree
        .outlines_for_page(None)
        .map(|(_id, item)| item.title.as_str())
        .collect();

    assert_eq!(titles, ["No dest", "Integer dest", "Direct page operand"]);
}
```

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests outlines_for_page -- --nocapture
```

Expected RED: `OutlineTree::outlines_for_page` does not exist.

- [ ] **Step 3: Add a lazy BFS index to `OutlineTree`**

Change the tree to:

```rust
use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;

pub struct OutlineTree {
    pub(crate) items: Vec<OutlineItem>,
    pub(crate) roots: Vec<OutlineId>,
    by_page: OnceLock<BTreeMap<Option<ObjectRef>, Vec<OutlineId>>>,
}
```

Initialize `by_page` in `OutlineTree::new`. Add:

```rust
fn page_key(item: &OutlineItem) -> Option<ObjectRef> {
    match item.dest_page() {
        Object::Reference(reference) => Some(reference),
        _ => None,
    }
}

fn by_page(&self) -> &BTreeMap<Option<ObjectRef>, Vec<OutlineId>> {
    self.by_page.get_or_init(|| {
        let mut index = BTreeMap::<Option<ObjectRef>, Vec<OutlineId>>::new();
        let mut queue: VecDeque<OutlineId> = self.roots.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            index.entry(Self::page_key(&self[id])).or_default().push(id);
            queue.extend(self[id].kids.iter().copied());
        }
        index
    })
}

pub fn outlines_for_page(
    &self,
    page: Option<ObjectRef>,
) -> impl Iterator<Item = (OutlineId, &OutlineItem)> {
    self.by_page()
        .get(&page)
        .into_iter()
        .flatten()
        .copied()
        .map(|id| (id, &self[id]))
}
```

Do not resolve direct destination page operands into indirect pages. All non-`Object::Reference` operands belong in `None`, matching qpdf `QPDFObjGen(0,0)`.

- [ ] **Step 4: Verify and finish the layer**

```bash
cargo test -p flpdf --test outline_document_helper_tests outlines_for_page -- --nocapture
cargo fmt --all -- --check
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base stack/flpdf-guru-outline-scalars
git add crates/flpdf/src/outline.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "feat(flpdf-7nu4): index outlines by destination page"
bd close flpdf-7nu4 --reason "OutlineTree now lazily groups items by destination page in qpdf breadth-first order, including the None bucket"
bd dolt push
git push -u origin stack/flpdf-7nu4-outline-page-index
```

Expected: BFS and `None` bucket tests pass; all gates pass; patch coverage is 100%. Request review before Task 6.

---

### Task 6: Project exact qpdf JSON v2 outlines (`flpdf-9hc.38.2`)

**Files:**

- Rewrite outline JSON code in: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf-cli/tests/cli_outline_pagelabels_qpdf.rs`
- Modify: `crates/flpdf-cli/tests/json_schema_diff.rs`
- Add binary oracle fixture: `tests/fixtures/json-diff/direct-outlines.pdf`
- Add provenance: `tests/fixtures/json-diff/README.md`
- Confirm unchanged: `tests/fixtures/json-diff/allowed-divergences.json`

- [ ] **Step 1: Create and claim the final stack layer**

```bash
git switch -c stack/flpdf-9hc-38-2-outline-json-v2
bd update flpdf-9hc.38.2 --claim
```

- [ ] **Step 2: Add the real qpdf outline fixture and its provenance**

Copy qpdf 11.9.0's Apache-2.0 test fixture:

```bash
cp -f /tmp/qpdf-1190/qpdf/qtest/qpdf/direct-outlines.pdf tests/fixtures/json-diff/direct-outlines.pdf
```

Create `tests/fixtures/json-diff/README.md`:

```markdown
# JSON differential fixtures

`direct-outlines.pdf` comes from qpdf 11.9.0's
`qpdf/qtest/qpdf/direct-outlines.pdf` test corpus and is used under qpdf's
Apache-2.0 license. It pins a direct Catalog `/Outlines` dictionary with a
non-trivial nested outline tree for `qpdf --json=2` differential testing.
```

Add it to `CORPUS` in `json_schema_diff.rs`:

```rust
FixtureSpec {
    label: "json-diff/direct-outlines.pdf",
    relative_path: "json-diff/direct-outlines.pdf",
    password: None,
},
```

- [ ] **Step 3: Replace current-behavior tests with exact RED JSON assertions**

In `json_inspect.rs`, replace old tests for `action`, `count`, `flags`, and `structureelement` with:

```rust
fn load_direct_outline_fixture() -> Pdf<std::io::Cursor<Vec<u8>>> {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/json-diff/direct-outlines.pdf"
    ));
    Pdf::open(std::io::Cursor::new(bytes.to_vec())).unwrap()
}

#[test]
fn outline_json_v2_has_exact_qpdf_keys_and_values() {
    let mut pdf = load_direct_outline_fixture();
    let JsonValue::Array(entries) = build_outlines_section(&mut pdf).unwrap() else {
        panic!("outlines must be an array");
    };
    let JsonValue::Object(first) = &entries[0] else {
        panic!("outline item must be an object");
    };

    let keys: Vec<_> = first.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        ["dest", "destpageposfrom1", "kids", "object", "open", "title"]
    );
}
```

Add direct-item projection using the small synthetic direct `/First` + direct `/Next` fixture measured against qpdf:

```rust
fn value_for_key<'a>(pairs: &'a [(String, JsonValue)], key: &str) -> &'a JsonValue {
    &pairs.iter().find(|(name, _)| name == key).unwrap().1
}

assert_eq!(value_for_key(&first, "object"), &JsonValue::Object(vec![
    ("/Count".into(), JsonValue::Integer(-1)),
    ("/Dest".into(), JsonValue::Array(vec![
        JsonValue::String("3 0 R".into()),
        JsonValue::String("/Fit".into()),
    ])),
    ("/Next".into(), JsonValue::Object(vec![
        ("/Title".into(), JsonValue::String("u:Direct B".into())),
    ])),
    ("/Title".into(), JsonValue::String("u:Direct A".into())),
]));
```

Also assert:

- `destpageposfrom1` is one-based for an indirect page in the current page tree and null otherwise;
- `open` is `count >= 0`;
- nested `kids` order equals arena order;
- indirect `object` is `"N G R"` while direct `object` is the direct JSON value;
- title is bare decoded UTF-8, while the raw direct object's `/Title` keeps qpdf's `u:` JSON encoding;
- missing destination is JSON null.

Update `cli_json_outlines_and_pagelabels_sections_are_populated` so the outline item asserts the exact six-key set and no removed keys.

Run:

```bash
cargo test -p flpdf --lib outline_json_v2_has_exact_qpdf_keys_and_values -- --nocapture
cargo test -p flpdf-cli --test cli_outline_pagelabels_qpdf cli_json_outlines_and_pagelabels_sections_are_populated -- --nocapture
```

Expected RED: current output still has `action`, `count`, `flags`, and `structureelement`, and lacks `destpageposfrom1`/`open`.

- [ ] **Step 4: Replace the independent JSON walker with an arena projection**

Delete `outline_entry_to_json` and `collect_outline_chain`. Implement a pure tree projection:

```rust
fn outline_item_to_json(
    tree: &crate::OutlineTree,
    id: crate::OutlineId,
    page_numbers: &std::collections::BTreeMap<crate::ObjectRef, i64>,
) -> Result<JsonValue, ConvertError> {
    let item = &tree[id];
    let dest = pdf_object_to_json(&item.dest)?;
    let destpageposfrom1 = match item.dest_page() {
        Object::Reference(reference) => page_numbers
            .get(&reference)
            .copied()
            .map(JsonValue::Integer)
            .unwrap_or(JsonValue::Null),
        _ => JsonValue::Null,
    };
    let kids = item
        .kids
        .iter()
        .copied()
        .map(|kid| outline_item_to_json(tree, kid, page_numbers))
        .collect::<Result<Vec<_>, _>>()?;
    let object = match item.source_ref {
        Some(reference) => JsonValue::String(reference.to_string()),
        None => pdf_object_to_json(&item.object)?,
    };

    Ok(JsonValue::Object(vec![
        ("dest".to_string(), dest),
        ("destpageposfrom1".to_string(), destpageposfrom1),
        ("kids".to_string(), JsonValue::Array(kids)),
        ("object".to_string(), object),
        ("open".to_string(), JsonValue::Bool(item.count >= 0)),
        ("title".to_string(), JsonValue::String(item.title.clone())),
    ]))
}
```

Implement `build_outlines_section` by collecting page refs first, then materializing one `OutlineTree`:

```rust
pub fn build_outlines_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<JsonValue, ConvertError> {
    let page_numbers = crate::pages::page_refs(pdf)?
        .into_iter()
        .enumerate()
        .map(|(index, reference)| (reference, index as i64 + 1))
        .collect::<std::collections::BTreeMap<_, _>>();
    let tree = pdf.outline().get_tree()?;
    let entries = tree
        .roots()
        .iter()
        .copied()
        .map(|id| outline_item_to_json(&tree, id, &page_numbers))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(JsonValue::Array(entries))
}
```

Use the existing `impl From<crate::Error> for ConvertError`; reader errors propagate through `?` and must never be mapped to JSON null.

- [ ] **Step 5: Run the live qpdf differential test**

```bash
qpdf --version
cargo test -p flpdf-cli --test json_schema_diff json_schema_diff_corpus -- --nocapture
```

Expected: qpdf is 11.9.0; `json-diff/direct-outlines.pdf` has no unknown divergence; `allowed-divergences.json` remains `{"entries":[]}`.

Also compare just the outline section for diagnosis if needed:

```bash
qpdf --json=2 --json-key=outlines tests/fixtures/json-diff/direct-outlines.pdf > /tmp/qpdf-outlines.json
cargo run -p flpdf-cli -- --json=2 --json-key=outlines tests/fixtures/json-diff/direct-outlines.pdf > /tmp/flpdf-outlines.json
diff -u /tmp/qpdf-outlines.json /tmp/flpdf-outlines.json
```

Expected: no diff.

- [ ] **Step 6: Run final epic verification**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf --test inspection_tests
cargo test -p flpdf-cli --test cli_outline_pagelabels_qpdf
cargo test -p flpdf-cli --test json_schema_diff -- --nocapture
cargo test -p flpdf-cli --test compat_matrix_tests -- --nocapture
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/patch-coverage.sh --base main
```

Expected: all focused, crate, workspace, compatibility, and Clippy checks pass; changed-line patch coverage for the full stack is 100%.

- [ ] **Step 7: Perform the final no-policy and no-placeholder audit**

```bash
rg -n 'legacy_dests|name_tree_dests|check_outline_links|check_legacy_dests|check_name_tree_dests|prune_outline_se|get_root_with_max_depth|MAX_OUTLINE_WALK_DEPTH|outline_items_with_max_depth|pub struct OutlineNode|pub struct Dest' crates docs README.md
rg -n 'TODO|FIXME|todo!\(|unimplemented!\(|placeholder|coming soon' crates/flpdf/src/outline.rs crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/json_inspect.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf-cli/tests/json_schema_diff.rs
```

Expected: only intentional historical/compile-fail documentation matches the first command; no implementation placeholders match the second.

- [ ] **Step 8: Commit, close the epic, and push all state**

```bash
git add crates/flpdf/src/json_inspect.rs crates/flpdf-cli/tests/cli_outline_pagelabels_qpdf.rs crates/flpdf-cli/tests/json_schema_diff.rs tests/fixtures/json-diff/direct-outlines.pdf tests/fixtures/json-diff/README.md
git commit -m "fix(flpdf-9hc.38.2): match qpdf outline JSON v2"
bd close flpdf-9hc.38.2 --reason "Outline JSON v2 now matches qpdf schema, direct/indirect object projection, values, order, and page positions"
bd close flpdf-9hc.38 --reason "All outline helper surfaces now reproduce qpdf 11.9.0 observable behavior with an idiomatic Rust arena"
bd dolt push
git push -u origin stack/flpdf-9hc-38-2-outline-json-v2
```

Open/update the stacked PRs, wait for CI on every layer, and merge from the bottom of the stack upward. After merges, sync `main`, remove merged worktree branches only after confirming they are on the remote, and run `bd dolt push` plus `git push` once more before handoff.

## Final Acceptance Checklist

- [ ] Public outline APIs map to qpdf behavior; no validation/pruning/normalization policy remains.
- [ ] Direct, indirect, mixed, repeated, cyclic, and non-dictionary outline values match qpdf's finite observable cases.
- [ ] No holder-chain traversal was added.
- [ ] Arena parent/kid identity never leaks `0 0 R`.
- [ ] Depth 51 is present and unexpanded; depth 52 is absent; no depth error is returned.
- [ ] Title decoding and count clamping/warnings match qpdf.
- [ ] Page lookup uses breadth-first order and the `None` zero-objgen bucket.
- [ ] JSON v2 has exactly six qpdf keys and matches the live qpdf 11.9.0 fixture without an allowlist entry.
- [ ] Raw `/SE`, unknown keys, `/Dests`, and `/Names` survive ordinary rewriting.
- [ ] Focused tests, workspace tests, Clippy, compatibility tests, and 100% patch coverage pass.
- [ ] Every Beads issue and the epic are closed only after its gates pass; Beads and Git pushes succeed.
