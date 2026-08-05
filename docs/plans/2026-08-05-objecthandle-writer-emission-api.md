# ObjectHandle Writer-Emission API Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add four `pub(crate)` methods to `ObjectHandle` —
`unparse_object`, `unparse_object_qdf`, `unparse_stream_body`,
`unparse_trailer` — that port `QPDFWriter::unparseObject` and
`QPDFWriter::writeTrailer` (qpdf 11.9.0), including qpdf's null-valued
dictionary-key suppression rule, without going through the existing
materialize-to-`Object` bridge.

**Architecture:** A shared private recursive walker writes a child
`ObjectHandle`'s bytes directly — an indirect child always writes as its
`"N G R"` reference form (never recursed into); a direct child writes
inline, dispatching to a scalar writer (delegating byte-formatting to
`object.rs`'s `write_name_escaped`/`write_string_value`/
`real_literal_is_safe`) or recursing for containers. A dictionary-entry
helper wraps this with qpdf's suppression rule — `try_is_null()`,
promoted out of dead-code — used by three of the four public methods;
`unparse_trailer` opts out, matching `writeTrailer`'s own unconditional key
loop.

**Tech Stack:** Rust, existing `flpdf` crate conventions (`stacker::maybe_grow`
for recursion-safe stack growth, matching `unparse_materialize`'s existing
pattern at `object_handle.rs:1804-1828`).

**Design reference:** `docs/plans/2026-08-05-objecthandle-writer-emission-api-design.md`
(architecture, qpdf citations, scope boundaries) and `bd show flpdf-egzr.3.2.13`
(acceptance criteria). Read both before starting.

---

## Before you start

Read in full:
- `crates/flpdf/src/object_handle.rs:589-621` (`try_dereference`/`try_is_null`)
- `crates/flpdf/src/object_handle.rs:1497-1520` (`with_value`/`with_value_mut`)
- `crates/flpdf/src/object_handle.rs:1760-1834` (`unparse_materialize_value`/
  `unparse_materialize`/`unparse_materialize_child` — the existing
  indirect-child-writes-as-reference pattern this plan reuses)
- `crates/flpdf/src/object_handle.rs:2120-2220` (`RecordingResolver`,
  `resolver_bearing_handle`, `logged_resolver_bearing_handle`,
  `MissingResolver`, `ErrorResolver` — the mock-resolver test harness)
- `crates/flpdf/src/object.rs:488-548` (`Object::write_pdf`, non-qdf — the
  byte shape `unparse_object` must match) and `:566-590`
  (`Object::write_pdf_qdf` — the byte shape `unparse_object_qdf` must match)
- `crates/flpdf/src/object.rs:913-939` (`Dictionary::write_pdf_stream`,
  refiltered-flag shape) and `:973-1003` (`Dictionary::write_pdf_trailer`,
  /ID and /Encrypt forced-last shape — **do not modify either function**;
  they are legacy, out of this issue's scope per the design doc)

qpdf source (pinned 11.9.0, already fetched during design) for direct
citation while writing doc comments:
- `QPDFWriter.cc:1318-1605` (`unparseObject`)
- `QPDFWriter.cc:1160-1236` (`writeTrailer`, full function including its
  final `>>` at `:1235` — written unconditionally by `writeTrailer` itself
  even when `xref_stream=true` and it wrote no opening `<<`)
- `QPDFObjectHandle.cc:352-356` (`isNull`), `:2375-2383` (`dereference`)
- `QPDF.cc:1699-1753` (`resolve`, cycle/dangling → null)

**Known scope limit, not part of this plan:** `writeTrailer`'s
`which == t_lin_second` branch (`QPDFWriter.cc:1170-1172`, linearization
second pass — only `/Size` is written, everything else skipped) is out of
scope. `unparse_trailer` covers the two cases AC5 asks for (classic/
incremental trailer, and the xref-stream dictionary's trailer-shaped keys)
via an `xref_stream: bool` parameter only. Note this in `unparse_trailer`'s
doc comment so a future linearization-writer consumer (`flpdf-3yn9.4`/`.5`)
knows the gap is deliberate, not missed.

---

### Task 1: Promote `real_literal_is_safe` to `pub(crate)`

**Files:**
- Modify: `crates/flpdf/src/object.rs:605`

**Step 1: Make the change**

Change:
```rust
fn real_literal_is_safe(literal: &[u8], value: f64) -> bool {
```
to:
```rust
pub(crate) fn real_literal_is_safe(literal: &[u8], value: f64) -> bool {
```

**Step 2: Verify it still compiles clean**

Run: `cargo build -p flpdf 2>&1 | grep -E "warning|error"`
Expected: no new warnings (the function already has callers in this file, so
no dead-code warning appears from this change alone).

**Step 3: Commit**

```bash
git add crates/flpdf/src/object.rs
git commit -m "refactor(object): promote real_literal_is_safe to pub(crate)

Needed by the new ObjectHandle writer-emission primitives
(flpdf-egzr.3.2.13) to format RealLiteral values without going through
Object::write_pdf."
```

---

### Task 2: Promote `try_dereference`/`try_is_null` out of dead-code

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:594,618`

**Step 1: Remove the dead-code allowances**

At `:594` and `:618`, remove the lines:
```rust
#[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
```
above both `try_dereference` and `try_is_null`.

**Step 2: Build and confirm no dead-code warning reappears**

Run: `cargo build -p flpdf 2>&1 | grep -E "warning|error"`
Expected: no output. (If a dead-code warning appears, it means neither
function has a caller yet — expected until Task 3 adds one. If that
happens, leave the `#[allow(dead_code)]` on `try_dereference` only — the
one `try_is_null` calls internally — and remove it from `try_is_null`,
whose caller lands in Task 3. Re-add both removals once Task 3's helper
compiles.)

**Step 3: Run the existing object_handle test suite (regression check)**

Run: `cargo test -p flpdf --lib object_handle:: 2>&1 | tail -5`
Expected: `test result: ok. 147 passed; 0 failed`

**Step 4: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "refactor(object_handle): promote try_dereference/try_is_null out of dead-code

First real caller lands in the next commit (flpdf-egzr.3.2.13's
null-suppression helper)."
```

(If Task 2 Step 2 required deferring `try_is_null`'s promotion, fold that
one-line removal into Task 3's commit instead — do not leave a broken
intermediate commit.)

---

### Task 3: Shared child-writer and suppression-aware dictionary-entry helper

This is the core recursion hub every other task builds on. It has two
pieces: (a) write one child handle's bytes (indirect → reference form,
direct → recurse), and (b) given a dictionary's entries, filter out the
ones `try_is_null()` reports true for.

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (new private functions, placed
  near `unparse_materialize` at `:1804+`, in the same `impl ObjectHandle`
  block region as the public methods added in later tasks — pick a location
  after `unparse_resolved` and before the `#[cfg(test)]` module)
- Test: `crates/flpdf/src/object_handle.rs` (new `mod unparse_object_tests`
  near the existing `unparse_tests` module)

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod unparse_object_tests {
    use super::*;

    #[test]
    fn visible_dict_entries_keeps_non_null_and_drops_direct_null() {
        let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![
            (b"Zulu".to_vec(), ObjectHandle::integer(26)),
            (b"DirectNull".to_vec(), ObjectHandle::null()),
        ];
        let visible = visible_dict_entries(&entries).expect("no resolver needed");
        let keys: Vec<&[u8]> = visible.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, [b"Zulu".as_slice()]);
    }

    #[test]
    fn visible_dict_entries_resolves_and_drops_an_indirect_null() {
        let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
        let entries: Vec<(Vec<u8>, ObjectHandle)> =
            vec![(b"RefNull".to_vec(), indirect_null)];
        let visible = visible_dict_entries(&entries).unwrap();
        assert!(visible.is_empty());
    }

    #[test]
    fn visible_dict_entries_propagates_a_dropped_document_error() {
        let (indirect_null, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let entries: Vec<(Vec<u8>, ObjectHandle)> =
            vec![(b"RefNull".to_vec(), indirect_null)];
        assert!(visible_dict_entries(&entries).is_err());
    }

    #[test]
    fn write_child_writes_indirect_handle_as_reference_form() {
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let mut out = Vec::new();
        write_child(&indirect, &mut out).unwrap();
        assert_eq!(out, b"20 0 R");
    }

    #[test]
    fn write_child_recurses_into_a_direct_scalar() {
        let mut out = Vec::new();
        write_child(&ObjectHandle::integer(7), &mut out).unwrap();
        assert_eq!(out, b"7");
    }
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --lib object_handle::unparse_object_tests 2>&1 | tail -20`
Expected: FAIL — `visible_dict_entries`/`write_child` not found.

**Step 3: Implement**

```rust
/// Whether `entry`'s value is qpdf-null under `unparseObject`'s dictionary
/// suppression rule (`QPDFWriter.cc:1490-1491`, via `isNull()`'s indirect
/// chain resolution, `QPDFObjectHandle.cc:352-356`/`:2375-2383`).
fn write_child(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk(handle, out)
}

/// Filters `entries` down to the ones `unparseObject`'s dictionary branch
/// would actually write (`QPDFWriter.cc:1490-1491`). Forces resolution of
/// every indirect value via `try_is_null` -- this is the one place in this
/// primitive family that performs the hidden I/O qpdf's own `isNull()`
/// performs and every other accessor in this file deliberately avoids (see
/// `unparse_resolved`'s own doc on why *it* does not resolve on the
/// caller's behalf; `QPDFWriter::unparseObject` is a writer-internal path
/// with no such constraint).
fn visible_dict_entries<'a>(
    entries: &'a [(Vec<u8>, ObjectHandle)],
) -> Result<Vec<(&'a Vec<u8>, &'a ObjectHandle)>> {
    let mut visible = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if !value.try_is_null()? {
            visible.push((key, value));
        }
    }
    Ok(visible)
}
```

`unparse_object_walk` is defined in Task 4 (the recursion this function
calls into); this task's tests for `write_child`'s direct-scalar case will
not compile until Task 4 lands `unparse_object_walk`. Write Task 3 and Task
4 as one commit if the split proves awkward in practice — the split above
is for review granularity, not a hard requirement.

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --lib object_handle::unparse_object_tests 2>&1 | tail -10`
Expected: `test result: ok. 5 passed; 0 failed`

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add unparse child-writer and null-suppression filter

Shared by unparse_object/unparse_object_qdf/unparse_stream_body
(flpdf-egzr.3.2.13). Ports QPDFWriter.cc:1490-1491's isNull-based
suppression onto ObjectHandle's own try_is_null rather than the legacy
Pdf/Object model's qpdf_null::visible_entries."
```

---

### Task 4: `unparse_object` (plain, non-QDF)

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: same file, `unparse_object_tests`

**Step 1: Write the failing tests**

```rust
#[test]
fn unparse_object_writes_a_scalar() {
    let mut out = Vec::new();
    ObjectHandle::integer(42).unparse_object(&mut out).unwrap();
    assert_eq!(out, b"42");
}

#[test]
fn unparse_object_writes_a_name_escaped() {
    let mut out = Vec::new();
    ObjectHandle::name(b"application/pdf".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"/application#2fpdf");
}

#[test]
fn unparse_object_writes_a_real_literal_when_safe() {
    let mut out = Vec::new();
    ObjectHandle::real_literal(0.4, b".4".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b".4");
}

#[test]
fn unparse_object_falls_back_to_canonical_when_literal_is_unsafe() {
    let mut out = Vec::new();
    ObjectHandle::real_literal(0.4, b"nope".to_vec())
        .unparse_object(&mut out)
        .unwrap();
    assert_eq!(out, b"0.4");
}

#[test]
fn unparse_object_writes_an_array_with_qpdf_spacing() {
    let handle = ObjectHandle::array(vec![
        ObjectHandle::integer(1),
        ObjectHandle::integer(2),
    ]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"[ 1 2 ]");
}

#[test]
fn unparse_object_writes_a_dict_and_suppresses_direct_null() {
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"B".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /A 1 >>");
}

#[test]
fn unparse_object_suppresses_an_indirect_entry_resolving_to_null() {
    let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"RefNull".to_vec(), indirect_null),
    ]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /A 1 >>");
}

#[test]
fn unparse_object_writes_a_retained_indirect_entry_as_reference_form() {
    let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
    let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), indirect)]);
    let mut out = Vec::new();
    handle.unparse_object(&mut out).unwrap();
    assert_eq!(out, b"<< /A 20 0 R >>");
}

#[test]
fn unparse_object_propagates_a_dropped_document_error() {
    let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
    drop(resolver);
    let mut out = Vec::new();
    assert!(indirect.unparse_object(&mut out).is_err());
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --lib object_handle::unparse_object_tests 2>&1 | tail -20`
Expected: FAIL — `unparse_object`/`unparse_object_walk` not found.

**Step 3: Implement**

Add to the `impl ObjectHandle` block (public surface) and as free functions
(recursion hub, same pattern as `unparse_materialize`/`unparse_materialize_child`
at `:1816-1834`):

```rust
impl ObjectHandle {
    /// This handle's plain (non-QDF) writer-emission form
    /// (`QPDFWriter::unparseObject`, `QPDFWriter.cc:1318-1527`, called with
    /// `level=0, flags=0`). Distinct from [`Self::unparse`]/
    /// [`Self::unparse_resolved`], which port a different qpdf function
    /// (`QPDFObjectHandle::unparse`) with a different contract -- do not
    /// conflate the two. Forces resolution of every indirect dictionary
    /// entry to apply qpdf's null-valued-key suppression rule
    /// (`:1490-1491`); an indirect entry that survives suppression writes
    /// as its own `"N G R"` reference form, never inlined.
    pub(crate) fn unparse_object(&self, out: &mut Vec<u8>) -> Result<()> {
        unparse_object_walk(self, out)
    }
}

// Sole recursion hub for the plain unparse family, mirroring
// `unparse_materialize`'s own single-hub pattern (`:1816-1828`) for the
// same stack-growth reason: an `ObjectHandle` tree built through public
// factories carries no depth bound the parser enforces on parsed input.
fn unparse_object_walk(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        handle.with_value(|value| match value {
            Some(value) => unparse_object_value(value, out),
            None => {
                out.extend_from_slice(b"null");
                Ok(())
            }
        })
    })
}

fn unparse_object_value(value: &ObjectValue, out: &mut Vec<u8>) -> Result<()> {
    match value {
        ObjectValue::Null => out.extend_from_slice(b"null"),
        ObjectValue::Boolean(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        ObjectValue::Integer(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ObjectValue::Real(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ObjectValue::RealLiteral { value, literal } => {
            if crate::object::real_literal_is_safe(literal, *value) {
                out.extend_from_slice(literal);
            } else {
                out.extend_from_slice(value.to_string().as_bytes());
            }
        }
        ObjectValue::Name(name) => {
            out.push(b'/');
            crate::object::write_name_escaped(out, name);
        }
        ObjectValue::String(value) => crate::object::write_string_value(out, value),
        ObjectValue::Operator(value) | ObjectValue::InlineImage(value) => {
            out.extend_from_slice(value);
        }
        ObjectValue::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child(child, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> =
                entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            unparse_dict_entries(&entries, out)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            // A stream is only ever a top-level indirect object in valid
            // qpdf usage; a direct handle wrapping one falls through to the
            // same inlining this port's other direct-value handling uses
            // (mirroring `unparse_resolved`'s own documented choice for the
            // same shape).
            unparse_object_walk(stream_dict, out)?;
        }
        ObjectValue::Reference(_) => {
            // qpdf-cutover-delete(flpdf-25kg.3.3) variant; unreachable from
            // a document-created handle by the time this primitive is
            // reachable. No qpdf counterpart to write.
        }
    }
    Ok(())
}

// Writes `<< /K1 v1 /K2 v2 >>` with qpdf's suppression rule applied
// (`QPDFWriter.cc:1488-1527`, non-stream case: no `/Length` tail).
fn unparse_dict_entries(entries: &[(Vec<u8>, ObjectHandle)], out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        out.push(b' ');
        crate::object::write_name_escaped(out, key);
        // write_name_escaped does not write the leading '/': confirm
        // against object.rs:509-512's own call shape before relying on
        // this -- adjust to `out.push(b'/'); write_name_escaped(...)` if
        // the helper's contract turns out to expect the slash already
        // stripped rather than added by the caller either way, whichever
        // matches its existing call sites.
        out.push(b' ');
        write_child(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}
```

> **[provisional — settled by TDD, not by this document]**
>
> *(implementation-detail sketch)* The exact call shape for
> `write_name_escaped` (does it write the leading `/` itself, or does the
> caller?) must match `object.rs:509-512`'s existing usage exactly — check
> that call site before trusting the sketch above; fix the sketch, not the
> byte output, if they disagree.
>
> **[/provisional]**

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --lib object_handle::unparse_object_tests 2>&1 | tail -15`
Expected: all tests pass.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add ObjectHandle::unparse_object

Ports QPDFWriter::unparseObject (level=0, flags=0), including its
null-valued-dictionary-key suppression (QPDFWriter.cc:1318-1527,
:1490-1491), for flpdf-egzr.3.2.13."
```

---

### Task 5: `unparse_object_qdf`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: same file

**Step 1: Write the failing tests**

```rust
#[test]
fn unparse_object_qdf_writes_a_scalar_like_plain_unparse() {
    let mut out = Vec::new();
    ObjectHandle::integer(42).unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"42");
}

#[test]
fn unparse_object_qdf_writes_an_array_with_newline_indent() {
    let handle = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"[\n  1\n]");
}

#[test]
fn unparse_object_qdf_writes_a_dict_with_newline_indent_and_suppresses_null() {
    let handle = ObjectHandle::dictionary(vec![
        (b"A".to_vec(), ObjectHandle::integer(1)),
        (b"B".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /A 1\n>>");
}

#[test]
fn unparse_object_qdf_nests_indent_one_level_deeper() {
    let handle = ObjectHandle::dictionary(vec![(
        b"Kids".to_vec(),
        ObjectHandle::array(vec![ObjectHandle::integer(1)]),
    )]);
    let mut out = Vec::new();
    handle.unparse_object_qdf(&mut out, 0).unwrap();
    assert_eq!(out, b"<<\n  /Kids [\n    1\n  ]\n>>");
}
```

Confirm the exact expected bytes against `Object::write_pdf_qdf`'s own
existing tests in `object.rs` before trusting the literals above — reuse
its exact indent arithmetic (`indent + 2` per nesting level) rather than
re-deriving it.

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --lib object_handle::unparse_object_tests 2>&1 | tail -20`

**Step 3: Implement**

Refactor `unparse_object_value`/`unparse_dict_entries` from Task 4 to take
an `indent: Option<usize>` (`None` = plain, `Some(level)` = QDF at that
level), mirroring `Object::write_pdf_qdf`'s array/dict arms
(`object.rs:566-590`) exactly for the container framing, falling through to
the same scalar-writing arms Task 4 already has for everything else:

```rust
impl ObjectHandle {
    /// QDF-mode counterpart of [`Self::unparse_object`]
    /// (`QPDFWriter::unparseObject` with `m->qdf_mode == true`) --
    /// same function in qpdf, this port's existing split between compact
    /// and QDF serializers (`Object::write_pdf` / `Object::write_pdf_qdf`)
    /// carried forward as the container shape, per
    /// `docs/qpdf-correspondence.md`'s ⚪ entries for that split.
    pub(crate) fn unparse_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        unparse_object_walk_qdf(self, indent, out)
    }
}
```

Add `unparse_object_walk_qdf`/an indent-aware `unparse_dict_entries_qdf`
following `Object::write_pdf_qdf`'s array/dict arms line-for-line, calling
`visible_dict_entries` for suppression and `write_child`'s QDF-aware sibling
(indirect → reference form regardless of qdf mode, matching
`unparse_materialize_child`'s same unconditional behavior) for each
retained value.

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --lib object_handle::unparse_object_tests 2>&1 | tail -15`

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add ObjectHandle::unparse_object_qdf

QDF-mode form of unparseObject, same suppression rule as unparse_object
(flpdf-egzr.3.2.13)."
```

---

### Task 6: `unparse_stream_body`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: same file

**Step 1: Write the failing tests**

```rust
#[test]
fn unparse_stream_body_writes_length_last_preserved() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Filter".to_vec(), ObjectHandle::name(b"FlateDecode".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< /Filter /FlateDecode /Length 3 >>");
}

#[test]
fn unparse_stream_body_refiltered_drops_filter_and_decodeparms_appends_flate() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Filter".to_vec(), ObjectHandle::name(b"ASCIIHexDecode".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(3)),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, true).unwrap();
    assert_eq!(out, b"<< /Length 3 /Filter /FlateDecode >>");
}

#[test]
fn unparse_stream_body_suppresses_a_null_valued_key() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Length".to_vec(), ObjectHandle::integer(3)),
        (b"Metadata".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    dict.unparse_stream_body(&mut out, false).unwrap();
    assert_eq!(out, b"<< /Length 3 >>");
}
```

Cross-check the exact expected bytes against `Dictionary::write_pdf_stream`'s
own shape (`object.rs:913-939`) before trusting the literals — this
primitive must match that layout (`/Length` pulled out and written last;
`/Filter`+`/DecodeParms` dropped when `refiltered`, `/Filter /FlateDecode`
appended after `/Length`), with suppression added on top, applied to
this issue's new callers only (per the design doc, the legacy
`write_pdf_stream` itself is untouched — `flpdf-umye`).

**Step 2: Run to verify failure**

**Step 3: Implement**

```rust
impl ObjectHandle {
    /// This stream-dictionary handle's writer-emission form, matching
    /// qpdf's `/Length`-last, optionally re-filtered stream-dictionary
    /// layout (`QPDFWriter::unparseObject`'s stream branch delegating to
    /// its dictionary branch, `QPDFWriter.cc:1440-1527`, entered with
    /// `flags |= f_stream` and, when `refiltered`, `f_filtered`) --
    /// including the same suppression rule as [`Self::unparse_object`],
    /// since this delegation target is the identical dictionary branch.
    /// `self` must resolve to a `Dictionary`; a non-dictionary value writes
    /// as an empty `<< >>` mirroring `write_pdf_stream`'s own typed-input
    /// assumption (this crate's writer never calls it on anything else).
    pub(crate) fn unparse_stream_body(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
    ) -> Result<()> {
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            unparse_stream_dict_entries(&entries, refiltered, out)
        })
    }
}

fn unparse_stream_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"Length" {
            length_value = Some(value);
            continue;
        }
        if refiltered && (key.as_slice() == b"Filter" || key.as_slice() == b"DecodeParms") {
            continue;
        }
        out.push(b' ');
        crate::object::write_name_escaped(out, key);
        out.push(b' ');
        write_child(value, out)?;
    }
    if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child(length, out)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}
```

**Step 4: Run to verify pass**

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add ObjectHandle::unparse_stream_body

Ports the dictionary branch unparseObject's stream case delegates to
(QPDFWriter.cc:1440-1550), with the same null-suppression rule as
unparse_object (flpdf-egzr.3.2.13). Closes the gap flpdf-umye tracks in
the legacy write_pdf_stream, for this primitive's own callers only."
```

---

### Task 7: `unparse_trailer`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: same file

**Step 1: Write the failing tests**

```rust
#[test]
fn unparse_trailer_classic_forces_id_and_encrypt_last() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (b"Root".to_vec(), ObjectHandle::integer(1)), // stand-in reference shape
        (b"Encrypt".to_vec(), ObjectHandle::integer(9)),
        (b"ID".to_vec(), ObjectHandle::array(vec![
            ObjectHandle::string(vec![0u8; 16]),
            ObjectHandle::string(vec![1u8; 16]),
        ])),
    ]);
    let mut out = Vec::new();
    dict.unparse_trailer(&mut out, false, None).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("trailer << "));
    assert!(text.ends_with(">>"));
    // /Root and /Size appear before /ID, /ID appears before /Encrypt,
    // regardless of the dict's own (alphabetical) key order.
    let root_pos = text.find("/Root").unwrap();
    let id_pos = text.find("/ID").unwrap();
    let encrypt_pos = text.find("/Encrypt").unwrap();
    assert!(root_pos < id_pos);
    assert!(id_pos < encrypt_pos);
}

#[test]
fn unparse_trailer_xref_stream_does_not_write_its_own_open_brace() {
    let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
    let mut out = Vec::new();
    dict.unparse_trailer(&mut out, true, None).unwrap();
    assert!(!String::from_utf8_lossy(&out).contains("<<"));
    assert!(String::from_utf8_lossy(&out).ends_with(">>"));
}

#[test]
fn unparse_trailer_without_id_or_encrypt_omits_both() {
    let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
    let mut out = Vec::new();
    dict.unparse_trailer(&mut out, false, None).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("/ID"));
    assert!(!text.contains("/Encrypt"));
}

#[test]
fn unparse_trailer_does_not_suppress_a_null_valued_key() {
    // writeTrailer has no isNull check anywhere in its key loop
    // (QPDFWriter.cc:1174-1192) -- unlike unparse_object.
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (b"Prev".to_vec(), ObjectHandle::null()),
    ]);
    let mut out = Vec::new();
    dict.unparse_trailer(&mut out, false, None).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("/Prev null"));
}

#[test]
fn unparse_trailer_id_writer_substitutes_the_id_value() {
    let dict = ObjectHandle::dictionary(vec![
        (b"Size".to_vec(), ObjectHandle::integer(9)),
        (b"ID".to_vec(), ObjectHandle::array(vec![
            ObjectHandle::string(vec![0u8; 16]),
            ObjectHandle::string(vec![0u8; 16]),
        ])),
    ]);
    let mut out = Vec::new();
    let mut id_writer = |out: &mut Vec<u8>| out.extend_from_slice(b"<computed>");
    dict.unparse_trailer(&mut out, false, Some(&mut id_writer)).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("/ID <computed>"));
}
```

**Step 2: Run to verify failure**

**Step 3: Implement**

```rust
impl ObjectHandle {
    /// This trailer-shaped dictionary handle's writer-emission form
    /// (`QPDFWriter::writeTrailer`, `QPDFWriter.cc:1160-1236`), covering
    /// both the classic/incremental trailer (`xref_stream = false`, which
    /// writes its own `"trailer <<"` opener) and the xref-stream
    /// dictionary's trailer-shaped keys (`xref_stream = true`, called from
    /// `writeXRefStream` -- `:2489` -- after the caller has already opened
    /// `<<` and written the xref-specific keys; this method still writes
    /// the closing `>>` in both cases, matching `writeTrailer`'s own
    /// `:1235`). No suppression: unlike `unparse_object`, `writeTrailer`'s
    /// key loop (`:1174-1192`) has no `isNull` check -- every key present
    /// is written. `/ID` and `/Encrypt` are excluded from the main loop
    /// and always written last, in that order, when present, matching
    /// `getTrimmedTrailer`'s exclusion and `:1194-1230`'s dedicated
    /// blocks. `id_writer`, when `Some`, substitutes for the stored `/ID`
    /// value; qpdf's own compact `[<hex1><hex2>]` shape is used when it is
    /// `None` (mirroring `write_id_style_value`, `object.rs`).
    ///
    /// Out of scope: `which == t_lin_second` (`QPDFWriter.cc:1170-1172`,
    /// linearization second pass, `/Size`-only) has no equivalent here --
    /// deliberately, not by oversight. A linearization-writer consumer
    /// needing that form is a different primitive.
    pub(crate) fn unparse_trailer(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        id_writer: Option<TrailerIdWriter>,
    ) -> Result<()> {
        self.with_value(|value| {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            unparse_trailer_entries(&entries, xref_stream, id_writer, out)
        })
    }
}

fn unparse_trailer_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    xref_stream: bool,
    mut id_writer: Option<TrailerIdWriter>,
    out: &mut Vec<u8>,
) -> Result<()> {
    if !xref_stream {
        out.extend_from_slice(b"trailer <<");
    }
    let mut id_value: Option<&ObjectHandle> = None;
    let mut encrypt_value: Option<&ObjectHandle> = None;
    for (key, value) in entries {
        match key.as_slice() {
            b"ID" => {
                id_value = Some(value);
                continue;
            }
            b"Encrypt" => {
                encrypt_value = Some(value);
                continue;
            }
            _ => {}
        }
        out.push(b' ');
        crate::object::write_name_escaped(out, key);
        out.push(b' ');
        write_child(value, out)?;
    }
    if let Some(value) = id_value {
        out.extend_from_slice(b" /ID ");
        match id_writer.as_mut() {
            Some(write_id) => write_id(out),
            None => crate::object::write_id_style_value_handle(out, value)?,
        }
    }
    if let Some(value) = encrypt_value {
        out.extend_from_slice(b" /Encrypt ");
        write_child(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}
```

> **[provisional — settled by TDD, not by this document]**
>
> *(implementation-detail sketch)* `write_id_style_value_handle` does not
> exist yet — `object.rs`'s existing `write_id_style_value` takes `&Object`,
> not `&ObjectHandle`. Either add an `ObjectHandle`-typed sibling in
> `object.rs` (promoting whatever shared logic it needs), or inline the
> compact `[<hex1><hex2>]` shape directly in `unparse_trailer_entries` by
> walking the `/ID` array's two string elements via `write_child`'s
> existing machinery plus hex-string framing. Pick whichever keeps the
> `object.rs` change smallest; either is additive, both stay within AC4's
> file scope.
>
> **[/provisional]**

**Step 4: Run to verify pass**

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/object.rs
git commit -m "feat(object_handle): add ObjectHandle::unparse_trailer

Ports QPDFWriter::writeTrailer (QPDFWriter.cc:1160-1236) as one primitive
with an xref_stream flag, covering both the classic/incremental trailer
and the xref-stream dictionary's trailer-shaped keys -- matching qpdf's
own function boundary rather than flpdf's legacy write_pdf_with_id_writer
split (flpdf-egzr.3.2.13). No dictionary-with-id-writer primitive is
added; this closes flpdf-wt2w's root cause once flpdf-egzr.3.2.5 adopts
it for xref-stream output."
```

---

### Task 8: Final gate

**Files:** none (verification only)

**Step 1: Confirm the file-scope gate**

Run: `git diff --name-only main...HEAD`
Expected: exactly `crates/flpdf/src/object_handle.rs` and
`crates/flpdf/src/object.rs` (plus this plan doc and the design doc under
`docs/plans/`).

**Step 2: Full crate test suite**

Run: `cargo test -p flpdf 2>&1 | tail -15`
Expected: all tests pass, zero new failures relative to the Task-2 baseline.

**Step 3: Coverage**

Run whatever this repo's standard coverage command is for changed-line
coverage (check `docs/qpdf-correspondence.md` or recent PRs for the exact
invocation — this repo uses `cargo llvm-cov` with a `qpdf-zlib-compat`
gate for byte-identical work, but that gate is not needed for this
issue's own new code, only for the existing byte-identical corpus check in
Step 4).
Expected: 100% on every line this plan's tasks added or changed.

**Step 4: Byte-identical corpus regression**

Run: `cargo test -p flpdf --features qpdf-zlib-compat 2>&1 | tail -15`
Expected: unchanged from before this branch — this is additive-only work;
no existing byte-identical test should have moved.

**Step 5: `docs/qpdf-correspondence.md` update**

Add a ⚪ row (or amend the existing `object.rs`/`object_handle.rs` row) noting
the new `unparse_object`/`unparse_object_qdf`/`unparse_stream_body`/
`unparse_trailer` primitives and their qpdf citations, per CLAUDE.md
classification (B)'s recording requirement (module-doc line + this table).

**Step 6: `cargo fmt` and final commit**

```bash
cargo fmt
git add -A
git status  # confirm only the expected files changed
git commit -m "docs(qpdf-correspondence): record ObjectHandle writer-emission primitives"
```

**Step 7: Update the beads issue**

```bash
bd update flpdf-egzr.3.2.13 --status=in_review  # or the repo's equivalent close-out step
```

Report the final `git diff --name-only main...HEAD` and test/coverage
results back before considering the issue done.
