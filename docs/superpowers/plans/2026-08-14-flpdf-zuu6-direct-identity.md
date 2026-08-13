# Stable Direct ObjectHandle Identity Lookup Implementation Plan

> For agentic workers: use superpowers:executing-plans or superpowers:subagent-driven-development to execute this plan task by task. Steps use checkbox syntax.

**Goal:** Replace the canonical page-repair route's quadratic direct-handle identity scans with a reusable qpdf-shaped canonical identity key while preserving indirect ObjGen lookup and direct-cycle behavior.

**Architecture:** ObjectHandle exposes an internal ObjectHandleIdentity value that owns a clone of the canonical Rc slot and compares/hashes by pointer identity, matching qpdf's isSameObjectAs without structural equality. pages/repair.rs keeps its existing BTreeSet<ObjectRef> for indirect objects and uses HashSet<ObjectHandleIdentity> for direct parent/cycle guards.

**Tech Stack:** Rust Rc<RefCell<ObjectSlot>>, HashSet, qpdf 11.9.0 source oracle, page-repair unit tests, cargo quality gates, patch coverage, and GitHub stacked Draft PR workflow.

---

### Task 1: Add the failing identity-contract regression

**Files:**
- Modify: crates/flpdf/src/object_handle.rs in the existing test module
- Test: the object_handle library test target

- [ ] Add this test beside the existing ptr_eq tests:

~~~rust
#[test]
fn identity_key_matches_qpdf_object_sameness_without_structural_equality() {
    use std::collections::HashSet;

    let original = ObjectHandle::dictionary(vec![(
        b"Value".to_vec(),
        ObjectHandle::integer(1),
    )]);
    let alias = original.clone();
    let distinct = ObjectHandle::dictionary(vec![(
        b"Value".to_vec(),
        ObjectHandle::integer(1),
    )]);
    let mut seen = HashSet::new();

    assert!(seen.insert(original.identity_key()));
    assert!(!seen.insert(alias.identity_key()));
    assert!(seen.insert(distinct.identity_key()));
}
~~~

- [ ] Run the RED test:

~~~bash
cargo test -p flpdf --lib object_handle::tests::identity_key_matches_qpdf_object_sameness_without_structural_equality
~~~

Expected: compilation fails because identity_key and its return type do not exist yet. This is the intended feature-missing failure.

- [ ] Commit the RED test:

~~~bash
git add crates/flpdf/src/object_handle.rs
git commit -m "test(object-handle): require canonical identity keys"
~~~

### Task 2: Add the wide direct page-tree regression

**Files:**
- Modify: crates/flpdf/src/pages/repair.rs in its existing test module
- Test: the page-repair library test target

- [ ] Add a WIDTH=512 fixture through canonical ObjectHandle mutation. Start from pdf_with_root_pages_parent_cycle(), resolve its catalog, create WIDTH direct /Pages dictionaries each containing one direct /Page dictionary with a valid MediaBox, install a direct root /Pages dictionary whose /Kids contains those nodes, mark the catalog dirty, and assert that prepare_for_optimization returns exactly WIDTH page refs. Keep the existing direct-parent and direct-kids cycle tests unchanged.

The core shape is:

~~~rust
let leaf = || ObjectHandle::dictionary(vec![
    (b"Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
    (b"MediaBox".to_vec(), ObjectHandle::array(vec![
        ObjectHandle::integer(0), ObjectHandle::integer(0),
        ObjectHandle::integer(612), ObjectHandle::integer(792),
    ])),
]);
let direct_nodes = (0..WIDTH).map(|_| ObjectHandle::dictionary(vec![
    (b"Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
    (b"Kids".to_vec(), ObjectHandle::array(vec![leaf()])),
    (b"Count".to_vec(), ObjectHandle::integer(1)),
])).collect();
let root = ObjectHandle::dictionary(vec![
    (b"Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
    (b"Kids".to_vec(), ObjectHandle::array(direct_nodes)),
    (b"Count".to_vec(), ObjectHandle::integer(WIDTH as i64)),
]);
catalog.replace_key(b"/Pages", root).unwrap();
pdf.mark_object_handle_dirty(&catalog).unwrap();
let prepared = prepare_for_optimization(&mut pdf).unwrap().unwrap();
assert_eq!(prepared.pages.len(), WIDTH);
~~~

- [ ] Run the new test before the production implementation:

~~~bash
cargo test -p flpdf --lib pages::repair::tests::wide_direct_page_tree_uses_canonical_identity_lookup
~~~

Expected: the target remains blocked by the missing identity_key from Task 1. Once implemented, the test must pass without changing page ordering or cycle errors.

- [ ] Commit the regression:

~~~bash
git add crates/flpdf/src/pages/repair.rs
git commit -m "test(pages): cover wide direct page trees"
~~~

### Task 3: Implement and consume the reusable identity key

**Files:**
- Modify: crates/flpdf/src/object_handle.rs near ObjectHandle and its identity methods
- Modify: crates/flpdf/src/pages/repair.rs direct parent and traversal guards

- [ ] Define crate-private ObjectHandleIdentity as a wrapper around a cloned Rc<RefCell<ObjectSlot>>. Implement Clone, PartialEq, Eq, and Hash manually. PartialEq must use Rc::ptr_eq; Hash must hash Rc::as_ptr; no resolved value may be inspected. Add:

~~~rust
pub(crate) fn identity_key(&self) -> ObjectHandleIdentity {
    ObjectHandleIdentity(self.0.clone())
}
~~~

Document the qpdf correspondence at include/qpdf/QPDFObjectHandle.hh:304-309 and libqpdf/QPDFObjectHandle.cc:224-227. Retaining the Rc in the identity value keeps the canonical slot alive while the set entry exists.

- [ ] In pages/repair.rs import HashSet and ObjectHandleIdentity. Replace seen_parent_direct: Vec<ObjectHandle> and visited_direct: Vec<ObjectHandle> with HashSet<ObjectHandleIdentity>. Replace each iter().any(is_same_object_as) branch with one insert(handle.identity_key()) membership test. Keep the BTreeSet<ObjectRef> indirect paths, error text, DFS order, direct/indirect classification, and mutation operations unchanged.

- [ ] Run the focused RED-to-GREEN checks:

~~~bash
cargo test -p flpdf --lib object_handle::tests::identity_key_matches_qpdf_object_sameness_without_structural_equality
cargo test -p flpdf --lib pages::repair::tests::wide_direct_page_tree_uses_canonical_identity_lookup
cargo test -p flpdf --lib pages::repair::tests::direct_parent_cycle_terminates
cargo test -p flpdf --lib pages::repair::tests::direct_kids_cycle_is_rejected_before_depth_overflow
~~~

Expected: all pass and direct-cycle diagnostics remain unchanged.

- [ ] Confirm no direct linear scan remains and commit:

~~~bash
rg -n "visited_direct|seen_parent_direct|iter\\(\\).*is_same_object_as" crates/flpdf/src/pages/repair.rs
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/pages/repair.rs
git commit -m "fix(pages): use stable direct object identity lookup"
~~~

### Task 4: Verify the complete implementation locally

- [ ] Run formatting and focused suites:

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --lib pages::repair::tests
cargo test -p flpdf --test page_document_helper_tests
~~~

- [ ] Run the workspace quality gates:

~~~bash
cargo +1.97.1 clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo +1.97.1 doc --workspace --no-deps --document-private-items
CARGO_BUILD_JOBS=2 cargo test --workspace
~~~

- [ ] Run changed-line coverage against origin/main:

~~~bash
CARGO_BUILD_JOBS=2 scripts/patch-coverage.sh --base origin/main
~~~

Expected: flpdf changed executable lines report uncovered 0 and PASS.

### Task 5: Publish Draft PR and apply the CI gate

- [ ] Push feature/flpdf-zuu6-direct-identity and create a Draft PR with base main. The body cites qpdf 11.9.0 QPDF_pages.cc:39-97, QPDFObjGen.hh:95-115, QPDFObjectHandle.hh:304-309, links flpdf-zuu6, and lists focused/full verification.

~~~bash
git push -u origin feature/flpdf-zuu6-direct-identity
gh pr create --draft --base main --head feature/flpdf-zuu6-direct-identity --title "fix(pages): use stable direct identity lookup" --body-file /tmp/flpdf-zuu6-pr-body.md
~~~

- [ ] Confirm Draft/base/head for the current branch:

~~~bash
gh pr view --json number,state,isDraft,baseRefName,headRefName,headRefOid
~~~

- [ ] Wait for all required checks on the current branch. Only after every required check succeeds, run gh pr ready. Do not merge without separate approval. Append the returned PR number, commit, and check evidence to flpdf-zuu6, run bd dep cycles, and finish with bd dolt push.
