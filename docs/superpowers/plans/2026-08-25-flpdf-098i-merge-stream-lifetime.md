# Merge Stream Lifetime Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve lazy provider-backed stream behavior through merge reachability cleanup, then document and test the precise source-lifetime contract exposed by `merge_documents`.

**Architecture:** First consume the existing `flpdf-3yn9.39` prerequisite, which moves xref reachability out of the mixed `subset_prune` module into a writer-owned canonical ObjectHandle walker that never materializes stream payloads. Then keep `flpdf-098i` focused on `MergeInput`/`merge_documents` documentation, the stale immediate-copy rustdoc, and public integration coverage. Do not add a merge-local walker or a Catalog/trailer-only eager-copy branch.

**Tech Stack:** Rust 2021, `ObjectHandle`, `Pdf`, `MergeInput`, writer-owned reachability, stream providers, `PdfWriter`, pinned qpdf 11.9.0 source, Cargo tests, strict rustdoc, Clippy, and patch coverage.

**Spec:** `docs/superpowers/specs/2026-08-25-flpdf-098i-merge-stream-lifetime-design.md`

## Global constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- `flpdf-098i` depends on `flpdf-3yn9.39`; do not implement the same reachability responsibility in both issues.
- qpdf writer reachability follows handles and visible dictionary/array edges without reading stream bytes.
- Keep provider-backed streams lazy by default.
- `Pdf::set_immediate_copy_from(true)` is source-side and must be called before merge/copy.
- Do not eagerly materialize only Catalog/trailer streams.
- Do not add resolver, copier, writer stream-source, or error branches in `flpdf-098i`.
- Use only public APIs from `crates/flpdf/tests/page_merge_tests.rs`.

### Task 1: Consume the canonical reachability prerequisite

**Files:**
- Dependency: `flpdf-3yn9.39`
- Downstream: `crates/flpdf/src/job/page_merge.rs`

- [ ] **Step 1: Verify the prerequisite is available before implementation**

Run:

```bash
bd show flpdf-3yn9.39
bd blocked
```

Expected: `flpdf-3yn9.39` is closed or its writer-owned reachability implementation is available on the branch being stacked. At the current base it is blocked by `flpdf-egzr.3.2.8` and `flpdf-egzr.3.2.6`; do not bypass those blockers or duplicate its work in this issue.

- [ ] **Step 2: After the prerequisite lands, rebase this branch onto its merged/base commit**

Run:

```bash
git fetch origin
git rebase origin/main
cargo test -p flpdf --test page_merge_tests
```

Expected: the existing page-merge suite passes before adding the new provider tests.

### Task 2: Add merge-level provider lifetime tests after the canonical sweep exists

**Files:**
- Modify: `crates/flpdf/tests/page_merge_tests.rs` imports and test helpers
- Test: `crates/flpdf/tests/page_merge_tests.rs`

- [ ] **Step 1: Add public test helpers and tests first**

Add these imports:

```rust
use flpdf::{
    merge_documents, pages, Error, MergeInput, Object, ObjectRef, Pdf, PdfWriter,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
```

Add a helper that opens the existing page fixture, installs an indirect provider-backed stream on the primary Catalog, and returns the provider call counter:

```rust
const PROVIDER_METADATA: &[u8] = b"provider-backed catalog metadata";

fn provider_metadata_source() -> (Pdf<std::io::Cursor<Vec<u8>>>, Rc<Cell<usize>>) {
    let mut source = Pdf::open_mem_owned(three_page_shared_font_pdf()).unwrap();
    let calls = Rc::new(Cell::new(0));
    let calls_for_provider = Rc::clone(&calls);
    let stream = source.new_stream().unwrap();
    stream
        .replace_stream_data_with_callback(
            move |pipeline| {
                calls_for_provider.set(calls_for_provider.get() + 1);
                pipeline.write(PROVIDER_METADATA).map_err(Error::from)?;
                pipeline.finish().map_err(Error::from)
            },
            None,
            None,
        )
        .unwrap();

    let root_ref = source.root_ref().unwrap();
    let root = source.get_object_handle(root_ref);
    source.resolve(&root).unwrap();
    root.replace_key(b"/Metadata", stream).unwrap();
    source.mark_object_handle_dirty(&root).unwrap();
    (source, calls)
}

fn write_merged(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> flpdf::Result<Vec<u8>> {
    let mut writer = PdfWriter::new(pdf);
    writer.set_compress_streams(false);
    writer.set_output_memory()?;
    writer.write()?;
    writer.get_buffer()
}

fn metadata_payload(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> Vec<u8> {
    let root_ref = pdf.root_ref().unwrap();
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root).unwrap();
    root.get_key(b"/Metadata")
        .get_raw_stream_data()
        .unwrap()
        .as_ref()
        .clone()
}
```

Add three tests:

```rust
#[test]
fn merge_provider_catalog_stream_stays_lazy_until_write() {
    let (mut source, calls) = provider_metadata_source();
    let mut merged = {
        let mut inputs = [MergeInput {
            source: &mut source,
            pages: vec![0],
        }];
        merge_documents(&mut inputs).unwrap()
    };

    assert_eq!(calls.get(), 0, "foreign provider must remain lazy during merge");
    let output = write_merged(&mut merged).unwrap();
    assert_eq!(calls.get(), 1, "provider must run when the merged stream is written");

    let mut reopened = Pdf::open_mem_owned(output).unwrap();
    assert_eq!(metadata_payload(&mut reopened), PROVIDER_METADATA);
    drop(source);
}

#[test]
fn merge_provider_catalog_stream_requires_source_until_write_by_default() {
    let (mut source, calls) = provider_metadata_source();
    let mut merged = {
        let mut inputs = [MergeInput {
            source: &mut source,
            pages: vec![0],
        }];
        merge_documents(&mut inputs).unwrap()
    };
    assert_eq!(calls.get(), 0, "default foreign provider must remain lazy");

    drop(source);
    let error = write_merged(&mut merged).expect_err(
        "dropping a provider-backed source before the destination write must retain qpdf's contract error",
    );
    assert!(matches!(
        error,
        Error::Internal(message) if message == "pipeStreamData called for non-stream"
    ));
}

#[test]
fn merge_provider_catalog_stream_with_immediate_copy_survives_source_drop() {
    let (mut source, calls) = provider_metadata_source();
    source.set_immediate_copy_from(true);
    let mut merged = {
        let mut inputs = [MergeInput {
            source: &mut source,
            pages: vec![0],
        }];
        merge_documents(&mut inputs).unwrap()
    };

    assert_eq!(calls.get(), 1, "immediate copy must materialize the provider during merge");
    drop(source);

    let output = write_merged(&mut merged).unwrap();
    let mut reopened = Pdf::open_mem_owned(output).unwrap();
    assert_eq!(metadata_payload(&mut reopened), PROVIDER_METADATA);
}
```

- [ ] **Step 2: Run the focused tests and verify the expected red state**

Run:

```bash
cargo test -p flpdf --test page_merge_tests merge_provider_catalog_stream -- --nocapture
```

Expected before `flpdf-3yn9.39`: the default-lazy and retained-source tests fail because the old `subset_prune` sweep invokes the provider during merge; the immediate-copy test passes. The failure must identify `page_merge.rs:1122` -> `subset_prune.rs:166`/`:229` -> `materialize`. After rebasing onto the prerequisite, rerun the same command and expect all three tests to pass.

- [ ] **Step 3: Commit the merge tests after the prerequisite-backed green run**

```bash
git add crates/flpdf/tests/page_merge_tests.rs
git commit -m "test: cover merge provider stream lifetime contract"
```

### Task 3: Document the precise contract

**Files:**
- Modify: `crates/flpdf/src/job/page_merge.rs:50-57,630-649`
- Modify: `crates/flpdf/src/reader.rs:1478-1487`

- [ ] **Step 1: Document the provider-only source rule on `MergeInput`**

Add to `MergeInput::source`:

```rust
/// A provider-backed stream copied from this source is read lazily. Keep this
/// `Pdf` alive until the returned merged document has written every copied
/// provider-backed stream, or call `Pdf::set_immediate_copy_from(true)` on the
/// source before merging when it must be dropped earlier. Ordinary parsed
/// file-backed streams capture their input source separately and do not require
/// this `Pdf` lifetime.
```

- [ ] **Step 2: Document merge reachability and source lifetime**

Add after the persistent foreign-copy paragraph in `merge_documents`:

```rust
/// Provider-backed streams reachable from selected pages or primary
/// Catalog/trailer values remain lazy through the merge. Their source `Pdf`
/// must remain alive until the merged document writes or otherwise reads the
/// copied stream, matching qpdf's `copyForeignObject` contract
/// (`include/qpdf/QPDF.hh:401-412`; `libqpdf/QPDF.cc:2216-2276`). To release a
/// source earlier, call [`Pdf::set_immediate_copy_from`] with `true` before
/// this merge. That source-side opt-in matches qpdf's
/// `setImmediateCopyFrom` and materializes the stream once at copy time. This
/// rule applies only to provider-backed sources; ordinary file-backed source
/// streams capture their input independently.
```

- [ ] **Step 3: Correct `copy_foreign_object`'s stale immediate-copy statement**

Replace the stale final sentence at `reader.rs:1485-1487` with:

```rust
/// source. flpdf exposes qpdf's source-side escape hatch as
/// [`Self::set_immediate_copy_from`]. Call it with `true` before copying when
/// provider-backed stream data must be materialized so the source `Pdf` need
/// not survive the copy.
```

- [ ] **Step 4: Run focused formatting, docs, and tests**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test page_merge_tests merge_provider_catalog_stream -- --nocapture
cargo test -p flpdf --test page_merge_tests
```

Expected: all three provider tests and all page-merge tests pass with zero failures.

- [ ] **Step 5: Commit the documentation update**

```bash
git add crates/flpdf/src/job/page_merge.rs crates/flpdf/src/reader.rs
git commit -m "docs: state merge provider stream lifetime"
```

### Task 4: Run implementation verification and review gates

**Files:**
- No additional files.

- [ ] **Step 1: Run core quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test -p flpdf
cargo test --workspace
```

Expected: every command exits 0; tests report 0 failures; rustdoc emits no broken/private-link errors; Clippy emits no warnings.

- [ ] **Step 2: Run qpdf and marker checks**

```bash
scripts/qpdf-stream-data-provider-probe.sh
python3 scripts/check-qpdf-deviation-markers.py --check
```

Expected: the provider probe prints `qpdf stream data provider probe: ok`; marker validation exits 0.

- [ ] **Step 3: Inspect the final diff**

```bash
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git status --short
```

Expected: only the approved design/plan documents, the canonical prerequisite integration, the merge docs, and the merge tests are present. No generated files or merge-local reachability duplicate appears.

- [ ] **Step 4: Request code review before handoff**

Provide the reviewer the qpdf citations, the design/plan files, the prerequisite relationship, the red-state backtrace, and the final verification output. Resolve every Critical or Important finding before handoff.
