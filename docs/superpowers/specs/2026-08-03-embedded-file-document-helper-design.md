# EmbeddedFileDocumentHelper design

## Goal

Complete the public Rust boundary corresponding to qpdf 11.9.0's
`QPDFEmbeddedFileDocumentHelper`, without moving the JSON attachment consumer.
The later Job-layer issue (`flpdf-q2fo`) remains responsible for replacing
`json_inspect::build_attachments_section`'s direct name-tree traversal.

## Public boundary

Add `EmbeddedFileDocumentHelper<'a, R>`, constructed from `&'a mut Pdf<R>`.
It has qpdf-shaped methods:

- `has_embedded_files() -> Result<bool>`
- `get_embedded_files() -> Result<BTreeMap<Vec<u8>, ObjectHandle>>`
- `get_embedded_file(key: &[u8]) -> Result<Option<ObjectHandle>>`
- `replace_embedded_file(key: &[u8], filespec: ObjectHandle) -> Result<()>`
- `remove_embedded_file(key: &[u8]) -> Result<bool>`

`Pdf::embedded_files()` constructs the helper. `lib.rs` re-exports it.

`ObjectHandle` is the Rust equivalent of qpdf's returned
`shared_ptr<QPDFFileSpecObjectHelper>` at this ownership boundary. Existing
`FileSpec<'a, R>` owns the document's mutable borrow, so a collection of
`FileSpec` values cannot be returned safely. Consumers take one returned handle
at a time and construct `FileSpec::new(handle, pdf)` after dropping the document
helper. The ordered `BTreeMap` retains qpdf's map semantics and represents both
indirect and direct Filespec values.

## Behavior

- `has_embedded_files` is true only when `/Root /Names /EmbeddedFiles` resolves
  to a dictionary/name-tree root, matching qpdf's cached-construction test.
- Lookup and enumeration use the existing `NameTree` walker and preserve direct
  values as `ObjectHandle`s; malformed absent catalog paths produce empty/false
  results rather than allocating state.
- `replace_embedded_file` initializes `/Names` and an empty embedded-files name
  tree when absent, then inserts or replaces the key. It accepts a direct handle
  or a canonical handle owned by this `Pdf`; foreign indirect handles fail before
  mutating the tree.
- `remove_embedded_file` returns false for an absent tree or key. For a present
  indirect filespec it removes the name-tree entry and replaces that Filespec
  object with `null`, matching qpdf. It does not run flpdf's broader attachment
  cleanup/GC policy; that remains `remove_attachment`'s distinct API.

## qpdf fidelity and scope

The source oracle is qpdf 11.9.0:
`QPDFEmbeddedFileDocumentHelper.hh:45-65` and
`QPDFEmbeddedFileDocumentHelper.cc:48-121`.

The helper preserves qpdf's object topology: a direct `/Names` dictionary
stays direct, and removing the final name-tree item leaves an empty
`/EmbeddedFiles` tree in place. It constructs `NameTree` with repair enabled,
matching qpdf's default `QPDFNameTreeObjectHelper` mode. There is no embedded
files-specific numeric depth cap: qpdf detects cycles structurally rather than
rejecting an otherwise valid deep tree.

The legacy free functions are not a compatibility boundary for this work.
`delete_embedded_file` adopts the same name-tree removal semantics. The
broader `remove_attachment` operation remains separate because its `/AF`
cleanup and reachability sweep are deliberately beyond qpdf's
`removeEmbeddedFile` responsibility.

## Tests

Add public-API integration coverage for: empty/absent trees; sorted enumeration
and single lookup; replace creation and replacement; remove absent/existing;
indirect Filespec nulling; direct Filespec removal without object nulling; and
foreign-handle rejection without mutation. Run the focused integration suite,
the Filespec suite, formatting, workspace quality gates, and fresh changed-line
coverage before delivery.
