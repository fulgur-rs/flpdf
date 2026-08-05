# ObjectHandle writer-emission API design (flpdf-egzr.3.2.13)

> **For Claude:** this is a durable-decision document under `AGENTS.md` §7. The
> qpdf oracle facts below were verified against pinned qpdf 11.9.0 source; review
> them for correctness like any other claim.

**Decision:** add four `pub(crate)` methods to `ObjectHandle` —
`unparse_object`, `unparse_object_qdf`, `unparse_stream_body`,
`unparse_trailer` — porting `QPDFWriter::unparseObject` and
`QPDFWriter::writeTrailer`, not `QPDFObjectHandle::unparse` (already ported,
`ObjectHandle::unparse`/`unparse_resolved`, flpdf-egzr.3.2.1). Additive only:
`object_handle.rs` and `object.rs` change, no existing consumer file does.
Full acceptance criteria are in the beads issue — `bd show flpdf-egzr.3.2.13`;
this document is the design that criteria was built from.

**Architecture:** the four primitives walk `ObjectHandle`'s own graph
directly (never materializing an intermediate `Object` tree first), forcing
resolution through `try_dereference`/`try_is_null` where qpdf's own
`isNull()` would, and delegate scalar byte-formatting to `object.rs`'s
existing helpers (`write_name_escaped`, `write_string_value`,
`real_literal_is_safe` promoted to `pub(crate)`) rather than duplicating
them.

**Design reference:** `bd show flpdf-egzr.3.2.13` for the full acceptance
criteria and the verification trail (three rounds of correction recorded in
NOTES) that produced them. Related: `bd show flpdf-umye` (legacy
`write_pdf_stream` null-suppression gap, deliberately out of this issue's
scope), `bd show flpdf-wt2w` (xref-stream dict byte-parity bug, closes as a
side effect of `flpdf-egzr.3.2.5` adopting `unparse_trailer`).

---

## Before you start

Read `crates/flpdf/src/object_handle.rs:589-621` (`try_dereference`/
`try_is_null`, currently `#[allow(dead_code)]`) and `:2120-2220` (the
`RecordingResolver`/`resolver_bearing_handle` mock-resolver test harness) in
full — both are load-bearing for this design, not incidental. Read
`crates/flpdf/src/object.rs:605-632` (`real_literal_is_safe`) and the
existing `write_pdf`/`write_pdf_qdf`/`write_pdf_stream`/
`write_pdf_with_id_writer`/`write_pdf_trailer` functions (`:775-1003`) to see
the legacy shapes these primitives parallel but do not call into.

## Why `QPDFObjectHandle::unparse` is the wrong port target

`ObjectHandle::unparse`/`unparse_resolved` (flpdf-egzr.3.2.1) already port
`QPDFObjectHandle::unparse` (`QPDFObjectHandle.hh:1159`,
`QPDFObjectHandle.cc:1574-1593`). That is a different qpdf class with a
different contract — an object's own self-description, usable at any time,
no writer state involved. This issue ports `QPDFWriter::unparseObject`
(`QPDFWriter.cc:1318-1605`) and `QPDFWriter::writeTrailer` (`:1160-1230`):
writer-internal serialization that assumes a document about to be fully
written, and is allowed to force resolution the way a pure accessor in this
file is not (see `unparse_resolved`'s own doc on why it does *not* resolve on
the caller's behalf — that constraint does not apply here). The two
qpdf-function families are unrelated in contract despite sharing the English
word "unparse"; the new methods use a `unparse_object`/`unparse_trailer` name
family specifically so a reader does not conflate them with `unparse`/
`unparse_resolved`.

## The null-suppression rule (AC2)

qpdf's rule lives in `unparseObject`'s dictionary branch:

```cpp
// QPDFWriter.cc:1490-1491
for (auto& item: object.getDictAsMap()) {
    if (!item.second.isNull()) {
```

`isNull()` (`QPDFObjectHandle.cc:352-356`) calls `dereference()`
(`:2375-2383`), which resolves an indirect chain through `QPDF::resolve()`
(`QPDF.cc:1699-1753`) to its terminal value. A reference cycle resolves to
null via the `m->resolving` re-entrancy guard (`:1706-1712`); a dangling
reference resolves to null because an unknown object resolves to null per
the PDF spec (`:1745-1749`).

Three candidate flpdf predicates were compared against this:

| predicate | chases indirect refs? | qpdf-correct here? |
|---|---|---|
| `Dictionary::write_pdf` (no check at all) | n/a | No |
| `unparse_is_known_null` (`is_resolved() && is_null()`) | No | No — its own doc already says so |
| `qpdf_null::visible_entries` (legacy `Pdf`/`Object` model) | Yes | Correct rule, wrong type system |
| `try_is_null` (`object_handle.rs:618-621`) | Yes, via `try_dereference` | **Correct rule, correct type** |

`try_is_null` was built for exactly this (confirmed by the issue owner) and
its own doc already states it mirrors `dereference()` → `resolve()`. Use it,
not a bridge to the legacy model.

**Scope limit on the rule itself:** it applies only to `unparseObject`'s
dictionary branch, including the stream-dictionary path it recurses into at
`:1550`. It does **not** apply to `writeTrailer` (`:1160-1230`, unconditional
key loop, no `isNull` check anywhere in it), regardless of whether
`writeTrailer` is called for the classic trailer or, with `xref_stream=true`,
for the xref-stream dictionary's trailer-shaped keys (`writeXRefStream`,
`QPDFWriter.cc:2391-2495`, calls `writeTrailer(..., true, ...)` at `:2489`
and hand-emits `/Type`/`/Length`/`/Filter`/`/DecodeParms`/`/W`/`/Index` as
literal writes — never routing through `unparseObject` at all).

**Known incompleteness accepted, not worked around:** `try_dereference`
returns `Error::Unsupported` for an indirect handle whose xref entry is
`Compressed` (ObjStm-backed) — `flpdf-25kg.3.5` (separate, in progress) has
not yet implemented that resolution arm. The new primitives propagate that
`Result` as-is. Test coverage for "unresolved indirect" uses an uncompressed
fixture (exercisable today via the mock resolver harness below); ObjStm-null
coverage is deferred to whenever `flpdf-25kg.3.5` lands. This is not a new
gap — it is the crate's existing, tracked incompleteness surfacing honestly
through a new caller, not silently worked around with a fallback.

## The API (AC3)

```rust
pub(crate) fn unparse_object(&self, out: &mut Vec<u8>) -> Result<()>
pub(crate) fn unparse_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()>
pub(crate) fn unparse_stream_body(&self, out: &mut Vec<u8>, refiltered: bool) -> Result<()>
pub(crate) fn unparse_trailer(&self, out: &mut Vec<u8>, xref_stream: bool, id_writer: Option<TrailerIdWriter>) -> Result<()>
```

Four primitives, not five. `unparse_object`/`unparse_object_qdf`/
`unparse_stream_body` apply the suppression rule above (they all bottom out
in `unparseObject`'s dictionary branch); `unparse_trailer` does not.

**No fifth "dictionary-with-id-writer" primitive.** Real qpdf uses one
`writeTrailer` function with an `xref_stream: bool` for both the classic/
incremental trailer and the xref-stream dictionary's trailer-shaped keys,
forcing `/ID` and `/Encrypt` last in both cases. flpdf's existing
`object.rs::write_pdf_with_id_writer` — used for both the incremental-update
trailer (`writer.rs:1923`, via `write_incremental_trailer`) and the
xref-stream dictionary (`writer/serialize.rs:40`) — instead keeps `/ID`/
`/Encrypt` in plain lexicographic position for *both* uses. This has no qpdf
counterpart; it is flpdf's own pre-existing simplification (already
documented as such in `write_pdf_with_id_writer`'s own doc comment for the
xref-stream case; the incremental-trailer case carries the identical
divergence per `write_incremental_trailer`'s doc, discovered during this
design). `unparse_trailer` must not reproduce it — implement `writeTrailer`'s
real forced-last positioning for both `xref_stream` values. This closes
`flpdf-wt2w`'s root cause once `flpdf-egzr.3.2.5` migrates the xref-stream
writer onto `unparse_trailer(xref_stream: true, ...)`.

## What this issue deliberately does not touch

- **`crate::object::Dictionary::write_pdf_stream`** (`object.rs:913-939`,
  legacy, non-`ObjectHandle`) is missing the same null-suppression rule for
  regular (non-xref-stream) stream dictionaries — a live production gap
  (`writer.rs:3003`, `linearization/writer.rs:620`), tracked as
  `flpdf-umye`. It cannot be fixed in place without threading a resolver
  through `writer.rs`/`writer/serialize.rs`/`linearization/writer.rs`, well
  outside this issue's `object_handle.rs`/`object.rs`-only, additive-only
  scope. `ObjectHandle` carries its own resolver reference
  (`Repr::Indirect.resolver`), so `unparse_stream_body` does not have this
  constraint — the bug closes naturally when `flpdf-egzr.3.2.5` swaps the
  legacy callers onto the new primitive, not by patching the legacy
  function.
- **`QPDFWriter::unparseObject`'s Extensions/ADBE special-casing**
  (`QPDFWriter.cc:1356-1436`, root-dictionary-only) — not enumerated in
  AC3's primitive list and not investigated for this design. Flag during
  implementation if a covered primitive's test surface turns out to need it;
  otherwise it stays out of scope.
- **`flpdf-3yn9.7`'s `StreamDataProvider`** and **`flpdf-3yn9.6`'s**
  `filterable`/decoder-chain construction — unrelated, explicitly excluded
  by the parent issue.

## Implementation approach

> **[provisional — settled by TDD, not by this document]**
>
> *(implementation-detail sketch)*
>
> Suggested order: (1) promote `real_literal_is_safe` to `pub(crate)` in
> `object.rs`; (2) promote `try_dereference`/`try_is_null` out of dead-code
> in `object_handle.rs`; (3) a private shared suppression-filter helper over
> `try_is_null`, used by all three suppressing primitives; (4)
> `unparse_object`/`unparse_object_qdf` (differ only in QDF indent/newline
> policy — factor the container-walk once, branch on a `qdf: bool` or
> equivalent internally, matching how `unparseObject` itself is one C++
> function with an internal `m->qdf_mode` check rather than two); (5)
> `unparse_stream_body`, reusing (3)'s dictionary logic for the stream's own
> dict; (6) `unparse_trailer`, independent (no suppression, but does need the
> `/ID`/`/Encrypt`-forced-last logic for both `xref_stream` values). Each
> step gets its own TDD cycle. The oracle for any of this — not this
> sketch's phrasing — is qpdf's own behavior.
>
> **[/provisional]**

## Test strategy (AC5)

Layer split: `try_is_null`/`try_dereference`'s own correctness (cycle
detection via `ResolveMark`, dangling → null) is `reader/resolver.rs`'s
responsibility and already has qpdf-cited coverage there
(`resolve_indirect`, `resolver.rs:1775-1814`, citing `QPDF.cc:1710-1711`
directly in its own doc). This issue's tests only need to prove *this*
layer's contract: "an entry whose `try_is_null()` returns `true` is
suppressed; one whose call errors propagates the error." They do not need to
re-simulate cycle detection.

Use the existing mock-resolver harness (`object_handle.rs:2120-2220`:
`RecordingResolver`, `resolver_bearing_handle`, `logged_resolver_bearing_handle`,
`MissingResolver`, `ErrorResolver`) to build indirect handles bound to a
synthetic resolver in-process — no real parsed PDF file, no dependency on
`flpdf-25kg.3.5`'s still-open ObjStm work, no dependency on `qpdf_null.rs`'s
fixture (which is typed for the legacy model and belongs to a different set
of consumers).

| AC5 case | Construction |
|---|---|
| direct null | `ObjectHandle::null()` as a dict value, no resolver |
| indirect-to-null / unresolved indirect | `resolver_bearing_handle(ObjectValue::Null)` |
| cycle / dangling resolving to null | same mock, standing in for "resolution concluded null" — the *path* to null is `resolve_indirect`'s tested concern, not this layer's |
| stream dict null-valued key | same mocks, entry inside a `Stream`'s dict passed to `unparse_stream_body` |
| RealLiteral round-trip vs canonical | delegates to `real_literal_is_safe`; test both a safe literal and one that fails closed |
| direct-vs-indirect stream unparse | direct `ObjectHandle` construction, no resolver needed for most cases |
| QDF indent nesting | direct construction, nested containers |
| `unparse_trailer` xref_stream × /ID,/Encrypt presence | direct construction; four combinations (`xref_stream` × ID/Encrypt present-or-absent) |
