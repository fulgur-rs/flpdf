# Shared qpdf Trailer Owner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task with review checkpoints.

**Goal:** Replace the split Rust trailer-emission semantics with one qpdf-shaped live-`ObjectHandle` owner, beginning with the plain classic-xref consumer.

**Architecture:** Keep trailer preparation in `build_writer_trailer_handle` and move form-sensitive serialization into `writer/object.rs`. A `TrailerKind` value carries qpdf's normal/linearized-first/linearized-second distinction; callers continue to own xref rows, xref-stream dictionaries, hint offsets, and physical padding. The first production route is plain classic xref, while the shared API is tested for all qpdf trailer forms before later route cutovers.

**Tech Stack:** Rust workspace, `ObjectHandle`, qpdf 11.9.0 source oracle, Cargo tests, qpdf differential fixtures, `cargo llvm-cov`, rustdoc, Clippy.

**Spec:** `docs/superpowers/specs/2026-09-07-writer-trailer-owner-design.md`

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative.
- Do not add a legacy bridge, sentinel, or compatibility-only route.
- Preserve qpdf's `/ID` then `/Encrypt` ordering and `t_lin_second` omission of `/Encrypt`.
- Preserve live `ObjectHandle` identity, indirect-reference spelling, null visibility, and existing `Error` boundaries.
- Keep qtest exceptions `.48.45` out of scope.
- Do not merge the eventual PR in this session.

---

### Task 1: Establish the shared trailer contract with RED tests

**Files:**
- Modify: `crates/flpdf/src/writer/object.rs` in `ObjectWriterEmission` and its unit-test module
- Test: existing `crates/flpdf/src/writer.rs` test module through the writer-object tests

**Interfaces:**
- Consumes: existing `ObjectHandle::write_trailer_with_ref_map`, `TrailerIdWriter`, reference-map and removed-reference contracts.
- Produces: a qpdf-shaped internal `TrailerKind` and trailer-write context that can represent normal, `lin_first { prev }`, and `lin_second` output without route-specific xref state.

- [ ] **Step 1: Add failing unit tests for all trailer forms.**

  Add focused tests that build a live trailer containing `/Info`, `/Root`, an unknown non-null key, a null-valued key, `/ID`, and `/Encrypt`, then assert:

  - normal classic output has sorted ordinary keys followed by `/ID` and `/Encrypt`;
  - QDF output has qpdf line-oriented key spelling;
  - `lin_first` emits `/Size N /Prev P` with exactly 21 bytes reserved for the decimal field;
  - `lin_second` emits `/Size N /ID ...` and omits `/Root`, `/Info`, `/Prev`, and `/Encrypt` when the caller supplies the trimmed second-half view;
  - a removed source reference does not remove writer-owned `/ID` or `/Encrypt`, and an indirect unknown value remains an indirect reference;
  - an ID writer changes only the ID value.

- [ ] **Step 2: Run the focused tests and verify RED.**

  Run:

  ```bash
  cargo test -p flpdf --lib trailer_kind
  ```

  Expected: the new tests do not compile or fail because the mode-aware shared contract does not yet exist.

- [ ] **Step 3: Commit the RED tests.**

  ```bash
  git add crates/flpdf/src/writer/object.rs crates/flpdf/src/writer.rs
  git commit -m "test(writer): pin shared qpdf trailer forms"
  ```

### Task 2: Implement qpdf-shaped mode-aware trailer emission

**Files:**
- Modify: `crates/flpdf/src/writer/object.rs`
- Inspect: `crates/flpdf/src/pdf_syntax.rs`
- Test: `crates/flpdf/src/writer.rs` focused trailer tests

**Interfaces:**
- Consumes: `ObjectHandle` live graph, `TrailerIdWriter`, `ObjectRef` map, removed-reference set, and qdf/null-suppression flags.
- Produces: an internal mode-aware method that emits ordinary trailer entries, `/Size`, optional `/Prev`, `/ID`, and `/Encrypt` in qpdf order.

- [ ] **Step 1: Add `TrailerKind` and the mode-specific options.**

  Define an internal enum with `Normal`, `LinearizedFirst { prev: u64 }`, and `LinearizedSecond` variants. Keep `/Size`, `xref_stream`, `qdf`, ID writer, reference map, removed refs, and null suppression as explicit inputs so the method does not infer qpdf state from sentinel values.

- [ ] **Step 2: Move the common key loop behind the new contract.**

  Reuse the existing live-handle traversal. Filter only the qpdf-trimmed/writer-owned keys selected by the caller, emit ordinary entries in decoded-key order, replace `/Size`, insert `/Prev` only for `LinearizedFirst`, append `/ID`, and append `/Encrypt` only when the kind is not `LinearizedSecond`. Keep xref-stream opening/closing delimiters with the caller.

- [ ] **Step 3: Run the focused RED tests and verify GREEN.**

  Run:

  ```bash
  cargo test -p flpdf --lib trailer_kind
  cargo test -p flpdf --lib root_object_emission
  ```

  Expected: all new trailer-form tests and existing root-emission tests pass.

- [ ] **Step 4: Commit the shared primitive.**

  ```bash
  git add crates/flpdf/src/writer/object.rs crates/flpdf/src/writer.rs
  git commit -m "feat(writer): share qpdf trailer emission contract"
  ```

### Task 3: Cut over the plain classic-xref consumer

**Files:**
- Modify: `crates/flpdf/src/writer/plain/xref.rs`
- Modify: `crates/flpdf/src/writer.rs` trailer-plan construction only where needed to supply the shared handle and mapping
- Test: `crates/flpdf/tests/cmp_diff_zero_tests.rs` and relevant plain-writer unit tests

**Interfaces:**
- Consumes: Task 2's mode-aware trailer emitter, `TrailerPlan`, classic xref row output, and existing ID/encryption plans.
- Produces: plain classic output whose xref rows and `startxref` remain locally owned while trailer bytes come from the shared qpdf owner.

- [ ] **Step 1: Add a RED parity case for an unknown key, null key, ID, and Encrypt.**

  Extend the existing plain classic differential fixture or add a focused generated PDF so qpdf and flpdf outputs exercise all writer-owned trailer ordering and null visibility branches under static ID.

- [ ] **Step 2: Run the focused parity test and verify RED.**

  Run:

  ```bash
  cargo test -p flpdf --test cmp_diff_zero_tests --features qpdf-zlib-compat
  ```

  Expected: the newly added case fails or shows the current split trailer output before the cutover.

- [ ] **Step 3: Route plain classic trailer bytes through the shared owner.**

  Preserve `write_xref_table`, object-0/type-1 row behavior, xref offset capture, and `startxref` assembly. Replace only `write_canonical_classic_trailer`'s semantic emission with the shared live-handle method and pass `TrailerKind::Normal`.

- [ ] **Step 4: Run focused plain and differential tests.**

  ```bash
  cargo test -p flpdf --lib writer::plain
  cargo test -p flpdf --test cmp_diff_zero_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test adbe_ext_qpdf_parity
  ```

  Expected: all pass with byte-identical qpdf output for the covered classic route.

- [ ] **Step 5: Commit the first consumer cutover.**

  ```bash
  git add crates/flpdf/src/writer/plain/xref.rs crates/flpdf/src/writer.rs crates/flpdf/tests/cmp_diff_zero_tests.rs
  git commit -m "refactor(writer): route plain trailer through shared owner"
  ```

### Task 4: Record route ownership and verify untouched follow-up boundaries

**Files:**
- Modify: `docs/qpdf-route-matrix/d-writer.md`
- Modify: `docs/qpdf-correspondence.md` only if the D14 annotation needs the exact new owner
- Test: `scripts/check-qpdf-deviation-markers.py`, route/caller checks

**Interfaces:**
- Consumes: the new shared owner and plain consumer evidence.
- Produces: route documentation that distinguishes the completed plain slice from remaining specialized/PCLm/linearized/xref-stream callers.

- [ ] **Step 1: Update D14 with exact owner and remaining callers.**

  Record the qpdf citations, the new `writer/object.rs` owner, the plain classic caller, and the explicit follow-up route boundaries. Do not claim all route consumers are migrated.

- [ ] **Step 2: Run route and deviation checks.**

  ```bash
  python3 scripts/check-qpdf-route-matrix.py
  python3 scripts/check-qpdf-deviation-markers.py --check
  ```

  Expected: both pass, with the D14 row retaining the literal remaining mixed callers.

- [ ] **Step 3: Commit documentation.**

  ```bash
  git add docs/qpdf-route-matrix/d-writer.md docs/qpdf-correspondence.md
  git commit -m "docs(writer): record shared trailer owner"
  ```

### Task 5: Full verification and handoff gates

**Files:**
- Verify: all changed files and the clean worktree
- Artifacts: fresh patch-coverage report and CI checks

- [ ] **Step 1: Run local quality gates.**

  ```bash
  cargo fmt --all -- --check
  RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace
  cargo test --workspace --features qpdf-zlib-compat
  scripts/patch-coverage.sh --base origin/main
  ```

- [ ] **Step 2: Rebase on the latest `origin/main` and rerun changed-line verification.**

  Fetch, rebase the dedicated branch onto the latest `origin/main`, resolve only verified conflicts, and rerun the focused tests, fmt, strict rustdoc, all-features Clippy, route/deviation checks, workspace tests, and fresh patch coverage after the rebase.

- [ ] **Step 3: Push and create a Draft PR.**

  Push `feature/flpdf-3yn9-48-56`, create a Draft PR with qpdf citations, exact verification commands, and the remaining route scope. Do not mention merge delegation in the PR body.

- [ ] **Step 4: Wait for all CI checks and mark Ready.**

  Re-query all required GitHub checks, including Coverage/Codecov patch, Fuzz, Release, all OS test jobs, Quality, Analyze, and approval gates. Run `gh pr ready` only after every required check is green; do not merge.

- [ ] **Step 5: Record Beads evidence and push state.**

  Append the implementation commits, PR URL/head, focused and full verification results, route boundaries, and unchanged qtest exception scope to `.48.56`. Run `bd show`, `bd dep cycles`, `bd dolt push` and require `Push complete.`; push Git state before handoff.

