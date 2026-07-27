# XRefEntry Complete Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the misleading public `XrefOffset` enum with a qpdf-shaped `XrefEntry` component and route every reader, cache, writer, object-stream, linearization, and test consumer through it.

**Architecture:** A new root `xref_entry.rs` owns only the three xref entry value variants. `xref.rs` retains xref parsing, repair, `LoadedXref`, and `XrefForm`; after adding the new type, every consumer switches in one compile-safe cutover before the old enum and re-export are deleted.

**Tech Stack:** Rust 2021; qpdf 11.9.0 `QPDFXRefEntry`; Cargo unit/integration tests; Clippy; strict rustdoc; `cargo llvm-cov`; `scripts/patch-coverage.sh`.

## Global Constraints

- Refresh the definition and callsite inventory immediately before implementation; the design-time inventory already exceeds one hundred occurrences.
- The final public type is exactly:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    Free { next: u32 },
    Uncompressed { offset: u64 },
    Compressed { stream: u32, index: u32 },
}
```

- `XrefForm`, `LoadedXref`, parsing, recovery, and merge orchestration remain in `xref.rs`.
- This is an intentional pre-1.0 public API break: do not keep a `type XrefOffset = XrefEntry`,
  deprecated alias, conversion wrapper, or `Offset` compatibility variant.
- Every final production and test consumer uses `XrefEntry`; `rg XrefOffset` must return no matches.
- Preserve xref parse, repair, cache, full/incremental writer, ObjStm, linearization, and byte output behavior.
- Coordinate with `flpdf-80b6`: if its writer branch is still active and overlaps the listed writer files, wait for it to settle or stack this work on its result. Do not edit the same writer surface concurrently.
- Every production change follows RED→GREEN→REFACTOR and fresh patch coverage against the immediate parent must reach 100%.
- `Pdf::source_xref_entries()` feeds writer, ObjStm, and linearization consumers directly, so
  Tasks 2 and 3 form one compile-safe RED→GREEN batch. Change every affected test consumer first,
  then migrate all production consumers and delete `XrefOffset`; do not land the intermediate
  Task 2 commit or introduce a conversion wrapper to force an artificial boundary.

## Current Inventory to Refresh

Production files containing `XrefOffset` at design time:

```text
crates/flpdf/src/cache.rs
crates/flpdf/src/lib.rs
crates/flpdf/src/linearization/plan.rs
crates/flpdf/src/linearization/writer.rs
crates/flpdf/src/reader.rs
crates/flpdf/src/writer.rs
crates/flpdf/src/writer/object_streams.rs
crates/flpdf/src/writer/plain/plan.rs
crates/flpdf/src/xref.rs
```

Integration-test files:

```text
crates/flpdf/tests/cmp_diff_zero_tests.rs
crates/flpdf/tests/object_streams_writer_tests.rs
crates/flpdf/tests/reader_tests.rs
crates/flpdf/tests/writer_tests.rs
crates/flpdf/tests/xref_tests.rs
```

## Delivery Boundary

**Branch:** `feature/flpdf-qxba-phase2-xref-entry`
**PR base:** `origin/main`
**Patch-coverage base:** `git merge-base HEAD origin/main`

The Pipeline layer and the writer work owned by `flpdf-80b6` are merged into `origin/main`.
If new overlapping writer work appears before execution, first land/rebase that result below this
branch. Do not change the immediate-parent coverage rule.

---

### Task 1: Add the truthful entry value component

**Files:**
- Create: `crates/flpdf/src/xref_entry.rs`
- Modify: `crates/flpdf/src/lib.rs:104-175,260-267`
- Test: `crates/flpdf/src/xref_entry.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/QPDFXRefEntry.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDFXRefEntry.cc`

**Interfaces:**
- Produces the exact public enum in Global Constraints.
- `lib.rs` declares `pub mod xref_entry;` and temporarily re-exports
  `pub use xref_entry::XrefEntry;`.
- The old `XrefOffset` remains only until Task 3 so intermediate commits compile; it is not an
  alias and no conversions are added.

- [ ] **Step 1: Write failing value-model tests**

```rust
#[test]
fn variants_represent_all_pdf_xref_entry_kinds() {
    assert_eq!(
        XrefEntry::Free { next: 7 },
        XrefEntry::Free { next: 7 }
    );
    assert_eq!(
        XrefEntry::Uncompressed { offset: 42 },
        XrefEntry::Uncompressed { offset: 42 }
    );
    assert_eq!(
        XrefEntry::Compressed {
            stream: 12,
            index: 3,
        },
        XrefEntry::Compressed {
            stream: 12,
            index: 3,
        }
    );
}

#[test]
fn entry_is_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<XrefEntry>();
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p flpdf xref_entry::tests --lib
```

Expected: compile failure because the module/type is absent.

- [ ] **Step 3: Implement the component and module doc**

Create:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/QPDFXRefEntry.cc.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    Free { next: u32 },
    Uncompressed { offset: u64 },
    Compressed { stream: u32, index: u32 },
}
```

Do not add constructors that duplicate enum construction or carry generation; generation remains
in the `ObjectRef` key / writer tuple as it does now.

- [ ] **Step 4: Run focused tests and public docs**

Run:

```bash
cargo test -p flpdf xref_entry::tests --lib
RUSTDOCFLAGS="-D warnings" cargo doc -p flpdf --no-deps
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/xref_entry.rs crates/flpdf/src/lib.rs
git commit -m "feat: add xref entry value component"
```

---

### Task 2: Migrate parser, repair, reader, and cache consumers

**Files:**
- Modify: `crates/flpdf/src/xref.rs:1-45,200-920`
- Modify: `crates/flpdf/src/reader.rs:1-90,530-885`
- Modify: `crates/flpdf/src/cache.rs:1-40`
- Modify: `crates/flpdf/tests/xref_tests.rs`
- Modify: `crates/flpdf/tests/reader_tests.rs`
- Test: `crates/flpdf/tests/xref_tests.rs`
- Test: `crates/flpdf/tests/reader_tests.rs`
- Test: `crates/flpdf/src/cache.rs`

**Interfaces:**
- Consumes `crate::XrefEntry`.
- Changes `LoadedXref.entries` to `BTreeMap<ObjectRef, XrefEntry>`.
- Changes reader source-entry storage and accessors to `BTreeMap<ObjectRef, XrefEntry>`.
- Keeps `XrefOffset` declaration temporarily unused only until Task 3; add no `allow(dead_code)`.

- [ ] **Step 1: Change xref tests first and verify RED**

Mechanically replace imports and expected values, including:

```rust
assert_eq!(
    loaded.entries.get(&ObjectRef::new(1, 0)),
    Some(&XrefEntry::Uncompressed { offset: 9 })
);
assert_eq!(
    loaded.entries.get(&ObjectRef::new(7, 0)),
    Some(&XrefEntry::Compressed {
        stream: 5,
        index: 2,
    })
);
assert_eq!(
    loaded.entries.get(&ObjectRef::new(0, 65535)),
    Some(&XrefEntry::Free { next: 0 })
);
```

Run:

```bash
cargo test -p flpdf --test xref_tests
```

Expected: compile failures where production APIs still return `XrefOffset`.

- [ ] **Step 2: Migrate xref parsing and recovery**

Replace every xref production construction/match:

```rust
XrefOffset::Offset(offset)
```

with:

```rust
XrefEntry::Uncompressed { offset }
```

Replace `Free` and `Compressed` only by type name. Change helper return types such as
`recover_xref_entries` and ObjStm recovery maps to `XrefEntry`.

- [ ] **Step 3: Migrate reader and cache**

Change reader fields/accessors and cache conversion:

```rust
match entry {
    XrefEntry::Free { .. } => CacheEntry::Deleted,
    XrefEntry::Uncompressed { offset } => CacheEntry::Unresolved { offset: *offset },
    XrefEntry::Compressed { stream, index } => CacheEntry::Compressed {
        stream: *stream,
        index: *index,
    },
}
```

Do not change cache state semantics or eager/lazy resolution.

- [ ] **Step 4: Run reader/xref/cache tests**

Run:

```bash
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf cache::tests --lib
```

Expected: PASS.

- [ ] **Step 5: Continue directly into Task 3**

Do not commit this intermediate state: the reader source-entry type is consumed directly by the
writer, ObjStm, and linearization paths. Complete Task 3 and commit the compile-safe cutover once.

---

### Task 3: Migrate writers, ObjStm, linearization, and delete XrefOffset

**Files:**
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/writer/object_streams.rs`
- Modify: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/linearization/plan.rs`
- Modify: `crates/flpdf/src/linearization/writer.rs`
- Modify: `crates/flpdf/src/xref.rs:27-37`
- Modify: `crates/flpdf/src/lib.rs:260-267`
- Modify: `crates/flpdf/tests/cmp_diff_zero_tests.rs`
- Modify: `crates/flpdf/tests/object_streams_writer_tests.rs`
- Modify: `crates/flpdf/tests/writer_tests.rs`
- Test: the three integration-test files above

**Interfaces:**
- All writer maps previously containing `(generation, XrefOffset)` become
  `(generation, XrefEntry)`.
- `Offset(value)` becomes `Uncompressed { offset: value }`.
- Deletes the `XrefOffset` enum from `xref.rs` and removes its re-export from `lib.rs`.

- [ ] **Step 1: Change writer integration tests and verify RED**

Update test imports and fixtures first:

```rust
let source_offsets = BTreeMap::from([
    (4, (0, XrefEntry::Uncompressed { offset: 100 })),
    (11, (0, XrefEntry::Uncompressed { offset: 200 })),
]);
```

Change compressed/free matches similarly. Run:

```bash
cargo test -p flpdf --test writer_tests
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --test cmp_diff_zero_tests
```

Expected: compile failures in writer signatures and pattern matches.

- [ ] **Step 2: Migrate plain/full/incremental writer maps**

Update imports, signatures, map values, and matches in `writer.rs` and
`writer/plain/plan.rs`. Preserve:

- free-list generation and `next`;
- uncompressed byte offset conversion/error messages;
- compressed stream number and member index;
- xref table/stream type-field encoding;
- final placement-derived `XrefForm`.

For xref stream output use:

```rust
match entry {
    XrefEntry::Free { next } => (0, u64::from(*next), generation),
    XrefEntry::Uncompressed { offset } => (1, *offset, generation),
    XrefEntry::Compressed { stream, index } => {
        (2, u64::from(*stream), u16::try_from(*index)?)
    }
}
```

Keep the existing checked-conversion error messages rather than replacing them with `unwrap`.

- [ ] **Step 3: Migrate object-stream and linearization consumers**

Update structural-container tests, source compressed-entry detection, source membership lookup,
and writer emission in:

```text
crates/flpdf/src/writer/object_streams.rs
crates/flpdf/src/linearization/plan.rs
crates/flpdf/src/linearization/writer.rs
```

Only rename the value model; do not alter routing, ordering, or object-stream mode policy.

- [ ] **Step 4: Delete old definition and public re-export**

Delete:

```rust
pub enum XrefOffset {
    Free { next: u32 },
    Offset(u64),
    Compressed { stream: u32, index: u32 },
}
```

Change `lib.rs` to export:

```rust
pub use xref::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort,
    load_xref_and_trailer_with_repair, LoadedXref, XrefForm,
};
pub use xref_entry::XrefEntry;
```

- [ ] **Step 5: Run all affected integration tests**

Run:

```bash
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf --test object_streams_writer_tests
cargo test -p flpdf --test cmp_diff_zero_tests
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test compat_matrix_tests
```

Expected: PASS; `compat_matrix_tests` uses its existing explicit skip when qpdf is unavailable.

- [ ] **Step 6: Prove the old API is gone**

Run:

```bash
rg -n "XrefOffset|::Offset\\(" crates scripts
```

Expected: no `XrefOffset` matches in code or scripts. Historical design and plan documents retain
the old symbol where they describe the pre-cutover implementation. Inspect any `::Offset(` match
before changing it; unrelated offset enums are not part of this task.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/cache.rs crates/flpdf/src/reader.rs crates/flpdf/src/writer.rs crates/flpdf/src/writer/object_streams.rs crates/flpdf/src/writer/plain/plan.rs crates/flpdf/src/linearization/plan.rs crates/flpdf/src/linearization/writer.rs crates/flpdf/src/xref.rs crates/flpdf/src/lib.rs crates/flpdf/tests/cmp_diff_zero_tests.rs crates/flpdf/tests/object_streams_writer_tests.rs crates/flpdf/tests/reader_tests.rs crates/flpdf/tests/writer_tests.rs crates/flpdf/tests/xref_tests.rs
git commit -m "refactor: cut over all xref entry consumers"
```

---

### Task 4: Correspondence, workspace verification, and patch coverage

**Files:**
- Modify if generated output changes: `docs/qpdf-correspondence.md`
- Verify: all Task 1-3 files

**Interfaces:**
- Produces no new API; verifies D1/D2 and public break completeness.

- [ ] **Step 1: Run the definition/callsite audit again**

Run:

```bash
rg -n "XrefOffset" crates scripts
rg -n "pub enum XrefEntry|pub use .*XrefEntry" crates/flpdf/src
rg -n "XrefEntry" crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/tests
```

Expected: zero old-symbol matches in code or scripts, one enum definition, one public re-export,
and every expected consumer listed in the refreshed inventory.

- [ ] **Step 2: Run correspondence and public docs**

Run:

```bash
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts.tests.test_qpdf_module_docs
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: PASS. If correspondence generation reports drift, regenerate and inspect that
`xref_entry.rs` maps to `QPDFXRefEntry.cc` while `xref.rs` no longer claims the value
representation.

- [ ] **Step 3: Run formatting, lint, and workspace tests**

Run:

```bash
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings --document-private-items" cargo doc --workspace --all-features --no-deps
cargo test --workspace --all-features
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 4: Measure fresh immediate-parent patch coverage**

Run:

```bash
base_ref="$(git merge-base HEAD origin/main)"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path /tmp/flpdf-xref-entry.lcov
scripts/patch-coverage.sh "$base_ref" HEAD /tmp/flpdf-xref-entry.lcov
```

Expected: 100% of changed executable lines. Cover missed match arms with focused xref fixtures;
do not add exclusions to mechanical rename lines.

- [ ] **Step 5: Commit truthful generated documentation when changed**

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: record xref entry correspondence"
```

Skip the commit if there is no tracked documentation diff.

- [ ] **Step 6: Record clean final state**

Run:

```bash
git status --short --branch
git log --oneline --decorate -6
```

Expected: clean worktree and no compatibility alias.
