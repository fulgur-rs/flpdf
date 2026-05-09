# Incremental Update Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `write_pdf` incremental output from source-preserving copy behavior to true touched-object incremental updates with xref-stream/object-stream handling and robust `/Prev` chaining.

**Architecture:** Keep `Pdf` as read API provider and move rewrite strategy into `writer.rs` by classifying source entries (unchanged, touched, added). Build update batches from reachable object graph, append only changed content, and emit a correct next xref/trailer pointing back to prior updates.

**Tech Stack:** Rust, existing `flpdf` crate primitives (`Object`, `ObjectRef`, `Dictionary`, `Parser`, `Pdf`, `load_xref_and_trailer*`), `cargo test`, `qpdf` in CI/compat tests.

---

### Task 1: Define touched-object diff graph and update plan

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/tests/writer_tests.rs`

- [ ] **Step 1: Write a failing test for touched-object only rewrite**

```rust
#[test]
fn write_pdf_emits_only_touched_objects() {
    let mut pdf = build_fixture_with_marked_untouched_object();
    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();
    assert!(output_keeps_prefix_intact(&pdf, &out));
    assert!(output_contains_new_object_revision("2 0 obj"));
}
```

- [ ] **Step 2: Run it and confirm RED**

Run: `cargo test -p flpdf --test writer_tests write_pdf_emits_only_touched_objects -- --nocapture`
Expected: failure that output still contains old untouched bytes but also old rewritten copies.

- [ ] **Step 3: Add diff plan representation in `writer.rs`**

```rust
#[derive(Debug, Clone)]
enum RewriteKind {
    Unchanged,
    Updated(ObjectRef),
    Added(ObjectRef),
}

struct RewriteManifest {
    object_refs: Vec<ObjectRef>,
    root_ref: ObjectRef,
}
```

- [ ] **Step 4: Implement minimal graph walk from root refs and collect touched refs only**

```rust
fn collect_touched_objects(
    pdf: &mut Pdf<impl Read + Seek>,
    root_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    // BFS from root to build set of references that should be rewritten
    unimplemented!()
}
```

- [ ] **Step 5: Emit only touched/additional objects to appended section**

```rust
for object_ref in manifest.object_refs {
    let object = pdf.resolve(object_ref)?;
    emit_object(&mut bytes, object_ref, &object, ...)?;
}
```

- [ ] **Step 6: Run targeted tests then full writer test subset**
Run: `cargo test -p flpdf --test writer_tests`

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/reader.rs crates/flpdf/tests/writer_tests.rs
git commit -m "writer: emit only touched objects in incremental update"
```

### Task 2: Implement xref-stream-aware generation

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/tests/xref_tests.rs`

- [ ] **Step 1: Add a fixture test for source xref stream update**

```rust
#[test]
fn write_pdf_preserves_and_updates_xref_streamed_document() { /* ... */ }
```

- [ ] **Step 2: Run it and verify RED**
Run: `cargo test -p flpdf --test writer_tests write_pdf_preserves_and_updates_xref_streamed_document`

- [ ] **Step 3: Add xref stream branch in output builder**

```rust
enum XrefForm {
    Table,
    Stream,
}
```

- [ ] **Step 4: Build xref stream bytes for new trailer block**
Implement minimal writer-side stream path and emit format that flpdf and qpdf can parse.

- [ ] **Step 5: Ensure mixed-mode compatibility (read source table, write stream and vice versa)**

```rust
let next_form = choose_next_xref_form(pdf.trailer());
```

- [ ] **Step 6: Run xref + writer tests**
Run: `cargo test -p flpdf --test xref_tests` and `cargo test -p flpdf --test writer_tests`

- [ ] **Step 7: Commit**

```bash
git commit -m "writer: support incremental updates from xref-stream sources"
```

### Task 3: Stable `/Prev` chain for repeated incremental runs

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/tests/writer_tests.rs`

- [ ] **Step 1: Add regression test for two consecutive writes**

```rust
#[test]
fn write_pdf_twice_keeps_prev_chain_valid() { /* rewrite twice and compare valid */ }
```

- [ ] **Step 2: Run test RED**
Run: `cargo test -p flpdf --test writer_tests write_pdf_twice_keeps_prev_chain_valid`

- [ ] **Step 3: Make trailer emission keep prior update chain**

```rust
let prev = pdf.previous_xref_offset()?;
trailer.insert("Prev", Object::Integer(prev as i64));
```

- [ ] **Step 4: Guard size calculation across repeated appends**
Preserve monotonic `/Size` and never shrink on update.

- [ ] **Step 5: Full validation run**
Run: `cargo test -p flpdf`

- [ ] **Step 6: Commit**

```bash
git commit -m "writer: maintain valid /Prev chain across repeated incremental updates"
```

### Task 4: Compressed object update path (`ObjStm`)

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/reader_tests.rs`

- [ ] **Step 1: Add fixture and test for compressed-object touched update**

```rust
#[test]
fn write_pdf_updates_objstm_objects() { /* create/update/delete through object stream */ }
```

- [ ] **Step 2: Run RED**
Run: `cargo test -p flpdf --test writer_tests write_pdf_updates_objstm_objects`

- [ ] **Step 3: Resolve compressed refs when collecting touched set**

```rust
match cache_entry {
    CacheEntry::Compressed { stream, index } => resolve_object_stream_entry(...),
    _ => ...
}
```

- [ ] **Step 4: Emit updates as new indirect objects or by updating stream payload**

- [ ] **Step 5: Verify with qpdf-read compatibility**
Run: `cargo run --bin flpdf -- fixtures/... /tmp/rewrite.pdf` + `qpdf --check /tmp/rewrite.pdf`

- [ ] **Step 6: Commit**

```bash
git commit -m "writer: support incremental updates for object stream entries"
```

### Task 5: Expand incremental regression matrix

**Files:**
- Add: `crates/flpdf-cli/tests/compat_matrix_tests.rs` (new cases)
- Add: `tests/fixtures/compat/*.pdf` and golden entries as needed

- [ ] **Step 1: Add fixture set for touched-only and xref-stream cases**

- [ ] **Step 2: Add matrix assertions comparing qpdf metadata and output validity**

```rust
#[test]
fn qpdf_incremental_parity_matrix_smoke() { /* use qpdf --show-npages and read-back */ }
```

- [ ] **Step 3: Run compat tests**
Run: `cargo test -p flpdf-cli --test compat_matrix_tests`

- [ ] **Step 4: Run full verification before handoff**
Run: `cargo test`

- [ ] **Step 5: Commit**

```bash
```

---

### Self-review notes

- [ ] 仕様の各要件（差分更新 / xref stream / /Prev 多段 / ObjStm / qpdf回帰）が各タスクに含まれている
- [ ] それぞれの要件で「どう動くか」がテストとして明示される
- [ ] `/Prev` と `startxref` の型変換境界（`i64`）が安全に扱われる
- [ ] 実装は `write_pdf` とCLI動線に影響しない形で統合される

Plan complete and ready for execution. Two execution options:

1. Subagent-Driven (recommended) - dispatch one fresh subagent per task, review between tasks
2. Inline Execution - execute tasks in-session using checks between each step
