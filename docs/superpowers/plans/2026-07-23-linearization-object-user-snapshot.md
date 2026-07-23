# Linearization Object-User Snapshot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build qpdf-compatible linearization object-user data once in `LinearizationPlan::from_pdf` and reuse it for classic partitioning plus Generate/Preserve ObjStm routing.

**Architecture:** Add a private `LinearizationRoutingUsers` snapshot to `LinearizationPlan`, while keeping `all_referenced_pages` as the existing authoritative object-to-page map. Refactor the document-user helpers to accept shared live/page-tree inputs, then make `route_objstm_containers` a pure classifier over the retained snapshot and member lists.

**Tech Stack:** Rust 2021, existing `Pdf`/`ObjectRef`/`LinearizationPlan` code, qpdf 11.9.0 as the behavioral and byte oracle, Beads (`bd`), and `cargo llvm-cov` through `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative.
- Keep existing byte-identical output unless a new fixture proves a difference within this object-user contract.
- Record unrelated parity gaps as separate Beads issues.
- Do not change non-linearized object streams, ObjStm eligibility, Generate even split, Preserve source-container reconstruction, compression, or general reader behavior.
- Every changed executable line under `crates/flpdf/src` must have 100% patch coverage from the final committed `HEAD`.
- Keep `LinearizationPlan::all_referenced_pages` as the sole retained object-to-page inverse map; do not duplicate it in the new snapshot.

---

## File Map

- Modify: `crates/flpdf/src/linearization/plan.rs`
  - Define and populate the retained routing snapshot.
  - Share xref-wide inputs among object-user traversals.
  - Convert ObjStm routing from PDF analysis to a pure classifier.
  - Update and extend unit tests in the existing `#[cfg(test)]` module.
- Verify unchanged: `crates/flpdf/src/linearization/writer.rs`
  - No routing reclassification or output behavior change is expected.
- Verify unchanged: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`
  - Existing strict qpdf 11.9.0 Generate/Preserve goldens are the byte gate.
- Verify unchanged: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`
  - Existing linearized ObjStm integration tests are the structural gate.

### Task 1: Retain One Qpdf-Derived Routing Snapshot

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:556-686`
- Modify: `crates/flpdf/src/linearization/plan.rs:793-952`
- Modify: `crates/flpdf/src/linearization/plan.rs:1316-1434`
- Modify: `crates/flpdf/src/linearization/plan.rs:1859-1886`
- Modify: `crates/flpdf/src/linearization/plan.rs:2363-2481`
- Modify: `crates/flpdf/src/linearization/plan.rs:2861-2889`
- Test: `crates/flpdf/src/linearization/plan.rs` unit-test module

**Interfaces:**
- Consumes: existing `PageObjectUsers`, `page_object_users`, `page_tree_node_refs`, `closure_from_seeds`, `open_document_set`, `document_other_set`, `outlines_set`, and `outlines_in_first_page_predicate`.
- Produces: private `LinearizationRoutingUsers`; private `LinearizationPlan::routing_users: Option<LinearizationRoutingUsers>`; context-taking document-user helpers that reuse `&BTreeSet<ObjectRef>` inputs.

- [ ] **Step 1: Add a failing snapshot-consistency test**

Add this test beside the existing `route_objstm_containers_distinguishes_first_page_private_and_shared` test:

```rust
#[test]
fn from_pdf_retains_routing_users_consistent_with_page_map() {
    let mut pdf = Pdf::open(Cursor::new(thumb_first_page_shared_pdf_bytes())).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
    let users = plan
        .routing_users
        .as_ref()
        .expect("from_pdf must retain object-user routing data");

    let page_zero: BTreeSet<ObjectRef> = plan
        .all_referenced_pages
        .iter()
        .filter_map(|(&object_ref, pages)| pages.contains(&0).then_some(object_ref))
        .collect();

    assert_eq!(users.first_page, page_zero);
    assert!(
        users.thumbnails.contains(&ObjectRef::new(5, 0)),
        "the fixture's /Thumb target must retain a thumbnail user"
    );
    assert!(
        users.first_page.contains(&ObjectRef::new(6, 0)),
        "the fixture's first-page-private object must retain page 0"
    );
}
```

- [ ] **Step 2: Run the test and confirm RED**

Run:

```bash
cargo test -p flpdf --lib from_pdf_retains_routing_users_consistent_with_page_map
```

Expected: compilation fails because `LinearizationPlan` has no `routing_users` field.

- [ ] **Step 3: Define the retained snapshot and plan field**

Add this private type immediately before `LinearizationPlan`:

```rust
#[derive(Debug, Clone)]
struct LinearizationRoutingUsers {
    first_page: BTreeSet<ObjectRef>,
    thumbnails: BTreeSet<ObjectRef>,
    outlines: BTreeSet<ObjectRef>,
    outlines_in_first_page: bool,
    open_document: BTreeSet<ObjectRef>,
    document_other: BTreeSet<ObjectRef>,
}
```

Add this private field at the end of `LinearizationPlan`:

```rust
/// Retained qpdf-style object-user signals used to route generated and
/// preserved ObjStm containers without re-reading the PDF.
routing_users: Option<LinearizationRoutingUsers>,
```

Initialize it to `None` in `Default::default()`.

- [ ] **Step 4: Split document-user helpers into shared-context implementations**

Retain the current one-argument entry points as wrappers for focused tests and
the independent writer call:

```rust
fn open_document_set<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<BTreeSet<ObjectRef>> {
    let page_tree = page_tree_node_refs(pdf)?;
    let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
    open_document_set_with_context(pdf, &page_tree, &live)
}

fn document_other_set<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let page_tree = page_tree_node_refs(pdf)?;
    let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
    document_other_set_with_context(pdf, &page_tree, &live)
}

pub(crate) fn outlines_set<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let page_tree = page_tree_node_refs(pdf)?;
    let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
    outlines_set_with_context(pdf, &page_tree, &live)
}
```

Rename each current function body to its `_with_context` form with these exact
signatures:

```rust
fn open_document_set_with_context<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_tree: &BTreeSet<ObjectRef>,
    live: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>>;

fn document_other_set_with_context<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_tree: &BTreeSet<ObjectRef>,
    live: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>>;

fn outlines_set_with_context<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_tree: &BTreeSet<ObjectRef>,
    live: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>>;
```

Within each renamed body, keep every existing seed-building statement
unchanged, delete only its final local `page_tree` and `live` declarations, and
replace the final expression with:

```rust
closure_from_seeds(pdf, seeds, page_tree, live)
```

Do not change seed collection, page-boundary handling, inherited-key handling,
or `visited` semantics.

- [ ] **Step 5: Compute shared inputs and sets once in `from_pdf`**

After page refs are collected, compute these once:

```rust
let page_refs: Vec<ObjectRef> = crate::pages::page_refs(pdf)?;
let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
let page_tree = page_tree_node_refs(pdf)?;
let page_object_users = page_object_users(pdf, &page_refs, &live, &resurrectable)?;

let open_document_set =
    open_document_set_with_context(pdf, &page_tree, &live)?;
let all_outline_refs =
    outlines_set_with_context(pdf, &page_tree, &live)?;
let document_other_set =
    document_other_set_with_context(pdf, &page_tree, &live)?;
```

Move the existing open-document/outline/document-other calculations to this
boundary. Keep `eligibility_context(pdf)` mode-dependent and unchanged. Remove
the later duplicate `page_refs` and `live` declarations.

- [ ] **Step 6: Populate the snapshot in the returned plan**

Reuse the already-computed `first_page_set`, `thumbnail_user_set`, and
`outlines_in_first_page` locals. Add this field to `Ok(Self { ... })`:

```rust
routing_users: Some(LinearizationRoutingUsers {
    first_page: first_page_set,
    thumbnails: thumbnail_user_set,
    outlines: all_outline_refs,
    outlines_in_first_page,
    open_document: open_document_set,
    document_other: document_other_set,
}),
```

Move these sets only at the final return, after classic partitioning and hint
construction have finished using them.

- [ ] **Step 7: Run focused tests and confirm GREEN**

Run:

```bash
cargo fmt --all
cargo test -p flpdf --lib from_pdf_retains_routing_users_consistent_with_page_map
cargo test -p flpdf --lib linearization::plan::tests
```

Expected: the new consistency test and all existing plan tests pass.

- [ ] **Step 8: Commit the retained-analysis layer**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "refactor(linearize): retain object-user routing snapshot"
```

### Task 2: Make ObjStm Routing Pure and Remove the Second Analysis

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:1956-1987`
- Modify: `crates/flpdf/src/linearization/plan.rs:2009-2027`
- Modify: `crates/flpdf/src/linearization/plan.rs:2134-2137`
- Modify: `crates/flpdf/src/linearization/plan.rs:2919-3058`
- Test: `crates/flpdf/src/linearization/plan.rs:6338-6443`
- Test: `crates/flpdf/src/linearization/plan.rs:6558-6606`
- Test: `crates/flpdf/src/linearization/plan.rs:6917-6959`

**Interfaces:**
- Consumes: `LinearizationPlan::routing_users`, `LinearizationPlan::all_referenced_pages`, and the `LinearizationRoutingUsers` type from Task 1.
- Produces: pure `route_objstm_containers(&LinearizationRoutingUsers, &BTreeMap<ObjectRef, BTreeSet<u32>>, &[Vec<ObjectRef>]) -> Vec<ContainerPart>`; explicit missing-snapshot invariant error before Generate/Preserve batching.

- [ ] **Step 1: Add a failing missing-snapshot test**

Add this test beside `objstm_batches_disable_yields_empty_plan`:

```rust
#[test]
fn objstm_batches_generate_rejects_missing_routing_snapshot() {
    let bytes = three_page_shared_content_bytes();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let err = LinearizationPlan::default()
        .objstm_batches(&mut pdf, &generate_config())
        .expect_err("hand-built Generate plan must require a routing snapshot");

    assert!(matches!(
        err,
        crate::Error::Unsupported(ref message)
            if message == "linearization plan: missing object-user routing snapshot"
    ));
}
```

- [ ] **Step 2: Run the test and confirm RED**

Run:

```bash
cargo test -p flpdf --lib objstm_batches_generate_rejects_missing_routing_snapshot
```

Expected: FAIL because the current implementation returns an empty successful
batch plan after filtering against the default plan's empty assigned set.

- [ ] **Step 3: Enforce the snapshot invariant at the batching boundary**

Immediately after the Disable early return in `objstm_batches`, add:

```rust
let routing_users = self.routing_users.as_ref().ok_or_else(|| {
    crate::Error::Unsupported(
        "linearization plan: missing object-user routing snapshot".to_string(),
    )
})?;
```

Pass `routing_users` to both private batching helpers:

```rust
self.objstm_batches_generate(
    pdf,
    config,
    &ctx,
    &length_exclusions,
    routing_users,
)?;

self.objstm_batches_preserve(
    pdf,
    config,
    &ctx,
    &length_exclusions,
    routing_users,
)?;
```

Add the matching final parameter to both helper signatures:

```rust
routing_users: &LinearizationRoutingUsers,
```

- [ ] **Step 4: Replace the PDF-reading router with a pure classifier**

Replace the current generic/fallible signature and all analysis at the top of
`route_objstm_containers` with:

```rust
fn route_objstm_containers(
    users: &LinearizationRoutingUsers,
    referenced_pages: &BTreeMap<ObjectRef, BTreeSet<u32>>,
    containers: &[Vec<ObjectRef>],
) -> Vec<ContainerPart> {
    containers
        .iter()
        .map(|members| {
            if members.iter().any(|m| users.outlines.contains(m)) {
                return if users.outlines_in_first_page {
                    ContainerPart::FirstPageOutlines
                } else {
                    ContainerPart::Rest
                };
            }
            if members.iter().any(|m| users.open_document.contains(m)) {
                return ContainerPart::OpenDocument;
            }
            if members.iter().any(|m| users.first_page.contains(m)) {
                let has_other_page = members.iter().any(|member| {
                    referenced_pages
                        .get(member)
                        .is_some_and(|pages| pages.iter().any(|&page| page != 0))
                });
                let has_document_other =
                    members.iter().any(|m| users.document_other.contains(m));
                let has_thumbnail =
                    members.iter().any(|m| users.thumbnails.contains(m));
                return if has_other_page || has_document_other || has_thumbnail {
                    ContainerPart::FirstPageShared
                } else {
                    ContainerPart::FirstPagePrivate
                };
            }

            let mut other_pages = BTreeSet::new();
            for member in members {
                if let Some(pages) = referenced_pages.get(member) {
                    other_pages.extend(pages.iter().copied().filter(|&page| page != 0));
                }
            }
            match other_pages.len() {
                0 => ContainerPart::Rest,
                1 if members.iter().any(|m| {
                    users.document_other.contains(m) || users.thumbnails.contains(m)
                }) =>
                {
                    ContainerPart::Rest
                }
                1 => ContainerPart::OtherPagePrivate,
                _ => ContainerPart::OtherPageShared,
            }
        })
        .collect()
}
```

Update its rustdoc: remove the `# Errors` section and state explicitly that it
does not resolve objects or read the PDF.

- [ ] **Step 5: Route Generate and Preserve from retained data**

Replace both production calls with:

```rust
let routes = route_objstm_containers(
    routing_users,
    &self.all_referenced_pages,
    &containers,
);
```

Do not change batching, stable part buckets, member sorting, source container
numbers, or `push_routed_objstm_batch`.

- [ ] **Step 6: Update direct routing tests without rebuilding analysis**

Add this test-only helper inside the existing unit-test module:

```rust
fn route_with_plan(
    plan: &LinearizationPlan,
    containers: &[Vec<ObjectRef>],
) -> Vec<ContainerPart> {
    route_objstm_containers(
        plan.routing_users
            .as_ref()
            .expect("test plan must have routing users"),
        &plan.all_referenced_pages,
        containers,
    )
}
```

For every direct routing test:

1. Construct `let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();`
   once.
2. Preserve the existing synthetic container members.
3. Replace `route_objstm_containers(&mut pdf, ...)?.unwrap()` with
   `route_with_plan(&plan, ...)`.

For tests that also call `objstm_membership_linearized`, retain the same `plan`
instead of constructing it only to extract `renumber_assigned_refs`:

```rust
let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
let assigned = plan.renumber_assigned_refs();
let containers = objstm_membership_linearized(&mut pdf, &assigned).unwrap();
let routes = route_with_plan(&plan, &containers);
```

- [ ] **Step 7: Run focused RED-to-GREEN tests**

Run:

```bash
cargo fmt --all
cargo test -p flpdf --lib objstm_batches_generate_rejects_missing_routing_snapshot
cargo test -p flpdf --lib route_objstm_containers
cargo test -p flpdf --lib linearized_routes
cargo test -p flpdf --lib generate_route_others_gate
cargo test -p flpdf --lib linearization::plan::tests
```

Expected: all commands pass. The routing function has no `Pdf`, `Read`, `Seek`,
or `crate::Result` in its signature.

- [ ] **Step 8: Run strict qpdf parity tests**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --test linearize_objstm_generate_tests
```

Expected: all existing Generate/Preserve byte goldens and structural tests pass
without golden changes.

If bytes differ, stop before editing any golden. Compare the affected output
with qpdf 11.9.0, add a minimal authored fixture only if the difference belongs
to this object-user contract, and record unrelated differences with `bd create`.

- [ ] **Step 9: Commit the pure routing refactor**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "perf(linearize): reuse object-user routing snapshot"
```

### Task 3: Final Quality, Coverage, and Publication Gates

**Files:**
- Verify: `crates/flpdf/src/linearization/plan.rs`
- Verify unchanged: `crates/flpdf/src/linearization/writer.rs`
- Verify unchanged: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`
- Verify unchanged: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`
- Tracker: Beads issue `flpdf-dpff`

**Interfaces:**
- Consumes: the two committed implementation layers from Tasks 1 and 2.
- Produces: CI-equivalent evidence, 100% changed-line coverage, closed/pushed Beads state, and a pushed git branch.

- [ ] **Step 1: Inspect scope and confirm no accidental output changes**

Run:

```bash
git status --short
git diff --stat main...HEAD
git diff --name-only main...HEAD
```

Expected: only the approved spec/plan and `crates/flpdf/src/linearization/plan.rs`
are changed. No golden, fixture, writer, reader, or CLI file is modified unless
a qpdf-oracle RED case justified it.

- [ ] **Step 2: Run formatting and full lint gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: both commands exit zero with no warnings.

- [ ] **Step 3: Run crate and workspace tests**

Run:

```bash
cargo test -p flpdf
cargo test
```

Expected: all tests pass; only repository-documented ignored tests remain
ignored.

- [ ] **Step 4: Re-run the strict qpdf byte gate**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test linearize_objstm_generate_tests
```

Expected: all tests pass with the checked-in qpdf 11.9.0 goldens unchanged.
Validate every checked-in strict linearized ObjStm golden:

```bash
find tests/golden/references -type f \
  \( -name 'linearize-objstm.pdf' -o -name 'linearize-objstm-preserve.pdf' \) \
  -print0 | xargs -0 -n1 qpdf --check-linearization
```

Expected: `qpdf` reports no linearization errors for every matching golden.

- [ ] **Step 5: Run the authoritative committed-HEAD patch-coverage gate**

Confirm the worktree is clean and both implementation commits exist, then run:

```bash
git status --short
scripts/patch-coverage.sh --base main
```

Expected: the worktree is clean, `crates/flpdf/src` changed-line coverage is
100%, and the script exits zero.

If coverage is below 100%, add a focused behavioral test for each reachable
line. Use `cov:ignore` only for a genuinely unreachable defensive branch or an
llvm-cov attribution artifact, and write the concrete reason in the inline
comment. Commit the test or annotation and rerun this step from the new
committed `HEAD`.

- [ ] **Step 6: Close and publish tracker state**

Run:

```bash
bd close flpdf-dpff --reason "qpdf-style linearization object-user analysis is retained once and reused by Generate/Preserve ObjStm routing; strict byte tests and 100% patch coverage pass"
bd dolt push
```

Expected: `flpdf-dpff` is closed and the Dolt push succeeds.

- [ ] **Step 7: Push the implementation branch**

Run:

```bash
git status --short --branch
git push -u origin refactor/flpdf-dpff-object-user-map
```

Expected: the worktree is clean and the remote branch is created or updated
successfully. Report the pushed commit IDs and verification results; do not
claim a PR exists unless one is explicitly created and verified separately.
