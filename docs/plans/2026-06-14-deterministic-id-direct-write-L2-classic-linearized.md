# For Claude

Layer 2 of the deterministic-`/ID` direct-write epic: convert the **classic**
(stream-free, `objstm_layout.is_empty()`) linearized `--deterministic-id` path
from qpdf's placeholder-then-patch mechanism to qpdf's **2-pass direct-write**
mechanism. Layer 1 already did this for the flat (non-linearized) write paths.

## Goal

For the classic linearized deterministic case, stop emitting an all-zero `/ID`
placeholder and overwriting it after the fact. Instead compute qpdf's
content-derived identifier from the pass-1 digest buffer, then write the real
identifier directly at both `/ID` sites in the final write pass. The ObjStm /
xref-stream linearized path is left on placeholder-then-patch (its qpdf pass-1
layout uses xref streams — a different reconstruction, deferred to a later
layer).

## Hard gate (must stay byte-identical)

`cargo test -p flpdf --features qpdf-zlib-compat` — `cmp_linearize_tests`
(one/two/three-page + lone-flate-l9) must stay byte-identical to the qpdf 11.9.0
goldens at `tests/golden/references/<stem>/linearize.pdf`. These goldens are
produced with `qpdf --linearize --deterministic-id`, so they exercise exactly
the changed classic path. The reorder is a pure no-op on output bytes (same id
value, same 70-byte fixed-width form, same byte position), so the goldens are
unchanged (NOT re-blessed).

## Mechanism

### Why a 2-pass scheme at all

qpdf seeds the linearized `--deterministic-id` from its *first* write pass — a
throwaway buffer with an empty linearization parameter dict, no primary hint
stream, and an unresolved first-page xref (`QPDFWriter::writeLinearized` →
`computeDeterministicIDData`, qpdf 11.9.0; the hint stream is written only
afterwards). flpdf already reconstructs that pass-1 buffer
(`build_pass1_part1` + `do_write_pass(.., pass1_digest = true)`); the only
change here is *when* the resulting identifier is used.

### The reorder (`write_linearized`, `crates/flpdf/src/linearization/writer.rs`)

1. **Before the convergence loop**, for the classic deterministic case, build
   the pass-1 digest buffer and compute `(id0, id1)` via
   `crate::writer::compute_deterministic_id(&pass1_bytes, pass1_bytes.len() - 1,
   info_suffix, source_id0)`. Store it in `classic_det_id:
   Option<([u8; 16], [u8; 16])>`. The pass-1 buffer is loop-invariant — it
   carries no hint stream, so it never depends on hint convergence — so this is
   hoisted above the loop. The pass-1 buffer itself keeps the all-zero `/ID`
   placeholder (its trailer writers get `id_writer = None`), exactly as qpdf's
   pass 1 does.
2. **Probe passes** run with `id_writer = None` (placeholder via
   `source_trailer["ID"]`). They only measure object byte lengths for hint
   convergence; the placeholder is the same fixed width as the final
   direct-written identifier, so probe offsets match the final pass.
3. **Final pass** builds a closure `|out| write_deterministic_id_array(out,
   &id0, &id1)` from `classic_det_id` and passes it as `id_writer = Some(..)`.
   Both trailer-writing functions call it at the `/ID` value position instead of
   serializing the stored placeholder.
4. The post-loop classic `build_pass1_part1` + `patch_linearized_deterministic_id`
   block is **deleted**. The ObjStm branch is kept unchanged.

### Fixed-width hex at the two sites

The two classic `/ID` sites are `write_part1_xref_and_trailer` and
`write_main_xref_and_trailer`, which write the trailer as raw bytes (to preserve
qpdf's key order and the fixed-width `/Prev` field). Each gained an
`id_writer: Option<crate::object::ReborrowableIdWriter>` parameter; when `Some`,
the closure emits the value at the `/ID` position, otherwise `id_obj.write_pdf`.

The closure MUST call `write_deterministic_id_array` (the forced-hex
`[<id0_hex><id1_hex>]`, fixed 70 bytes = `DETERMINISTIC_ID_ARRAY_LEN`), NOT
`write_deterministic_id_inline` (L1's flat helper). The inline helper re-digests
bytes-so-far at each call, which would give the two sites different ids and the
wrong digest range (a linearized file has no single `[` cutoff). The forced-hex
form guarantees the value is the same width as the placeholder regardless of its
bytes — a literal `(...)` from `Object::write_pdf` on an all-printable digest
would corrupt every downstream offset (`/L`, `/T`, `startxref`, hint stream).

### Lifetime note

`crate::object::TrailerIdWriter<'a> = &'a mut dyn FnMut(&mut Vec<u8>)` couples
the borrow and trait-object lifetimes, so it cannot be reborrowed and forwarded
to two callees. A new alias `ReborrowableIdWriter<'r, 'd> = &'r mut (dyn
FnMut(&mut Vec<u8>) + 'd)` decouples them. `do_write_pass` reborrows it
(`as_deref_mut()`) for the first `/ID` site and moves it into the last.

## Tests

- `deterministic_id_linearized_classic_direct_writes_no_placeholder` (new):
  asserts no all-zero placeholder array survives anywhere in the classic
  output, both `/ID` sites carry the real byte-equal identifier, and the output
  is byte-stable across runs.
- Existing classic suite (`deterministic_id_linearized_is_self_stable`,
  `_depends_on_content`, `_preserves_source_permanent_id`,
  `_id0_equals_id1_without_source_id`, `_info_seed_changes_id`,
  `_no_info_boundary`, `_classic_main_trailer_has_id`,
  `_does_not_clobber_body_placeholder`) — all stay green; they pin the id
  value, content-dependence, and the crafted-decoy regression.
- ObjStm path (`deterministic_id_linearized_xref_stream_is_self_stable`,
  `_all_ids_match`) — unchanged, still placeholder-then-patch.
- Hard gate `cmp_linearize_tests` — byte-identical to qpdf goldens.

## Result

cmp_linearize byte-identical (goldens not re-blessed). Patch coverage: flpdf
changed lines 100%. One `cov:ignore` on the hoisted pass-1 `do_write_pass` error
arm (unreachable — pass-1 mode only omits emission relative to passes that
already succeeded on the same inputs).
