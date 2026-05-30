# resolve_borrowed Call-Site Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate every safe internal `Pdf::resolve()` call site to `Pdf::resolve_borrowed()` while preserving behavior and public API compatibility.

**Architecture:** Treat this as a staged refactor. First inventory all call sites, then migrate modules in order from focused writer object-stream code to broader writer/traversal/CLI code, leaving `resolve()` only where ownership is needed or borrowing would require non-local restructuring.

**Tech Stack:** Rust workspace, `flpdf` library crate, `flpdf-cli`, `cargo fmt`, `cargo test`, `roborev-refine`.

---

### Task 1: Inventory And Baseline

**Files:**
- Modify: `docs/superpowers/plans/2026-05-30-resolve-borrowed-call-sites.md`
- Inspect: `crates/flpdf/src/**/*.rs`, `crates/flpdf-cli/src/main.rs`, `crates/flpdf/tests/**/*.rs`

- [ ] **Step 1: Count current call sites**

Run:

```bash
cargo test -p flpdf --test writer_tests
python3 - <<'PY'
from pathlib import Path
for p in sorted(Path('crates').rglob('*.rs')):
    text = p.read_text()
    n = text.count('.resolve(')
    if n:
        print(f'{n:3} {p}')
PY
```

Expected: `writer_tests` passes, then a per-file inventory of `.resolve(` call sites.

- [ ] **Step 2: Record inventory result in the PR summary notes**

Do not edit code yet. Use the output to prioritize files in this order:

```text
1. crates/flpdf/src/writer/object_streams.rs
2. crates/flpdf/src/writer.rs
3. crates/flpdf/src/pages.rs, resources.rs, page_split.rs, outline_dest_remap.rs
4. crates/flpdf/src/reader.rs, crates/flpdf-cli/src/main.rs, tests
```

### Task 2: Stage 1 - Writer Object Streams

**Files:**
- Modify: `crates/flpdf/src/writer/object_streams.rs`

- [ ] **Step 1: Verify the characterization test is green before editing**

Run:

```bash
cargo test -p flpdf --test writer_tests
```

Expected: pass. This is the behavior guard for writer object-stream emission.

- [ ] **Step 2: Migrate immediate-inspection call sites**

Change call sites shaped like this:

```rust
let obj = pdf.resolve(r)?;
match obj {
    Object::Stream(_) => ...,
    Object::Dictionary(_) => ...,
    _ => ...,
}
```

to this shape:

```rust
match pdf.resolve_borrowed(r)? {
    Object::Stream(_) => ...,
    Object::Dictionary(_) => ...,
    _ => ...,
}
```

Keep `resolve()` for call sites that build owned `(ObjectRef, Object)` pairs or pass an owned object into writer serialization.

- [ ] **Step 3: Run focused test**

Run:

```bash
cargo test -p flpdf --test writer_tests
```

Expected: pass.

- [ ] **Step 4: Commit Stage 1**

Run:

```bash
git add crates/flpdf/src/writer/object_streams.rs
git commit -m "refactor(writer): borrow resolved object-stream inputs"
```

### Task 3: Stage 2 - Top-Level Writer

**Files:**
- Modify: `crates/flpdf/src/writer.rs`

- [ ] **Step 1: Migrate read-only writer inspections**

Change call sites where the resolved object is only inspected in a `match`, `if let`, or `matches!` expression:

```rust
if let Ok(Object::Stream(stream)) = pdf.resolve(*object_ref) {
    ...
}
```

to:

```rust
if let Ok(Object::Stream(stream)) = pdf.resolve_borrowed(*object_ref) {
    ...
}
```

Keep `resolve()` where the code mutates a resolved object before writing, such as `let mut object = pdf.resolve(*object_ref)?;`.

- [ ] **Step 2: Run writer tests**

Run:

```bash
cargo test -p flpdf --test writer_tests
```

Expected: pass.

- [ ] **Step 3: Commit Stage 2**

Run:

```bash
git add crates/flpdf/src/writer.rs
git commit -m "refactor(writer): borrow resolved objects for inspection"
```

### Task 4: Stage 3 - Library Traversal Helpers

**Files:**
- Modify: `crates/flpdf/src/pages.rs`
- Modify: `crates/flpdf/src/resources.rs`
- Modify: `crates/flpdf/src/page_split.rs`
- Modify: `crates/flpdf/src/outline_dest_remap.rs`

- [ ] **Step 1: Migrate page/resource traversal matches**

For page-tree and resource traversal code, prefer this pattern:

```rust
let Object::Dictionary(dict) = pdf.resolve_borrowed(page_ref)? else {
    return Err(Error::Malformed("expected page dictionary".into()));
};
```

or this pattern for optional references:

```rust
match pdf.resolve_borrowed(r)? {
    Object::Dictionary(dict) => { /* inspect dict */ }
    Object::Null => { /* keep existing null behavior */ }
    _ => { /* keep existing error behavior */ }
}
```

Do not hold a borrowed dictionary across a later `pdf.resolve_borrowed(...)` call unless the compiler accepts the lifetime and the code remains local and clear.

- [ ] **Step 2: Run traversal-focused tests**

Run:

```bash
cargo test -p flpdf --test reader_tests
cargo test -p flpdf-cli --test cli_tests pages
```

Expected: both pass.

- [ ] **Step 3: Commit Stage 3**

Run:

```bash
git add crates/flpdf/src/pages.rs crates/flpdf/src/resources.rs crates/flpdf/src/page_split.rs crates/flpdf/src/outline_dest_remap.rs
git commit -m "refactor(reader): borrow resolved traversal objects"
```

### Task 5: Stage 4 - Reader Internals, CLI, And Tests

**Files:**
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf/tests/reader_tests.rs` if assertions can be clearer with borrowed objects

- [ ] **Step 1: Migrate remaining obvious read-only call sites**

Use `resolve_borrowed()` where the value is formatted, matched, or inspected immediately:

```rust
let object = pdf.resolve_borrowed(object_ref)?;
println!("{}", object);
```

Keep `resolve()` where owned values are returned or passed to APIs expecting `Object`.

- [ ] **Step 2: Run CLI and reader tests**

Run:

```bash
cargo test -p flpdf --test reader_tests
cargo test -p flpdf-cli --test cli_tests
```

Expected: both pass.

- [ ] **Step 3: Commit Stage 4**

Run:

```bash
git add crates/flpdf/src/reader.rs crates/flpdf-cli/src/main.rs crates/flpdf/tests/reader_tests.rs
git commit -m "refactor(reader,cli): borrow resolved objects at call sites"
```

### Task 6: Final Verification And Review

**Files:**
- Inspect: all modified files

- [ ] **Step 1: Final formatting and tests**

Run:

```bash
cargo fmt -- --check
cargo test -p flpdf
cargo test
```

Expected: all pass.

- [ ] **Step 2: Confirm remaining `resolve()` call sites are intentional**

Run:

```bash
python3 - <<'PY'
from pathlib import Path
for p in sorted(Path('crates').rglob('*.rs')):
    lines = [(i, line.strip()) for i, line in enumerate(p.read_text().splitlines(), 1) if '.resolve(' in line]
    if lines:
        print(p)
        for i, line in lines[:20]:
            print(f'  {i}: {line}')
PY
```

Expected: remaining call sites are owned-value paths, tests preserving owned API coverage, or deferred `MaybeLater` paths.

- [ ] **Step 3: Push branch**

Run:

```bash
git status --short
git push -u origin flpdf-f4d-resolve-borrowed-call-sites
```

Expected: branch is pushed and working tree is clean.

- [ ] **Step 4: Run roborev-refine**

Run the repository's `roborev-refine` workflow for the current branch. Fix any findings in follow-up commits, rerun focused tests and final tests as needed, then push again.
