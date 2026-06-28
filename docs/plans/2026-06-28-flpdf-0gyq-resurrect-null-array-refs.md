<!-- For Claude: full oracle + design in the beads issue (bd show flpdf-0gyq). This
     plan is the implementation breakdown only. Scope (user, 2026-06-28): full
     4-cell linearize matrix; non-linearized is flpdf-v58c. -->

# flpdf-0gyq — resurrect reachable null-resolving array refs as null objects (linearize)

## Goal

Match qpdf 11.9.0 byte-for-byte for null-resolving (missing-xref OR free) indirect refs
that appear as ARRAY elements: resurrect them as indirect `null` body objects. Cover all 4
cells (free/missing × disable/generate); keep free-disable (the working anchor) unchanged.

## Confirmed facts (oracle ladder)
- qpdf resurrects null-resolving array elements as null objects in ALL modes; free vs missing
  qpdf outputs are STRUCTURALLY byte-identical (disable AND generate). So "make missing behave
  like a free entry" yields parity.
- free-disable already byte-identical (anchor). free-generate diverges (resurrected null emitted
  plain, not in ObjStm). missing-{disable,generate} diverge (Layer A inline null).
- Mechanism: free entry -> CacheEntry::Deleted -> in object_refs() -> all_refs -> null body.
  Missing -> not in object_refs() -> bypasses. Generate membership = objstm_membership_linearized
  (plan.rs:2089) via compressible_objgens (excludes Deleted/Missing via live) + assigned retain.

## The core trap (drop-aware, NOT raw reachable)
A null-resolving ref reached ONLY via a to-be-dropped dict value (`/Held 99 0 R`, no array) must
NOT be resurrected (qpdf GCs it; flpdf currently correct — /Size matches). Raw `reachable` admits
it -> stray null regression. Admission must gate on SURVIVING edges: array elements, and dict
values that are not themselves null-resolving refs.

## Implementation stages (TDD; validate against byte goldens each stage)

### Stage 1 — drop-aware resurrectable primitive (unit-tested)
Add a reachability variant that returns the **resurrectable set**: refs with `number > 0` that
are NOT live (`!live.contains(r)`, i.e. free/missing) and are reachable via surviving edges.
- Do NOT mutate the shared `reachable_object_set` / `collect_refs`; add a sibling (e.g.
  `resurrectable_null_refs(pdf)`), or a `collect_refs` variant that, in the DICTIONARY branch,
  skips a value that is `Reference(r)` with `r` null-resolving (dropped key), while the ARRAY
  branch follows every element. `live = live_object_refs()` decides null-resolving.
- object-0 (`0 0 R`) is NOT resurrectable (Layer A inline null) — exclude `number == 0`.
- Unit tests: array-reached missing -> in set; dict-only-missing -> NOT in set; nested dict
  dangling -> NOT in set; free entry array-reached -> in set.

### Stage 2 — wire to all_refs -> missing-disable byte-parity
In `LinearizationPlan::from_pdf` all_refs construction (plan.rs:580): after the object_refs
loop, add the resurrectable missing refs (those not already present via object_refs) so they
receive a renumber slot and emit a `null` body. Preserve qpdf placement/numbering by inserting
them the way free entries already flow (validate against the free-disable anchor: a free-entry
fixture and an equal-shape missing fixture must produce identical flpdf output).
- GREEN: missing-disable byte == qpdf; free-disable UNCHANGED; dict-only-missing UNCHANGED
  (no stray null, /Size stable).

### Stage 3 — wire to ObjStm membership -> generate byte-parity
In `objstm_membership_linearized` (plan.rs:2089): add the resurrectable refs to `eligible` as
TRAILING members (qpdf places them last in the ObjStm), before the even split + assigned retain
(they now have slots from Stage 2, so they survive the retain).
- GREEN: free-generate AND missing-generate byte == qpdf.

### Stage 4 — regression guards + full suite
- Byte goldens (new fixtures, qpdf-blessed via regenerate.sh, wired into the already-listed
  cmp_linearize_tests / cmp_linearize_objstm_tests): free+missing × disable+generate (anchor +
  3 newly-fixed cells).
- Structural guard in dangling_body_ref_linearize_tests.rs: dict-only-missing -> no stray null,
  /Size unchanged (proves the drop-aware gate).
- Full `cargo test -p flpdf`, `--features qpdf-zlib-compat` cmp_linearize* + cmp_generate*,
  fmt/clippy, patch-coverage 100%.

## Risk
all_refs reachability + ObjStm membership are shared with the free path and existing fixtures;
re-run the full byte suites. The free-disable anchor + the 4-cell matrix are the guard.
