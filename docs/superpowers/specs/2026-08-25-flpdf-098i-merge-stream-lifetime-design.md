# flpdf-098i Merge Stream Lifetime Design

## Goal

Make the public `merge_documents` contract and tests accurately expose qpdf's
lazy provider-backed foreign-stream lifetime behavior, without adding a
Catalog/trailer-only eager-copy special case.

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

The pinned qpdf sources are:

- `include/qpdf/QPDF.hh:401-432`
- `libqpdf/QPDF.cc:126-163,2019-2097,2216-2276`
- `libqpdf/QPDF_Stream.cc:571-622,640-660`
- `libqpdf/QPDFJob.cc:465-480,514-519,2360-2432`
- `qpdf/test_driver.cc:1003-1075`

The qpdf job does not retain its local secondary-QPDF heap through the final
writer call. Normal file-backed copies remain valid because their input source
is captured separately. The provider lifetime rule comes from the generic
foreign-copy API and provider source, not from a claim that `--pages` retains
all source QPDF objects through writing.

## flpdf behavior and boundary

`job/page_merge.rs` copies primary Catalog and trailer values through
`Pdf::copy_foreign_value`, which shares the canonical `ForeignObjectCopier`
stream path with selected-page copying. Therefore a provider-backed primary
`/Metadata` or other Catalog/trailer stream remains lazy and depends on the
source `Pdf` during a later write.

The returned `Pdf<Cursor<Vec<u8>>>` cannot own borrowed `MergeInput` sources.
The public contract will state the precise rule:

- Keep a source `Pdf` alive until the merged document has written every copied
  provider-backed stream, or
- call `Pdf::set_immediate_copy_from(true)` on that source before merging when
  early release is required.

The default lazy behavior stays unchanged. We will not materialize only
Catalog/trailer streams. That policy has no qpdf counterpart and would create
an observable special case in provider invocation timing, memory use, and
error timing.

The existing `reader.rs` rustdoc sentence claiming that flpdf lacks the
`setImmediateCopyFrom` counterpart will be corrected because the counterpart
already exists as `Pdf::set_immediate_copy_from`.

## Tests

Add merge-level coverage using a real indirect provider-backed stream attached
to the primary Catalog:

1. The default lazy path remains deferred until the copied stream is read.
2. Keeping the source alive through the write succeeds and preserves bytes.
3. Dropping the source before the write reports the existing
   `Error::Internal("pipeStreamData called for non-stream")` failure rather
   than silently omitting data.
4. Setting `set_immediate_copy_from(true)` before merge, then dropping the
   source before writing, succeeds and preserves bytes.

The test must use the public provider and merge APIs. It must not inspect or
mutate private resolver state. Existing foreign-copy, original-file-source,
and page-merge metadata tests remain unchanged except for shared helpers if a
small, behavior-neutral extraction is needed.

## Files

- Modify `crates/flpdf/src/job/page_merge.rs` to document the source lifetime
  rule on `MergeInput`/`merge_documents` and add focused unit coverage if the
  public provider setup is suitable there.
- Modify `crates/flpdf/src/reader.rs` to remove the stale claim that
  `setImmediateCopyFrom` has no flpdf counterpart.
- Modify `crates/flpdf/tests/page_merge_tests.rs` for public integration tests
  covering default lazy, retained-source, dropped-source, and immediate-copy
  behavior.

No resolver, copier, writer, or stream-source behavior changes are planned.
