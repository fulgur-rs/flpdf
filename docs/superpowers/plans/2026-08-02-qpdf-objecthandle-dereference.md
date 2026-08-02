# QPDFObjectHandle Dereference Primitive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ObjectHandle` resolve its canonical indirect slot through its document-owned resolver, matching qpdf 11.9.0, without retaining a raw-`Object` or `ref_chain` bridge.

**Architecture:** `Pdf` keeps its legacy raw-object fields untouched while callers remain, but wraps its input in a cloneable `SharedInput<R>` and owns a separate qpdf-native resolver/cache that never converts from the legacy cache. Each indirect `ObjectHandle` stores a weak resolver link, so its fallible `try_*` accessors dereference the same canonical slot in place. The first consumer receives a new handle-native route; concrete replacements are then deprecated and the old route is deleted only after its caller is redirected.

**Tech Stack:** Rust stable, `Rc`/`Weak`/`RefCell`, pinned qpdf 11.9.0 source, C++ qpdf oracle probe, Cargo tests and llvm-cov.

---

### Task 1: Pin the qpdf accessor contract with a live oracle probe

**Files:**

- Create: `tests/oracle/qpdf_objecthandle_dereference_probe.cc`
- Create: `scripts/qpdf-objecthandle-dereference-diff.sh`
- Create: `scripts/tests/qpdf-objecthandle-dereference-diff-contract.sh`
- Create: `tests/fixtures/compat/objecthandle-dereference.pdf`
- Create: `tests/fixtures/compat/objecthandle-dereference-dangling.pdf`

- [ ] **Step 1: Write the C++ probe before adding Rust behavior**

  Create a probe that opens its single PDF argument, obtains the trailer `/Root`
  handle, and prints the following tab-separated observations in this order:

  ```cpp
  auto root = qpdf.getTrailer().getKey("/Root");
  std::cout << "root-indirect\t" << root.isIndirect() << '\n';
  std::cout << "root-dictionary\t" << root.isDictionary() << '\n';
  std::cout << "root-has-pages\t" << root.hasKey("/Pages") << '\n';
  auto pages = root.getKey("/Pages");
  std::cout << "pages-indirect\t" << pages.isIndirect() << '\n';
  std::cout << "pages-dictionary\t" << pages.isDictionary() << '\n';
  ```

  `objecthandle-dereference.pdf` is a minimal PDF whose trailer `/Root` is
  indirect and whose catalog `/Pages` value is indirect. The dangling fixture
  uses the same catalog shape with `/Missing 9 0 R` and no `9 0` xref entry;
  add `missing-null\t1` after `root.getKey("/Missing").isNull()`.

- [ ] **Step 2: Compile and run the probe against the pinned qpdf source**

  Run:

  ```bash
  scripts/qpdf-objecthandle-dereference-diff.sh tests/fixtures/minimal.pdf
  ```

  Expected: exit 0 and output proves that `isIndirect()` is true before and
  after type inspection, while `isDictionary()` and `hasKey()` inspect the
  resolved value.

- [ ] **Step 3: Add the runner source-integrity checks**

  Make the runner follow the repository's `qpdf-tokenizer-diff.sh` pattern:
  resolve the pinned source using `scripts/fetch-qpdf-source.sh --print-path`,
  reject a dirty source worktree before compiling, compile the probe with both
  `-I<source>/include` and `-I<source>/libqpdf`, link only the build-local
  `libqpdf`, then execute the probe. The contract test must reject a swapped
  source leaf, missing public/private include, compiler failure, and a library
  resolved outside the pinned build.

- [ ] **Step 4: Run the contract test**

  Run:

  ```bash
  bash scripts/tests/qpdf-objecthandle-dereference-diff-contract.sh
  ```

  Expected: exit 0.

- [ ] **Step 5: Commit the oracle contract**

  ```bash
  git add tests/oracle/qpdf_objecthandle_dereference_probe.cc scripts/qpdf-objecthandle-dereference-diff.sh scripts/tests/qpdf-objecthandle-dereference-diff-contract.sh
  git commit -m "test: add qpdf objecthandle dereference oracle"
  ```

### Task 2: Add the resolver-bearing indirect slot and test it in isolation

**Files:**

- Modify: `crates/flpdf/src/object_handle.rs:70-180,360-650`
- Test: `crates/flpdf/src/object_handle.rs` unit-test module

- [ ] **Step 1: Write failing slot tests**

  Add a test-only resolver that records its requested `ObjectRef` and changes
  the supplied canonical handle to a dictionary containing `/A 1`. Add these
  tests:

  ```rust
  #[test]
  fn try_get_key_resolves_the_same_indirect_slot_once() {
      let (handle, resolver) = unresolved_handle_with_recording_resolver(ObjectRef::new(7, 0));
      let clone = handle.clone();
      assert_eq!(handle.try_get_key(b"A").unwrap().as_integer(), Some(1));
      assert!(clone.try_has_key(b"A").unwrap());
      assert_eq!(resolver.calls(), vec![ObjectRef::new(7, 0)]);
      assert!(handle.ptr_eq(&clone));
      assert_eq!(handle.object_ref(), Some(ObjectRef::new(7, 0)));
  }

  #[test]
  fn try_dereference_reports_a_dropped_document_without_reconnecting() {
      let handle = unresolved_handle_with_dropped_resolver(ObjectRef::new(8, 0));
      assert!(handle.try_dereference().is_err());
      assert!(!handle.is_resolved());
  }
  ```

- [ ] **Step 2: Run the focused tests and verify RED**

  Run:

  ```bash
  cargo test -p flpdf --lib object_handle::tests::try_get_key_resolves_the_same_indirect_slot_once
  cargo test -p flpdf --lib object_handle::tests::try_dereference_reports_a_dropped_document_without_reconnecting
  ```

  Expected: both fail because `try_get_key` and `try_dereference` do not exist.

- [ ] **Step 3: Introduce the sealed resolver interface and handle methods**

  Add a crate-private resolver trait and attach its `Weak` link to
  `IndirectSlot`:

  ```rust
  pub(crate) trait DocumentResolver {
      fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()>;
  }

  struct IndirectSlot {
      object_ref: ObjectRef,
      resolver: Weak<dyn DocumentResolver>,
      state: IndirectState,
      parsed_offset: i64,
  }
  ```

  Implement `try_dereference` by copying `object_ref` and upgrading `resolver`
  after the slot borrow ends. It returns `Ok(())` for direct and terminal
  slots, returns the resolver error unchanged, and never substitutes a
  terminal clone. Implement `try_is_null`, `try_as_dictionary`, `try_get_key`,
  and `try_has_key` as `try_dereference` followed by the existing slot read.
  Keep the current non-fallible accessors unchanged.

- [ ] **Step 4: Add missing and resolver-error tests, then run GREEN**

  Add tests proving a resolver can set `Missing` and that the same error is
  observed by `try_is_null`, `try_as_dictionary`, `try_get_key`, and
  `try_has_key`. Run:

  ```bash
  cargo test -p flpdf --lib object_handle::tests
  ```

  Expected: all object-handle unit tests pass.

- [ ] **Step 5: Commit the isolated primitive**

  ```bash
  git add crates/flpdf/src/object_handle.rs
  git commit -m "feat: add objecthandle dereference primitive"
  ```

### Task 3: Add the qpdf-native resolver state beside the untouched legacy route

**Files:**

- Modify: `crates/flpdf/src/reader.rs:56-160,336-355,560-655,1550-1870`
- Modify: `crates/flpdf/tests/object_handle_parity_tests.rs`

- [ ] **Step 1: Write reader-level RED tests using only new APIs**

  Add these integration tests, using `Pdf::open_mem_owned` and a fixture whose
  catalog and pages are separate indirect objects:

  ```rust
  #[test]
  fn handle_accessor_lazily_resolves_catalog_and_preserves_holder_identity() {
      let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).unwrap();
      let root = pdf.get_object(pdf.root_ref().unwrap());
      assert!(root.is_indirect());
      assert!(!root.is_resolved());
      assert!(root.try_has_key(b"Pages").unwrap());
      assert!(root.is_resolved());
      assert_eq!(root.object_ref(), pdf.root_ref());
      assert!(root.try_get_key(b"Pages").unwrap().is_indirect());
  }
  ```

  Add a second test opening a PDF with a dangling indirect dictionary value;
  `try_get_key` must return an indirect child, and that child's `try_is_null`
  must be true after one resolver attempt.

- [ ] **Step 2: Run the reader tests and verify RED**

  Run:

  ```bash
  cargo test -p flpdf --test object_handle_parity_tests handle_accessor_lazily_resolves_catalog_and_preserves_holder_identity
  ```

  Expected: FAIL because handles returned from `Pdf` have no resolver link.

- [ ] **Step 3: Share only the input and add an independent resolver state**

  Add `SharedInput<R>(Rc<RefCell<R>>)` with `Clone`, `Read`, and `Seek`
  implementations. Change `Pdf.reader` from `R` to `SharedInput<R>` and wrap
  the input once in `open_with_repair_mode`; existing reader method bodies and
  every legacy cache field remain unchanged.

  Add `QpdfResolver<R>` containing a cloned `SharedInput<R>`, the source xref
  snapshot, canonical handle registry, in-progress-resolution set, and only
  the parser/decryption metadata needed to produce `ObjectValue` directly.
  Implement `DocumentResolver` for it. `resolve_indirect` must parse directly
  into the supplied handle and may not call `resolve_to_cache`,
  `resolve_object_handle`, `resolve_borrowed`, `ObjectHandle::materialize`, or
  any raw-`Object` conversion.

  Add `Pdf::get_object(ObjectRef) -> ObjectHandle` as the only entry into this
  registry. Do not modify or call the existing `get_object_handle`; it remains
  marked for deletion until its legacy callers migrate.

- [ ] **Step 4: Register the resolver when creating an indirect handle**

  Implement the new canonical constructor path so `Pdf::get_object` creates:

  ```rust
  ObjectHandle::new_indirect_unresolved(object_ref, NO_PARSED_OFFSET, Rc::downgrade(&resolver))
  ```

  The resolver itself must be held strongly by `Pdf`; the handle stores only
  the weak link. Preserve the existing cache key and `ObjectRef`; do not
  materialize an `Object` to construct the handle.

- [ ] **Step 5: Preserve qpdf teardown and run GREEN**

  In `Drop for Pdf<R>`, disconnect every canonical indirect handle before the
  strong resolver owner is dropped, matching qpdf teardown. Add a test
  retaining a resolved `/Pages` handle after `Pdf` drops:
  `try_dereference` must not access the former reader, and the handle must
  remain `Destroyed`.

  Run:

  ```bash
  cargo test -p flpdf --test object_handle_parity_tests
  cargo test -p flpdf --lib reader::tests
  ```

  Expected: both suites pass.

- [ ] **Step 6: Commit the document resolver integration**

  ```bash
  git add crates/flpdf/src/reader.rs crates/flpdf/tests/object_handle_parity_tests.rs
  git commit -m "feat: resolve canonical objecthandle slots in place"
  ```

### Task 4: Add the first handle-native page-tree repair route

**Files:**

- Modify: `crates/flpdf/src/pages/repair.rs:1-620`
- Modify: `crates/flpdf/src/page_tree_rebuild.rs:220-255`
- Test: `crates/flpdf/tests/page_document_helper_tests.rs`

- [ ] **Step 1: Write the page-tree RED regression**

  Build a synthetic page tree with direct `/Kids` entries that point through
  indirect `/Pages` and `/Page` holders. Assert that the new repair entry point
  preserves the root `/Kids` holder reference while classifying the resolved
  target as a page or page-tree node. The assertion must inspect both the
  holder `ObjectRef` and the resolved `/Type` through `try_get_key`.

- [ ] **Step 2: Run the focused test and verify RED**

  Run:

  ```bash
  cargo test -p flpdf --test page_document_helper_tests direct_kids_holder_resolves_through_objecthandle
  ```

  Expected: FAIL because no handle-native repair entry point exists.

- [ ] **Step 3: Create, rather than rewrite, the handle-native route**

  Add a new private `repair_page_tree_with_handles` route beside the existing
  raw-`Object` repair route. It obtains child handles from the current holder,
  calls `try_get_key` / `try_as_dictionary` for structural inspection, and
  records the original child `ObjectRef` for rewrites. It must not import
  `ref_chain`, call `resolve_borrowed`, or call the old repair route.

  Redirect the page-tree rebuild caller to the new route. Leave the existing
  repair function unchanged until `rg` proves it has no callers, then delete
  it and its raw-Object-only tests in the same commit.

- [ ] **Step 4: Run GREEN and prove old-route deletion eligibility**

  Run:

  ```bash
  cargo test -p flpdf --test page_document_helper_tests
  rg -n 'repair_page_tree\(' crates/flpdf/src crates/flpdf/tests
  ```

  Expected: page-document tests pass and the search shows only the legacy
  function definition plus tests scheduled for deletion, never a production
  caller.

- [ ] **Step 5: Commit the first component replacement**

  ```bash
  git add crates/flpdf/src/pages/repair.rs crates/flpdf/src/page_tree_rebuild.rs crates/flpdf/tests/page_document_helper_tests.rs
  git commit -m "refactor: repair page trees through objecthandle"
  ```

### Task 5: Remove reached bridges and verify the slice

**Files:**

- Modify: `crates/flpdf/src/reader.rs:1699-2000`
- Modify: `crates/flpdf/src/ref_chain.rs` only if its page-tree callers are gone
- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/superpowers/specs/2026-08-02-qpdf-objecthandle-dereference-design.md`

- [ ] **Step 1: Write deletion-gate tests before deleting code**

  Add compile-time or integration witnesses that the new page-tree route calls
  only `ObjectHandle::try_*` accessors. Keep legacy resolver tests temporarily
  to demonstrate their behavior has not been silently re-exported by the new
  APIs.

- [ ] **Step 2: Run the witnesses and verify RED for forbidden dependencies**

  Run:

  ```bash
  rg -n 'ref_chain|resolve_borrowed|resolve_object_handle_to_terminal' crates/flpdf/src/pages/repair.rs crates/flpdf/src/page_tree_rebuild.rs
  ```

  Expected: no matches. If there is a match, do not delete legacy code; return
  to Task 4 and remove the dependency first.

- [ ] **Step 3: Delete only zero-user legacy code**

  Delete the old page-tree repair route after its production callers and tests
  have moved. Do not delete global `ref_chain` or raw `Object` routes that
  still have other component callers; record their remaining call sites in the
  correspondence document as explicit future deletions.

- [ ] **Step 4: Run focused and full verification**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo test -p flpdf --lib object_handle::tests
  cargo test -p flpdf --test object_handle_parity_tests
  cargo test -p flpdf --test page_document_helper_tests
  cargo test -p flpdf --test reader_tests
  cargo test -p flpdf
  cargo test
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-25kg-3-3.lcov
  scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-25kg-3-3.lcov
  ```

  Expected: every test and clippy exits 0; patch coverage reports 100% changed
  executable lines.

- [ ] **Step 5: Commit verification metadata and update the Bead**

  ```bash
  git add docs/qpdf-correspondence.md docs/superpowers/specs/2026-08-02-qpdf-objecthandle-dereference-design.md
  git commit -m "docs: record objecthandle dereference cutover"
  bd update flpdf-25kg.3.3 --append-notes="Verified pinned qpdf dereference contract, canonical slot resolution, and page-tree handle-native cutover; exact commands recorded in the implementation plan."
  bd dolt push
  ```
