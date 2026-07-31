# filters.rs / stream_filter.rs ObjectHandle Migration Implementation Plan (SUPERSEDED)

**Status: abandoned, 2026-08-01. Do not implement this plan.**

The migration this plan describes (moving `/Filter` and `/DecodeParms`
interpretation in `crates/flpdf/src/filters.rs` and
`crates/flpdf/src/stream_filter.rs` from the legacy `Object` enum onto
`ObjectHandle` via the transitional `ObjectHandle::from_resolved_object`
bridge) was implemented on `feat/flpdf-egzr-3-2-2-filters-objecthandle` and
reviewed as PR #605, but was reverted after four consecutive rounds of
Codex Review findings (11 threads across 3 rounds, plus a 4th round of 2
further P1s) all converging on the same root cause: `from_resolved_object`
materializes an owned `ObjectHandle` tree before its shape is validated, so
an attacker-controlled `/Filter` or `/DecodeParms` value pays allocation
cost proportional to its size even when the shape is ultimately rejected or
the value is never read. Each round's fix narrowed one instance of this
class (array length, whether-to-convert, which-keys-to-convert) and the next
round found another (per-array-item shape, per-key value shape) — the
pattern would not converge by patching call sites.

The legacy `&Object`-based code was safe because it inspected shape on a
borrow and extracted only the specific bytes it needed (zero-copy,
validate-then-extract). The `from_resolved_object` bridge inverts that order
(materialize-then-validate), and no per-call-site gating recovers the
original order. `/Filter` and `/DecodeParms` are read-only, shallow, and
untrusted-input-derived — exactly the shape of value that should not cross
a materializing `Object` -> `ObjectHandle` bridge.

`crates/flpdf/src/filters.rs`, `crates/flpdf/src/stream_filter.rs`, and
`crates/flpdf/src/object_handle.rs` were reverted to their
`feat/flpdf-egzr-3-2-1-objecthandle-api` (pre-migration) state. See
`flpdf-egzr.3.2.2` (bd issue) notes for the full history and PR #605 for the
review threads. A future re-attempt at this slice needs a bounded,
borrowing `ObjectHandle` accessor (validate-shape-on-a-borrow, not
materialize-then-validate) added to `ObjectHandle` first — this is likely to
be forced by 3.2.4-3.2.7 (writer/page/json/cli) hitting the same pattern
against other untrusted PDF data, ahead of 3.2.8's full `Object` removal.
