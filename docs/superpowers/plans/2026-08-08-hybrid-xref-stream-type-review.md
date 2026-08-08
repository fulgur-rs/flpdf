# Hybrid Xref Stream Type Review Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reject `/XRefStm` targets that are streams but not `/Type /XRef`, matching qpdf 11.9.0.

**Architecture:** `parse_xref_stream` owns xref-stream identity for both direct `startxref` streams and classic-trailer `/XRefStm` streams. The test reuses the valid hybrid fixture and replaces the same-width `/Type /XRef` dictionary token, keeping all byte offsets valid while making the target an ordinary stream.

**Tech Stack:** Rust workspace, flpdf xref loader, pinned qpdf 11.9.0, `cargo test`.

---

## File structure

- `crates/flpdf/tests/xref_tests.rs` owns synthetic-PDF regression coverage for classic tables, xref streams, and hybrid `/XRefStm` documents.
- `crates/flpdf/src/xref.rs` owns parsing and validation of all xref-stream objects.

### Task 1: Prove the hybrid type-validation gap

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs`, directly after `classic_xref_table_reads_entries_from_its_xrefstm`
- Test: `crates/flpdf/tests/xref_tests.rs::rejects_hybrid_xref_stream_without_xref_type`

- [ ] **Step 1: Add the failing regression test**

```rust
#[test]
fn rejects_hybrid_xref_stream_without_xref_type() {
    let (mut bytes, _) = classic_xref_with_hybrid_only_entry();
    let type_marker = b"/Type /XRef";
    let type_pos = bytes
        .windows(type_marker.len())
        .position(|window| window == type_marker)
        .expect("hybrid fixture contains an xref type");
    bytes[type_pos..type_pos + type_marker.len()].copy_from_slice(b"/Bogus /Yep");

    let err = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect_err("untyped hybrid stream must not be accepted as xref");
    let message = format!("{err}");
    assert!(message.contains("xref not found"), "got {message}");
    assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
}
```

`/Bogus /Yep` has the same byte length as `/Type /XRef`, so the fixture's
stored xref offsets stay valid and the stream still has valid `/Size`, `/W`,
`/Index`, and entry bytes.

- [ ] **Step 2: Run the regression test and verify RED**

Run:

```bash
cargo test -p flpdf --test xref_tests rejects_hybrid_xref_stream_without_xref_type -- --exact
```

Expected: FAIL because the existing loader accepts the ordinary stream and
returns `Ok`.

### Task 2: Enforce qpdf's xref-stream identity boundary

**Files:**
- Modify: `crates/flpdf/src/xref.rs`, in `parse_xref_stream` immediately after the `Object::Stream` match
- Test: `crates/flpdf/tests/xref_tests.rs::rejects_hybrid_xref_stream_without_xref_type`

- [ ] **Step 1: Add the minimal type validation**

Insert after the existing stream match:

```rust
if !matches!(
    stream.dict.get("Type"),
    Some(Object::Name(type_name)) if type_name.as_slice() == b"XRef"
) {
    return Err(Error::parse(xref_pos, "xref not found"));
}
```

Do not add a second `/XRefStm`-specific check. The shared parser must reject
the same malformed object for both entry paths, just as qpdf's
`QPDF::read_xrefStream` does.

- [ ] **Step 2: Run the focused test and verify GREEN**

Run:

```bash
cargo test -p flpdf --test xref_tests rejects_hybrid_xref_stream_without_xref_type -- --exact
```

Expected: PASS; the error contains `xref not found` and is `Error::Parse`.

- [ ] **Step 3: Run related xref coverage**

Run:

```bash
cargo test -p flpdf --test xref_tests
```

Expected: all xref tests pass, including direct xref streams and hybrid xref
streams.

- [ ] **Step 4: Confirm the qpdf probe classification**

Use the already-recorded malformed hybrid probe result: qpdf 11.9.0 reports
`xref not found` at the `/XRefStm` offset before its repair attempt. Confirm
the focused flpdf regression reports the same xref-not-found category before
object resolution.

- [ ] **Step 5: Run repository gates and commit**

Run:

```bash
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 scripts/qpdf-module-docs.py --check
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Then commit only the implementation and regression files:

```bash
git add crates/flpdf/src/xref.rs crates/flpdf/tests/xref_tests.rs
git commit -m "fix(xref): require type on hybrid xref streams"
```

### Task 3: Publish the verified PR update

**Files:**
- No repository files.

- [ ] **Step 1: Push the verified commit**

Run:

```bash
git push
```

Do not reply to or resolve Thread 1, modify Thread 2, merge the PR, or close
the Bead without explicit user direction.
