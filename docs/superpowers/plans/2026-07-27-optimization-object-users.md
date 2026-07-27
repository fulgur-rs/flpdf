# Optimization Object-User Map Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create the bidirectional qpdf `ObjUser` maps in a focused `optimization` component and make every current linearization classification consumer derive its sets from those maps instead of bespoke traversals in `linearization/plan.rs`.

**Architecture:** `optimization.rs` owns qpdf's ordered `ObjectUser` value and the single page/trailer/catalog object traversal. `LinearizationPlan` retains one `Optimization` snapshot and derives page, thumbnail, outlines, open-document, and other-document classifications from it; compressed-object folding and inherited-attribute ownership are completed by the dependent plan.

**Tech Stack:** Rust 2021; qpdf 11.9.0 `QPDF_optimization.cc`; existing `Pdf`, `Object`, `Stream`, and null-visibility helpers; Cargo tests, Clippy, strict rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- This plan starts only after the Pipeline foundation PR if delivered as the approved stack; it does not depend on Pipeline APIs directly.
- qpdf 11.9.0 `ObjUser`, `updateObjectMaps`, and `updateObjectMapsInternal` are the component oracle.
- The current `linearization/inherited_attrs.rs::push_inherited_attributes_to_pages` remains the prerequisite mutation in this slice. The dependent Optimization-completion plan moves and completes that responsibility.
- Do not claim `QPDF_optimization` D1 complete after this plan. `optimize`, direct `/Outlines` indirectization, inherited-attribute ownership, and `filterCompressedObjects` remain for the dependent plan.
- Use one bidirectional map implementation. Delete bespoke `page_object_users`, `closure_from_seeds`, `open_document_set`, `document_other_set`, `outlines_set`, and their context variants after consumer cutover.
- Preserve qpdf null visibility: dictionary keys resolving to null are absent; array references retain indirect identity where qpdf does; page and thumbnail traversal share one visited set; non-top `/Page` objects are boundaries.
- The linearized writer skips stream `/Length`; retain the exact callback/policy boundary so later consumers can also skip `/Filter` and `/DecodeParms`.
- Keep current linearization partitioning, ordering, hint bytes, and ObjStm routing behavior unchanged.
- Every production change follows RED→GREEN→REFACTOR and fresh immediate-parent patch coverage must reach 100%.

## File Structure

```text
crates/flpdf/src/optimization.rs
    ObjectUser, Optimization maps, update_object_maps traversal, query helpers

crates/flpdf/src/linearization/plan.rs
    linearization-only part classification consuming Optimization

crates/flpdf/src/linearization/writer.rs
    outline-hint consumer reading the plan's retained Optimization snapshot
```

## Delivery Boundary

**Branch:** `feature/flpdf-qxba-phase2-optimization-users`
**PR base / patch-coverage base:** `origin/feature/flpdf-qxba-phase2-xref-entry`

---

### Task 1: Add ObjectUser ordering and bidirectional map invariants

**Files:**
- Create: `crates/flpdf/src/optimization.rs`
- Modify: `crates/flpdf/src/lib.rs:104-175`
- Test: `crates/flpdf/src/optimization.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/QPDF.hh:1292-1312`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_optimization.cc:8-48`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ObjectUser {
    Bad,
    Page(u32),
    Thumbnail(u32),
    TrailerKey(Vec<u8>),
    RootKey(Vec<u8>),
    Root,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Optimization {
    user_to_objects: BTreeMap<ObjectUser, BTreeSet<ObjectRef>>,
    object_to_users: BTreeMap<ObjectRef, BTreeSet<ObjectUser>>,
}

impl Optimization {
    pub(crate) fn objects_for(&self, user: &ObjectUser) -> &BTreeSet<ObjectRef>;
    pub(crate) fn users_for(&self, object: ObjectRef) -> &BTreeSet<ObjectUser>;
    pub(crate) fn object_users(
        &self,
    ) -> impl Iterator<Item = (ObjectRef, &BTreeSet<ObjectUser>)>;
    fn record(&mut self, user: ObjectUser, object: ObjectRef);
}
```

- Missing queries return a shared empty set without allocating.
- Raw dictionary key bytes omit flpdf's internal leading slash consistently; ordering remains the
  same because every compared key uses the same representation.

- [ ] **Step 1: Write failing ordering and inverse-map tests**

```rust
#[test]
fn object_user_order_matches_qpdf_discriminant_page_and_key_order() {
    let users = BTreeSet::from([
        ObjectUser::Root,
        ObjectUser::RootKey(b"Z".to_vec()),
        ObjectUser::Page(2),
        ObjectUser::Page(1),
        ObjectUser::Thumbnail(0),
        ObjectUser::TrailerKey(b"Info".to_vec()),
        ObjectUser::Bad,
    ]);
    assert_eq!(
        users.into_iter().collect::<Vec<_>>(),
        vec![
            ObjectUser::Bad,
            ObjectUser::Page(1),
            ObjectUser::Page(2),
            ObjectUser::Thumbnail(0),
            ObjectUser::TrailerKey(b"Info".to_vec()),
            ObjectUser::RootKey(b"Z".to_vec()),
            ObjectUser::Root,
        ]
    );
}

#[test]
fn record_updates_both_maps_and_deduplicates() {
    let mut maps = Optimization::default();
    let user = ObjectUser::Page(0);
    let object = ObjectRef::new(7, 0);
    maps.record(user.clone(), object);
    maps.record(user.clone(), object);
    assert_eq!(maps.objects_for(&user), &BTreeSet::from([object]));
    assert_eq!(maps.users_for(object), &BTreeSet::from([user]));
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p flpdf optimization::tests --lib
```

Expected: compile failure because `optimization.rs` is absent.

- [ ] **Step 3: Implement the value and map**

Add:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/QPDF_optimization.cc.
```

Declare `pub(crate) mod optimization;` in `lib.rs`. Use a static
`once_cell::sync::Lazy<BTreeSet<ObjectRef>>` only if `once_cell` is already a normal dependency;
otherwise return `Option<&BTreeSet<_>>` internally and expose `contains`/iterator helpers so no
new dependency is added merely for the empty set. Do not clone whole sets on lookup.

- [ ] **Step 4: Run focused tests**

Run:

```bash
cargo test -p flpdf optimization::tests --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/optimization.rs crates/flpdf/src/lib.rs
git commit -m "feat: add optimization object-user maps"
```

---

### Task 2: Port updateObjectMaps as the sole object-user traversal

**Files:**
- Modify: `crates/flpdf/src/optimization.rs`
- Test: `crates/flpdf/src/optimization.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_optimization.cc:237-338`
- Read-only current behavior: `crates/flpdf/src/linearization/plan.rs:2558-2735,2797-2870`

**Interfaces:**
- Adds:

```rust
impl Optimization {
    pub(crate) fn build_after_inherited<R, F>(
        pdf: &mut Pdf<R>,
        page_refs: &[ObjectRef],
        skip_stream_parameters: F,
    ) -> crate::Result<Self>
    where
        R: Read + Seek,
        F: FnMut(&Stream) -> u8;

    fn update_object_maps<R, F>(
        &mut self,
        pdf: &mut Pdf<R>,
        user: ObjectUser,
        object: Object,
        skip_stream_parameters: &mut F,
    ) -> crate::Result<()>
    where
        R: Read + Seek,
        F: FnMut(&Stream) -> u8;
}
```

- Callback return values match qpdf: `0` keeps all stream parameters, `1` skips `/Length`, and
  `2` skips `/Length`, `/Filter`, and `/DecodeParms`. Values above 2 behave as 2.
- `build_after_inherited` traverses pages in page order, then trailer keys except `Root`, then all
  catalog keys, and finally records the catalog object under `ObjectUser::Root`.

- [ ] **Step 1: Move the existing high-value traversal tests to optimization**

Move fixture helpers and assertions for:

- page and thumbnail first-edge-wins;
- direct thumbnail descendants;
- `/Parent` exclusion;
- non-top page boundary;
- dictionary-null versus array-null identity;
- cyclic references;
- stream `/Length` skip;
- open-document, document-other, and outlines closure membership.

Rewrite assertions against the maps:

```rust
#[test]
fn page_and_thumbnail_share_one_visited_set() {
    let mut pdf = Pdf::open_mem_owned(thumb_before_ordinary_first_edge_wins_pdf_bytes()).unwrap();
    push_inherited_attributes_to_pages(&mut pdf).unwrap();
    let pages = crate::pages::page_refs(&mut pdf).unwrap();
    let maps = Optimization::build_after_inherited(&mut pdf, &pages, |_| 1).unwrap();
    let target = ObjectRef::new(5, 0);
    assert!(maps.objects_for(&ObjectUser::Thumbnail(0)).contains(&target));
    assert!(!maps.objects_for(&ObjectUser::Page(0)).contains(&target));
}

#[test]
fn stream_skip_level_one_excludes_length_only() {
    let mut pdf = Pdf::open_mem_owned(stream_parameter_fixture()).unwrap();
    push_inherited_attributes_to_pages(&mut pdf).unwrap();
    let pages = crate::pages::page_refs(&mut pdf).unwrap();
    let maps = Optimization::build_after_inherited(&mut pdf, &pages, |_| 1).unwrap();
    assert!(!maps.users_for(ObjectRef::new(20, 0)).contains(&ObjectUser::Page(0)));
    assert!(maps.users_for(ObjectRef::new(21, 0)).contains(&ObjectUser::Page(0)));
}
```

- [ ] **Step 2: Run moved tests and verify RED**

Run:

```bash
cargo test -p flpdf optimization::tests --lib
```

Expected: compile failures because the traversal methods are absent.

- [ ] **Step 3: Implement the iterative traversal**

Use one `BTreeSet<ObjectRef>` visited set per top-level user call and a stack carrying:

```rust
struct Pending {
    object: Object,
    user: ObjectUser,
    top: bool,
    via_array: bool,
    inline_depth: usize,
}
```

Rules:

1. reject inline depth above `crate::object::MAX_INLINE_DEPTH`;
2. stop before recording/descending a non-top `/Page`;
3. record an indirect object once in both maps;
4. traverse arrays in reverse push order and retain `via_array`;
5. traverse visible dictionary keys in sorted order, skipping `/Parent` on a page;
6. switch only `/Thumb` descendants to `Thumbnail(page_no)` while sharing `visited`;
7. apply callback skip level to stream dictionary keys;
8. preserve array identity for null-resolving/missing refs and omit null dictionary values through
   `qpdf_null::visible_entries`.

Do not call any old `linearization::plan` traversal from this module.

- [ ] **Step 4: Build the full maps in qpdf order**

For each page use `ObjectUser::Page(index)` and its resolved page dictionary as top. For trailer
and catalog dictionaries, call `update_object_maps` once per key so each key gets its own visited
set. Record root explicitly:

```rust
if let Some(root_ref) = pdf.root_ref() {
    maps.record(ObjectUser::Root, root_ref);
}
```

Use exact dictionary key bytes from the current `Dictionary` API.

- [ ] **Step 5: Run traversal tests and the existing plan suite**

Run:

```bash
cargo test -p flpdf optimization::tests --lib
cargo test -p flpdf linearization::plan::tests --lib
```

Expected: PASS; the production plan still uses its old traversal until Task 3.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/optimization.rs
git commit -m "feat: build qpdf object-user maps"
```

---

### Task 3: Cut linearization consumers over and delete bespoke traversals

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:1,539-705,760-1465,1950-2210,2380-3040,3042-3078,6550-7225`
- Modify: `crates/flpdf/src/linearization/writer.rs:1540-1605,2880-2910`
- Modify: `crates/flpdf/src/optimization.rs`
- Test: `crates/flpdf/src/linearization/plan.rs`
- Test: `crates/flpdf/src/linearization/writer.rs`
- Test: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`

**Interfaces:**
- Replaces `LinearizationRoutingUsers` with:

```rust
pub(crate) optimization: Option<crate::optimization::Optimization>,
```

- Keeps public `all_referenced_pages` temporarily as a derived snapshot because downstream hint
  APIs consume it; only `Optimization` performs traversal.
- Adds query helpers:

```rust
impl Optimization {
    pub(crate) fn referenced_pages(&self, object: ObjectRef) -> BTreeSet<u32>;
    pub(crate) fn thumbnail_objects(&self) -> BTreeSet<ObjectRef>;
    pub(crate) fn objects_for_root_key(&self, key: &[u8]) -> BTreeSet<ObjectRef>;
    pub(crate) fn objects_for_trailer_key(&self, key: &[u8]) -> BTreeSet<ObjectRef>;
}
```

- Linearization-only constants such as `OPEN_DOCUMENT_CATALOG_KEYS` stay in `plan.rs`; they
  classify generic map entries but contain no traversal.

- [ ] **Step 1: Add a failing retained-map consistency test**

```rust
#[test]
fn from_pdf_retains_bidirectional_optimization_as_single_source() {
    let mut pdf = open_two_page_shared_font();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let maps = plan.optimization.as_ref().unwrap();
    let shared = ObjectRef::new(6, 0);
    assert_eq!(maps.referenced_pages(shared), BTreeSet::from([0, 1]));
    assert_eq!(
        plan.all_referenced_pages.get(&shared),
        Some(&BTreeSet::from([0, 1]))
    );
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p flpdf linearization::plan::tests::from_pdf_retains_bidirectional_optimization_as_single_source -- --exact
```

Expected: compile failure because `LinearizationPlan` has no `optimization` field.

- [ ] **Step 3: Build one Optimization snapshot in from_pdf**

Immediately after the existing inherited-attribute push and page collection:

```rust
let optimization =
    crate::optimization::Optimization::build_after_inherited(pdf, &page_refs, |_| 1)?;
```

Derive:

- page sets from `ObjectUser::Page(index)`;
- thumbnail union from every `ObjectUser::Thumbnail(index)`;
- outlines from `RootKey(b"Outlines")`;
- open-document from the five existing catalog keys plus trailer `Encrypt`;
- document-other from remaining root/trailer key users;
- `all_referenced_pages` by filtering each object's users to `Page(index)`.

Use the same precedence already present in `LinearizationPlan`; only replace how sets are
obtained.

- [ ] **Step 4: Replace ObjStm routing inputs without folding yet**

Change `route_objstm_containers` to accept `&Optimization` plus the existing derived page map.
Compute current first-page/thumbnail/outlines/open-document/document-other predicates from
`users_for(member)`. Preserve member-union classification exactly; actual qpdf
`filterCompressedObjects` folding remains Task 3 of the dependent completion plan.

Change missing-snapshot errors from “routing snapshot” to “optimization snapshot” and update their
tests.

- [ ] **Step 5: Change outline-hint writer to consume the retained snapshot**

Pass the plan's outline set into `compute_outline_hint_info`:

```rust
fn compute_outline_hint_info(
    outlines: &BTreeSet<ObjectRef>,
    pdf: &mut Pdf<impl Read + Seek>,
    renumber: &RenumberMap,
    objstm_layout: &ObjStmLayout,
) -> Result<Option<OutlineHintInfo>>;
```

Do not re-run a catalog traversal in the writer. The writer may still resolve `/Outlines` itself
to identify the first output unit.

- [ ] **Step 6: Delete old definitions and tests that exercise them directly**

Delete from `linearization/plan.rs`:

```text
LinearizationRoutingUsers
PageObjectUser
PageObjectUsers
page_dictionary_for_user_traversal
page_object_users
page_tree_node_refs
closure_from_seeds
open_document_set and _with_context
document_other_set and _with_context
outlines_set and _with_context
```

Move their behavior tests to `optimization.rs`; retain only linearization precedence/routing tests
in `plan.rs`.

- [ ] **Step 7: Run focused linearization suites**

Run:

```bash
cargo test -p flpdf optimization::tests --lib
cargo test -p flpdf linearization::plan::tests --lib
cargo test -p flpdf linearization::writer::tests --lib
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf-cli --test cli_linearize
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_linearize_qpdf
```

Expected: PASS with unchanged partitioning, object order, and bytes.

- [ ] **Step 8: Prove old traversal deletion**

Run:

```bash
rg -n "LinearizationRoutingUsers|PageObjectUsers|page_object_users|closure_from_seeds|open_document_set|document_other_set|outlines_set" crates/flpdf/src
```

Expected: no matches. Query-helper names in `optimization.rs` must use the new
`objects_for_*` spelling and must not hide copied traversal code.

- [ ] **Step 9: Commit**

```bash
git add crates/flpdf/src/optimization.rs crates/flpdf/src/linearization/plan.rs crates/flpdf/src/linearization/writer.rs crates/flpdf/tests/linearize_objstm_generate_tests.rs
git commit -m "refactor: cut linearization over to optimization maps"
```

---

### Task 4: Correspondence, workspace verification, and patch coverage

**Files:**
- Modify if generated output changes: `docs/qpdf-correspondence.md`
- Verify: all Task 1-3 files

**Interfaces:**
- Leaves `optimization.rs` explicitly partial until the dependent completion plan.

- [ ] **Step 1: Audit definitions and all consumers**

Run:

```bash
rg -n "QPDF_optimization|ObjectUser|Optimization" crates/flpdf/src
rg -n "page_object_users|closure_from_seeds|open_document_set|document_other_set|outlines_set" crates/flpdf/src
rg -n "routing_users|LinearizationRoutingUsers" crates/flpdf/src
```

Expected: new component/callsites are present; every old helper/snapshot query returns no matches.

- [ ] **Step 2: Check truthful correspondence**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts.tests.test_qpdf_module_docs
```

If regeneration is required, ensure `optimization.rs` says it mirrors the object-user-map portion
of `QPDF_optimization.cc`, while `linearization/inherited_attrs.rs` remains an explicit partial
correspondence until the next plan.

- [ ] **Step 3: Run formatting, lint, rustdoc, and workspace tests**

Run:

```bash
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings --document-private-items" cargo doc --workspace --all-features --no-deps
cargo test --workspace --all-features
git diff --check
```

Expected: all exit 0.

- [ ] **Step 4: Measure fresh immediate-parent patch coverage**

For a stacked branch, use its actual direct parent, not `origin/main` by assumption:

```bash
parent_ref="origin/feature/flpdf-qxba-phase2-xref-entry"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path /tmp/flpdf-optimization-users.lcov
scripts/patch-coverage.sh "$parent_ref" HEAD /tmp/flpdf-optimization-users.lcov
```

Expected: 100% changed executable lines across every commit in this plan.

- [ ] **Step 5: Commit generated correspondence only when changed**

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: record optimization map correspondence"
```

Skip an empty commit.

- [ ] **Step 6: Record clean final state**

Run:

```bash
git status --short --branch
git log --oneline --decorate -7
```

Expected: clean worktree and a retained partial-completion note for the dependent plan.
