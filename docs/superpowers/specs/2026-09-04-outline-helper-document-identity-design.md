# Outline helper document-identity guard

**Issue:** `flpdf-e584`

## Goal

Make the live outline accessors reject an `OutlineDocumentHelper` backed by a
different `Pdf` from the one that owns the `OutlineItem` handle. This restores
the document-ownership boundary that qpdf gets from
`QPDFOutlineObjectHelper::Members::dh` being private and fixed at construction.

## Oracle and current gap

In pinned qpdf 11.9.0, `QPDFOutlineObjectHelper` has private constructors and
can only be created by `QPDFOutlineDocumentHelper`
(`include/qpdf/QPDFOutlineObjectHelper.hh:44-45,77-90`). Its `Members` stores
the owning `QPDFOutlineDocumentHelper&` (`:100-106`). `getDest` resolves a name
or string through that stored helper (`libqpdf/QPDFOutlineObjectHelper.cc:47-70`),
while the document helper owns the `/Dests` and `/Names/Dests` lookup state
(`libqpdf/QPDFOutlineDocumentHelper.cc:65-95`). A cross-document helper/item
combination is therefore not representable in qpdf.

flpdf must pass a mutable helper to each accessor because `OutlineItem` is an
arena entry and cannot retain a mutable `Pdf`. The current
`OutlineItem::get_title`, `get_count`, `get_dest`, and `get_dest_page` accept
any helper and do not verify that its `Pdf` owns `OutlineItem::object`.

## Design

`OutlineDocumentHelper` already borrows the owning `Pdf`, and the canonical
`ObjectHandle` already carries the ownership information needed for both
indirect handles and direct children. Add one crate-private helper method:

```rust
fn ensure_handle_belongs_to_pdf(&self, handle: &ObjectHandle) -> Result<()>
```

It returns `Ok(())` when `handle.belongs_to_pdf(self.pdf.unique_id())` is true;
otherwise it returns the existing
`Error::Unsupported("ObjectHandle belongs to another Pdf")` contract used by
`Pdf::mark_object_handle_dirty`.

Call this guard before reading `OutlineItem::object` in `get_title`,
`get_count`, and `get_dest`. `get_dest_page` continues to delegate to
`get_dest`, so it receives the same check without a second ownership path.
Unowned detached direct handles remain accepted according to the existing
`belongs_to_pdf` semantics; no sentinel or new bridge is introduced.

The guard is a Rust API safety adaptation, not a new qpdf behavior. It does not
change live recomputation, named-destination cache behavior, qpdf warning
ordering, the existing synthetic `Pdf::set_object` terminal-handle chase, or
any consumer route.

## Testing

Add a real regression test using two separately opened `Pdf` values from the
same outline fixture. The source tree's item is queried with a helper backed by
the second PDF and each accessor is asserted to return
`Error::Unsupported("ObjectHandle belongs to another Pdf")`; the named
destination case specifically proves that the foreign catalog cannot be used.
Existing same-document outline tests remain the compatibility checks for title,
count, explicit destinations, and destination pages.

The RED run must fail because the current implementation accepts the foreign
helper and resolves the named destination. After the guard is added, the same
test must pass, followed by the focused outline tests, workspace tests, strict
Rustdoc, all-features Clippy, qpdf module-doc checks, and changed-line patch
coverage.

## Non-goals

- Do not redesign `OutlineItem` lifetimes or store a mutable `Pdf` in the tree.
- Do not modify named-destination resolution or its caches.
- Do not add a legacy compatibility adapter, synthetic identity sentinel, or
  qpdf-deviation marker.
- Do not change unrelated outline consumers or merge/linearization behavior.
