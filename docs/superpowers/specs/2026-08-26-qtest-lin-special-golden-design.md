# qtest linearization.test lin-special golden completion

## Goal

Make qtest rows 23, 29, and 35 byte-identical to qpdf 11.9.0 for
lin-special.pdf under object-streams disable, preserve, and generate. The fix
belongs to the canonical linearization planner/writer route; qtest goldens and
manifest attribution are not edited.

This slice is tracked by flpdf-25kg.6.19 and is stacked on the CLI slice from
flpdf-25kg.5.5.1.

## Evidence and semantic authority

A fresh qtest run with the CLI slice passes 306/309. The only failures are the
three lin-special golden comparisons. Live probes show:

- qpdf disable and preserve output is 3085 bytes; flpdf is the same size but
  emits the /Pages part9 object after the other document object.
- qpdf generate output is 2849 bytes; flpdf is 2828 bytes and places two
  inherited /MediaBox arrays inside the generated ObjStm.
- qpdf's generated object stream contains 10 members; flpdf contains 12.

Pinned qpdf source is authoritative:

- QPDFWriter.cc:2059-2161 runs object-stream setup before linearized writing.
- QPDFWriter.cc:1970-2005 generates the eligible set and even split.
- QPDFWriter.cc:2520-2561 runs QPDF::optimize and then obtains the
  linearization parts.
- QPDF_optimization.cc:57-65 and its inherited-attribute walk can mint
  indirect arrays after the object-stream set was fixed.
- QPDF_linearization.cc:1286-1336 emits the Pages tree first in part9, then
  thumbnails/outlines, then the remaining lc_other set.

The current flpdf route computes Optimization and inherited attributes before
objstm_membership_linearized recomputes Generate eligibility, and the plan's
physical part4_rest order does not enforce the qpdf Pages-first part9 rule even
though the renumber map reserves the Pages slot first.

## Design

Record the Generate object-stream eligibility snapshot before the operation
that can mint optimization-time indirect objects. Retain that snapshot with
the existing optimization/user map and use it for the linearized even split;
the ordinary test-only membership entry point keeps its current behavior when
no snapshot is supplied. Page and Catalog erasure still happens after the
global split, matching QPDFWriter.cc:2141-2160.

Normalize the physical part9 plan order so pages_tree_ref is first when it
belongs to part4_rest. Keep the existing outline and per-page partition
responsibilities unchanged. The renumber map and body emitter must consume the
same ordered part list, so object numbers and physical order agree.

The new tests exercise the boundary directly: a synthetic Pages node with
inherited direct arrays verifies those arrays stay plain, and a hand-built
part4 plan verifies Pages-first physical order. The qtest golden comparisons
remain the final byte-level authority.

## Acceptance and verification

- Rows 23, 29, and 35 compare byte-for-byte with the vendored qpdf goldens.
- All three resulting PDFs pass qpdf --check-linearization.
- Existing linearization, object-stream, reader, and writer tests remain green.
- The focused suite is 309/309 before any manifest promotion.

No vendored qtest fixture, golden, or parity manifest line is changed.
