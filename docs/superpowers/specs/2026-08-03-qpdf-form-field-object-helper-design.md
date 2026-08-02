# QPDFFormFieldObjectHelper boundary design

## Goal

Make `form_field_object_helper.rs` the sole flpdf implementation boundary for
qpdf 11.9.0 `QPDFFormFieldObjectHelper`. Move form-field reading, mutation,
and appearance-generation responsibilities out of `annotation_helper.rs`,
`appearance.rs`, and their callers. This implements Bead `flpdf-ceun` (Tier
A1) and unblocks Tier A2 and Tier D1.

## Oracle and scope

The oracle is pinned qpdf 11.9.0:

- `include/qpdf/QPDFFormFieldObjectHelper.hh` declares the public contract.
- `libqpdf/QPDFFormFieldObjectHelper.cc` defines inheritance, typed access,
  `/NeedAppearances`, button-value, and text/choice appearance semantics.

The helper owns all public qpdf methods declared in that header: parent and
top-level traversal; inheritable values; names, values, defaults, resources,
quadding, and flags; type predicates and choices; field mutation; and
appearance generation. `setRadioButtonValue`, `setCheckBoxValue`, font lookup,
and text appearance generation remain private implementation details.

`AnnotationObjectHelper` keeps only annotation-specific operations such as
`/Subtype`, `/Rect`, `/AP`, and `/A`. `default_appearance.rs` remains a pure
`/DA` parser; it does not resolve field inheritance. Low-level content and
stream rendering helpers may remain in `appearance.rs`, but no public
form-field API remains there.

## API and ownership

`FormFieldObjectHelper` is reintroduced in the new module around the canonical
`ObjectHandle`/`Pdf` boundary rather than a copied dictionary. Typed accessors
must dereference indirect values, matching qpdf. In particular, `/FT`, `/T`,
`/Ff`, `/TU`, and `/TM` remain observable through indirect references.

The Rust surface uses explicit `Result` for document resolution failures and
byte-oriented values where PDF name/text-string representation matters. It does
not add a compatibility adapter for the existing `annotation_helper` helper.
Consumers move to the new helper and the old implementation is deleted.

Field-tree traversal follows qpdf's `/Parent` order with a cycle and depth
guard appropriate for malformed PDFs. qpdf's field type is a PDF name and is
kept as such: consumers must not silently change `/Tx` to `Tx`.

## Mutation and appearance behavior

`set_field_attribute` and `set_value` implement qpdf's ownership and mutation
semantics at the helper boundary. `set_value` applies qpdf's `/NeedAppearances`
behavior for text and choice fields, and dispatches button values through the
qpdf-derived checkbox/radio paths.

`generate_appearance` is owned by the form-field helper. Its low-level drawing
may call the existing pure parser and rendering helpers, but it resolves
inherited `/DA`, `/Q`, `/Ff`, values, and document `/AcroForm` resources through
the helper rather than through duplicate call-site walkers.

## Migration order

1. Inventory every qpdf-header method, existing public or crate-private
   form-field symbol, re-export, and call site in library, CLI, and tests.
2. Add source-derived regression/probe coverage for ambiguous qpdf behavior.
3. Create the new helper and migrate read-only accessors first.
4. Move mutation and appearance-generation entry points, then switch all
   consumers.
5. Remove superseded form-field code from `annotation_helper.rs` and public
   form-field entry points from `appearance.rs`; retain only annotation and
   low-level rendering primitives in those modules.

## Verification

Each behavior change starts RED and is resolved against qpdf source and, where
needed, a smallest real-PDF qpdf probe. Focused helper, appearance, JSON, and
CLI regressions cover direct and indirect field values, inheritance, malformed
parent chains, `/NeedAppearances`, button values, and generated appearances.

The final change must pass formatting, workspace clippy with all features,
relevant focused suites, workspace tests, and per-PR changed-line coverage at
100 percent. Completion additionally requires mechanical proof that the old
form-field implementation was removed rather than retained as an adapter.
