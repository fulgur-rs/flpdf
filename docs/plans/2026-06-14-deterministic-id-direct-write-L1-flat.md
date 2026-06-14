# Deterministic /ID direct-write — Layer 1 (FLAT paths) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** On the FLAT write paths, write qpdf's real deterministic `/ID` directly
at serialization time (no all-zero placeholder, no post-hoc byte-search), so the
`/ID`-mislocate concern (flpdf-9hc.13.12) is structurally impossible.

**Architecture:** qpdf's `generateID` mechanism — at the `/ID` value site, push
`[`, MD5-digest the bytes written so far (inclusive of `[`), compute the two-level
id, write `<id0><id1>]`. Output is byte-identical to today for benign files
(same digest range, same id values); only the *sourcing* changes from
placeholder+patch to inline direct-write. This is Layer 1 of an epic; L2
(classic-linearized, flpdf-u5m8), L3 (ObjStm full byte-parity, flpdf-vvjr), L4
(ObjStm /ID parity, flpdf-ari7) follow.

**Tech Stack:** Rust, `md5` crate, existing `compute_deterministic_id`
(UNCHANGED), `push_hex_lower`.

**Scope (Layer 1 only):** classic trailer (`Dictionary::write_pdf_trailer`,
object.rs:686), qdf trailer (`write_qdf_trailer`, writer.rs:3729), xref-stream
flat dict (`write_stream_to_buf` → `Dictionary::write_pdf`, object.rs:619).
NOT linearized (L2/L4).

---

### Task 1: Inline /ID direct-write helper

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (near `compute_deterministic_id`, ~2211)
- Test: same file `#[cfg(test)] mod tests`

**Step 1: Write the failing test**

```rust
#[test]
fn write_deterministic_id_inline_matches_placeholder_then_patch_digest_range() {
    // Inline direct-write must produce the SAME id bytes as the legacy
    // placeholder-then-patch for an identical prefix, proving the digest range
    // is inclusive of '[' exactly as compute_deterministic_id expects.
    let mut prefix = b"%PDF-1.7\n... body ...\ntrailer << /Size 4 /Root 1 0 R /ID ".to_vec();
    let mut inline = prefix.clone();
    write_deterministic_id_inline(&mut inline, b"", None);
    // Legacy: same prefix, placeholder at '[', then compute over [..='[']
    let id_off = prefix.len(); // where '[' will go
    let mut legacy = prefix.clone();
    write_deterministic_id_array(&mut legacy, &[0u8; 16], &[0u8; 16]); // placeholder
    let (id0, id1) = compute_deterministic_id(&legacy, id_off, b"", None);
    let mut expect = prefix.clone();
    write_deterministic_id_array(&mut expect, &id0, &id1);
    assert_eq!(inline, expect, "inline direct-write must equal placeholder+patch result");
}
```

**Step 2: Run to verify it fails** — `cargo test -p flpdf write_deterministic_id_inline_matches` → FAIL (fn missing).

**Step 3: Implement**

```rust
/// Direct-write qpdf's deterministic `/ID` array value INLINE at the current
/// output position (mirrors `QPDFWriter::generateID`): push `[`, digest the
/// bytes written so far (inclusive of `[`, the range
/// [`compute_deterministic_id`] expects), compute the two-level id, then write
/// `<id0_hex><id1_hex>]`. Replaces the placeholder-then-byte-search scheme on
/// the flat paths, so a crafted placeholder-shaped byte run elsewhere can never
/// be mistaken for `/ID`.
pub(crate) fn write_deterministic_id_inline(
    out: &mut Vec<u8>,
    info_suffix: &[u8],
    source_id0: Option<[u8; 16]>,
) {
    out.push(b'[');
    let id_array_offset = out.len() - 1; // index of the just-pushed '['
    let (id0, id1) = compute_deterministic_id(out, id_array_offset, info_suffix, source_id0);
    out.push(b'<');
    push_hex_lower(out, &id0);
    out.push(b'>');
    out.push(b'<');
    push_hex_lower(out, &id1);
    out.push(b']'); // note: closing '>' of id1 written below
}
```

NOTE during impl: ensure the byte sequence is exactly `[<id0><id1>]` (32 hex each,
no inner spaces) — mirror `write_deterministic_id_array`. Fix the `>`/`]` ordering
so it emits `<id0_hex><id1_hex>]` (the snippet above is a sketch; match
`write_deterministic_id_array` byte-for-byte).

**Step 4: Run** — `cargo test -p flpdf write_deterministic_id_inline_matches` → PASS.

**Step 5: Commit** — `feat(flpdf): add inline deterministic /ID direct-write helper (flpdf-9hc.13.12 L1)`

---

### Task 2: Route classic trailer through the helper

**Files:**
- Modify: `crates/flpdf/src/object.rs:686-704` (`write_pdf_trailer`) — add
  `id_writer: Option<&mut dyn FnMut(&mut Vec<u8>)>`; at the deferred `/ID` site,
  if `Some(f)` write `" /ID "` then `f(out)`, else current `value.write_pdf(out)`.
- Modify: `crates/flpdf/src/writer.rs:3273-3275` (flat classic trailer caller):
  build the closure `|o| write_deterministic_id_inline(o, &det_id_info_suffix, det_id_source_id0)`
  and pass `Some(&mut closure)` when `options.deterministic_id`, else `None`.
- Modify callers/tests of `write_pdf_trailer` to pass `None`.

**Step 1: failing test** — full-rewrite a fixture with `deterministic_id=true`
(classic trailer), assert (a) no all-zero placeholder `[<0x00..32><0x00..32>]`
in output, (b) `/ID` equals the digest-derived value, (c) output byte-identical
to the pre-change placeholder+patch output for the same fixture (capture a golden
const from current code first).

**Steps 2-5:** verify fail → implement → pass → commit. Run the EXISTING
deterministic-id tests (`deterministic_id_*`) to confirm byte-identity.

---

### Task 3: Route qdf trailer through the helper

**Files:**
- Modify: `crates/flpdf/src/writer.rs:3729-3753` (`write_qdf_trailer`) — same
  `id_writer` param + deferred `/ID` site.
- Modify: `writer.rs:3266` caller to pass the closure when deterministic.

**Steps:** failing test (qdf + deterministic_id, byte-identical + no placeholder)
→ implement → pass → commit.

---

### Task 4: Route xref-stream flat dict through the helper

**Files:**
- Modify: `crates/flpdf/src/object.rs:619` — add
  `Dictionary::write_pdf_with_id_writer(&self, out, id_writer)` mirroring
  `write_pdf` but, at the `/ID` key in its lexicographic position, invoking the
  closure instead of `value.write_pdf`. (Keep `write_pdf` unchanged; the new
  method delegates for all non-/ID keys.)
- Modify: `writer.rs:3604` `write_stream_to_buf` — add a det-id-aware variant (or
  param) so the xref-stream dict serialization at `writer.rs:3441-3443` routes
  `/ID` through the helper. The digest range (`..='['`) lands mid-dict, exactly
  as the current anchor search targets.

**Steps:** failing test — full-rewrite a fixture that emits an xref STREAM
(`XrefForm::Stream`) with `deterministic_id=true`; assert no placeholder + `/ID`
is digest-derived + byte-identical to current placeholder+patch output → implement
→ pass → commit.

---

### Task 5: Delete the placeholder + byte-search machinery (flat)

**Files:**
- Delete `apply_deterministic_id_placeholder` (writer.rs:1540), its calls
  (writer.rs:2337), `patch_deterministic_id` (writer.rs:2257) and its two call
  sites (writer.rs:3278, 3446).
- Remove the flat dependence on `DETERMINISTIC_ID_ARRAY_LEN` (keep the const only
  if still used by linearized L2/L4; otherwise move/scope it).
- Remove now-dead flat tests (`patch_deterministic_id_targets_id_not_earlier_placeholder`
  writer.rs:4329 etc.) — replaced by Task 6 crafted-string tests.

**Steps:** compile (failing on removed-symbol refs) → fix refs → `cargo test -p flpdf` green → commit.

---

### Task 6: Crafted-string regression tests (the .13.12 acceptance)

**Files:**
- Test: `crates/flpdf/tests/deterministic_id_*` (new test file or extend existing).

A crafted SOURCE trailer key `/Probe (… /ID [<0x00…32><0x00…32>] …)` survives into
flpdf's output (verified: clone-and-strip preserves arbitrary keys —
object.rs:686 `write_pdf_trailer` writes all keys verbatim). Assert under
`deterministic_id=true`: (a) the `/Probe` string is intact in output, (b) the real
`/ID` is the digest value (not all-zero), for BOTH classic trailer and xref-stream
flat. NON-VACUOUS: assert the crafted literal actually appears in output first.

**Steps:** write → it must PASS already (direct-write makes mislocate impossible)
→ if it fails, the injection didn't land (fix the fixture) → commit.

---

### Task 7: Coverage + full verification

**Steps:**
- `cargo test -p flpdf` and `cargo test -p flpdf --features qpdf-zlib-compat`
  (deterministic-id parity goldens still green — flat /ID values UNCHANGED).
- `cargo fmt` (CI gate).
- `scripts/patch-coverage.sh --base main` — changed flpdf lines 100%; `cov:ignore`
  only truly-unreachable arms with a reason.
- Commit, then proceed to Layer 2 (flpdf-u5m8).
