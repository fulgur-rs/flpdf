# Parser Description Template Sharing Design

## Status

Approved implementation scope for `flpdf-ymuj.7`.

## Goal

Remove the per-value byte duplication of qpdf's parser-owned object-description
template so that a single parse call owns one template payload and every value
created by that parse call shares it. This is a memory-convergence slice for
the parent RSS goal; it does not claim that this slice alone reaches qpdf RSS.

## Evidence and oracle

The pinned qpdf tree is `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` at commit
`3b97c9bd266b7c32ea36d3536e22dab77412886d`; `/usr/bin/qpdf` is version 11.9.0.

qpdf's `QPDFParser` constructor creates one
`std::shared_ptr<QPDFValue::Description>` for a parse call
(`libqpdf/qpdf/QPDFParser.hh:14-27,78`). `addInt`, `addScalar`, `withDescription`,
and `setDescription` pass that same pointer to every constructed value
(`libqpdf/QPDFParser.cc:394-444`). `QPDFValue::setDescription` stores the shared
description and the set-once parsed offset
(`libqpdf/qpdf/QPDFValue.hh:60-66,90-100`); rendering is performed later by
`QPDFValue::getDescription` (`libqpdf/QPDFValue.cc:14-60`).

The current flpdf route has the same observable description semantics, but its
ownership differs: `HandleResolver::description_template` returns a cloned
`Vec<u8>`, `ChildHandles` and `DetachedHandles` own `Vec<u8>` templates, and
`ObjectDescription::Template(Vec<u8>)` is copied again by
`ObjectHandle::set_description`. The correspondence row for
`QPDFObject.cc / QPDFValue.cc` records this value-state boundary in
`docs/qpdf-correspondence.md`.

## Proposed representation

Keep one canonical `ObjectDescription` enum and change only the template
variant to a thin shared owner:

```rust
enum ObjectDescription {
    Template(Rc<Vec<u8>>),
    Json(Box<JsonDescription>),
    Child(Box<ChildDescription>),
}
```

The parser boundary creates one `Rc<Vec<u8>>` per parse call. The
`HandleResolver` contract returns an `Rc` clone, and `direct_handle_at` passes
that owner to a setter that clones only the `Rc`, not the byte payload.
Programmatic `set_description` callers continue to create an independent
template owner from their supplied bytes. JSON and child descriptions remain
separate allocations and retain their existing behavior.

`Rc` is intentional: the canonical object graph is single-threaded and already
uses `Rc` for qpdf-shaped value/slot ownership. `Arc`, global interning, a
resolver-owned description arena, and numeric template IDs would add a new
ownership or lifetime concept that qpdf does not have and are outside this
slice.

## Invariants

- All non-null values created by one parser call share the same template
  payload; independent parser calls do not share mutable storage.
- `$PO`, `$OG`, and `$VD` rendering, repeated/unknown marker behavior, raw
  non-UTF-8 bytes, child-parent descriptions, and JSON descriptions are
  unchanged.
- Parsed offsets remain set-once. Array/dictionary render-time shifts remain
  exactly as they are today.
- Explicit contextless parsing remains contextless; live parser values retain
  their existing weak resolver and warning sink.
- Alias assignment, replacement, disconnect, teardown, stream values,
  encryption, and writer output retain their existing behavior.
- The common value layout and direct scalar allocation evidence do not regress;
  the parser template allocation must be constant with respect to value count.

## Implementation boundaries

Production changes are limited to the description ownership seam:

- `crates/flpdf/src/object_handle.rs`: shared template variant, shared setter,
  rendering access, and test-only identity observation if needed.
- `crates/flpdf/src/parser.rs`: `HandleResolver` return type and detached/
  offset forwarding without byte cloning.
- `crates/flpdf/src/reader/resolver.rs`: `ChildHandles` ownership and parser
  construction of the shared template.
- Existing description tests and allocation tests: assert semantics and the
  ownership/allocation slope.
- `docs/qpdf-correspondence.md` and source module docs: record the shared
  template ownership correspondence if the existing row needs clarification.

No changes are planned to `StateOwners`, containment edges, source extents,
xref maps, writer buffering, linearization, legacy `Object`, or materialization
routes.

## Test-first design

The first implementation test must fail on the current `Vec<u8>` ownership
shape. It will exercise the real parser and inspect shared-template identity or
the equivalent allocation invariant, not a mock. The RED test will cover a
nested value graph and verify that values have distinct parsed offsets but one
description payload. The GREEN implementation will then run the focused
description and parser suites before any refactor.

Regression coverage will retain the existing tests for:

- `$PO`/`$OG` replacement and repeated markers;
- JSON and child descriptions;
- parser warning/error context and explicit contextless parsing;
- parsed-offset set-once behavior;
- replacement, disconnect, alias, stream, and encryption paths.

## Performance acceptance

Using identical qpdf/flpdf workloads, report before/after wall time, maximum
RSS, allocation count, allocated bytes, and peak live heap for pages/content/
objects at 1000 and 5000 values. The parser template payload allocation must
not grow linearly with value count. Re-run the established stream case to show
that this change does not regress stream-heavy behavior. The parent operational
gate remains per-case RSS <= qpdf * 1.10, with 1.00x as the convergence target.

## Alternatives rejected

1. Global byte-content interning: retains templates beyond their parser call,
   changes lifetime/ownership, and has no qpdf counterpart.
2. Moving description bytes into resolver/cache state: makes a value's qpdf
   description no longer value-owned and complicates direct/contextless values.
3. Keeping `Vec<u8>` and deduplicating only benchmark inputs: would preserve
   the production ownership divergence and optimize a test representation.

## Verification gates

Before PR creation, run the focused RED/GREEN tests, `cargo fmt --all -- --check`,
the workspace all-features test suite, all-features clippy with repository
allowances, strict private rustdoc matching CI, qpdf correspondence/deviation/
route checkers, and fresh `scripts/patch-coverage.sh --base origin/main`.
Run the same qpdf/flpdf performance matrix and retain the artifacts in the PR
description. CI must be green before marking the PR Ready; merge remains with
the integration session.
