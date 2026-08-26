# AcroForm Document Handle Resolution Design

## Goal

Remove the active AcroForm document helper's raw snapshot resolution for the
Catalog, `/AcroForm`, `/Fields`, and field dictionaries. The helper will keep
the qpdf-shaped `ObjectHandle` identity through field traversal, inherited
value lookup, and `/AcroForm/DA` mutation.

This is the bounded implementation slice `flpdf-egzr.3.2.8.16`. It does not
close the parent legacy-route aggregate or migrate the separate foreign graph
copy helper used by the overlay legacy bridge.

## qpdf oracle

Pinned qpdf 11.9.0 defines the relevant responsibilities as follows:

- `QPDFObjectHandle::isDictionary` dereferences the handle before testing its
  type (`libqpdf/QPDFObjectHandle.cc:432-435`).
- `QPDFObjectHandle::getKey` performs the handle's lazy dereference and returns
  qpdf's null/type-warning behavior for dictionary lookup
  (`libqpdf/QPDFObjectHandle.cc:978-988`; declaration and key contract in
  `include/qpdf/QPDFObjectHandle.hh:762-780`).
- `QPDFAcroFormDocumentHelper::analyze` obtains `/AcroForm` through
  `getKey`, stops when it is not a dictionary or has no `/Fields`, and warns
  only when `/Fields` is not an array
  (`libqpdf/QPDFAcroFormDocumentHelper.cc:234-249`).
- `QPDFFormFieldObjectHelper` stores a live `QPDFObjectHandle` and resolves
  parent/inheritable values from that handle (`libqpdf/QPDFFormFieldObjectHelper.cc:11-85`).

The live `/usr/bin/qpdf` 11.9.0 probe was run with
`qpdf --json --json-key=acroform` against both a direct non-dictionary
`/AcroForm` and an indirect reference to a non-dictionary. Both produced an
empty `fields` array and `needappearances: false` (only the expected damaged
input reconstruction warnings were emitted).

## Existing flpdf route

The existing canonical AcroForm association cache and `FormFieldObjectHelper`
already retain live handles. The remaining active raw snapshot boundary is in
`crates/flpdf/src/acroform_document_helper.rs`:

- `acroform_dict` resolves a raw `Dictionary` and clones an indirect target.
- `resolve_dict` resolves a raw `Object` and clones its `Dictionary`.
- `fields`, `field_infos`, `top_level_fields`, and `has_fields_array` consume
  those raw dictionaries.
- `set_default_appearance` mutates a raw dictionary and writes it through
  `Pdf::set_object`.
- `FieldInheritance` and `resolve_array_value` carry the same raw values into
  the field-info snapshot.

The raw `collect_reachable_refs` / `collect_refs_in_object` foreign graph-copy
helper remains outside this slice; its overlay legacy caller is owned by
`flpdf-3yn9.37`. The existing `resolve_to_terminal` bare-reference
compensation is also not broadened or reinterpreted here.

## Design

1. Change the active AcroForm dictionary boundary to return a resolved
   `ObjectHandle`, using `Pdf::get_object_handle`, `Pdf::resolve`,
   `try_get_key`, and `try_as_dictionary`. A non-dictionary `/AcroForm`
   remains absent, while a field/catalog reference that is known to be a
   dictionary still returns the existing `Unsupported` error with its label.
2. Migrate the field and array walks to vectors of `ObjectHandle`, preserving
   qpdf's one-hop/terminal behavior already represented by the existing
   canonical helper and preserving field order, cycle/depth guards, and
   malformed-value behavior.
3. Make `AcroFormFieldInfo`'s resolved value fields (`value`, `default_value`,
   and `default_appearance`) `Option<ObjectHandle>`. This matches qpdf's
   handle-returning field accessors and prevents a new ObjectHandle-to-Object
   materialization bridge. Update the example and tests to inspect handle
   payloads rather than comparing raw snapshots.
4. Rewrite `set_default_appearance` to replace `/DA` on the live AcroForm
   handle and mark that handle dirty. Do not rebuild or write a cloned raw
   dictionary.
5. Leave `collect_reachable_refs`, overlay legacy graph copying, public raw
   `Object` APIs outside this helper, and canonical naming-prefix cleanup out
   of scope.

## Acceptance behavior

- Direct and indirect `/AcroForm` dictionaries yield the same field order and
  inherited values.
- Direct and indirect non-dictionary `/AcroForm` values remain absent without
  inventing an error.
- Non-dictionary Catalog/field objects retain the existing labeled
  `Unsupported` error behavior.
- Indirect `/Fields`, `/Kids`, `/T`, `/V`, `/DV`, `/DA`, `/Q`, and `/MaxLen`
  retain their qpdf-compatible resolution and null handling.
- `/AcroForm/DA` mutation changes the live handle and write-back dirty state,
  without stamping fields or losing an indirect `/AcroForm` identity.

## Verification

Run the focused AcroForm and helper API suites, route-contract tests, qpdf
11.9.0 probes/differentials, formatting, strict private rustdoc, all-features
Clippy, workspace tests, qpdf module/deviation checks, and fresh parent-relative
100% patch coverage before publishing a Draft PR.
