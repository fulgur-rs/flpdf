# flpdf-098i Merge Stream Lifetime Design

## Goal

Preserve qpdf's lazy provider-backed stream behavior through
`merge_documents`, including its pre-writer reachability cleanup, and expose
the resulting source-lifetime contract in the public API documentation.

## qpdf contract

qpdf's `QPDF::copyForeignObject` copies stream dictionaries and installs a
lazy stream-data provider. A provider-backed copied stream calls back into the
foreign stream when the destination is written, so the source `QPDF` must stay
alive until that read. This is distinct from an original file-backed stream:
qpdf captures the input source, encryption state, object identity, offset, and
length in `ForeignStreamData`, so the source `QPDF` itself may be destroyed.

`QPDF::setImmediateCopyFrom(true)` is the source-side opt-in for callers that
need to release a source early. It materializes each lazy stream the first time
it is copied and shares the resulting buffer with the destination. The flag
must be set before the source's streams are copied.

qpdf's writer reachability walk follows indirect object and dictionary/array
references without reading an indirect stream's payload. Stream data is
obtained only at the stream-writing boundary. The pinned qpdf sources are:

- `include/qpdf/QPDF.hh:401-432`
- `libqpdf/QPDF.cc:126-163,2019-2097,2216-2276`
- `libqpdf/QPDF_Stream.cc:571-622,640-660`
- `libqpdf/QPDFWriter.cc:1072-1141,1240-1315,1488-1517,2907-2925`
- `libqpdf/QPDFJob.cc:465-480,514-519,2360-2432`
- `qpdf/test_driver.cc:1003-1075`

The qpdf job does not retain its local secondary-QPDF heap through the final
writer call. Normal file-backed copies remain valid because their input source
is captured separately. The provider lifetime rule comes from the generic
foreign-copy API and provider source, not from retaining every source QPDF in
`--pages`.

## flpdf behavior and boundary

`job/page_merge.rs` copies primary Catalog and trailer values through
`Pdf::copy_foreign_value`, which shares the canonical `ForeignObjectCopier`
stream path with selected-page copying. A provider-backed primary `/Metadata`
or other Catalog/trailer stream therefore remains lazy and depends on the
source `Pdf` when it is later read.

The first merge-level regression test showed that, before the `flpdf-3yn9.39`
writer-reachability split landed, the implementation called the provider once
before `merge_documents` returned. That call path was:

```text
page_merge.rs:1122
  -> subset_prune::sweep_unreachable_objects_except
  -> subset_prune.rs:166 trailer().materialize()
  -> subset_prune.rs:229 resolve_borrowed()
  -> ObjectHandle::materialize()
  -> CopiedStreamDataProvider
```

This is not a `copy_foreign_value` behavior and must not be fixed with a
Catalog-only branch. qpdf's corresponding reachability responsibility belongs
to the writer. The existing `flpdf-3yn9.39` issue owns moving
`sweep_unreachable_objects`, `sweep_unreachable_objects_except`,
`collect_reachable`, and `walk_refs` to a writer-owned canonical
ObjectHandle reachability module. `flpdf-098i` depends on that responsibility
unit and only owns the merge contract/tests and public documentation.

The returned `Pdf<Cursor<Vec<u8>>>` cannot own borrowed `MergeInput` sources.
The public contract will state the precise rule:

- Keep a source `Pdf` alive until the merged document has written every copied
  provider-backed stream, or
- call `Pdf::set_immediate_copy_from(true)` on that source before merging when
  early release is required.

The default lazy behavior stays unchanged. Ordinary parsed file-backed streams
capture their input source separately and do not require the source `Pdf`
lifetime.

## Tests

After the canonical writer-owned reachability prerequisite lands, add
merge-level coverage using a real indirect provider-backed stream attached to
the primary Catalog:

1. The provider is not called during merge cleanup.
2. Keeping the source alive through the write succeeds and preserves bytes.
3. Dropping the source before the write reports the existing
   `Error::Internal("pipeStreamData called for non-stream")` contract error.
4. Setting `set_immediate_copy_from(true)` before merge, then dropping the
   source before writing, succeeds and preserves bytes.

The tests must use public provider, merge, and writer APIs. They must not
inspect or mutate private resolver state. Existing foreign-copy,
original-file-source, and page-merge metadata tests remain green.

## Files and ownership

- `flpdf-3yn9.39` owns the prerequisite migration from `subset_prune` to the
  writer-owned canonical reachability boundary. No duplicate reachability
  implementation belongs in this issue.
- Modify `crates/flpdf/src/job/page_merge.rs` to document the provider-only
  source lifetime rule on `MergeInput` and `merge_documents`.
- Modify `crates/flpdf/src/reader.rs` to remove the stale claim that
  `setImmediateCopyFrom` has no flpdf counterpart.
- Modify `crates/flpdf/tests/page_merge_tests.rs` for merge-level provider
  lifetime and deferred-call coverage.

No resolver, foreign copier, writer stream-source, or Catalog-only eager-copy
behavior changes are planned in `flpdf-098i`.
