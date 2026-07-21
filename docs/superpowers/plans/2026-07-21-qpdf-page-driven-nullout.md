# qpdf Page-Driven Null-Out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace action-specific removed-page discovery in `extract_pages` and `merge_documents` with qpdf-compatible page-set null-out that safely handles long indirect `/Next` array-holder chains.

**Architecture:** Extend the page-closure BFS with a direct-object root that always treats referenced pages as boundaries. After each source copy, intersect the closure with the source's unselected page leaves and replace only those copied page objects with `Object::Null`.

**Tech Stack:** Rust workspace, `flpdf` unit/integration tests, qpdf 11.9.0 oracle, Cargo fmt/clippy/test.

## Global Constraints

- Page membership, never action subtype parsing, decides which copied objects become null.
- Do not reject malformed nested `/Next` arrays solely because they are nested.
- Keep `MAX_INLINE_DEPTH` for one direct PDF object; indirect holder chains use iterative BFS.
- Do not allocate placeholders for removed pages absent from the generic closure.
- Keep the CLI `--pages` pipeline unchanged; it already nulls `RebuildResult::removed_pages`.
- Do not weaken or delete tests merely to obtain a green build.

---

### Task 1: Add a generic direct-object closure root

**Files:**
- Modify: `crates/flpdf/src/page_closure.rs`

**Interfaces:**
- Consumes: `collect_refs_in_object`, `Pdf::resolve_borrowed`, page/catalog boundary detection.
- Produces: `pub(crate) fn extend_object_closure<R: Read + Seek>(pdf: &mut Pdf<R>, root: &Object, visited: &mut BTreeSet<ObjectRef>) -> Result<()>`.
- Preserves: `extend_page_object_closure` force-traverses its selected top-level page.

- [ ] **Step 1: Write the failing unit test**

Add this test to `page_closure.rs`'s existing test module:

```rust
#[test]
fn direct_root_follows_long_indirect_array_chain_but_stops_at_page() {
    let mut owned: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (3, "<< /Type /Page /Parent 2 0 R /Contents 90 0 R >>".into()),
        (90, "<< /Length 0 >>\nstream\n\nendstream".into()),
    ];
    for number in 10..80 {
        owned.push((number, format!("[{} 0 R]", number + 1)));
    }
    owned.push((80, "[3 0 R]".into()));
    let borrowed: Vec<(u32, &str)> = owned
        .iter()
        .map(|(number, body)| (*number, body.as_str()))
        .collect();
    let mut pdf = Pdf::open_mem_owned(build_pdf(&borrowed, 1)).unwrap();
    let mut closure = BTreeSet::new();

    extend_object_closure(
        &mut pdf,
        &Object::Reference(ObjectRef::new(10, 0)),
        &mut closure,
    )
    .unwrap();

    assert!(closure.contains(&ObjectRef::new(3, 0)));
    assert!(!closure.contains(&ObjectRef::new(90, 0)));
}
```

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p flpdf page_closure::tests::direct_root_follows_long_indirect_array_chain_but_stops_at_page -- --exact
```

Expected: compilation fails because `extend_object_closure` does not exist.

- [ ] **Step 3: Implement the shared iterative BFS**

Extract the queue loop from `extend_page_object_closure` into:

```rust
fn extend_closure_from_queue<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut queue: VecDeque<ObjectRef>,
    top_page: Option<ObjectRef>,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut refs_found = Vec::new();
    while let Some(current_ref) = queue.pop_front() {
        let obj = pdf.resolve_borrowed(current_ref)?;
        if Some(current_ref) != top_page {
            if let Object::Dictionary(dict) = obj {
                let boundary = dict
                    .get("Type")
                    .and_then(Object::as_name)
                    .is_some_and(|name| name == b"Page" || name == b"Catalog");
                if boundary {
                    continue;
                }
            }
        }
        collect_refs_in_object(obj, 0, &mut refs_found)?;
        for reference in refs_found.drain(..) {
            if visited.insert(reference) {
                queue.push_back(reference);
            }
        }
    }
    Ok(())
}

pub(crate) fn extend_object_closure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: &Object,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut refs = Vec::new();
    collect_refs_in_object(root, 0, &mut refs)?;
    let mut queue = VecDeque::new();
    for reference in refs {
        if visited.insert(reference) {
            queue.push_back(reference);
        }
    }
    extend_closure_from_queue(pdf, queue, None, visited)
}
```

Rewrite `extend_page_object_closure` to enqueue `page_ref` unconditionally and
call `extend_closure_from_queue(pdf, queue, Some(page_ref), visited)`.

- [ ] **Step 4: Verify GREEN**

```bash
cargo test -p flpdf page_closure::tests
```

Expected: all page-closure tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/page_closure.rs
git commit -m "refactor(flpdf-0hrl): add generic page-boundary closure root"
```

---

### Task 2: Make `extract_pages` null copied pages by page membership

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs`
- Modify: `crates/flpdf/tests/page_extract_tests.rs`

**Interfaces:**
- Consumes: `all_pages`, selected source refs, generic closure, and `copy_objects` map.
- Produces: `pub(crate) fn null_copied_removed_pages<R: Read + Seek>(target: &mut Pdf<R>, all_pages: &[ObjectRef], selected: &BTreeSet<ObjectRef>, closure: &BTreeSet<ObjectRef>, map: &BTreeMap<ObjectRef, ObjectRef>)`.

- [ ] **Step 1: Write the failing extraction regression**

Add this fixture and test near existing `/Next` extraction tests:

```rust
fn long_indirect_next_array_pdf() -> Vec<u8> {
    let mut owned: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into()),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>".into()),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>".into()),
        (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (https://example.test) /Next 10 0 R >> >>".into()),
    ];
    for number in 10..80 {
        owned.push((number, format!("[{} 0 R]", number + 1)));
    }
    owned.push((80, "[<< /S /GoTo /D [4 0 R /Fit] >>]".into()));
    let borrowed: Vec<(u32, &str)> = owned
        .iter()
        .map(|(number, body)| (*number, body.as_str()))
        .collect();
    build_pdf(&borrowed, 1)
}

fn action_after_71_array_holders(
    doc: &mut Pdf<std::io::Cursor<Vec<u8>>>,
    mut value: Object,
) -> flpdf::Dictionary {
    for _ in 0..=70 {
        let concrete = match value {
            Object::Reference(reference) => doc.resolve(reference).unwrap(),
            direct => direct,
        };
        let mut items = concrete.into_array().expect("singleton action array");
        assert_eq!(items.len(), 1);
        value = items.remove(0);
    }
    value.into_dict().expect("terminal GoTo action")
}

#[test]
fn long_indirect_next_array_keeps_carrier_and_nulls_removed_page() {
    let bytes = long_indirect_next_array_pdf();
    let mut source = Pdf::open_mem(&bytes).unwrap();
    let mut out = extract_page(&mut source, 0).unwrap();

    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = leaf
        .get("Annots")
        .and_then(Object::as_array)
        .and_then(|items| items.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").and_then(Object::as_dict).unwrap();
    let terminal = action_after_71_array_holders(
        &mut out,
        action.get("Next").cloned().expect("/Next is preserved"),
    );
    let removed_page = terminal
        .get("D")
        .and_then(Object::as_array)
        .and_then(|items| items.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    assert!(matches!(out.resolve(removed_page).unwrap(), Object::Null));
}
```

- [ ] **Step 2: Verify RED**

```bash
cargo test -p flpdf --test page_extract_tests long_indirect_next_array_keeps_carrier_and_nulls_removed_page -- --exact
```

Expected: FAIL because the second page remains a live off-tree `/Type /Page`.

- [ ] **Step 3: Add the shared null-out helper**

Import `BTreeMap` beside `BTreeSet`, then add:

```rust
pub(crate) fn null_copied_removed_pages<R: Read + Seek>(
    target: &mut Pdf<R>,
    all_pages: &[ObjectRef],
    selected: &BTreeSet<ObjectRef>,
    closure: &BTreeSet<ObjectRef>,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) {
    for source_page in all_pages {
        if !selected.contains(source_page) && closure.contains(source_page) {
            if let Some(&copied_page) = map.get(source_page) {
                target.set_object(copied_page, Object::Null);
            }
        }
    }
}
```

In `extract_pages`, create `selected_set` from `unique`, call the helper after
`copy_objects`, and remove the loop calling `neutralize_absent_dests`.

- [ ] **Step 4: Remove obsolete extraction traversal**

Delete `MAX_ACTION_CHAIN_DEPTH` and these private functions, which have no
callers after Step 3:

```text
neutralize_absent_dests
neutralize_bead_ring
neutralize_annot_if_absent
neutralize_aa_if_absent
neutralize_action_chain
neutralize_action_array
dest_targets_absent_page
sd_targets_absent_page
p_targets_absent_page
```

Remove unused imports and update module docs: carriers keep their page refs and
the copied removed page resolves to `null`.

- [ ] **Step 5: Update old extraction assertions**

Tests that expected `/Dest`, `/D`, `/SD`, or `/P` removal must instead assert
that the carrier remains and its copied removed-page target resolves to
`Object::Null`. Keep surviving-page and non-page destination assertions intact.

- [ ] **Step 6: Verify GREEN and commit**

```bash
cargo test -p flpdf --test page_extract_tests
git add crates/flpdf/src/page_extract.rs crates/flpdf/tests/page_extract_tests.rs
git commit -m "fix(flpdf-0hrl): null copied removed pages during extraction"
```

Expected: all extraction tests pass before the commit.

---

### Task 3: Make `merge_documents` use page-driven null-out

**Files:**
- Modify: `crates/flpdf/src/page_merge.rs`
- Modify: `crates/flpdf/tests/page_merge_tests.rs`

**Interfaces:**
- Consumes: `page_extract::null_copied_removed_pages` and `page_closure::extend_object_closure`.
- Produces: unchanged public `merge_documents` API.

- [ ] **Step 1: Write the failing merge regression**

Create `long_indirect_open_action_array_pdf` with the same objects as Task 2,
except put the action directly on the catalog:

```rust
(1, "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /URI /URI (https://example.test) /Next 10 0 R >> >>")
```

Keep objects 10 through 79 as one-element arrays referencing the next object,
and object 80 as `[<< /S /GoTo /D [4 0 R /Fit] >>]`. Add:

```rust
#[test]
fn merge_long_indirect_open_action_array_chain_nulls_removed_page() {
    let mut source = Pdf::open_mem_owned(long_indirect_open_action_array_pdf()).unwrap();
    let mut inputs = [MergeInput {
        source: &mut source,
        pages: vec![0],
    }];
    let mut out = merge_documents(&mut inputs).unwrap();

    assert_eq!(count_type(&mut out, b"Page"), 1);
    let catalog = catalog_dict(&mut out);
    let open_action = catalog
        .get("OpenAction")
        .and_then(Object::as_dict)
        .expect("direct /OpenAction is preserved");
    let terminal = action_after_71_array_holders(
        &mut out,
        open_action
            .get("Next")
            .cloned()
            .expect("/OpenAction /Next is preserved"),
    );
    let removed_page = terminal
        .get("D")
        .and_then(Object::as_array)
        .and_then(|items| items.first())
        .and_then(Object::as_ref_id)
        .unwrap();
    assert!(matches!(out.resolve(removed_page).unwrap(), Object::Null));
}
```

Copy the exact `count_type` and `action_after_71_array_holders` helpers from
Task 2 into `page_merge_tests.rs`; integration-test crates cannot share private
helpers.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p flpdf --test page_merge_tests merge_long_indirect_open_action_array_chain_nulls_removed_page -- --exact
```

Expected: FAIL because the inline `/OpenAction` fold/remap stops at 64 levels.

- [ ] **Step 3: Fold and wire direct `/OpenAction` generically**

Import `extend_object_closure`. Replace the inline-action-specific fold with:

```rust
if let Some(inline) = &doc.open_action_inline {
    extend_object_closure(source, inline, closure)?;
}
```

Replace inline `/OpenAction` wiring with:

```rust
} else if let Some(inline) = &doc.open_action_inline {
    catalog.insert("OpenAction", remap_refs_in_object(inline.clone(), map));
}
```

- [ ] **Step 4: Replace semantic target collection**

After all selected-page and primary document-level closure folds, construct
`selected_set` from `unique`. Delete the calls to
`collect_removed_dest_targets` and `collect_doc_level_removed_targets`. After
`copy_objects`, call:

```rust
null_copied_removed_pages(
    &mut target,
    &all,
    &selected_set,
    &closure,
    &map,
);
```

Do not insert every unselected page into `closure`.

- [ ] **Step 5: Remove obsolete merge walkers**

Delete the now-unused constant/functions:

```text
MAX_ACTION_CHAIN_DEPTH
collect_removed_dest_targets
collect_aa_dest_targets
collect_dest_target
collect_action_chain_targets
collect_sd_target
fold_inline_action_operands
collect_doc_level_removed_targets
collect_outline_doc_dests
collect_outline_removed_targets
collect_name_tree_removed_targets
remap_inline_action
remap_inline_action_depth
remap_next_continuation
```

Retain destination/name-tree helpers still used by inline legacy destinations
or inline name-tree reconstruction. Confirm each deletion with `rg` and remove
only imports left with zero callers.

- [ ] **Step 6: Update tests/docs, verify GREEN, and commit**

Update old assertions to expect retained carriers pointing at copied null page
boundaries. Remove module-doc claims about semantic `/Next` target discovery.

```bash
cargo test -p flpdf --test page_merge_tests
git add crates/flpdf/src/page_merge.rs crates/flpdf/tests/page_merge_tests.rs
git commit -m "fix(flpdf-0hrl): mirror qpdf page-boundary null-out in merges"
```

Expected: all merge tests pass before the commit.

---

### Task 4: Verify, review, close, and push

**Files:**
- Modify only if evidence requires: `docs/superpowers/specs/2026-07-21-qpdf-page-driven-nullout-design.md`
- Beads state: `flpdf-0hrl`

**Interfaces:**
- Consumes: Task 1 through Task 3 commits.
- Produces: reviewed, tested, pushed branch and closed issue.

- [ ] **Step 1: Run focused and formatting gates**

```bash
cargo fmt
cargo fmt --all -- --check
cargo test -p flpdf --test page_extract_tests
cargo test -p flpdf --test page_merge_tests
cargo test -p flpdf outline_dest_remap::tests
```

Expected: every command exits 0.

- [ ] **Step 2: Run full quality gates**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf
cargo test
```

Expected: every command exits 0 and clippy emits no warning promoted to error.

- [ ] **Step 3: Run byte-identical and compatibility gates**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf-cli --test compat_matrix_tests
scripts/patch-coverage.sh
```

Expected: byte gates pass, qpdf-backed compatibility passes, and patch
coverage reaches the repository's 100% threshold.

- [ ] **Step 4: Request code review**

Use `superpowers:requesting-code-review` for all implementation commits after
design commit `5bc1fd93`. Address each actionable finding with a failing test
before changing production code.

- [ ] **Step 5: Close and push Beads state**

```bash
bd close flpdf-0hrl --reason="Replaced action-specific removed-page discovery with qpdf-style page-driven null-out; deep indirect holder regressions and quality gates pass"
bd dolt push
```

- [ ] **Step 6: Push Git and verify clean status**

```bash
git push -u origin fix/flpdf-0hrl-qpdf-page-nullout
git status --short --branch
```

Expected: clean branch tracking its pushed remote.
