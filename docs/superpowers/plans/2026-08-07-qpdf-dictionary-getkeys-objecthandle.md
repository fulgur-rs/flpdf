# QPDF Dictionary getKeys ObjectHandle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a crate-private `ObjectHandle::try_get_keys` primitive that lazily resolves a dictionary holder and every stored value, omits null-resolving entries, propagates resolver errors, and returns the remaining keys in sorted byte order.

**Architecture:** Keep resolution in the object-model layer. Snapshot the dictionary through `try_as_dictionary()`, then resolve each cloned child handle only after the container borrow has ended; collect non-null keys in the existing `BTreeSet` representation. Do not change writer visibility helpers or migrate stream-filter consumers in this issue.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source oracle, Cargo tests, cargo-llvm-cov, Beads.

## Global Constraints

- Pinned qpdf 11.9.0 `QPDF_Dictionary::getKeys`, `QPDFObjectHandle::getKeys`, and `QPDFObjectHandle::isNull` are authoritative.
- Produce exactly `pub(crate) fn try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>>`.
- Resolve the holder and every stored child value without inspecting key names; omit direct null, indirect null, missing/dangling-to-null, and canonical loop-to-null outcomes.
- Release the dictionary container borrow before any child resolver call.
- Propagate holder and child resolver errors unchanged; do not convert them to an empty set or an omitted key.
- A resolved non-dictionary holder returns an empty set; qpdf's public type-warning surface remains out of scope.
- Do not change `try_as_dictionary`, `try_get_key`, `try_has_key`, `replace_key`, raw dictionary storage, or `visible_dict_entries`.
- Do not change `stream_filter.rs`, `filters.rs`, `/Filter`, `/DecodeParms`, or retained-key policy; consumer integration remains `flpdf-h8mv`.
- Update only the `ObjectHandle` correspondence entry in `docs/qpdf-correspondence.md`.
- Require focused tests, workspace formatting and tests, and fresh 100% changed executable-line coverage.

---

### Task 1: Add the null-resolving ObjectHandle key enumeration

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:878-890` and `crates/flpdf/src/object_handle.rs:4040-4120`
- Test: `crates/flpdf/src/object_handle.rs` (`identity_tests`)

**Interfaces:**
- Consumes: `ObjectHandle::try_as_dictionary(&self) -> Result<Option<BTreeMap<Vec<u8>, ObjectHandle>>>`, `ObjectHandle::try_is_null(&self) -> Result<bool>`, `MissingResolver`, `error_resolving_handle`, `resolver_bearing_handle`, and `logged_resolver_bearing_handle`.
- Produces: `pub(crate) fn try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>>` for the later `flpdf-h8mv` consumer.

- [ ] **Step 1: Reconfirm the pinned qpdf source boundary**

  Run:

  ```bash
  qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
  sed -n '117,127p' "$qpdf_source/libqpdf/QPDF_Dictionary.cc"
  sed -n '265,268p;345,356p;997,1022p' "$qpdf_source/libqpdf/QPDFObjectHandle.cc"
  sed -n '762,780p' "$qpdf_source/include/qpdf/QPDFObjectHandle.hh"
  ```

  Expected: `getKeys` delegates through a resolving dictionary accessor, dictionary enumeration calls `isNull()` for every map value, null keys are omitted, and the result is a `std::set`.

- [ ] **Step 2: Add the failing ObjectHandle tests**

  Add these tests to `identity_tests`, beside the existing fallible dictionary accessor tests:

  ```rust
  #[test]
  fn try_get_keys_resolves_every_value_omits_nullish_and_sorts_keys() {
      let (indirect_null, _indirect_null_resolver) =
          resolver_bearing_handle(ObjectValue::Null);

      let missing_resolver: Rc<dyn DocumentResolver> = Rc::new(MissingResolver);
      let missing = ObjectHandle::new_indirect_with_resolver(
          ObjectRef::new(21, 0),
          Rc::downgrade(&missing_resolver),
      );

      let (unknown, _unknown_resolver, unknown_calls) =
          logged_resolver_bearing_handle(ObjectValue::Integer(2));

      let dict = ObjectHandle::dictionary(vec![
          (b"Zulu".to_vec(), ObjectHandle::integer(1)),
          (b"DirectNull".to_vec(), ObjectHandle::null()),
          (b"IndirectNull".to_vec(), indirect_null.clone()),
          (b"Dangling".to_vec(), missing.clone()),
          (b"Unknown".to_vec(), unknown),
          (b"Alpha".to_vec(), ObjectHandle::boolean(true)),
      ]);

      assert_eq!(
          dict.try_get_keys().unwrap(),
          BTreeSet::from([
              b"Alpha".to_vec(),
              b"Unknown".to_vec(),
              b"Zulu".to_vec(),
          ])
      );
      assert!(indirect_null.is_resolved());
      assert!(missing.is_resolved());
      assert_eq!(*unknown_calls.borrow(), vec![ObjectRef::new(20, 0)]);
  }

  #[test]
  fn try_get_keys_lazily_resolves_dictionary_and_non_dictionary_holders() {
      let (dict, _dict_resolver, dict_calls) = logged_resolver_bearing_handle(
          ObjectValue::Dictionary(
              [(b"Keep".to_vec(), ObjectHandle::integer(1))]
                  .into_iter()
                  .collect(),
          ),
      );
      assert!(!dict.is_resolved());
      assert_eq!(
          dict.try_get_keys().unwrap(),
          BTreeSet::from([b"Keep".to_vec()])
      );
      assert!(dict.is_resolved());
      assert_eq!(*dict_calls.borrow(), vec![ObjectRef::new(20, 0)]);

      let (scalar, _scalar_resolver, scalar_calls) =
          logged_resolver_bearing_handle(ObjectValue::Integer(7));
      assert_eq!(
          scalar.try_get_keys().unwrap(),
          BTreeSet::<Vec<u8>>::new()
      );
      assert_eq!(*scalar_calls.borrow(), vec![ObjectRef::new(20, 0)]);
  }

  #[test]
  fn try_get_keys_propagates_a_child_resolver_error() {
      let (child, _resolver) = error_resolving_handle(ObjectRef::new(30, 0));
      let dict = ObjectHandle::dictionary(vec![(b"Broken".to_vec(), child.clone())]);

      assert_eq!(dict.try_get_keys().unwrap_err().to_string(), "resolver failed");
      assert!(!child.is_resolved());
  }
  ```

  Extend `every_fallible_accessor_propagates_the_resolver_error` with the holder-error assertion:

  ```rust
  assert_eq!(
      handle.try_get_keys().unwrap_err().to_string(),
      "resolver failed"
  );
  ```

  The mixed test deliberately uses a key named `Unknown`; its call log proves enumeration resolves a value independently of any filter/parameter allowlist.

- [ ] **Step 3: Run the focused test filter and verify RED**

  Run:

  ```bash
  cargo test -p flpdf --lib object_handle::identity_tests::try_get_keys
  ```

  Expected: compilation fails with `E0599` because `ObjectHandle::try_get_keys` does not exist. This is the required RED evidence; do not edit production code before observing it.

- [ ] **Step 4: Implement the minimal primitive**

  Add the method immediately after `try_as_dictionary` so the holder-resolving dictionary accessors stay grouped:

  ```rust
  /// Return the sorted keys whose values do not lazily resolve to null.
  ///
  /// Ports `QPDF_Dictionary::getKeys` and its `QPDFObjectHandle::getKeys`
  /// delegation (`libqpdf/QPDF_Dictionary.cc:117-127`;
  /// `libqpdf/QPDFObjectHandle.cc:997-1009`). The dictionary snapshot is
  /// owned before child resolution, so no container borrow crosses a
  /// resolver call.
  #[allow(dead_code)] // consumed by flpdf-h8mv after this prerequisite lands
  pub(crate) fn try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>> {
      let Some(entries) = self.try_as_dictionary()? else {
          return Ok(BTreeSet::new());
      };
      let mut result = BTreeSet::new();
      for (key, child) in entries {
          if !child.try_is_null()? {
              result.insert(key);
          }
      }
      Ok(result)
  }
  ```

  Do not add special cases for object references, missing state, resolver type, cycles, key names, or filters. Those outcomes must flow through `try_is_null()`.

- [ ] **Step 5: Run the ObjectHandle tests and canonical loop regression for GREEN**

  Run:

  ```bash
  cargo test -p flpdf --lib object_handle::identity_tests::try_get_keys
  cargo test -p flpdf --lib object_handle::identity_tests::every_fallible_accessor_propagates_the_resolver_error
  cargo test -p flpdf --lib reader::resolver::tests::a_reference_already_being_resolved_takes_the_loop_branch_and_leaves_the_outer_mark
  ```

  Expected: all tests pass. The resolver regression must still prove the canonical cycle outcome is a cached terminal null; `try_get_keys` must not implement an independent cycle mechanism.

- [ ] **Step 6: Format, inspect scope, and commit the tested primitive**

  Run:

  ```bash
  cargo fmt --all
  git diff --check
  git diff -- crates/flpdf/src/object_handle.rs
  git status --short
  git add crates/flpdf/src/object_handle.rs
  git commit -m "feat: add null-aware ObjectHandle key enumeration"
  ```

  Expected: only `object_handle.rs` is in this implementation commit; the production diff contains the single primitive and the focused tests.

---

### Task 2: Record correspondence and run release-quality verification

**Files:**
- Modify: `docs/qpdf-correspondence.md:121`
- Verify: `crates/flpdf/src/object_handle.rs`

**Interfaces:**
- Consumes: committed `ObjectHandle::try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>>` and the focused tests from Task 1.
- Produces: documented qpdf responsibility mapping, a clean verified feature branch, and completed Bead `flpdf-25kg.3.23`.

- [ ] **Step 1: Update only the ObjectHandle correspondence row**

  In the `QPDFObjectHandle.cc` row of `docs/qpdf-correspondence.md`, add this exact responsibility statement to the existing ObjectHandle note; do not edit the `QPDF.cc` or `QPDFStreamFilter.cc` rows:

  ```markdown
  `try_get_keys` は `QPDFObjectHandle::getKeys` → `QPDF_Dictionary::getKeys`（`QPDFObjectHandle.cc:997-1009`; `QPDF_Dictionary.cc:117-127`）に対応し、holder と全 child を lazy resolve して null value のキーを除外した `BTreeSet` を返す。child resolve 前に辞書 snapshot の borrow は終了し、resolver error は伝播する。filter 固有知識と consumer 移行は `flpdf-h8mv` の責務
  ```

  The row must continue to classify the overall multi-file `QPDFObjectHandle` port as in-progress; this narrow addition does not justify changing `🔀` to `✅`.

- [ ] **Step 2: Check formatting and lint with the exact workspace gates**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  git diff --check
  ```

  Expected: all commands exit 0 without warnings promoted to errors.

- [ ] **Step 3: Run the focused tests and the full workspace suite**

  Run:

  ```bash
  cargo test -p flpdf --lib object_handle::identity_tests::try_get_keys
  cargo test -p flpdf --lib object_handle::identity_tests::every_fallible_accessor_propagates_the_resolver_error
  cargo test -p flpdf --lib reader::resolver::tests::a_reference_already_being_resolved_takes_the_loop_branch_and_leaves_the_outer_mark
  cargo test
  ```

  Expected: every command exits 0; ignored oracle tests may remain ignored, but there must be no failures.

- [ ] **Step 4: Commit correspondence documentation**

  Run:

  ```bash
  git status --short
  git add docs/qpdf-correspondence.md
  git commit -m "docs: map qpdf null-aware key enumeration"
  git status --short
  ```

  Expected: the worktree is clean. A clean committed tree is required because the patch-coverage gate compares `HEAD` while instrumenting the current tree.

- [ ] **Step 5: Generate fresh coverage and require 100% changed-line coverage**

  Run:

  ```bash
  cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
  scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
  ```

  Expected: the coverage command completes and `patch-coverage.sh` reports 100% coverage for changed executable lines under `crates/flpdf/src`. If it reports an uncovered branch, add a behavior-focused test, rerun the focused/full gates, commit the test, regenerate LCOV, and rerun the patch gate; do not add a coverage-ignore marker for reachable behavior.

- [ ] **Step 6: Review the final diff against the issue boundaries**

  Run:

  ```bash
  git diff --stat origin/main...HEAD
  git diff origin/main...HEAD -- crates/flpdf/src/object_handle.rs docs/qpdf-correspondence.md docs/superpowers/specs/2026-08-07-qpdf-dictionary-getkeys-objecthandle-design.md docs/superpowers/plans/2026-08-07-qpdf-dictionary-getkeys-objecthandle.md
  git status --short
  ```

  Expected: no changes to `stream_filter.rs`, `filters.rs`, raw map semantics, `try_has_key`, `replace_key`, or writer helpers; the tree is clean.

- [ ] **Step 7: Close and persist the Bead, then publish the feature branch**

  Run only after every preceding gate passes:

  ```bash
  bd close flpdf-25kg.3.23 --reason="Implemented qpdf-compatible ObjectHandle null-aware sorted key enumeration with focused resolver coverage, correspondence docs, workspace gates, and 100% changed-line coverage"
  bd dolt push
  git push -u origin feature/flpdf-25kg.3.23-get-keys
  bd show flpdf-25kg.3.23 --json | jq '.[0] | {id, status, assignee}'
  git status --short
  ```

  Expected: Beads push and git push both succeed, the issue reads `closed`, and the worktree remains clean. Do not open or merge a pull request unless separately requested.
