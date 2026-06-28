<!-- For Claude: this plan implements flpdf-5apf Layer A. Oracle model + scope in the
     beads issue design field (bd show flpdf-5apf). Layer B = flpdf-0gyq. -->

# flpdf-5apf — drop / null-ize dangling body refs in the linearization writer (Layer A)

## Goal

Stop `rewrite --linearize` from (a) exiting 2 on a dangling/object-0 body ref and
(b) keeping a dead ref + stray null object. Match qpdf 11.9.0 byte-for-byte for the
tractable cases; emit inline `null` for the one intricate case (missing-xref array
element), deferred to Layer B (flpdf-0gyq) as a documented deviation.

## Confirmed oracle (qpdf 11.9.0) — position × ref-kind

A **null-resolving** ref = object-0 (`0 0 R`) OR missing-xref (`N 0 R`, N>0, no xref entry).

|                  | dict VALUE | ARRAY element                                  |
| object-0         | drop key   | inline `null`                                  |
| missing-xref N>0 | drop key   | qpdf: resurrect null obj (Layer B) — Layer A: inline `null` |

Dict-key drop is recursive (`<< /Inner DANGLING >>` -> `<< >>`). Real direct-`null`
literals are NOT touched here (separate concern, flpdf-v58c).

## Root cause (verified empirically)

- `0 0 R` / missing-xref refs in the **first-page closure** are pushed to `compute_closure`'s
  `order` (plan.rs:189 / resources DFS 240), get a plan slot, and are emitted as a stray
  null object (disable: `/Bad 8 0 R` + obj 8 = `null`). Confirmed: `new_for_original(0 0 R)`
  returns `Some(8 0 R)` in disable mode.
- Refs outside the first-page closure (e.g. a dangling Catalog `/Junk`) never enter the
  plan, so they reach **emission** (`renumber_object`, writer.rs:414) with no map entry and
  hit the hard `Unsupported` error (generate mode exit 2).

## Two-layer fix

### 1. Plan: `compute_closure` (linearization/plan.rs)
Compute `let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();`
once before the BFS. Skip admitting a ref to `order` when
`r.number == 0 || !live.contains(&r)` — at both the main loop (after `visited.insert`,
before `order.push`) and the `/Resources` DFS (before its `order.push(r)`). These refs
become no body object, matching qpdf.

### 2. Emission: `renumber_object` (linearization/writer.rs)
Thread `live: &BTreeSet<ObjectRef>` (computed once in `do_write_pass`, passed to the 6
`renumber_object` calls + `append_objstm_container_object`, which forwards it to its call).
Define `is_null_resolving(r, live) = r.number == 0 || !live.contains(&r)`.

- `Object::Reference(r)`, None arm: if `is_null_resolving` -> `Ok(Object::Null)` (inline,
  for array elements / bare-ref bodies); else keep the existing `Unsupported` error
  (live-object-missing-from-map safety net — do NOT silently drop).
- `Object::Dictionary`: for each `(key, value)`, if value is a null-resolving reference
  (`matches!(value, Object::Reference(r) if is_null_resolving(*r, live))`) -> `continue`
  (drop the key); else recurse.
- `Object::Stream` dict: keep the `/Length` direct-ization special-case; otherwise apply
  the same drop-the-key rule for null-resolving reference values.
- `Object::Array`: unchanged structurally — each element recurses; a null-resolving
  reference element falls through the Reference arm to inline `Object::Null`.

## Tests

### A. Structural / de-crash (default build, no qpdf-zlib-compat) — new `dangling_body_ref_linearize_tests.rs`
Build small PDFs in-memory (Cursor) per `linearize_classic_tests.rs`, linearize via
`write_linearized` with `LinearizationPlan::from_pdf(.., use_generate)` for both
`use_generate=false` and `true`; assert on back-patched bytes / re-parsed `Pdf`:
1. dict-value object-0 (`/Bad 0 0 R`) -> no error; `/Bad` absent; no stray null object
   (object count == live count); round-trips.
2. dict-value missing-xref (`/Junk 99 0 R`) -> no error; `/Junk` absent.
3. nested dict dangling (`/Nested << /Inner 99 0 R >>`) -> `/Nested` present but empty.
4. array object-0 / missing-xref (`/Arr [0 0 R <font> 99 0 R]`) -> no error; array length
   preserved; dead slots are inline `null`.
5. safety net: a ref to a live-but-not-in-map object still errors (construct via a unit
   test on `renumber_object` with a non-empty `live` and an unmapped live ref).

### B. Byte-parity (qpdf-zlib-compat gated) — fixtures + qpdf goldens
Add fixtures to `tests/fixtures/compat/` and bless goldens via `tests/golden/regenerate.sh`
(qpdf 11.9.0, both `--linearize` and `--object-streams=generate --linearize`). Add cases to
`cmp_linearize_tests.rs` (classic) and `cmp_linearize_objstm_tests.rs` (generate). Wire the
new gated tests into `.github/workflows/ci.yml` (they are not auto-discovered — see memory
`flpdf-ci-bytes-identical-explicit-test-list`). Note the missing-xref-array fixture asserts
the Layer-A interim (inline null); Layer B (flpdf-0gyq) re-blesses it to the resurrected-null
golden.

## Quality gates
`cargo test -p flpdf`, `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests`,
`cargo fmt`, `cargo clippy`, `scripts/patch-coverage.sh` (changed lines 100% in flpdf).
