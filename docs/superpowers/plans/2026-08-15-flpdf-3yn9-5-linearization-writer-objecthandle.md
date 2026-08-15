# flpdf-3yn9.5 Linearization Writer ObjectHandle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the qpdf 11.9.0 linearized writer consumer from mixed legacy `Object` emission to the canonical live `ObjectHandle`/xref pipeline in dependency order.

**Architecture:** Keep the existing Annex F layout, planner, hint tables, renumber map, and fixed-width back-patch regions. Replace only the graph traversal and writer emission boundaries in three stacked slices: body/stream, ObjStm/xref/trailer, and two-pass encryption/ID cleanup.

**Tech Stack:** Rust workspace, pinned qpdf 11.9.0 source and `/usr/bin/qpdf`, `qpdf-zlib-compat`, Cargo tests, llvm-cov patch coverage, Beads, stacked GitHub PRs.

## Global Constraints

- qpdf 11.9.0 source and observed output are the semantic and byte-output oracle.
- `ObjectHandle` is the live graph boundary; do not add a linearization-only writer or compatibility adapter.
- Preserve qpdf `QPDFWriter.cc:1072-1809`, `2191-2210`, `2335-2495`, and `2537-2904` call order and pass semantics.
- Keep `main` unchanged and stack each branch on its predecessor, starting at #841.
- Existing legacy-route tests are migration guards; every new behavior test exercises the canonical route.
- Do not merge PRs in this session. Mark a PR Ready only after all required checks pass.

### Task 1: Record the slice issues and baseline

**Files:**

- Read: `crates/flpdf/src/linearization/writer.rs`
- Read: `docs/qpdf-correspondence.md`
- Read: `/home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDFWriter.cc`
- Modify: Beads children under `flpdf-3yn9.5`

**Interfaces:**

- Consumes: existing `flpdf-3yn9.5` dependencies and the approved design.
- Produces: three ordered child issues with explicit PR scope and blocker edges.

- [ ] **Step 1: Capture the current route counts and baseline outputs.**

  Run:

  ```bash
  rg -n 'resolve_borrowed|Object::|decode_stream_data|encode_stream_data' \
    crates/flpdf/src/linearization/{writer,part1,back_patch,renumber}.rs
  cargo test -p flpdf --test cmp_linearize_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test cmp_linearize_objstm_tests --features qpdf-zlib-compat
  ```

  Record the counts and the passing baseline in the first child issue note; do not treat the old route as the new acceptance authority.

- [ ] **Step 2: Create the body/stream child issue.**

  Create a P1 `pre-v1`, `qpdf-parity` task under `flpdf-3yn9.5` whose acceptance names `append_body_object`, `append_object`, `stream_is_data_modified`, and `renumber_object_with_removed`; require a canonical RED differential test and zero migrated legacy emission calls.

- [ ] **Step 3: Create the ObjStm/xref/trailer child issue.**

  Create a P1 child depending on the body/stream child. Its acceptance names `append_objstm_container_object`, `write_first_page_xref_stream`, `write_main_xref_stream_and_trailer`, `write_part1_xref_and_trailer`, and `write_main_xref_and_trailer`; require `/Prev`, `/Index`, `/W`, `/Size`, `/ID`, and compressed-entry evidence.

- [ ] **Step 4: Create the two-pass cleanup child issue.**

  Create a P1 child depending on the ObjStm/xref/trailer child. Its acceptance names `do_write_pass`, `write_linearized_impl`, `finalize_linearized_id`, `append_hint_stream_object`, and encryption/metadata behavior; require no remaining migrated-scope legacy route.

- [ ] **Step 5: Read back all issues and verify the dependency graph.**

  Run:

  ```bash
  bd show flpdf-3yn9.5 --json
  bd dep cycles
  ```

  Expected: the three children form a strict chain, all depend on the parent, and `bd dep cycles` reports no cycles.

### Task 2: Body and stream canonical cutover (stack layer 1)

**Files:**

- Modify: `crates/flpdf/src/linearization/writer.rs:301-750`
- Test: `crates/flpdf/tests/cmp_linearize_tests.rs`
- Test: `crates/flpdf/tests/cmp_null_visibility_tests.rs`
- Test: `crates/flpdf/tests/stream_data_tests.rs` when the canonical route is exercised

**Interfaces:**

- Consumes: `ObjectHandle::unparse_object_with_ref_map_and_removed`, `ObjectHandle::unparse_stream_body_with_ref_map_and_removed`, `ObjectHandle::pipe_stream_data`, `RenumberMap`, and the existing writer stream policy.
- Produces: a body emitter that accepts live handles and emits final-number-space bytes without materializing `crate::Object`.

- [ ] **Step 1: Add a canonical RED test for a remapped nested dictionary and stream.**

  Extend the existing linearization differential helper with a fixture containing a null-valued dictionary child, a nested indirect reference, and a filtered stream. Assert qpdf-zlib-compat output equality and run it through the public linearized writer path.

- [ ] **Step 2: Run the focused test and verify the expected RED failure.**

  ```bash
  cargo test -p flpdf --test cmp_linearize_tests canonical_linearized_body --features qpdf-zlib-compat
  ```

  Expected: the new assertion fails because the current linearization writer still serializes through the legacy materialized object path.

- [ ] **Step 3: Replace recursive legacy remapping with live-handle emission.**

  Use the handle's removed-aware ref-map emitter for scalar, array, and dictionary bodies. Preserve `RenumberMap` errors and qpdf null visibility. For streams, use the canonical stream dictionary method and the same provider/filter pipeline used by `writer/plain/body.rs`; derive `/Length` from the final payload and preserve metadata identity handling.

- [ ] **Step 4: Run the RED test and focused migration checks.**

  ```bash
  cargo test -p flpdf --test cmp_linearize_tests canonical_linearized_body --features qpdf-zlib-compat
  qpdf --check-linearization /tmp/flpdf-linearized-body.pdf
  rg -n 'resolve_borrowed|Object::|decode_stream_data|encode_stream_data' \
    crates/flpdf/src/linearization/writer.rs
  ```

  Expected: the canonical test passes; remaining matches are limited to explicitly unmigrated xref/ID code documented in the child issue.

- [ ] **Step 5: Run the body/stream fixture matrix.**

  ```bash
  cargo test -p flpdf --test cmp_linearize_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test cmp_null_visibility_tests --features qpdf-zlib-compat
  cargo fmt --all -- --check
  ```

- [ ] **Step 6: Commit the layer.**

  ```bash
  git add crates/flpdf/src/linearization/writer.rs \
    crates/flpdf/tests/cmp_linearize_tests.rs \
    crates/flpdf/tests/cmp_null_visibility_tests.rs
  git commit -m "refactor(linearization): emit bodies through ObjectHandle"
  ```

### Task 3: ObjStm, xref, and trailer canonical cutover (stack layer 2)

**Files:**

- Modify: `crates/flpdf/src/linearization/writer.rs:301-1805`
- Modify: `crates/flpdf/src/writer/object_streams.rs` only when an existing handle primitive lacks the required writer contract
- Modify: `crates/flpdf/src/writer/serialize.rs` only for an opt-in linearization xref field; never add unconditional `/Encrypt`
- Test: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`
- Test: `crates/flpdf/tests/cmp_null_visibility_tests.rs`

**Interfaces:**

- Consumes: the body/stream layer, `emit_objstm_body_from_handles_with_writer`, handle trailer/dictionary emission, and `xref_stream`'s opt-in encryption/reference fields.
- Produces: xref type-0/type-1/type-2 records and trailers that use live handles while retaining qpdf's two xref regions.

- [ ] **Step 1: Add a RED test for two ObjStm containers and a first-page/main xref chain.**

  Use a fixture with at least two containers and more than one first/second-half compressed object. Assert qpdf `--check-linearization`, `/Index`, `/W`, `/Prev`, and byte equality under qpdf-zlib-compat.

- [ ] **Step 2: Run the test and confirm the old path fails the canonical assertion.**

  ```bash
  cargo test -p flpdf --test cmp_linearize_objstm_tests two_container_linearization --features qpdf-zlib-compat
  ```

- [ ] **Step 3: Convert ObjStm member traversal to handles.**

  Resolve each member with `Pdf::get_object_handle`, pass it through the removed-aware handle emitter, and use the existing handle-based ObjStm body wrapper. Keep container encryption applied once to the container stream and keep member objects plaintext inside the compressed payload.

- [ ] **Step 4: Convert first-page and main xref stream dictionaries/trailers.**

  Use `unparse_dictionary_with_ref_map_and_id_writer` or the exact existing canonical xref serializer with an opt-in `/Encrypt` field. Use `unparse_trailer_with_ref_map` for trailer values, preserving qpdf's distinction between ordinary dictionary null suppression and `writeTrailer` behavior.

- [ ] **Step 5: Run the ObjStm and xref matrix.**

  ```bash
  cargo test -p flpdf --test cmp_linearize_objstm_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test cmp_null_visibility_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test xref_tests --features qpdf-zlib-compat
  ```

- [ ] **Step 6: Commit the layer.**

  ```bash
  git add crates/flpdf/src/linearization/writer.rs \
    crates/flpdf/src/writer/object_streams.rs \
    crates/flpdf/src/writer/serialize.rs \
    crates/flpdf/tests/cmp_linearize_objstm_tests.rs \
    crates/flpdf/tests/cmp_null_visibility_tests.rs
  git commit -m "refactor(linearization): route ObjStm and xref emission through handles"
  ```

### Task 4: Two-pass ID, encryption, and hint cleanup (stack layer 3)

**Files:**

- Modify: `crates/flpdf/src/linearization/writer.rs:1142-1805,1982-2670,3168-3775`
- Modify: `crates/flpdf/src/linearization/part1.rs` only if the canonical parameter-dictionary emitter requires a stable fixed-width boundary
- Test: `crates/flpdf/tests/encrypted_linearize_tests.rs`
- Test: `crates/flpdf/tests/deterministic_id_qpdf_parity_tests.rs`
- Test: `crates/flpdf/tests/cmp_linearize_tests.rs`

**Interfaces:**

- Consumes: layers 1-2, `EncryptedStringEmitter`, `EncryptionContext`, fixed-width ID writer, hint stream framing, and pass-1/pass-2 offset metadata.
- Produces: one-pass hint framing and qpdf-compatible encrypted/deterministic linearized output with no second payload traversal.

- [ ] **Step 1: Add RED tests for deterministic ID and encrypted metadata.**

  Assert two identical deterministic writes are byte-identical, encrypted output passes `qpdf --check-linearization`, metadata cleartext uses `/Crypt /Identity`, and the hint stream is encrypted exactly once.

- [ ] **Step 2: Run the tests and confirm the existing mixed route fails at the canonical assertions.**

  ```bash
  cargo test -p flpdf --test encrypted_linearize_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test deterministic_id_qpdf_parity_tests --features qpdf-zlib-compat
  ```

- [ ] **Step 3: Migrate ID/trailer and pass-state handling.**

  Keep fixed-width placeholders and the existing digest timing, but emit IDs and encryption references through the canonical handle-aware writer contracts. Do not derive the encryption key from a post-encryption ID or re-encrypt a pass-2 hint payload.

- [ ] **Step 4: Run all linearization and encryption tests.**

  ```bash
  cargo test -p flpdf --test cmp_linearize_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test cmp_linearize_objstm_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test encrypted_linearize_tests --features qpdf-zlib-compat
  cargo test -p flpdf --test deterministic_id_qpdf_parity_tests --features qpdf-zlib-compat
  ```

- [ ] **Step 5: Remove obsolete migrated-scope helpers and verify no callers.**

  ```bash
  rg -n 'resolve_borrowed|Object::|decode_stream_data|encode_stream_data|write_pdf' \
    crates/flpdf/src/linearization/{writer,part1,back_patch,renumber}.rs
  rg -n 'append_object|renumber_object_with_removed|write_main_xref_and_trailer' \
    crates/flpdf/src/linearization
  ```

  Delete only helpers with zero callers after the canonical route is proven; do not retain them as compatibility bridges.

- [ ] **Step 6: Commit the cleanup layer.**

  ```bash
  git add crates/flpdf/src/linearization crates/flpdf/tests
  git commit -m "refactor(linearization): finish qpdf writer ObjectHandle cutover"
  ```

### Task 5: Per-PR review, CI, Beads, and stack handoff

**Files:**

- Modify: PR bodies and Beads child notes only

**Interfaces:**

- Consumes: all three layer commits and fresh verification output.
- Produces: three Ready PRs with tested dependency edges and no merge action.

- [ ] **Step 1: Run the full local quality gates on each layer head.**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --features qpdf-zlib-compat
  scripts/patch-coverage.sh --base origin/main
  git diff --check
  ```

- [ ] **Step 2: Push the layer branch and create/update its stacked PR.**

  Use a PR base equal to the previous layer branch. The body must include qpdf source ranges, canonical route, remaining bridge callers, focused test commands, and the exact Beads child ID. Do not include the sentence that says merge is delegated.

- [ ] **Step 3: Wait for all CI checks and inspect review state.**

  ```bash
  gh pr checks <number> --watch
  gh pr view <number> --json reviews,comments,latestReviews,state,isDraft,mergeable
  ```

  Address only source-derived actionable review findings; classify each with the qpdf review oracle and preserve the canonical route.

- [ ] **Step 4: Mark the PR Ready only after CI is green.**

  ```bash
  gh pr ready <number>
  ```

- [ ] **Step 5: Update and push Beads after each ready PR.**

  ```bash
  bd update <child> --append-notes '<PR, tests, review, coverage evidence>'
  bd dep cycles
  bd dolt push
  ```

  Close a child only when its PR is merged by the separate integration worker; until then leave it `IN_PROGRESS` and record the handoff evidence.
