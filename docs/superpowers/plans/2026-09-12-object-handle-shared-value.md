# ObjectHandle Shared Value Ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the canonical flpdf `ObjectHandle` value state occupy one shared allocation per qpdf value while preserving qpdf 11.9.0 identity, lazy resolution, warning, containment, teardown, and output contracts.

**Architecture:** Keep `ObjectHandle` as an outer `Rc<RefCell<ObjectSlot>>` so handle identity remains distinct from value identity. Replace the separate payload, identity, parsed metadata, and auxiliary `Rc` cells with one `Rc<RefCell<SharedValueState>>` that mirrors qpdf's `QPDFObject` → `QPDFValue` boundary. Keep cache span/provenance and the explicitly marked per-slot containment edge on `ObjectSlot`; keep `state_owners` inside the shared state because its propagation preserves existing per-slot containment semantics.

**Tech Stack:** Rust workspace, `Rc<RefCell<...>>`, qpdf 11.9.0 pinned source, cargo tests, custom global allocator regression, qpdf-zlib byte gates, `cargo llvm-cov`, strict rustdoc, and Python correspondence checkers.

**Spec:** `docs/superpowers/specs/2026-09-12-object-handle-shared-value-design.md`

## Global Constraints

- The pinned qpdf source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` with commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` is the semantic authority.
- `QPDFObject` owns one `shared_ptr<QPDFValue>`; `QPDFValue` owns payload/type/description/qpdf/ObjGen/parsed-offset state (`QPDFObject_private.hh:19-29,117-151`; `QPDFValue.hh:18-27,60-80,123-152`).
- `ObjectHandle::is_same_object_as` must continue to compare outer handle allocation identity, not structural or shared-value equality.
- `containment_parents` and `state_owners` remain explicitly identified as flpdf-only bookkeeping; they must not become writer scheduling or output semantics.
- Do not modify qtest exceptions, add a raw-object bridge, add a sentinel, or alter traversal order and output bytes.
- Every code edit uses `apply_patch`; every changed Rust path receives focused tests before broad gates.

## File Map

- Modify `crates/flpdf/src/object_handle.rs`: define `SharedValueState`, centralize all slot construction, migrate value/identity/metadata/aux accessors and mutation/alias/teardown logic, and extend the existing internal test modules.
- Modify `crates/flpdf/src/reader/resolver.rs`: update the canonical replacement and object-swap call sites to use the refactored private value-state operations without adding a second route.
- Read `crates/flpdf/src/writer.rs:2300-2330`: verify the catalog extension replacement caller still uses the canonical shared-state assignment and keep writer ownership and output ordering unchanged.
- Create `crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs`: isolate a process-global counting allocator and assert the direct scalar allocation budget plus wide container/alias measurements.
- Modify `docs/qpdf-correspondence.md`: replace the current `QPDFObject.cc / QPDFValue.cc` split description with the final shared-state mapping and the retained containment bookkeeping rationale.
- Do not modify `crates/flpdf-cli` or `flpdf-qtest` for this issue; existing CLI and qpdf-zlib suites are verification consumers.

### Task 1: Add the RED allocation and alias contract tests

**Files:**
- Create: `crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs`
- Modify: `crates/flpdf/src/object_handle.rs` internal tests near `mutation_tests`, `resolution_state_tests`, `parsed_offset_tests`, and `drop_tests`

**Interfaces:**
- Consumes: existing public factories (`ObjectHandle::integer`, `array`, `dictionary`, `stream`) and existing crate-private `assign_value_state`, `swap_value_state_with`, `disconnect_and_destroy` test surfaces.
- Produces: failing assertions that the implementation must satisfy; no public API changes.

- [ ] **Step 1: Write the failing direct-scalar allocation test.**

  Define a `CountingAllocator` using `GlobalAlloc` and `AtomicUsize`, reset the counter immediately before constructing one `ObjectHandle::integer(7)`, and assert that the construction uses at most three allocations. Construct the test value inside the measured closure so the current separate `state`, `state_owners`, `identity`, and auxiliary cells are counted.

  ```rust
  #[test]
  fn direct_scalar_uses_at_most_three_allocations() {
      let allocations = allocations_during(|| {
          let scalar = ObjectHandle::integer(7);
          std::hint::black_box(scalar);
      });
      assert!(allocations <= 3, "direct scalar used {allocations} allocations");
  }
  ```

- [ ] **Step 2: Run the new test and verify RED.**

  Run: `cargo test -p flpdf --test object_handle_shared_value_allocation_tests direct_scalar_uses_at_most_three_allocations`

  Expected: FAIL because the current split representation allocates more than three blocks for one direct scalar.

- [ ] **Step 3: Add alias behavior assertions to the existing unit test modules.**

  Add tests that establish the pre-refactor contract the new state must preserve: `assign_value_state` shares mutations but leaves `is_same_object_as` false for distinct slots, detached replacement mutates only the target, `swap_value_state_with` moves values while retaining each object reference, and `disconnect_and_destroy` does not leave a shared alias destroyed accidentally. Use existing helpers and assertions such as `try_get_key`, `try_is_null`, `object_ref`, `description`, and `containing_object_refs` rather than introducing a structural-equality oracle.

- [ ] **Step 4: Run the focused alias and teardown tests and record any baseline failures.**

  Run: `cargo test -p flpdf --lib replacement_validation_and_value_assignment_have_separate_contracts`

  Run: `cargo test -p flpdf --lib disconnecting_shared_canonical_state_rebinds_only_the_canonical_slot`

  Run: `cargo test -p flpdf --lib deep_direct_disconnect_is_stack_independent`

  Expected: existing behavior remains green; the new allocation test is the intentional RED test.

- [ ] **Step 5: Commit the RED tests.**

  ```bash
  git add crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs crates/flpdf/src/object_handle.rs
  git commit -m "test: pin shared ObjectHandle allocation budget"
  ```

### Task 2: Introduce the single shared value state and central constructors

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:1040-1360,1860-2055`

**Interfaces:**
- Consumes: `ObjectValue`, `ValueIdentity`, `ObjectDescription`, `StreamTokenFilterList`, and the four existing `ObjectSlot` construction paths.
- Produces: `SharedValueState` and `ObjectSlot::shared: Rc<RefCell<SharedValueState>>`; all constructors produce the same representation.

- [ ] **Step 1: Add `SharedValueState` beside `ObjectSlot` and move value-owned fields into it.**

  Define the concrete fields below. Use direct `Vec`, `bool`, and `u64` fields for auxiliary state so an empty direct scalar does not allocate an additional `Rc`; retain the `Vec<Weak<...>>` owner list inside this same allocation.

  ```rust
  struct SharedValueState {
      value: ObjectValue,
      identity: ValueIdentity,
      parsed_offset: i64,
      description: Option<ObjectDescription>,
      state_owners: Vec<Weak<RefCell<ObjectSlot>>>,
      stream_token_filters: Vec<StreamTokenFilter>,
      content_normalization_applied: bool,
      mutation_generation: u64,
  }
  ```

  Change `ObjectSlot` so it retains `initialized`, `shared`, `end_before_space`, `end_after_space`, `tree_pdf_unique_id`, and `containment_parents`. Do not move cache-span or per-slot containment state into the shared value.

- [ ] **Step 2: Change child descriptions to point at the shared value state.**

  Change `ChildDescription::parent` from `Weak<RefCell<ObjectSlot>>` to `Weak<RefCell<SharedValueState>>`. `set_child_description` must downgrade the parent's shared value state, and description rendering must recurse through `SharedValueState::description` and `SharedValueState::value`, matching qpdf's `QPDFValue::ChildDescr` parent pointer (`QPDFValue.hh:41-58,74-84`).

- [ ] **Step 3: Add one shared-state constructor.**

  Implement a private constructor that initializes every `SharedValueState` field once and returns `Rc<RefCell<SharedValueState>>`. Route `empty_object_slot`, `new_reserved_for_pdf`, `new_reserved_direct`, `new_indirect_unresolved_qpdf_obj_gen_with_identity`, and `new_direct_with_resolver_inner` through it. Preserve each constructor's initialized flag, identity arguments, parsed-offset sentinel, and child attachment behavior.

- [ ] **Step 4: Check the representation diff before the dependent read migration.**

  Run: `cargo fmt --all -- --check`

  Run: `git diff --check`

  Expected: formatting and patch syntax pass. The type/constructor edit is intentionally compiled together with Task 3 because removing the old fields makes stale reads fail closed at the compiler boundary; do not add temporary accessors or a second representation.

### Task 3: Migrate value reads, identity, descriptions, and offsets

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:1174-1243,1650-1778,2340-2495,2780-3010,3900-3990,4120-4160,7280-7335`

**Interfaces:**
- Consumes: `ObjectSlot::shared` from Task 2.
- Produces: unchanged `ObjectHandle` public methods and crate-private resolver APIs backed by shared fields.

- [ ] **Step 1: Move `ObjectSlot` identity helpers onto shared state.**

  Update `qpdf_obj_gen`, `object_ref`, `is_indirect`, `active_pdf_unique_id`, and `resolver` to borrow `slot.shared` and read `shared.identity`. Keep the synthetic `ObjectRef(0, generation)` projection comment and behavior unchanged.

- [ ] **Step 2: Migrate `Debug` without recursively formatting child values.**

  Read the shared value kind and shared parsed offset through a short borrow, then format the existing metadata fields. Preserve the cycle-safe hand-written `Debug` implementation.

- [ ] **Step 3: Migrate descriptions and child-description context.**

  Update `get_description`, `set_object_description`, `set_description_json`, `set_child_description`, `description_template`, and warning-context readers to use shared `description`, `identity`, `value`, and `parsed_offset`. Do not convert raw bytes to lossy UTF-8 and do not hold a `RefCell` borrow across resolver calls.

- [ ] **Step 4: Keep cache-only offsets per slot and migrate value parsed offsets.**

  Make `get_parsed_offset`, `try_get_parsed_offset`, `set_parsed_offset_if_unset`, and `reset_parsed_offset` use `SharedValueState::parsed_offset`. Keep `set_end_offsets` and `end_offsets` on `ObjectSlot`, because qpdf stores them in `ObjCache`, not `QPDFValue`.

- [ ] **Step 5: Migrate `with_value` and all silent accessors.**

  Make `with_value` borrow `shared.value`. Verify `as_array`, `as_dictionary`, `as_stream_dict`, type code/name, null/reserved/destroyed checks, and JSON/unparse readers observe the same value pointer and do not allocate a child snapshot unless their documented API returns an owned collection.

- [ ] **Step 6: Run the value/description/offset tests.**

  Run: `cargo test -p flpdf --lib object_value_tests`

  Run: `cargo test -p flpdf --lib parsed_offset_tests`

  Run: `cargo test -p flpdf --lib object_description_`

  Expected: all existing value, raw-byte description, and parsed-offset tests pass.

- [ ] **Step 7: Commit the read-side migration.**

  ```bash
  git add crates/flpdf/src/object_handle.rs
  git commit -m "refactor: route ObjectHandle reads through shared value state"
  ```

### Task 4: Migrate replacement, assignment, swap, and resolver integration

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:2110-2360,2400-2558`
- Modify: `crates/flpdf/src/reader/resolver.rs:1760-1840`
- Modify: `crates/flpdf/src/writer.rs:2300-2330`

**Interfaces:**
- Consumes: shared-state accessors from Task 3.
- Produces: unchanged `assign_value_state`, `swap_value_state_with`, `set_resolved`, `remove_from_document`, and resolver replacement behavior.

- [ ] **Step 1: Rewrite shared-state replacement without allocating sibling cells.**

  Replace the old `replace_shared_state` body so it replaces `SharedValueState::value`, resets the shared auxiliary fields in place, and updates the existing state-owner containment bookkeeping. Do not create new `Rc<Cell<_>>` or `Rc<RefCell<_>>` fields for each replacement.

- [ ] **Step 2: Rewrite detached replacement.**

  Make `replace_detached_state` allocate exactly one fresh `SharedValueState` for the departing outer slot, preserve the slot's per-slot identity/cache rules, remove the slot from the old shared state's owner list, and detach old direct children from the slot's containment parent. An alias still pointing at the old shared state must continue to observe the old value.

- [ ] **Step 3: Rewrite `assign_value_state` as a shared-pointer assignment.**

  Preserve the early self-assignment return. Replace the target's `shared` pointer with the source's pointer, register the target in the shared owner list, and reconcile only the target's containment parent. Do not copy the `ObjectValue`, `ValueIdentity`, description, or auxiliary cells. Keep target cache-span and per-slot provenance fields according to their existing qpdf replacement behavior.

- [ ] **Step 4: Rewrite `swap_value_state_with`.**

  Swap the two shared pointers so payload, shared identity, parsed offset, description, and auxiliary state move together. Leave `end_before_space` and `end_after_space` on their original `ObjectSlot`s because qpdf keeps those `ObjCache` span fields outside `QPDFObject::swapWith`. Restore each shared identity's qpdf ObjGen projection exactly as `QPDFObject::swapWith` does. Detach/attach direct children through the shared owner list without borrowing either slot across a resolver call.

- [ ] **Step 5: Update promotion, removal, and resolution.**

  Change `promote_to_indirect`, `promote_to_indirect_qpdf_obj_gen`, `remove_from_document`, and `set_resolved` to mutate shared identity/value fields. Preserve the no-offset sentinel, resolved-null reset, resolver weak lifetime, and active `pdf_unique_id` behavior.

- [ ] **Step 6: Re-run the resolver and writer callers.**

  Keep `ResolverCore::replace_object` using the canonical `assign_value_state` operation and keep `swap_objects` using `swap_value_state_with`. Update only field access or helper names required by the new representation; do not add a raw materialization path.

- [ ] **Step 7: Run mutation and owner tests.**

  Run: `cargo test -p flpdf --lib mutation_tests`

  Run: `cargo test -p flpdf --lib resolution_state_tests`

  Run: `cargo test -p flpdf --test make_indirect_object_owner_tests`

  Expected: alias assignment, direct promotion, swap, replacement, null/reserved/destroyed, and writer-owner tests pass.

- [ ] **Step 8: Commit the mutation migration.**

  ```bash
  git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader/resolver.rs crates/flpdf/src/writer.rs
  git commit -m "refactor: preserve ObjectHandle alias mutations in shared state"
  ```

### Task 5: Migrate containment, stream auxiliary state, and teardown

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:4830-5080,6120-6370,7380-7435,8010-8030`

**Interfaces:**
- Consumes: shared value access and mutation operations from Tasks 3–4.
- Produces: unchanged containment observers, stream provider/filter methods, mutation fingerprints, and stack-safe document teardown.

- [ ] **Step 1: Move owner-list helpers into shared state.**

  Update `register_state_owner`, `remove_state_owner`, and `state_owner_handles` to borrow `SharedValueState::state_owners`. Retain weak references and dead-owner pruning. The list must not receive a separate allocation beyond the shared state.

- [ ] **Step 2: Preserve per-slot containment semantics.**

  Keep `containment_parents` on each `ObjectSlot`. Update attach/detach operations so every existing state owner still receives the same direct-child edge updates as before. Do not use containment to assign document ownership or affect writer reachability.

- [ ] **Step 3: Migrate stream filters and normalization marker.**

  Store `stream_token_filters`, `content_normalization_applied`, and `mutation_generation` in `SharedValueState`. Update `add_token_filter`, `is_data_modified`, `clear_token_filters`, `mark_content_normalization_applied`, `content_normalization_applied`, `bump_mutation_generation`, and `mutation_fingerprint` to share these fields through the value pointer. Preserve the stream type error and alias-visible state.

- [ ] **Step 4: Migrate direct-value replacement and stream dictionary replacement.**

  Update `replace_direct_value`, `replace_stream_dict`, and the stream provider/buffer accessors. A replacement must reset only the same shared fields as the old implementation and keep qpdf's stream data/provider exclusivity.

- [ ] **Step 5: Migrate stack-safe disconnect.**

  Make `disconnect` traverse `SharedValueState::value` and direct stream dictionaries, clear shared identity fields once, and leave indirect children for the resolver cache walk. Keep the existing raw allocation-address visited set and deep DAG/cycle behavior. `disconnect_and_destroy` must create a new shared destroyed state only for the canonical slot after disconnect.

- [ ] **Step 6: Run containment, stream, and teardown tests.**

  Run: `cargo test -p flpdf --lib deep_containment_traversals_do_not_overflow_the_stack`

  Run: `cargo test -p flpdf --lib stream_dictionary_membership_tracks_replacement_and_root_disconnect`

  Run: `cargo test -p flpdf --lib stream_provider_contract_tests`

  Run: `cargo test -p flpdf --lib drop_tests`

  Expected: all existing containment, provider, mutation-fingerprint, cycle, and deep teardown tests pass.

- [ ] **Step 7: Commit the containment/teardown migration.**

  ```bash
  git add crates/flpdf/src/object_handle.rs
  git commit -m "refactor: co-locate ObjectHandle auxiliary state"
  ```

### Task 6: Make the allocation regression GREEN and measure the benchmark matrix

**Files:**
- Modify: `crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs`
- Modify: `docs/qpdf-correspondence.md`
- Read: `docs/qpdf-correspondence.md` QPDFObject/QPDFValue and containment sections

**Interfaces:**
- Consumes: completed shared-state implementation and unchanged public ObjectHandle API.
- Produces: measured allocation evidence and final qpdf correspondence text.

- [ ] **Step 1: Run the allocation test and verify GREEN.**

  Run: `cargo test -p flpdf --test object_handle_shared_value_allocation_tests -- --nocapture`

  Expected: direct scalar allocation count is at most 3; wide array/dictionary construction and alias measurements complete without failures.

- [ ] **Step 2: Add live-byte size-class measurements.**

  Extend the isolated allocator to count allocated bytes and size-class totals while constructing scalar-heavy, dictionary-heavy, and stream-heavy values. Reset counters only after fixture setup, and print stable labeled measurements for the issue note. Do not make the test depend on allocator addresses or platform-specific absolute output.

- [ ] **Step 3: Measure qpdf/flpdf page matrix.**

  Use qpdf-generated 1,000, 2,000, 5,000, 10,000, and 20,000-page inputs plus a one-page baseline. Run the exact same `--check` command through `/usr/bin/time -v` for `/usr/bin/qpdf` and the release `target/release/flpdf`; record wall time, maximum RSS, and exit code. Subtract each tool's one-page RSS from its corresponding matrix value. Keep generated PDFs and logs outside the repository.

- [ ] **Step 4: Compare qpdf/flpdf output and exit behavior.**

  Compare `qpdf --check` and `flpdf --check` combined stdout/stderr with `diff -u` and assert both exit 0. Run representative rewrite/round-trip probes under qpdf-zlib compatibility and compare output bytes with the existing qpdf goldens.

- [ ] **Step 5: Update the correspondence row.**

  Change the `QPDFObject.cc / QPDFValue.cc` row to document `SharedValueState` as the qpdf value counterpart, the outer `ObjectSlot` as the qpdf object counterpart, the preserved cache-span/per-slot fields, and the retained `state_owners`/containment deviation. Cite the pinned source ranges and do not claim that qpdf has a reverse containment index.

- [ ] **Step 6: Run focused GREEN checks.**

  Run: `cargo test -p flpdf --test object_handle_shared_value_allocation_tests`

  Run: `cargo test -p flpdf --lib warning_emission_tests`

  Run: `cargo test -p flpdf --lib mutation_tests`

  Expected: allocation, warning, alias, replacement, and teardown checks pass.

- [ ] **Step 7: Commit measured implementation documentation.**

  ```bash
  git add crates/flpdf/tests/object_handle_shared_value_allocation_tests.rs docs/qpdf-correspondence.md
  git commit -m "test: measure shared ObjectHandle value allocation"
  ```

### Task 7: Run full verification and prepare the PR

**Files:**
- Read: `.github/workflows/ci.yml`
- Read: `scripts/patch-coverage.sh`
- Modify: Beads issue `flpdf-ymuj.3.1` notes only after committed evidence is available

**Interfaces:**
- Consumes: all committed code/tests/docs from Tasks 1–6.
- Produces: a rebased Draft PR, Beads evidence, and a Ready PR after all GitHub checks pass.

- [ ] **Step 1: Run local quality gates on the implementation branch.**

  ```bash
  cargo fmt --all -- --check
  cargo test --workspace --all-features --quiet
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
  python3 scripts/qpdf-module-docs.py --check
  python3 scripts/check-qpdf-deviation-markers.py --check
  python3 scripts/check-qpdf-route-matrix.py --check
  ```

  Expected: every command exits 0; do not proceed with a dirty source tree.

- [ ] **Step 2: Run the explicit qpdf-zlib CI test list.**

  Run the `bytes-identical zlib compat (Linux amd64)` commands from `.github/workflows/ci.yml:333-409`, including the flpdf, flpdf-cli, and flpdf-qtest-tools tests. Expected: every command exits 0.

- [ ] **Step 3: Re-fetch and rebase before PR creation.**

  ```bash
  git fetch --prune origin
  GIT_EDITOR=true git rebase origin/main
  git status --short
  ```

  Expected: the branch is based directly on the current `origin/main`, with no unresolved conflict or uncommitted source change. Re-run the focused allocation/alias tests after any rebase conflict resolution.

- [ ] **Step 4: Run fresh committed patch coverage.**

  ```bash
  cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
  scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
  ```

  Expected: `flpdf` changed lines are 100% covered and the source worktree is clean apart from ignored `target/` output. Remove only generated coverage side files if they appear untracked.

- [ ] **Step 5: Request read-only code review and resolve qpdf-backed findings.**

  Review `ObjectSlot`/`SharedValueState` field placement, qpdf citations, alias semantics, containment bookkeeping, and allocation measurements against the pinned source. For every finding, re-check qpdf source before editing, run the focused regression, then rerun affected quality gates.

- [ ] **Step 6: Push and create the Draft PR.**

  ```bash
  git push -u origin refactor/flpdf-ymuj-3-1
  gh pr create --draft --base main --head refactor/flpdf-ymuj-3-1 --title "perf(object-handle): co-locate shared value state" --body $'Port qpdf 11.9.0 QPDFObject/QPDFValue ownership to one shared ObjectHandle value state.\n\nSource: QPDFObject_private.hh:19-29,117-151; QPDFValue.hh:18-27,60-80,123-152. Scope includes alias assignment, swap, detached replacement, lazy resolution, warning descriptions, containment bookkeeping, teardown, allocation measurements, and qpdf-zlib verification.'
  PR_NUMBER=$(gh pr view --head refactor/flpdf-ymuj-3-1 --json number --jq .number)
  ```

  The PR body must not claim merge or mention a merge plan; it must identify `flpdf-ymuj.3.1` and the exact qpdf source/verification evidence.

- [ ] **Step 7: Record Beads implementation evidence and graph state.**

  Append the commit, the numeric value in `PR_NUMBER`, allocation/size-class/RSS/time measurements, test totals, review resolution, and patch coverage result with `bd update flpdf-ymuj.3.1 --external-ref gh-$PR_NUMBER --append-notes ...`. Read back the issue, run `bd dep cycles`, and run `bd dolt push`; require the literal `Push complete.` before continuing.

- [ ] **Step 8: Wait for every required GitHub check and mark Ready.**

  ```bash
  gh pr checks "$PR_NUMBER" --watch --interval 30
  gh pr view "$PR_NUMBER" --json state,isDraft,headRefOid,baseRefOid,mergeStateStatus,statusCheckRollup
  gh pr ready "$PR_NUMBER"
  ```

  Execute `gh pr ready` only when every required check, including patch coverage, is successful, the PR head is the rebased commit, `mergeStateStatus` is `CLEAN`, and the worktree is clean. Do not merge this PR in the session. After Ready, return to `bd ready` and re-check `flpdf-3yn9.48` before selecting the next issue.
