# OutlineDocumentHelper Exact qpdf Parity Design

**Epic:** `flpdf-9hc.38`

**Oracle:** qpdf 11.9.0, especially:

- `/tmp/qpdf-1190/libqpdf/QPDFOutlineDocumentHelper.cc`
- `/tmp/qpdf-1190/libqpdf/QPDFOutlineObjectHelper.cc`
- `/tmp/qpdf-1190/include/qpdf/QPDFOutlineDocumentHelper.hh`
- `/tmp/qpdf-1190/include/qpdf/QPDFOutlineObjectHelper.hh`
- `/tmp/qpdf-1190/libqpdf/QPDFObjGen.cc`
- `/tmp/qpdf-1190/include/qpdf/QPDFObjGen.hh`

## Goal

Make flpdf's outline helper reproduce the observable behavior of qpdf 11.9.0.
This replaces the typed, arbitrary-depth, validation-and-repair design inherited
from closed epic `flpdf-9hc.14`.

Parity applies to traversal order, accepted object shapes, depth truncation,
destination resolution, scalar decoding, page indexing, and JSON output. It
does not require a mechanical translation of qpdf's C++ API. Idiomatic Rust
types and zero-policy adapters are allowed when they preserve qpdf behavior.

This is an intentional pre-1.0 breaking change.

## Scope

The epic contains this dependency stack:

1. `flpdf-9hc.38.1`: remove qpdf-incompatible outline policy and APIs;
2. `flpdf-x5yi`: support direct and mixed outline values;
3. `flpdf-3g9k`: reproduce qpdf's depth-50 silent truncation;
4. `flpdf-guru`: reproduce title decoding and count conversion;
5. `flpdf-7nu4`: add qpdf-compatible page-to-outline lookup;
6. `flpdf-9hc.38.2`: reproduce qpdf JSON v2 outline output.

PageLabels JSON remains in `flpdf-q28i`. Global reader treatment of malformed
top-level bare-reference objects is outside this outline-helper epic. The
outline helper must not add holder-chain traversal that qpdf does not perform.

## Compatibility Boundary

The implementation must remove policy that has no qpdf counterpart or produces
different results:

- normalized `Dest` enumeration through `legacy_dests` and `name_tree_dests`;
- named-destination, outline-link, and structure-element diagnostic checkers;
- outline `/SE` pruning;
- caller-configurable traversal limits that return an error;
- typed fields whose values encode flpdf-only interpretation rather than a raw
  qpdf-equivalent object view.

Thin Rust adapters such as iterators may remain if they only expose the already
materialized qpdf-equivalent tree. They must not perform additional resolution,
normalization, validation, pruning, or repair.

Ordinary parsing and rewriting continue to preserve raw outline dictionary keys,
including `/SE`. Removing a typed accessor or pruning API must not cause the
writer to delete the underlying PDF data.

## Rust Public Model

Use an arena-backed tree so direct objects can have parents and children without
self-referential Rust values or a public `0 0 R` sentinel:

```rust
pub struct OutlineTree {
    items: Vec<OutlineItem>,
    roots: Vec<OutlineId>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlineId(usize);

pub struct OutlineItem {
    pub source_ref: Option<ObjectRef>,
    pub parent: Option<OutlineId>,
    pub kids: Vec<OutlineId>,
    pub object: Object,
    pub title: String,
    pub count: i32,
    pub dest: Object,
}
```

`OutlineId` is valid only within its owning `OutlineTree`. `source_ref` is
`Some` for an indirect item and `None` for a direct item. This preserves qpdf's
distinction without exposing `QPDFObjGen(0, 0)` as a fake PDF reference.

The raw `object` is the Rust equivalent of the object handle inherited through
qpdf's `QPDFObjectHelper`. Keys without dedicated qpdf outline accessors remain
available through this raw object rather than through flpdf-specific typed
fields. The arena supplies the equivalent of qpdf's parent pointer and kids
vector. It also provides safe Rust iteration without changing traversal
semantics.

The exact method names may follow Rust naming conventions. Their behavior must
map directly to qpdf's `hasOutlines`, `getTopLevelOutlines`, `getParent`,
`getKids`, `getDest`, `getDestPage`, `getCount`, `getTitle`,
`resolveNamedDest`, and `getOutlinesForPage` surfaces.

## Tree Construction

Tree construction follows qpdf's observable order:

1. Read Catalog `/Outlines`.
2. Start only when the resolved value is a dictionary with `/First`.
3. Traverse top-level items in `/First` then `/Next` order.
4. For each item, traverse children in its `/First` then `/Next` order.
5. Number top-level depth as 1.
6. Materialize the node at depth 51 but do not expand its children because qpdf
   returns immediately when constructor depth is greater than 50.
7. Do not return a depth error.

An internal cursor represents either a direct value or one indirect reference.
Resolving an indirect cursor takes the normal single object lookup required to
inspect its value. It must not recursively follow a bare-reference holder chain.

Only indirect object identities participate in cycle detection. Direct values
are admitted on every encounter, matching `QPDFObjGen::set::add`, which ignores
object generation `0,0`. The implementation must reproduce qpdf's placement of
seen checks for top-level and descendant construction; oracle fixtures pin
repeated and cyclic cases before implementation.

qpdf may materialize a non-null, non-dictionary outline value and then return
default accessor values for it. flpdf must preserve that observable node rather
than silently dropping it. Its raw object remains available while title, count,
destination, and children use qpdf's defaults.

## Item Accessors

### Destination

The qpdf-compatible behavior established by `flpdf-nm2o` remains authoritative:

1. key presence gives `/Dest` unconditional precedence;
2. otherwise only dictionary `/A` with exact name `/S /GoTo` and present `/D`
   contributes a destination;
3. Name candidates consult only Catalog `/Dests`;
4. String candidates consult only `/Names /Dests`;
5. missing or unresolved candidates return `Object::Null`;
6. other object shapes remain raw;
7. destination page is array element zero only when the destination is a
   non-empty array, and is null otherwise.

### Title

Title decoding reproduces qpdf `getUTF8Value()`, including PDFDocEncoding,
UTF-16 byte-order markers, explicit UTF-8 markers, and malformed-input fallback.
An absent or non-string title is the empty string.

### Count

Count uses Rust `i32`, matching qpdf `getIntValueAsInt()`. Absent and
non-integer values return zero. Out-of-range parsed integers clamp to `i32::MIN`
or `i32::MAX` with the same observable warning behavior available through
flpdf's reader diagnostics.

## Page Index

Build the page-to-outline index lazily from the materialized tree, as qpdf does.
Use breadth-first order: enqueue all roots, then append each dequeued item's
kids. Group every item by the object identity returned from its destination page.

The Rust lookup key is `Option<ObjectRef>`:

- `Some(page_ref)` represents an indirect page operand;
- `None` represents qpdf's `QPDFObjGen(0,0)` bucket, including missing,
  non-array, empty-array, direct, or otherwise non-indirect destination pages.

The returned item order must equal qpdf's breadth-first insertion order.

## Error and Warning Behavior

The outline layer performs no independent validation, repair, or mutation.
Missing keys, wrong types, and unresolved destinations fall back exactly as the
qpdf accessors do. Depth overflow silently truncates descendants.

Only unrecoverable reader I/O or parse failures cross the Rust `Result`
boundary. Cases that qpdf handles through null/default values or warnings must
not become outline-helper-specific errors.

## JSON v2 Projection

`flpdf --json=2` emits exactly qpdf's outline item keys:

- `dest`;
- `destpageposfrom1`;
- `kids`;
- `object`;
- `open`;
- `title`.

It removes flpdf-only `action`, `count`, `flags`, and `structureelement` keys.
The JSON layer translates the Rust direct-object representation into the exact
qpdf string/null representation established by a live qpdf 11.9.0 oracle. It
does not leak arena `OutlineId` values.

## TDD and Oracle Strategy

Every behavior change follows RED, GREEN, REFACTOR. Each new test must be run
before implementation and must fail for the missing behavior rather than a
fixture or syntax error.

### Layer 1: API and policy removal

- Add compile-fail coverage for removed public names before deleting them.
- Preserve ordinary raw `/SE` and unknown-key round trips.
- Remove normalizers, diagnostics, pruning, configurable depth, re-exports,
  documentation, and tests that assert the deleted policy.

### Layer 2: direct and mixed values

- Pin direct `/Outlines`, direct `/First`, direct `/Next`, mixed chains, direct
  parents, repeated direct values, and non-dictionary items against qpdf.
- Introduce the arena and value-aware cursor with only the code needed to pass.

### Layer 3: depth

- Pin depths 50, 51, and 52 against qpdf.
- Replace the 5000-level erroring traversal with qpdf's silent boundary.

### Layer 4: title and count

- Cover PDFDocEncoding, UTF-16BE/LE, explicit UTF-8, malformed strings, absent
  values, wrong types, and both integer clamp boundaries.

### Layer 5: page index

- Cover breadth-first order, multiple outlines for one page, named and explicit
  destinations, nested outlines, and the `None` bucket.

### Layer 6: JSON

- Add a real outline fixture to the JSON diff corpus.
- Compare representative output live with qpdf 11.9.0.
- Pin key set, ordering, page positions, open state, direct-object projection,
  and title encoding.

Normal Rust tests must not require qpdf. They encode oracle-confirmed expected
results. Representative live-oracle tests skip when qpdf 11.9.0 is unavailable.

## Verification

Each stack layer runs, in order:

```sh
cargo fmt --all -- --check
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Changed behavior also requires the relevant qpdf live-oracle command and the
repository's patch-coverage gate before review.

Each layer is committed and reviewed independently. Higher layers depend on the
previous layer's branch so public API removal, traversal, scalar accessors, page
indexing, and JSON projection remain small review surfaces.

