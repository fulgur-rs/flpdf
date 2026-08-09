# ObjStm Eligibility QPDF Convergence Design

**Issue:** `flpdf-25kg.6.4`

**Status:** Approved for implementation on 2026-08-10

## Goal

Make flpdf's object-stream membership decisions match qpdf 11.9.0. The
candidate set must be derived from the trailer-rooted object graph and object
type. The writer must not inspect whether the input document was linearized.
Output-specific restrictions remain a separate writer stage.

## Oracle facts

`QPDF::getCompressibleObjGens` in `libqpdf/QPDF.cc:2393-2469` starts at the
trailer, tracks visited object numbers, removes superseded generations, excludes
the trailer's `/Encrypt` object, rejects streams and complete signature value
dictionaries, and still traverses excluded objects' dictionaries. It does not
call `QPDF::isLinearized()`.

`QPDFWriter::doWriteSetup` in `libqpdf/QPDFWriter.cc:2141-2160` applies the
remaining restrictions after object-stream planning: page dictionaries are
removed only when this write is linearized, and the document Catalog is removed
when this write is linearized or encrypted.

`QPDF::isLinearized()` in `libqpdf/QPDF_linearization.cc:84-155` is a separate
check/JSON detector. Its port is already tracked by `flpdf-25kg.3.29` and is
not part of this issue.

## Design

### 1. Base eligibility is input-linearization independent

`EligibilityContext` retains only the source trailer's `/Encrypt` identity.
`linearization_param_ref` is removed. Building the context must not call
`Pdf::linearized_hint_ref`, so an unreachable or malformed object 1 cannot
abort ObjStm planning.

The existing qpdf-shaped traversal remains the source of the Generate and
source-ObjStm Preserve candidate order. Its existing stream, signature,
generation, `/Type /ObjStm`, `/Type /XRef`, encryption, null-visibility, and
stale-generation behavior remain covered by their current tests.

### 2. Output restrictions are applied after planning

Add one output-mode filter for legacy/plain ObjStm batches. It receives the
actual output mode, not an input probe:

- `output_linearized = true`: remove every page dictionary and the Catalog.
- `output_encrypted = true`: remove the Catalog only.
- both false: remove neither.

The filter operates on original object references before container numbering,
preserves the order of surviving members, and drops empty batches. The
linearization planner keeps its existing equivalent page/Catalog erasure and
routing behavior; it must use the same output-mode rule and never call the
input detector.

### 3. Consumers and boundaries

The plain non-encrypted full-rewrite path uses the base qpdf candidate set with
no output exclusions. The legacy encrypted full-rewrite path applies the
Catalog-only filter before assigning ObjStm container numbers. Linearized
output applies the page-and-Catalog filter. Incremental Generate uses the
base predicate only because it does not emit a linearized file or an output
Catalog ObjStm.

`Pdf::linearized_hint_ref` remains untouched in this issue's implementation
until `flpdf-25kg.3.29` replaces it with the faithful `isLinearized` detector.

## Error handling

Removing the writer's detector call converts malformed unreachable object 1
from an irrelevant resolution error into a normal rewrite. Errors resolving
objects that are actually reached by the qpdf candidate traversal continue to
propagate. Output filtering reports the existing structural resolution errors
from page enumeration or the missing `/Root` guard; it does not introduce
sentinel values or panic branches.

## Verification

Tests will cover:

1. An invalid, trailer-unreachable object 1 does not prevent Generate planning
   or a full rewrite with ObjStms.
2. An encrypted output excludes the Catalog but retains page dictionaries as
   possible members.
3. A linearized output excludes both page dictionaries and the Catalog.
4. Existing Generate, Preserve, Disable, encrypted, and linearization parity
   tests remain unchanged.
5. The malformed-object fixture is accepted by a live qpdf 11.9.0 oracle and
   both outputs pass `qpdf --check`.
6. Changed executable lines reach 100% patch coverage.

## Non-goals

- Porting `QPDF::isLinearized()`; tracked separately by `flpdf-25kg.3.29`.
- Changing `checkLinearization()`/hint-table validation.
- Changing ObjStm packing size, routing, or source-container identity policy.
- Preserving the old input-linearization compatibility bridge.
