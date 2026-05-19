# flpdf-9hc.5.9: Incremental generate-mode ObjStm Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** In the incremental writer, when `object_streams=Generate` and the source's last xref is a stream, pack eligible touched plain objects into one fresh ObjStm container appended in the new generation, referenced via type-2 xref entries.

**Architecture:** Add a gated branch to `write_pdf_incremental` (writer.rs:346). Gate = `Generate` mode AND `XrefForm::Stream` AND non-empty eligible packable set; otherwise 100% fallback to the existing plain incremental path (zero regression). Reuse existing `object_streams::emit_objstm_body` + `wrap_objstm_body` for the container body, and the **already-existing** `XrefOffset::Compressed` plumbing in `write_incremental_xref_stream` / `build_xref_stream_bytes` (`/W [1 8 4]`).

**Tech Stack:** Rust, flpdf crate. Tests via `cargo test -p flpdf`. qpdf CLI cross-check via existing compat test helpers.

---

## Key codebase facts (verified 2026-05-20)

- `XrefOffset` enum (`crates/flpdf/src/xref.rs:17`) **already has** `Compressed { stream: u32, index: u32 }`.
- `build_xref_stream_bytes` (writer.rs:981) **already emits type-2** for `XrefOffset::Compressed`; `/W` is `[1 8 4]` (fits stream-num + index). **No xref enum/format change needed.**
- `write_incremental_xref_stream` (writer.rs:892) takes `source_offsets: &BTreeMap<u32,(u16,XrefOffset)>`; feeding `Compressed` entries Just Works.
- `merge_source_and_touched_offsets_for_xref_stream` (writer.rs:685) currently merges plain `touched_offsets: &BTreeMap<u32,(u16,usize)>` (→ `XrefOffset::Offset`) + deleted. Needs a new param for compressed entries.
- `collect_touched_object_refs` (writer.rs:416) → `(touched: Vec<ObjectRef>, deleted, objstm_touched)`. We only repartition `touched`.
- `object_streams::is_eligible_for_objstm(object_ref, &Object, &EligibilityContext)` (object_streams.rs:40); build ctx via `object_streams::eligibility_context(pdf)`.
- `object_streams::emit_objstm_body(pdf, &[ObjectRef]) -> ObjStmBody`; `object_streams::wrap_objstm_body(&body, CompressStreams) -> Stream`.
- `write_object(bytes, ObjectRef, &Object)` appends an indirect object and is used by the incremental path; container is written the same way.
- `write_pdf_incremental` flow: writer.rs:346–412. `XrefForm` via `pdf.last_xref_form()`.

---

### Task 1: `partition_objstm_eligible` helper

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (add fn near `collect_touched_object_refs`, ~writer.rs:447)
- Test: `crates/flpdf/src/writer.rs` (`#[cfg(test)] mod` inline, or `crates/flpdf/tests/writer_tests.rs`)

**Step 1: Write the failing test**

A unit test that builds an in-memory PDF with: a plain dict object (eligible), a stream object (ineligible), an object with generation != 0 (ineligible). Assert `partition_objstm_eligible` splits a touched slice into `(packable, plain_remaining)` correctly (packable contains only the eligible plain dict; order preserved).

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf partition_objstm_eligible -- --nocapture`
Expected: FAIL — `partition_objstm_eligible` not defined.

**Step 3: Write minimal implementation**

```rust
/// Split `touched` into (objstm_packable, plain_remaining) using the same
/// eligibility predicate as the full-rewrite ObjStm packer. Order-preserving.
fn partition_objstm_eligible<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    touched: &[ObjectRef],
) -> Result<(Vec<ObjectRef>, Vec<ObjectRef>)> {
    let ctx = object_streams::eligibility_context(pdf)?;
    let mut packable = Vec::new();
    let mut plain = Vec::new();
    for &r in touched {
        let obj = pdf.resolve(r)?;
        if object_streams::is_eligible_for_objstm(r, &obj, &ctx) {
            packable.push(r);
        } else {
            plain.push(r);
        }
    }
    Ok((packable, plain))
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf partition_objstm_eligible`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(writer): partition_objstm_eligible helper (flpdf-9hc.5.9)"
```

---

### Task 2: `allocate_incremental_objstm_container` helper

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Test: inline unit test

**Step 1: Write the failing test**

Given a `source_offsets` map (max key = 11), a touched set (max num 9), and a deleted set (max num 15), assert the allocated container `ObjectRef` has `number == 16` (max across all + 1) and `generation == 0`.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf allocate_incremental_objstm_container`
Expected: FAIL — not defined.

**Step 3: Write minimal implementation**

```rust
/// Allocate a fresh ObjStm container number strictly above the existing input
/// space (source xref max, touched, deleted) so it never collides with a
/// delete_object free entry.
fn allocate_incremental_objstm_container(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    touched: &[ObjectRef],
    deleted: &[ObjectRef],
    declared_size: usize,
) -> Result<ObjectRef> {
    let max_source = source_offsets.keys().copied().next_back().unwrap_or(0);
    let max_touched = touched.iter().map(|r| r.number).max().unwrap_or(0);
    let max_deleted = deleted.iter().map(|r| r.number).max().unwrap_or(0);
    let base = max_source
        .max(max_touched)
        .max(max_deleted)
        .max(declared_size.saturating_sub(1) as u32);
    let number = base.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported("ObjStm container number does not fit u32".to_string())
    })?;
    Ok(ObjectRef::new(number, 0))
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf allocate_incremental_objstm_container`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(writer): allocate_incremental_objstm_container (flpdf-9hc.5.9)"
```

---

### Task 3: extend `merge_source_and_touched_offsets_for_xref_stream` for compressed entries

**Files:**
- Modify: `crates/flpdf/src/writer.rs:685-698` and its 1 call site (writer.rs:371-375)
- Test: inline unit test

**Step 1: Write the failing test**

Call the function with a `compressed: &BTreeMap<u32,(u32,u32)>` (member num → (container num, index)). Assert the merged map contains `XrefOffset::Compressed { stream, index }` for those numbers and that plain/deleted behaviour is unchanged.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf merge_source_and_touched_offsets_for_xref_stream`
Expected: FAIL — arity/signature mismatch.

**Step 3: Write minimal implementation**

Add a 4th param `compressed: &BTreeMap<u32, (u32, u32)>`:

```rust
fn merge_source_and_touched_offsets_for_xref_stream(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    touched_offsets: &BTreeMap<u32, (u16, usize)>,
    deleted_object_refs: &[ObjectRef],
    compressed: &BTreeMap<u32, (u32, u32)>,
) -> BTreeMap<u32, (u16, XrefOffset)> {
    let mut merged = source_offsets.clone();
    for (number, (generation, offset)) in touched_offsets {
        merged.insert(*number, (*generation, XrefOffset::Offset(*offset as u64)));
    }
    for (number, (stream, index)) in compressed {
        merged.insert(*number, (0, XrefOffset::Compressed { stream: *stream, index: *index }));
    }
    for (number, (generation, next)) in build_deleted_entries(source_offsets, deleted_object_refs) {
        merged.insert(number, (generation, XrefOffset::Free { next }));
    }
    merged
}
```

Update the call site (writer.rs:371) to pass `&BTreeMap::new()` for now (compressed wired in Task 5).

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf` (function test + ensure no other call sites break)
Expected: PASS, full crate still compiles.

**Step 5: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(writer): xref-stream merge accepts compressed entries (flpdf-9hc.5.9)"
```

---

### Task 4: `write_incremental_objstm` — emit container + member index map

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Test: integration-style unit test (in-memory PDF)

**Step 1: Write the failing test**

Build an in-memory xref-stream PDF, pick 2 plain eligible objects as `packable`. Call `write_incremental_objstm(&mut bytes, pdf, container_ref, &packable, options)`. Assert: returns `(container_offset, members: Vec<(ObjectRef,u32)>)` where indices are `0,1` in `packable` order; `bytes[container_offset..]` contains `container_ref.number 0 obj` and `/Type /ObjStm` and `/N 2`.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf write_incremental_objstm`
Expected: FAIL — not defined.

**Step 3: Write minimal implementation**

```rust
fn write_incremental_objstm<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    container_ref: ObjectRef,
    packable: &[ObjectRef],
    options: &WriteOptions,
) -> Result<(usize, Vec<(ObjectRef, u32)>)> {
    let body = object_streams::emit_objstm_body(pdf, packable)?;
    let stream = object_streams::wrap_objstm_body(&body, options.compress_streams)?;
    let offset = bytes.len();
    write_object(bytes, container_ref, &Object::Stream(stream))?;
    let members = packable
        .iter()
        .enumerate()
        .map(|(i, &r)| (r, i as u32))
        .collect();
    Ok((offset, members))
}
```

> Note: confirm `options.compress_streams` is the right field/type that `wrap_objstm_body` expects (`CompressStreams`); the full-rewrite path calls `wrap_objstm_body(&body, options.compress_streams)` at writer.rs:1596 — mirror that exactly.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf write_incremental_objstm`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(writer): write_incremental_objstm container emitter (flpdf-9hc.5.9)"
```

---

### Task 5: wire the gated branch into `write_pdf_incremental`

**Files:**
- Modify: `crates/flpdf/src/writer.rs:346-412`
- Test: `crates/flpdf/tests/writer_tests.rs` (round-trip integration)

**Step 1: Write the failing test**

Integration test: open an xref-stream fixture, mutate ≥1 plain eligible object via `set_object`, write with `WriteOptions { object_streams: Generate, full_rewrite: false, .. }`. Re-open the output with flpdf; assert:
- (a) the mutated object resolves to the new value;
- (b) all other objects still resolve;
- (c) **the mutated object is now compressed**: `pdf.compressed_parent(mutated_ref) == Some((container_ref, 0))` where `container_ref.number == old_max + 1`, AND `pdf.resolve(container_ref)` is an `Object::Stream` whose dict `/Type` is `ObjStm`. **Do NOT hand-parse xref bytes** — use the reader API.
- (d) trailer `/Size` ≥ container number + 1;
- (e) `/Prev` points at the previous xref (compare with `pdf.previous_xref_offset()` of the source, or assert the appended trailer's `/Prev` integer equals the source's last startxref).

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf incremental_generate_roundtrip`
Expected: FAIL — branch not wired (object emitted plain, no ObjStm).

**Step 3: Write minimal implementation**

Introduce a single struct so the data flow is unambiguous (no loose `/* remember */`):

```rust
struct ObjStmIncremental {
    container: ObjectRef,
    container_offset: usize,
    /// member number -> (container number, index)
    compressed: BTreeMap<u32, (u32, u32)>,
}
```

In `write_pdf_incremental`, after `collect_touched_object_refs` and before `write_incremental_objects`:

```rust
let use_objstm = options.object_streams == ObjectStreamMode::Generate
    && matches!(pdf.last_xref_form(), XrefForm::Stream);

let mut objstm_inc: Option<ObjStmIncremental> = None;
let plain_touched: Vec<ObjectRef> = if use_objstm {
    let (packable, plain_remaining) = partition_objstm_eligible(pdf, &touched_object_refs)?;
    if packable.is_empty() {
        touched_object_refs.clone()
    } else {
        let declared = resolve_xref_stream_object_count(
            pdf.trailer().get("Size"), &source_xref_offsets);
        let container = allocate_incremental_objstm_container(
            &source_xref_offsets, &touched_object_refs, &deleted_object_refs, declared)?;
        let (container_offset, members) =
            write_incremental_objstm(&mut bytes, pdf, container, &packable, options)?;
        let mut compressed = BTreeMap::new();
        for (r, idx) in members {
            compressed.insert(r.number, (container.number, idx));
        }
        objstm_inc = Some(ObjStmIncremental { container, container_offset, compressed });
        plain_remaining
    }
} else {
    touched_object_refs.clone()
};
```

Then:
- `let mut xref_offsets = write_incremental_objects(&mut bytes, pdf, &plain_touched)?;` (NOT the full touched set).
- If `let Some(oi) = &objstm_inc`: `xref_offsets.insert(oi.container.number, (0, oi.container_offset));` — container becomes a type-1 entry. (`write_incremental_objects` returns `BTreeMap<u32,(u16,usize)>`, verified writer.rs:455-459, so `.insert` after the call is the canonical pattern.)
- Pass the compressed map to `merge_source_and_touched_offsets_for_xref_stream` (writer.rs:371): `objstm_inc.as_ref().map(|o| &o.compressed).unwrap_or(&empty)` (bind `let empty = BTreeMap::new();` once).
- After computing `object_count`, if `let Some(oi) = &objstm_inc`: `object_count = object_count.max(oi.container.number as usize + 1);`.

> The container is emitted inside the `if` block via `write_incremental_objstm`, so it precedes plain objects in `bytes` and `container_offset` is its true byte position. No ordering ambiguity.

**Framing-divergence decision (resolved during Task 4 review — DO NOT re-litigate, just apply):**
Keep `write_incremental_objstm` emitting via `write_object` as-is. It does NOT honor `options.newline_before_endstream`, whereas the full-rewrite ObjStm path uses manual `format!` + `write_stream_to_buf(..., options.newline_before_endstream)` + `endobj`. This divergence is **unobservable under the default config**: `Object::Stream::write_pdf` and `write_stream_to_buf` are byte-identical under `NewlineBeforeEndstream::Yes`; they differ only under `NewlineBeforeEndstream::No` AND an ObjStm payload whose last byte is `\n`/`\r` (near-empty surface with Flate output). Task 6's `qpdf --check` is the gate that would surface any real delta; the fix (if ever needed) is a local ~4-line swap to manual-emit with zero API impact. As part of this task's commit, **add to `write_incremental_objstm`'s doc comment** a sentence recording this bounded, intentional contract, e.g.:
> Container framing is emitted via `write_object` (single unconditional `\n` before `endstream`); unlike the full-rewrite ObjStm path it does not consult `options.newline_before_endstream`. Observable only under `NewlineBeforeEndstream::No` with a payload ending in `\n`/`\r` (not the default). Revisit if the Task 6 qpdf cross-check exposes a delta.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf incremental_generate_roundtrip`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/tests/writer_tests.rs
git commit -m "feat(writer): wire generate-mode ObjStm into incremental path (flpdf-9hc.5.9)"
```

---

### Task 6: qpdf cross-check integration test

**Files:**
- Test: `crates/flpdf/tests/writer_tests.rs` or the existing compat-matrix test module (reuse the qpdf-invocation helper used by `compat_matrix_tests`).

**Step 1: Write the failing test**

Produce a generate-mode incremental output as in Task 5, run `qpdf --check <out>` via the existing helper; assert exit status 0 / "no errors". Additionally `qpdf --show-object=<mutated num> <out>` (or `--json`) shows the object resolvable. Gate the test on qpdf availability the same way existing compat tests do.

**Step 2: Run test to verify it fails / passes**

Run: `cargo test -p flpdf incremental_generate_qpdf_check`
Expected: PASS if Task 5 correct (this test mostly validates structural correctness; if it fails it reveals a real xref/Prev bug).

**Step 3-4: Fix any structural defects surfaced, re-run.**

**Step 5: Commit**

```bash
git add crates/flpdf/tests/
git commit -m "test(writer): qpdf --check cross-check for incremental generate ObjStm (flpdf-9hc.5.9)"
```

---

### Task 7: fallback regression + full 9hc.1 suite

**Files:**
- Test: `crates/flpdf/tests/writer_tests.rs`

**Step 1: Write the failing/guard tests**

Three byte-identical fallback assertions (output with `object_streams=Generate` MUST equal output with the pre-existing plain incremental path):
1. **Table-form source** + Generate → byte-identical to default incremental.
2. **preserve & disable** modes + Stream source → byte-identical to default incremental.
3. **empty packable** (only Catalog/stream touched) + Generate + Stream → byte-identical to default incremental (no empty ObjStm emitted).

**Step 2: Run tests to verify they fail or pass**

Run: `cargo test -p flpdf incremental_generate_fallback`
Expected: PASS (gate ensures fallback). A failure means the gate leaks.

**Step 3: Fix gate if any fallback diverges. Re-run.**

**Step 4: Full regression**

Run: `cargo test -p flpdf 2>&1 | grep -E "^test result:"`
Expected: ALL suites pass, count ≥ 1312 (baseline), 0 failed. Specifically the existing flpdf-9hc.1 incremental / ObjStm-member / /Prev / /Extends / deletion tests pass unmodified.

**Step 5: Commit**

```bash
git add crates/flpdf/tests/writer_tests.rs
git commit -m "test(writer): fallback regression for incremental generate gate (flpdf-9hc.5.9)"
```

---

## Final verification (before declaring done)

```bash
cargo fmt -- --check
cargo clippy -p flpdf --all-targets 2>&1 | tail -5
cargo test -p flpdf 2>&1 | grep -E "^test result:" | awk '{t+=$4;f+=$6} END{print "passed",t,"failed",f}'
```

Acceptance (from beads flpdf-9hc.5.9): generate + xref-stream-source incremental produces a valid /Prev chain with a new ObjStm container in the appended generation; eligible touched plain objects via type-2; untouched / existing-ObjStm-member / deleted retain prior behaviour; qpdf --check clean; ALL existing 9hc.1 tests pass unmodified; fallback paths byte-identical.

## Out of scope (future issues)
preserve/disable incremental; Table→stream upgrade; /Extends linkage; qpdf byte/member-set parity; multi-ObjStm batch splitting.
