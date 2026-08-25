# Merge Stream Lifetime Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document and test the qpdf-compatible lifetime contract for provider-backed Catalog/trailer streams copied by `merge_documents`, including the existing `set_immediate_copy_from` escape hatch.

**Architecture:** Keep `ForeignObjectCopier`, `ResolverHandle::copy_stream_data`, and the writer unchanged. Add public API documentation at the `MergeInput`/`merge_documents` boundary, correct the stale `copy_foreign_object` rustdoc, and add integration tests that construct a real provider-backed `/Metadata` stream through public APIs.

**Tech Stack:** Rust 2021, `Pdf`, `MergeInput`, `ObjectHandle` stream providers, `PdfWriter`, pinned qpdf 11.9.0 source, Cargo tests, rustdoc, Clippy, and patch coverage.

**Spec:** `docs/superpowers/specs/2026-08-25-flpdf-098i-merge-stream-lifetime-design.md`

## Global constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- Keep provider-backed streams lazy by default.
- `Pdf::set_immediate_copy_from(true)` is source-side and must be called before merge/copy.
- Do not eagerly materialize only Catalog/trailer streams.
- Do not add resolver, copier, writer, ownership, or error branches.
- Use only public APIs from `crates/flpdf/tests/page_merge_tests.rs`.
- Existing page-merge and foreign-copy behavior must remain unchanged.

### Task 1: Add merge-level provider lifetime tests

**Files:**
- Modify: `crates/flpdf/tests/page_merge_tests.rs` imports and test helpers
- Test: `crates/flpdf/tests/page_merge_tests.rs`

- [ ] **Step 1: Add public test helpers and the failing/behavior-locking tests before documentation edits**

Add these imports:

```rust
use flpdf::{
    merge_documents, pages, Error, MergeInput, Object, ObjectHandle, ObjectRef, Pdf, PdfWriter,
    Pipeline,
};
use std::cell::Cell;
use std::rc::Rc;
```

Add a helper that opens the existing one-page fixture, installs an indirect provider-backed stream on the primary Catalog, and returns the provider call counter:

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

Add three integration tests immediately after the existing primary Catalog/trailer metadata test:

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

- [ ] **Step 2: Run the focused tests before changing production documentation**

Run:

```bash
cargo test -p flpdf --test page_merge_tests merge_provider_catalog_stream -- --exact --nocapture
```

Expected: the three tests compile and pass on the existing canonical copier. This confirms that the requested behavior is already implemented and that this slice needs no production behavior change. If a test fails, fix only the test setup until it exercises the intended public boundary; do not weaken the assertions.

- [ ] **Step 3: Commit the test-only behavior coverage**

```bash
git add crates/flpdf/tests/page_merge_tests.rs
git commit -m "test: cover merge provider stream lifetime contract"
```

### Task 2: Document the public lifetime contract and correct stale rustdoc

**Files:**
- Modify: `crates/flpdf/src/job/page_merge.rs:50-57,630-649`
- Modify: `crates/flpdf/src/reader.rs:1478-1487`

- [ ] **Step 1: Document the precise provider-only source rule on `MergeInput` and `merge_documents`**

Extend the `MergeInput::source` documentation with:

```rust
/// A provider-backed stream copied from this source is read lazily. Keep this
/// `Pdf` alive until the returned merged document has written every copied
/// provider-backed stream, or call `Pdf::set_immediate_copy_from(true)` on the
/// source before merging when it must be dropped earlier. Ordinary parsed
/// file-backed streams capture their input source separately and do not require
/// this `Pdf` lifetime.
```

Add the same rule to the `merge_documents` rustdoc after the paragraph describing the persistent foreign-copy route, including qpdf citations:

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

- [ ] **Step 2: Correct `copy_foreign_object`'s stale immediate-copy statement**

Replace the final sentence at `reader.rs:1485-1487`:

```rust
/// source. qpdf's escape hatch, `setImmediateCopyFrom` (materializing
/// provider-backed stream data into memory at copy time so the source
/// need not survive), has no flpdf counterpart yet.
```

with:

```rust
/// source. flpdf exposes qpdf's source-side escape hatch as
/// [`Self::set_immediate_copy_from`]. Call it with `true` before copying when
/// provider-backed stream data must be materialized so the source `Pdf` need
/// not survive the copy.
```

- [ ] **Step 3: Run rustfmt and the focused tests**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test page_merge_tests merge_provider_catalog_stream -- --exact --nocapture
cargo test -p flpdf --test page_merge_tests
```

Expected: formatting exits 0; all three new tests pass; all existing page-merge tests pass with 0 failures.

- [ ] **Step 4: Commit the documentation update**

```bash
git add crates/flpdf/src/job/page_merge.rs crates/flpdf/src/reader.rs
git commit -m "docs: state merge provider stream lifetime"
```

### Task 3: Run implementation verification and review gates

**Files:**
- No additional files.

- [ ] **Step 1: Run the core quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test -p flpdf
cargo test --workspace
```

Expected: every command exits 0; tests report 0 failures; rustdoc emits no broken or private-link errors; Clippy emits no warnings.

- [ ] **Step 2: Run qpdf/documentation checks**

Run:

```bash
scripts/qpdf-stream-data-provider-probe.sh
python3 scripts/check-qpdf-deviation-markers.py --check
```

Expected: the provider probe prints `qpdf stream data provider probe: ok`; deviation-marker validation exits 0.

- [ ] **Step 3: Inspect the final diff and status**

Run:

```bash
git diff --check HEAD~2..HEAD
git diff --stat HEAD~2..HEAD
git status --short
```

Expected: only the design/plan commits and the intended three source/test files are present; no generated files or unrelated changes appear.

- [ ] **Step 4: Request code review before handoff**

Provide the reviewer the two implementation commits, the qpdf contract, the spec, and the verification output. Resolve any Critical or Important findings before handoff.
