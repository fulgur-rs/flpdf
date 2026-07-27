# Optimization Inherited-Attribute Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the qpdf 11.9.0 Optimization component by assigning page-tree repair to `pages`, moving inherited-attribute push under `optimization`, implementing full `optimize` and compressed-object folding, and deleting the old linearization-specific implementation.

**Architecture:** `pages/repair.rs` owns the `QPDF_pages.cc` preparation that produces a safe page view. `optimization/inherited_attrs.rs` owns only `pushInheritedAttributesToPage`, while `Optimization::optimize` performs direct-outline normalization, page preparation, inherited push, object-user map construction, and source ObjStm folding in qpdf order.

**Tech Stack:** Rust 2021; qpdf 11.9.0 `QPDF_pages.cc` and `QPDF_optimization.cc`; existing `Pdf` mutation/diagnostics APIs; Cargo tests, Clippy, strict rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- This plan depends on `2026-07-27-optimization-object-users.md` and consumes its exact
  `ObjectUser` / `Optimization` interfaces.
- `QPDF_pages.cc` repair and `QPDF_optimization.cc` inherited push must not remain fused in one
  function. Page-tree structure is owned by `pages`; optimization consumes its prepared root/pages.
- Preserve qpdf operation order: make direct `/Outlines` indirect; prepare/get all pages; push
  inherited attributes; build page/trailer/root user maps; record root; fold source compressed
  members to containers.
- Implement qpdf's actual `allow_changes=false` behavior: page repair and direct `/Outlines`
  indirectization still run first; the flag rejects an inheritable attribute when the push reaches
  it. Do not strengthen this into an atomic/no-mutation contract that qpdf 11.9.0 does not provide.
- Implement `warn_skipped_keys` through `Pdf::push_warning`; do not wait for `QPDFLogger`.
- Preserve the current safety bounds, iterative traversal, deterministic object-number allocation,
  alphabetical inheritable-key mint order, null visibility, and idempotence.
- Generated/preserved ObjStm routing must obtain member-user union from `Optimization`, not repeat
  union logic in `linearization/plan.rs`.
- Delete `linearization/inherited_attrs.rs`, its module declaration, direct calls, and duplicate
  traversal after cutover.
- Do not change normal non-linearized writer behavior: inherited attributes are pushed only on the
  existing optimization/linearization paths.
- Completion means D1/D2 for `QPDF_optimization`; correspondence may no longer describe
  `linearization/plan.rs` as implementing object-user traversal.
- Every production change follows RED→GREEN→REFACTOR and fresh direct-parent patch coverage must
  reach 100%.

## File Structure

```text
crates/flpdf/src/pages.rs
    page traversal facade; declares private repair child

crates/flpdf/src/pages/repair.rs
    QPDF_pages.cc root correction, duplicate leaf repair, /Type and /MediaBox repair

crates/flpdf/src/optimization.rs
    ObjectUser maps, optimize orchestration, compressed-object folding

crates/flpdf/src/optimization/inherited_attrs.rs
    QPDF_optimization.cc pushInheritedAttributesToPage only
```

## Delivery Boundary

**Branch:** `feature/flpdf-qxba-phase2-optimization-complete`
**PR base / patch-coverage base:** `origin/feature/flpdf-qxba-phase2-optimization-users`

---

### Task 1: Extract page-tree preparation into the pages component

**Files:**
- Create: `crates/flpdf/src/pages/repair.rs`
- Modify: `crates/flpdf/src/pages.rs`
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs:44-133,322-565,1998-3995`
- Test: `crates/flpdf/src/pages/repair.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_pages.cc:39-138`

**Interfaces:**
- Produces:

```rust
pub(crate) struct PreparedPages {
    pub(crate) root: ObjectRef,
    pub(crate) pages: Vec<ObjectRef>,
}

pub(crate) fn prepare_for_optimization<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> crate::Result<Option<PreparedPages>>;
```

- Returns `Ok(None)` for missing/non-dictionary root or missing `/Pages`, preserving current
  lenient behavior.
- Corrects catalog `/Pages` through `/Parent`, repairs interior/leaf `/Type`, defaults missing
  invalid `/MediaBox`, makes direct leaves indirect, clones repeated page leaves, and returns page
  refs in qpdf traversal order.
- Repairs run unconditionally, matching the `getAllPages()` call that precedes qpdf's
  `allow_changes` check.

- [ ] **Step 1: Move repair tests first and verify RED**

Move these existing test families from `linearization/inherited_attrs.rs` to
`pages/repair.rs`:

- catalog `/Pages` pointing into tree and parent cycles;
- interior and leaf `/Type`;
- missing/invalid/indirect `/MediaBox`;
- direct leaf minting;
- duplicate leaf cloning and idempotence;
- direct/non-dictionary kid behavior;
- excessive depth and allocation order.

Rewrite their call:

```rust
let prepared = prepare_for_optimization(&mut pdf).unwrap().unwrap();
assert_eq!(prepared.pages.len(), 2);
```

Add:

```rust
#[test]
fn page_preparation_repairs_the_tree_before_optimization_policy() {
    let mut pdf = Pdf::open_mem_owned(pdf_with_leaf_type_not_page()).unwrap();
    let prepared = prepare_for_optimization(&mut pdf).unwrap().unwrap();
    assert_eq!(prepared.pages, vec![ObjectRef::new(3, 0)]);
    assert!(matches!(
        pdf.resolve(ObjectRef::new(3, 0)).unwrap(),
        Object::Dictionary(ref d)
            if matches!(d.get("Type"), Some(Object::Name(name)) if name == b"Page")
    ));
}
```

- [ ] **Step 2: Run moved tests and verify RED**

Run:

```bash
cargo test -p flpdf pages::repair::tests --lib
```

Expected: compile failure because `pages::repair` and `prepare_for_optimization` are absent.

- [ ] **Step 3: Move the production repair code**

Move, without semantic rewriting:

```text
catalog /Pages parent correction
repair_page_tree
next_object_ref
type_name_is
is_rectangle
repair-only constants/helpers
```

from `linearization/inherited_attrs.rs` into `pages/repair.rs`. Change the traversal to accumulate
the final page refs into `PreparedPages.pages` so Optimization does not immediately walk the same
tree a second time.

Keep the running allocator and depth/visited sets. Do not thread
`Optimization::allow_changes` into this page component; qpdf's `getAllPages` repairs occur before
that flag is consulted.

- [ ] **Step 4: Delegate the old function temporarily**

Until Task 2 moves inherited push, change the start of
`push_inherited_attributes_to_pages` to call:

```rust
let Some(prepared) = crate::pages::repair::prepare_for_optimization(pdf)? else {
    return Ok(());
};
```

Pass `prepared.root` to the remaining push-only traversal. This keeps production green while
proving repair ownership has moved.

- [ ] **Step 5: Run page repair and inherited regression tests**

Run:

```bash
cargo test -p flpdf pages::repair::tests --lib
cargo test -p flpdf linearization::inherited_attrs::tests --lib
cargo test -p flpdf linearization::plan::tests --lib
```

Expected: PASS.

- [ ] **Step 6: Prove repair definitions moved once**

Run:

```bash
rg -n "fn repair_page_tree|fn next_object_ref|fn is_rectangle|prepare_for_optimization" crates/flpdf/src
```

Expected: repair helpers are defined only in `pages/repair.rs`; the old module contains only its
temporary call plus inherited-push logic.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/pages.rs crates/flpdf/src/pages/repair.rs crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "refactor: move optimization page repair into pages"
```

---

### Task 2: Move and complete pushInheritedAttributesToPage

**Files:**
- Create: `crates/flpdf/src/optimization/inherited_attrs.rs`
- Modify: `crates/flpdf/src/optimization.rs`
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`
- Test: `crates/flpdf/src/optimization/inherited_attrs.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_optimization.cc:120-235`

**Interfaces:**
- Produces:

```rust
pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    prepared: &crate::pages::repair::PreparedPages,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> crate::Result<()>;
```

- The ordinary Optimization call uses `allow_changes=true, warn_skipped_keys=false`.
- An explicit flattening-style caller can use warning mode; warnings are appended through
  `Pdf::push_warning`.

- [ ] **Step 1: Move push-only tests and add missing qpdf modes**

Move scalar/non-scalar inheritance, nearest ancestor, leaf override, null visibility, direct
resource mint order, malformed root, no-op, cycle, and depth tests from the old module.

Add:

```rust
#[test]
fn no_change_mode_rejects_inheritable_key_before_mutation() {
    let mut pdf = Pdf::open_mem_owned(pdf_with_inherited_scalar_rotate()).unwrap();
    let prepared = prepare_for_optimization(&mut pdf).unwrap().unwrap();
    let before = pdf.resolve(prepared.root).unwrap();
    let err =
        push_inherited_attributes_to_pages(&mut pdf, &prepared, false, false).unwrap_err();
    assert!(err.to_string().contains("inheritable attribute"));
    assert_eq!(pdf.resolve(prepared.root).unwrap(), before);
}

#[test]
fn warning_mode_reports_unknown_intermediate_pages_key() {
    let mut pdf = Pdf::open_mem_owned(pdf_with_unknown_intermediate_pages_key()).unwrap();
    let prepared = prepare_for_optimization(&mut pdf).unwrap().unwrap();
    push_inherited_attributes_to_pages(&mut pdf, &prepared, true, true).unwrap();
    assert!(pdf
        .repair_diagnostics()
        .iter()
        .any(|d| d.message.contains("Unknown key") && d.message.contains("/Pages")));
}
```

- [ ] **Step 2: Run new module tests and verify RED**

Run:

```bash
cargo test -p flpdf optimization::inherited_attrs::tests --lib
```

Expected: compile failure because the module/function is absent.

- [ ] **Step 3: Move push-only production code**

Move `INHERITABLE_KEYS`, ancestor stacks, direct non-scalar minting, null handling, leaf
application, and balanced stack cleanup into `optimization/inherited_attrs.rs`.

Use the prepared page root; do not call repair or rediscover the catalog root. For every
inheritable key:

```rust
if !allow_changes {
    return Err(crate::Error::Unsupported(
        "optimize detected an inheritable attribute when called in no-change mode".to_owned(),
    ));
}
```

For unknown keys on a non-root `/Pages` node when `warn_skipped_keys` is true:

```rust
pdf.push_warning(format!(
    "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
    String::from_utf8_lossy(key),
));
```

Do not remove the unknown key in this operation; qpdf only warns here because a later flattening
operation discards the intermediate node.

- [ ] **Step 4: Make the old module a temporary forwarding shell**

For one compile-safe commit, the old function may:

```rust
let Some(prepared) = crate::pages::repair::prepare_for_optimization(pdf)? else {
    return Ok(());
};
crate::optimization::inherited_attrs::push_inherited_attributes_to_pages(
    pdf,
    &prepared,
    true,
    false,
)
```

This forwarding shell is deleted in Task 4 and is not allowed in the final PR.

- [ ] **Step 5: Run inherited and plan tests**

Run:

```bash
cargo test -p flpdf optimization::inherited_attrs::tests --lib
cargo test -p flpdf linearization::inherited_attrs::tests --lib
cargo test -p flpdf linearization::plan::tests --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/optimization.rs crates/flpdf/src/optimization/inherited_attrs.rs crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "refactor: move inherited attributes into optimization"
```

---

### Task 3: Implement full optimize orchestration and compressed-object folding

**Files:**
- Modify: `crates/flpdf/src/optimization.rs`
- Modify: `crates/flpdf/src/optimization/inherited_attrs.rs`
- Test: `crates/flpdf/src/optimization.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_optimization.cc:50-119,340-380`

**Interfaces:**
- Replaces `build_after_inherited` with:

```rust
impl Optimization {
    pub(crate) fn optimize<R, F>(
        pdf: &mut Pdf<R>,
        object_stream_data: &BTreeMap<u32, u32>,
        allow_changes: bool,
        skip_stream_parameters: F,
    ) -> crate::Result<Self>
    where
        R: Read + Seek,
        F: FnMut(&Stream) -> u8;

    pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> crate::Result<()>;

    pub(crate) fn filter_compressed_objects(
        &mut self,
        object_stream_data: &BTreeMap<u32, u32>,
    );

    pub(crate) fn users_for_members<'a>(
        &self,
        members: impl IntoIterator<Item = &'a ObjectRef>,
    ) -> BTreeSet<ObjectUser>;
}
```

- Deletes `build_after_inherited` after all Task 4 consumers switch.

- [ ] **Step 1: Write failing optimize-order and direct-outline tests**

```rust
#[test]
fn optimize_makes_direct_outlines_indirect_before_building_maps() {
    let mut pdf = Pdf::open_mem_owned(direct_outlines_fixture()).unwrap();
    let maps = Optimization::optimize(&mut pdf, &BTreeMap::new(), true, |_| 1).unwrap();
    let outlines = maps.objects_for(&ObjectUser::RootKey(b"Outlines".to_vec()));
    assert_eq!(outlines.len(), 1);
    let catalog = pdf.resolve(pdf.root_ref().unwrap()).unwrap();
    assert!(matches!(
        catalog,
        Object::Dictionary(ref d) if matches!(d.get("Outlines"), Some(Object::Reference(_)))
    ));
}

#[test]
fn no_change_optimize_still_indirectizes_outlines_like_qpdf() {
    let mut pdf = Pdf::open_mem_owned(direct_outlines_fixture()).unwrap();
    let root = pdf.root_ref().unwrap();
    Optimization::optimize(&mut pdf, &BTreeMap::new(), false, |_| 1).unwrap();
    assert!(matches!(
        pdf.resolve(root).unwrap(),
        Object::Dictionary(ref d)
            if matches!(d.get("Outlines"), Some(Object::Reference(_)))
    ));
}
```

- [ ] **Step 2: Run optimize tests and verify RED**

Run:

```bash
cargo test -p flpdf optimization::tests::optimize_makes_direct_outlines_indirect_before_building_maps -- --exact
cargo test -p flpdf optimization::tests::no_change_optimize_still_indirectizes_outlines_like_qpdf -- --exact
```

Expected: compile failure because `optimize` is absent.

- [ ] **Step 3: Implement direct outline normalization and orchestration**

If catalog `/Outlines` is a direct dictionary, allocate exactly one new indirect object using the
same running-next-object rule as qpdf and replace the catalog value before consulting
`allow_changes`.

Then:

```rust
let prepared = crate::pages::repair::prepare_for_optimization(pdf)?;
if let Some(ref prepared) = prepared {
    inherited_attrs::push_inherited_attributes_to_pages(
        pdf,
        prepared,
        allow_changes,
        false,
    )?;
}
let page_refs = prepared
    .as_ref()
    .map(|prepared| prepared.pages.clone())
    .unwrap_or_default();
let mut maps = Self::build_maps(pdf, &page_refs, skip_stream_parameters)?;
maps.filter_compressed_objects(object_stream_data);
Ok(maps)
```

Rename the Task 2 map builder to private `build_maps`; there must be one public crate-level
orchestration route.

- [ ] **Step 4: Write failing compressed folding and member-union tests**

```rust
#[test]
fn filter_compressed_objects_rekeys_both_maps_to_container() {
    let member = ObjectRef::new(7, 3);
    let container = ObjectRef::new(20, 0);
    let user = ObjectUser::Page(0);
    let mut maps = Optimization::default();
    maps.record(user.clone(), member);
    maps.filter_compressed_objects(&BTreeMap::from([(7, 20)]));
    assert!(!maps.users_for(member).contains(&user));
    assert!(maps.users_for(container).contains(&user));
    assert!(maps.objects_for(&user).contains(&container));
}

#[test]
fn users_for_members_returns_the_union_once() {
    let mut maps = Optimization::default();
    let a = ObjectRef::new(3, 0);
    let b = ObjectRef::new(4, 0);
    maps.record(ObjectUser::Page(0), a);
    maps.record(ObjectUser::Thumbnail(1), b);
    assert_eq!(
        maps.users_for_members([&a, &b]),
        BTreeSet::from([ObjectUser::Page(0), ObjectUser::Thumbnail(1)])
    );
}
```

- [ ] **Step 5: Implement folding from a fresh pair of maps**

Do not mutate one direction and try to repair the other. Rebuild both maps from the existing
`user_to_objects` exactly like qpdf:

```rust
let target = object_stream_data
    .get(&object.number)
    .map(|&stream| ObjectRef::new(stream, 0))
    .unwrap_or(object);
filtered.record(user.clone(), target);
```

Replace `self` with the filtered map. Generation is always zero for the container.
Implement `users_for_members` through the same private set-union helper used while rebuilding the
filtered maps; do not create a second member-user union algorithm.

- [ ] **Step 6: Run full optimization tests**

Run:

```bash
cargo test -p flpdf optimization::tests --lib
cargo test -p flpdf pages::repair::tests --lib
cargo test -p flpdf optimization::inherited_attrs::tests --lib
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/optimization.rs crates/flpdf/src/optimization/inherited_attrs.rs
git commit -m "feat: complete qpdf optimization orchestration"
```

---

### Task 4: Cut every consumer over and delete the old module

**Files:**
- Modify: `crates/flpdf/src/linearization/mod.rs`
- Delete: `crates/flpdf/src/linearization/inherited_attrs.rs`
- Modify: `crates/flpdf/src/linearization/plan.rs:760-925,1450-1465,1950-2210,2940-3040`
- Modify: `crates/flpdf/src/linearization/writer.rs:2560-2600,3760-3820`
- Modify: `crates/flpdf/src/optimization.rs`
- Test: `crates/flpdf/src/linearization/plan.rs`
- Test: `crates/flpdf/src/linearization/writer.rs`
- Test: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`

**Interfaces:**
- `LinearizationPlan::from_pdf_with_object_stream_mode` calls
  `Optimization::optimize` with an empty compressed map because flpdf computes the final generated
  membership only after the initial per-object partition.
- The separate write handle calls `Optimization::push_inherited_attributes_to_pages`.
- ObjStm route classification consumes `Optimization::users_for_members`.
- Deletes `build_after_inherited` and the temporary old forwarding shell.

- [ ] **Step 1: Add a failing consumer-route test**

```rust
#[test]
fn generated_objstm_classification_uses_optimization_member_union() {
    let mut pdf = Pdf::open_mem_owned(one_other_page_plus_thumbnail_user_pdf_bytes()).unwrap();
    let plan =
        LinearizationPlan::from_pdf_with_object_stream_mode(
            &mut pdf,
            ObjectStreamMode::Generate,
        )
        .unwrap();
    let maps = plan.optimization.as_ref().unwrap();
    let members = [ObjectRef::new(5, 0), ObjectRef::new(6, 0)];
    assert!(maps
        .users_for_members(members.iter())
        .iter()
        .any(|user| matches!(user, ObjectUser::Thumbnail(_))));
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf linearization::plan::tests::generated_objstm_classification_uses_optimization_member_union -- --exact
```

Expected: FAIL/compile failure while routing still uses duplicated member predicates.

- [ ] **Step 3: Switch plan construction to optimize**

Build the unfiltered object-user snapshot:

```rust
let optimization = Optimization::optimize(
    pdf,
    &BTreeMap::new(),
    true,
    |_| 1,
)?;
```

Remove the earlier direct inherited push and Task 2 `build_after_inherited` call.
qpdf's writer knows final ObjStm membership before `optimize`; flpdf's planner computes membership
after its first per-object partition. The next step applies the same `filterCompressedObjects`
member-user union centrally at that later boundary, without pretending source xref membership is
the final generated membership.

- [ ] **Step 4: Centralize generated/preserved member union**

In both generate and preserve ObjStm batch routes:

```rust
let users = optimization.users_for_members(members.iter());
let part = classify_container_users(&users, optimization);
```

`classify_container_users` may remain in `linearization/plan.rs` because part numbers and
precedence are linearization responsibilities, but it receives an already-unioned set and
contains no member loop or map traversal.

- [ ] **Step 5: Switch the writer's separate handle**

Replace:

```rust
crate::linearization::inherited_attrs::push_inherited_attributes_to_pages(pdf)?;
```

with:

```rust
crate::optimization::Optimization::push_inherited_attributes_to_pages(pdf)?;
```

Keep the existing placement after option validation and before any write-handle object
resolution.

- [ ] **Step 6: Delete old module and obsolete helpers**

Delete:

- `crates/flpdf/src/linearization/inherited_attrs.rs`;
- `mod inherited_attrs;` from `linearization/mod.rs`;
- `Optimization::build_after_inherited`;
- duplicated member-union loops/predicates;
- old comments claiming `linearization/plan.rs` implements `QPDF_optimization`.

- [ ] **Step 7: Run focused and byte suites**

Run:

```bash
cargo test -p flpdf pages::repair::tests --lib
cargo test -p flpdf optimization::tests --lib
cargo test -p flpdf optimization::inherited_attrs::tests --lib
cargo test -p flpdf linearization::plan::tests --lib
cargo test -p flpdf linearization::writer::tests --lib
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf-cli --test cli_linearize
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_linearize_qpdf
```

Expected: PASS with unchanged output bytes.

- [ ] **Step 8: Prove old implementation deletion**

Run:

```bash
test ! -e crates/flpdf/src/linearization/inherited_attrs.rs
rg -n "linearization::inherited_attrs|build_after_inherited|fn push_inherited_attributes_to_pages|fn repair_page_tree|users\\.thumbnails|users\\.document_other" crates/flpdf/src
```

Expected:

- no old path or builder matches;
- exactly one inherited-push definition in `optimization/inherited_attrs.rs`;
- exactly one repair definition in `pages/repair.rs`;
- no direct field-by-field member-union implementation in `linearization/plan.rs`.

- [ ] **Step 9: Commit**

```bash
git add crates/flpdf/src/pages.rs crates/flpdf/src/pages/repair.rs crates/flpdf/src/optimization.rs crates/flpdf/src/optimization/inherited_attrs.rs crates/flpdf/src/linearization/mod.rs crates/flpdf/src/linearization/plan.rs crates/flpdf/src/linearization/writer.rs crates/flpdf/tests/linearize_objstm_generate_tests.rs
git add -u crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "refactor: complete optimization consumer cutover"
```

---

### Task 5: D1/D2 evidence, workspace gates, and patch coverage

**Files:**
- Modify if generated output changes: `docs/qpdf-correspondence.md`
- Verify: all Task 1-4 files

**Interfaces:**
- Produces no new API; closes the Optimization component only if every gate passes.

- [ ] **Step 1: Compare public qpdf responsibility inventory**

Against qpdf 11.9.0, record evidence for:

```text
ObjUser construction and ordering
optimize(object_stream_data, allow_changes, skip_stream_parameters)
pushInheritedAttributesToPage
pushInheritedAttributesToPageInternal
updateObjectMaps
updateObjectMapsInternal
filterCompressedObjects
bidirectional maps
```

Run:

```bash
rg -n "ObjectUser|fn optimize|push_inherited|update_object_maps|filter_compressed|user_to_objects|object_to_users" crates/flpdf/src/optimization.rs crates/flpdf/src/optimization
```

Expected: every responsibility maps to one Rust definition or a documented Rust ownership
substitution.

- [ ] **Step 2: Run D2 duplicate audit**

Run:

```bash
rg -n "QPDF_optimization|page_object_users|closure_from_seeds|open_document_set|document_other_set|outlines_set|linearization::inherited_attrs|build_after_inherited" crates/flpdf/src
rg -n "fn repair_page_tree|fn push_inherited_attributes_to_pages|fn update_object_maps|fn filter_compressed_objects" crates/flpdf/src
```

Expected: correspondence references are truthful; old helper names are absent; each responsibility
has one definition.

- [ ] **Step 3: Check/regenerate qpdf correspondence**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts.tests.test_qpdf_module_docs
```

Regenerate only if required. Inspect that:

- `optimization.rs` / `optimization/inherited_attrs.rs` mirror
  `QPDF_optimization.cc`;
- `pages/repair.rs` mirrors `QPDF_pages.cc`;
- `linearization/plan.rs` maps only to `QPDF_linearization.cc` responsibilities.

- [ ] **Step 4: Run formatting, lint, rustdoc, and workspace tests**

Run:

```bash
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings --document-private-items" cargo doc --workspace --all-features --no-deps
cargo test --workspace --all-features
git diff --check
```

Expected: all exit 0.

- [ ] **Step 5: Run affected byte/oracle tests**

Run:

```bash
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf-cli --test cli_linearize
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_linearize_qpdf
cargo run --bin flpdf -- --check tests/fixtures/minimal.pdf
```

Expected: PASS.

- [ ] **Step 6: Measure fresh direct-parent patch coverage**

Use the recorded tip of the object-user-map parent branch:

```bash
base_ref="origin/feature/flpdf-qxba-phase2-optimization-users"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path /tmp/flpdf-optimization-complete.lcov
scripts/patch-coverage.sh "$base_ref" HEAD /tmp/flpdf-optimization-complete.lcov
```

Expected: 100% changed executable lines.

- [ ] **Step 7: Commit generated correspondence only if changed**

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: mark optimization component complete"
```

Skip an empty commit.

- [ ] **Step 8: Record clean completion evidence**

Run:

```bash
git status --short --branch
git log --oneline --decorate -10
```

Expected: clean worktree, no old module, and a truthful complete correspondence entry.
