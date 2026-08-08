# Linearized Hint Single-Shot Splice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make flpdf linearized writing use qpdf 11.9.0's single-shot hint-buffer generation and pass-2 splice.

**Architecture:** `do_write_pass` produces a typed pass result and accepts an optional complete hint-object buffer. The writer runs one hint-free pass, calculates all hint tables from its virtual coordinates, emits the encrypted/framed hint object once, and runs one final pass that copies those bytes unchanged.

**Tech Stack:** Rust workspace, qpdf 11.9.0 source/oracle, `cargo test`, qpdf-zlib-compatible differential fixtures.

---

## File structure

- Modify: `crates/flpdf/src/linearization/writer.rs` — pass result, hint-object splice boundary, single-shot orchestration, and writer unit tests.
- Create: `docs/superpowers/specs/2026-08-09-linearized-hint-single-shot-splice-design.md` — approved qpdf responsibility and verification contract.
- Create: `docs/superpowers/plans/2026-08-09-linearized-hint-single-shot-splice.md` — this execution plan.
- Verify: `crates/flpdf/tests/cmp_linearize_tests.rs` — classic qpdf byte parity.
- Verify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs` — non-encrypted ObjStm qpdf byte parity.

### Task 1: Fix the hint builder's pass-1 coordinate contract

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`, the existing
  `build_outline_hint_table` unit test and helper.

- [ ] **Step 1: Write the failing test**

Change the regression to model qpdf's pass-1 coordinate directly:

```rust
#[test]
fn build_outline_hint_table_uses_pass1_virtual_offset() {
    let info = OutlineHintInfo {
        first_object: 3,
        nobjects: 2,
    };
    let byte_lengths = BTreeMap::from([(3u32, 60usize), (4u32, 70usize)]);

    let table = build_outline_hint_table(
        &info,
        &BTreeMap::from([(3u32, 500usize)]),
        &byte_lengths,
    )
    .expect("pass-1 outline offset is already virtual");

    assert_eq!(table.first_object_offset, 500);
    assert_eq!(table.group_length, 130);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --lib linearization::writer::tests::build_outline_hint_table_uses_pass1_virtual_offset -- --exact
```

Expected: compile failure because the old helper still requires a guessed
hint-object length and the new pass-1 contract is not implemented.

- [ ] **Step 3: Implement the minimal helper change**

Remove `hint_stream_obj_total_len` from `build_outline_hint_table`, use the
pass-1 `first_off` directly for `first_object_offset`, and update its docs and
the existing caller. Do not retain a fallback subtraction or sentinel.

- [ ] **Step 4: Run GREEN**

Run the focused test again; it must pass, then run:

```bash
cargo test -p flpdf --lib linearization::writer::tests::build_outline_hint_table -- --exact
```

Expected: all outline-hint helper tests pass.

### Task 2: Replace the inline hint payload with a qpdf-shaped saved object

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`, `do_write_pass`, the
  convergence-loop call sites, and related unit-test documentation.

- [ ] **Step 1: Introduce the typed pass result and splice input**

Add `LinearizedPassOutput` for the current tuple return values. Change
`do_write_pass` to accept `hint_stream_object: Option<&[u8]>` and return the
typed result. At the hint slot, `None` records no hint xref entry and emits no
bytes; `Some(object)` appends the complete object and records its offset. Keep
`pass1_digest` for the existing qpdf xref/ID placeholder behavior.

- [ ] **Step 2: Run the focused writer tests**

Run:

```bash
cargo test -p flpdf --lib linearization::writer::tests
```

Expected: the compiler identifies every old call-site argument that still
expects a payload/S/O/IV rather than a complete hint-object buffer.

- [ ] **Step 3: Update all three pass call sites**

Pass 1 supplies `None`; the final pass supplies `Some(&hint_stream_object)`.
Remove the convergence-only `HintStreamMeasure`,
`hint_stream_convergence_len`, and unused `adjusted_offset` code after all
callers are migrated. Keep `append_hint_stream_object` as the one qpdf-shaped
framing/encryption emitter.

### Task 3: Implement one pass-1 calculation and one final splice

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`,
  `write_linearized_impl` and its module-level docs/tests.

- [ ] **Step 1: Run the pass-1 write unconditionally**

Reuse the existing `pass1_digest` call, but always retain its bytes, xref
offsets, last-xref metadata, and pass-1 hint-slot offset. The same result feeds
deterministic-ID hashing and `write_linearized_with_pass1_file`.

- [ ] **Step 2: Build hint tables from pass-1 measurements**

Move the current page-length, shared-object, and outline-table calculation
body out of the `for iter in 0..max_iters` loop and feed it the pass-1
`xref_offsets`/`compute_byte_lengths` result. Set the page-table location to
the pass-1 hint-slot offset. Use pass-1 offsets directly for shared and outline
locations; no hint-length subtraction is permitted.

- [ ] **Step 3: Generate and retain the complete hint object once**

Encode the patched tables once, choose the compressed/raw payload according to
`effective_stream_policy`, and call:

```rust
let mut hint_stream_object = Vec::new();
append_hint_stream_object(
    &mut hint_stream_object,
    ObjectRef::new(hint_stream_new_num, 0),
    &hint_payload,
    shared_section_offset,
    outline_section_offset,
    structural_streams_filtered,
    encrypt_ctx.as_ref(),
    hint_stream_aes_iv,
)?;
```

This call is the only hint encryption/framing call in the invocation.

- [ ] **Step 4: Run exactly one final pass**

Call `do_write_pass` once with `Some(&hint_stream_object)`. Remove the
`max_iters`, convergence comparison, and `did not converge` error. Preserve
the final offsets, pass-1 comments, deterministic-ID direct-write/back-patch,
and all existing fixed-padding paths.

- [ ] **Step 5: Run focused RED/GREEN verification**

Run:

```bash
cargo test -p flpdf --lib linearization::writer::tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
```

Expected: writer unit tests and all classic/ObjStm qpdf parity tests pass.

### Task 4: Quality gates and handoff

**Files:**
- No additional implementation files.

- [ ] **Step 1: Run formatting, lint, and docs checks**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/qpdf-module-docs.py --check
```

- [ ] **Step 2: Run the workspace and parity verification**

```bash
cargo test --workspace
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
```

- [ ] **Step 3: Inspect the patch and commit**

```bash
git diff --check
git status --short
git add crates/flpdf/src/linearization/writer.rs docs/superpowers/specs/2026-08-09-linearized-hint-single-shot-splice-design.md docs/superpowers/plans/2026-08-09-linearized-hint-single-shot-splice.md
git commit -m "fix(linearization): splice one qpdf hint buffer"
```

- [ ] **Step 4: Publish and persist Beads state**

```bash
git push -u origin feature/flpdf-26l3-linearized-hint-splice
bd dolt push
```

After verification and push, read back `bd show flpdf-26l3`; close the Bead
only if the implementation is fully verified and the user has not requested a
PR/merge handoff instead.
