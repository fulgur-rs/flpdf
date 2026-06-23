# Preserve non-16-byte /ID[0] under --deterministic-id Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `--deterministic-id` preserve a source `/ID[0]` of any length verbatim (qpdf `getOriginalID1` parity), instead of only when it is exactly 16 bytes.

**Architecture:** qpdf writes the permanent identifier `/ID[0]` verbatim regardless of length and only ever generates a 16-byte changing identifier `/ID[1]`. flpdf currently constrains `/ID[0]` to 16 bytes because the deterministic `/ID` array is a fixed width (`DETERMINISTIC_ID_ARRAY_LEN`). We make the id0 portion variable-width: `source_permanent_id` returns the verbatim bytes (`Option<Vec<u8>>`), all callers take/pass borrows (`Option<&[u8]>`, never `Option<Vec<u8>>` by value — the `id_writer` FnMut closures capture by value and cannot move a non-Copy out per call), the fixed-width const becomes a per-call helper, and the linearized pass-1 zero placeholder is sized to the source id0 length so its digested bytes match qpdf's pass-1.

**Tech Stack:** Rust, `md5` crate, qpdf 11.9.0 as oracle, `qpdf-zlib-compat` feature for byte-parity goldens.

**Risk split (verified empirically with qpdf 11.9.0 on a 20-byte `/ID[0]` fixture, /tmp/nonid.pdf):**
- FLAT = low risk. flat digest = `md5(bytes[..=offset_of '['])`; id0 is written AFTER `[`, so id0 length cannot touch the digest → `/ID[1]` is unchanged. Only `source_permanent_id` returning the verbatim bytes is needed.
- LINEARIZED = the real work. Pass-1 runs with `id_writer = None`, so the stored `/ID` placeholder bytes are inside the digested range; placeholder width AND content must be byte-exact to qpdf pass-1 (a same-length zero placeholder for id0). The committed linearized golden is the arbiter.

**Verified raw serialization (both paths):** `/ID [<id0_hex><id1_hex>]` — one space after `/ID`, no spaces inside the array. Linearized emits this at TWO sites (first-page xref dict + main xref dict), same value.

**Out of scope (file as a separate follow-up):** qpdf `getOriginalID1` reads element 0 only (ignores element 1's type/arity); flpdf's "both strings, len == 2" guard is a pre-existing divergence independent of length. Do NOT fold it in here.

**CI wiring facts (verified):**
- `cmp_linearize_tests` IS already in `.github/workflows/ci.yml` (gated list) → adding a test method auto-runs in CI; no ci.yml change for the linearized case.
- `deterministic_id_qpdf_parity_tests` is NOT in ci.yml → adding the flat case there requires a one-line ci.yml addition (Task 6). Its golden constants are pure md5 of a stream-free fixture (libz-independent), so adding the whole file to CI is safe.

---

## Task 1: Add the non-16-byte fixture + linearized qpdf golden

**Files:**
- Create: `tests/fixtures/compat/nonid-id0.pdf` (1-page PDF; trailer `/ID[0]` = 20 bytes (40 hex), `/ID[1]` = 16 bytes; include `/Info` with a `/Producer` string)
- Create: `tests/golden/references/nonid-id0/linearize.pdf` (`qpdf --linearize --deterministic-id`)
- Modify: `tests/golden/regenerate.sh` (add a block generating the linearize golden for this fixture)

**Step 1: Build the source fixture.** Use this exact construction (validated during design: `qpdf --check` accepts it, qpdf preserves the 20-byte id0):
```python
objs = [
  b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
  b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
  b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>\nendobj\n",
  # obj 4: a content stream
  # obj 5: << /Producer (handmade) >>  (the /Info)
]
# header "%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"; classic xref; trailer:
#   << /Size N /Root 1 0 R /Info 5 0 R /ID [<aa*20><bb*16>] >>
```
(id0 = `"aa"*20` = 40 hex = 20 bytes; id1 = `"bb"*16` = 32 hex = 16 bytes.)

**Step 2: Generate the linearized golden with qpdf 11.9.0.**
```bash
mkdir -p tests/golden/references/nonid-id0
qpdf --linearize --deterministic-id --warning-exit-0 \
    tests/fixtures/compat/nonid-id0.pdf tests/golden/references/nonid-id0/linearize.pdf
```
Verify id0 preserved verbatim (40 hex), id1 a 16-byte digest, at BOTH /ID sites:
```bash
grep -ao "/ID \[<[0-9a-f]*><[0-9a-f]*>\]" tests/golden/references/nonid-id0/linearize.pdf
```

**Step 3: Wire into `regenerate.sh`** following the existing per-fixture pattern (the `one-page` block uses `qpdf --linearize --deterministic-id ... "$REF/<stem>/linearize.pdf"`). Add a `nonid-id0` block. Keep `--warning-exit-0`.

**Step 4: Commit.**
```bash
git add tests/fixtures/compat/nonid-id0.pdf tests/golden/references/nonid-id0/ tests/golden/regenerate.sh
git commit -m "test(flpdf-9hc.13.11): add non-16-byte /ID[0] fixture + qpdf linearize golden"
```

---

## Task 2: RED — flat: invert unit test + add flat byte-parity (live qpdf)

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (test `deterministic_id_ignores_non_16_byte_source_id` at ~`:4705`)
- Modify: `crates/flpdf/tests/deterministic_id_qpdf_parity_tests.rs` (add a non-16-byte fixture builder + golden id0/id1 constants + a golden test + live-qpdf full-byte-parity test, mirroring `one_page_with_info_fixture` / `deterministic_id_matches_live_qpdf_when_available`)

**Step 1: Invert the flat unit test** → rename to `deterministic_id_preserves_non_16_byte_source_id`:
```rust
#[test]
fn deterministic_id_preserves_non_16_byte_source_id() {
    // qpdf's getOriginalID1 preserves /ID[0] verbatim regardless of length;
    // only /ID[1] is regenerated (always a 16-byte md5).
    let src = build_det_id_source(
        &format!("/ID [<{}><{}>]", "aa".repeat(20), "bb".repeat(16)),
        &[],
    );
    let out = write_det_id(&src);
    let (id0, id1) = trailer_id_pair(&out);
    assert_eq!(id0, vec![0xAAu8; 20], "/ID[0] must be preserved verbatim (20 bytes)");
    assert_eq!(id1.len(), 16, "/ID[1] is always a 16-byte digest");
    assert_eq!(id1, expected_changing_id(&out, b"").to_vec());
    assert_ne!(id0, id1);
}
```

**Step 2: Add the flat byte-parity test** to `deterministic_id_qpdf_parity_tests.rs`:
- `fn one_page_non16_id0_fixture() -> Vec<u8>`: stream-free 1-page classic PDF, trailer `/ID [<aa*20><bb*16>] /Root 1 0 R` (no `/Info`, so seed suffix is empty — keeps it simple).
- Capture goldens from live qpdf during dev: `qpdf --deterministic-id --object-streams=disable` on the EXACT builder bytes; `extract_id_words` → `GOLDEN_NON16_ID0` (40 hex `aa…`), `GOLDEN_NON16_ID1` (32 hex).
- `#[test] deterministic_id_non16_id0_matches_qpdf_golden_id_words` (asserts id0 == 40-hex `aa…`, id1 == captured constant).
- `#[test] deterministic_id_non16_id0_matches_live_qpdf_when_available` (full byte parity when qpdf on PATH, self-skip otherwise).

**Step 3: Run — expect RED.**
```bash
cargo test -p flpdf --lib deterministic_id_preserves_non_16_byte_source_id
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests deterministic_id_non16
```
Expected: FAIL (current code regenerates id0; `id0 == [0xAA;20]` and id0-hex assertions fail).

**Step 4: Commit RED.**
```bash
git add crates/flpdf/src/writer.rs crates/flpdf/tests/deterministic_id_qpdf_parity_tests.rs
git commit -m "test(flpdf-9hc.13.11): RED non-16-byte /ID[0] preservation (flat)"
```

---

## Task 3: GREEN — variable-width id0 plumbing (shared + flat)

These signatures are shared across both paths, so they must change together to compile.

**Files: `crates/flpdf/src/writer.rs`**
- `source_permanent_id` (`:2198`): `-> Option<Vec<u8>>`; return `Some(first.clone())` (any length) when `/ID` is a 2-element array of two strings. Update the doc comment (drop the 16-byte rationale; cite qpdf getOriginalID1 verbatim-any-length).
- `compute_deterministic_id` (`:2283`): `source_id0: Option<&[u8]>` → returns `(Vec<u8>, [u8;16])`; `let id0 = source_id0.map(<[u8]>::to_vec).unwrap_or_else(|| id1.to_vec());`.
- `write_deterministic_id_inline` (`:2315`): `source_id0: Option<&[u8]>`; write variable-length id0 hex then 16-byte id1 hex.
- `write_deterministic_id_array` (`:2181`): `id0: &[u8]` (id1 stays `&[u8;16]`).
- `id_writer` closures (`:3443/:3464/:3633/:3941`): pass `det_id_source_id0.as_deref()`.
- Capture sites (`:2762/:3794`): keep `source_permanent_id(...)` (now yields `Option<Vec<u8>>`); the closures deref via `.as_deref()`.
- `apply_deterministic_id_placeholder` (`:1606`): LEAVE 16+16 zeros (value overridden by the hook, never serialized on the flat path — verify there's no non-hook trailer serialization for deterministic flat).
- Fix any now-stale unit-test call sites in writer.rs that pass `[u8;16]` to the changed signatures (e.g. `write_deterministic_id_array(&mut .., &id0, &id1)` at `:4921/:4924/:4932/:4951/:4954/...` — `&[u8;16]` coerces to `&[u8]`, so these likely still compile; confirm).

**Step 1: Apply the changes.**

**Step 2: Run flat tests — expect GREEN.**
```bash
cargo build -p flpdf
cargo test -p flpdf --lib deterministic_id
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
```
Expected: inverted unit test passes; flat byte-parity passes; all other deterministic_id unit tests stay green; the existing 16-byte golden constants in deterministic_id_qpdf_parity_tests stay green (no regression).

**Step 3: Read first-diff if RED.** flat diff at id0 = `source_permanent_id` wrong bytes; diff at id1 = digest range wrong (should not happen on flat — id0 is after `[`).

**Step 4: Commit.**
```bash
git add crates/flpdf/src/writer.rs
git commit -m "feat(flpdf-9hc.13.11): preserve non-16-byte /ID[0] on flat deterministic-id path"
```

---

## Task 4: RED — linearized: invert unit test + add committed-golden parity

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` (test `deterministic_id_linearized_preserves_source_permanent_id` at `:4465`; the `ID0_HEX`/`ID1_HEX` slice consts assume a 32-hex id0 — for a 20-byte fixture either compute the id0 span as `2*20=40` hex or add a sibling test that does)
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs` (add `#[test] fn nonid_id0_linearized_is_byte_identical_to_qpdf() { assert_linearize_byte_identical("nonid-id0.pdf", "nonid-id0"); }` — the strict path does NOT use `mask_id1`, so the non-32-hex id0 is fine)

**Step 1: Add the committed-golden linearized test** (`assert_linearize_byte_identical`, the arbiter for the hard path).

**Step 2: Add/extend a linearized unit test** asserting the 20-byte id0 is preserved at the first `/ID` site (compute the id0 hex span from `2*20 = 40` hex chars, not the fixed 32).

**Step 3: Run — expect RED.**
```bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests nonid
```
Expected: FAIL. First diff likely at `/ID[1]` (sizes equal) = pass-1 placeholder width wrong; OR at id0 = `patch_linearized_deterministic_id`/direct-write width wrong; OR a length difference if the placeholder is mis-sized.

**Step 4: Commit RED.**
```bash
git add crates/flpdf/src/linearization/writer.rs crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "test(flpdf-9hc.13.11): RED non-16-byte /ID[0] preservation (linearized)"
```

---

## Task 5: GREEN — variable-width id0 on the linearized path

**Files:**
- `crates/flpdf/src/writer.rs`
  - Replace `pub(crate) const DETERMINISTIC_ID_ARRAY_LEN: usize` with `pub(crate) fn deterministic_id_array_len(id0_len: usize) -> usize { 1 + (1 + 2*id0_len + 1) + (1 + 32 + 1) + 1 }` (= 70 at id0_len=16). Keep a `DETERMINISTIC_ID_ARRAY_LEN` alias only if many 16-byte test sites depend on it; otherwise migrate those sites to `deterministic_id_array_len(16)`.
- `crates/flpdf/src/linearization/writer.rs`
  - `finalize_linearized_id` (`:939`): deterministic arm installs `[<0×(2*len0)><0×32>]` sized to the source id0 length. Read the source id0 length before the placeholder overwrites `/ID` (the `det_id_source_id0` capture at `:2714` already reads it — pass its length in, or have `finalize_linearized_id` read `source_trailer["ID"][0]` length directly).
  - `classic_det_id` (`:2747`): `Option<(Vec<u8>, [u8;16])>`.
  - direct-write site (`:3257`): `write_deterministic_id_array(out, &id0, &id1)` — id0 now `&[u8]`.
  - `patch_linearized_deterministic_id` (`:1001`): `id0: &[u8]`; `let len = deterministic_id_array_len(id0.len());`; build placeholder/final via `write_deterministic_id_array`; patch-scan stride uses `len` (`while i + len <= end`).
  - `id_ranges` recording: ensure each `/ID` span width matches the emitted width (search where ranges are pushed; the "exactly one placeholder" debug_assert at `:1035` fires on mismatch).
  - migrate test helpers using `DETERMINISTIC_ID_ARRAY_LEN` (`:4116/:4137/:4218/:4510/:4699/:4754`) to `deterministic_id_array_len(16)`.

**Step 1: Apply the changes.**

**Step 2: Run — expect GREEN + no regression.**
```bash
cargo build -p flpdf
cargo test -p flpdf --lib deterministic_id
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
```
Expected: all green, including the existing 16-byte linearize goldens.

**Step 3: Read first-diff if RED** (memory: id1-with-equal-sizes = placeholder shaping; id0 = patch/direct-write width).

**Step 4: Commit.**
```bash
git add crates/flpdf/src/writer.rs crates/flpdf/src/linearization/writer.rs
git commit -m "feat(flpdf-9hc.13.11): preserve non-16-byte /ID[0] on linearized deterministic-id path"
```

---

## Task 6: CI wiring, coverage gate, full verification

**Files:**
- Modify: `.github/workflows/ci.yml` (add `cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests` to the gated list, next to `cmp_diff_zero_tests` — it currently is NOT run in CI; the linearized case rides on the already-listed `cmp_linearize_tests`).

**Step 1: Add the flat parity test file to ci.yml.**

**Step 2: Full unit + gated test run.**
```bash
cargo test -p flpdf
cargo test -p flpdf --features qpdf-zlib-compat
```

**Step 3: fmt + clippy.**
```bash
cargo fmt --all && cargo fmt --all --check
cargo clippy -p flpdf --all-targets
```

**Step 4: Patch-coverage gate (commit first; run WITHOUT qpdf-zlib-compat per repo policy).**
```bash
scripts/patch-coverage.sh --base main
```
Expected: changed lines in `flpdf` 100% covered. The non-16-byte preservation branch is covered by the inverted unit test; the variable-width helper by unit + golden tests.

**Step 5: Commit.**
```bash
git add .github/workflows/ci.yml
git commit -m "ci(flpdf-9hc.13.11): run deterministic-id /ID parity tests in CI"
```

---

## Acceptance (from the issue)
- A PDF with a non-16-byte source `/ID[0]` under `--deterministic-id` preserves `/ID[0]` verbatim and only `/ID[1]` changes — flat AND linearized. ✓ (Tasks 2–5)
- Feature-gated (`qpdf-zlib-compat`) parity vs qpdf for such a fixture. ✓ (Tasks 1, 2, 4, 6)
