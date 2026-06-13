# Linearized classic-xref byte-parity with qpdf Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `flpdf rewrite --linearize --deterministic-id` produce byte-for-byte identical output to `qpdf 11.9.0 --linearize --deterministic-id` on the stream-free fixture corpus (one/two/three-page), which makes the linearized `/ID[1]` match qpdf automatically (beads flpdf-9hc.13.10).

**Architecture:** Diff-driven convergence. The committed qpdf goldens at `tests/golden/references/<stem>/linearize.pdf` are the oracle. flpdf's object renumbering, plain-stream serialization, and hint-table *structure* already match qpdf byte-for-byte; the only gaps are in the linearized classic-xref *layout* (first-page xref coverage, main xref coverage, main trailer keys, final startxref target, param-dict pad) plus one layout-independent hint-dict key-order bug. Fix the structure to match the golden; the param-dict numeric fields (`/L /H /E /T`) and the hint-stream bytes then converge through the existing hint-stream convergence loop.

**Tech Stack:** Rust, `crates/flpdf/src/linearization/writer.rs`, `qpdf-zlib-compat` Cargo feature (links classic libz so FlateDecode bytes match qpdf), qpdf 11.9.0 CLI (golden regeneration + `--check-linearization` regression).

---

## Background: the target layout (verified against goldens 2026-06-13)

Let `param_slot` = the param-dict object number (renumber map; 3/5/7 for one/two/three-page),
`total` = object count incl. the free object 0.

qpdf's `--linearize --deterministic-id` lays the classic (stream-free) file out as:

```
0:            %PDF-1.3\n%<4 binary bytes>\n                       (15-byte header)
15:           {param_slot} 0 obj\n<< /Linearized 1 /L .. /H [ .. .. ] /O .. /E .. /N .. /T .. >>\nendobj\n
              <spaces padding so the next line starts at offset 216>
216:          xref\n{param_slot} {total-param_slot}\n             FIRST-PAGE xref (objects param_slot..total-1)
              <20-byte entries for the whole first-page section>
              trailer << /Info R /Root R /Size {total} /Prev {main_xref_off}<pad> /ID [..] >>\nstartxref\n0\n%%EOF\n
              <first-page section objects: catalog, hint stream, first page, page-1 private>
{/E offset}:  <rest objects: other-pages + Pages + Info, the LOW-numbered objects 0..param_slot-1>
{main off}:   xref\n0 {param_slot}\n                              MAIN xref (objects 0..param_slot-1)
              <20-byte entries: object 0 free head + rest>
              trailer << /Size {param_slot} /ID [..] >>\nstartxref\n{first_page_xref_off=216}\n%%EOF\n
```

Empirical subsections: one-page `xref 3 7` / `xref 0 3`; two-page `xref 5 7` / `xref 0 5`;
three-page `xref 7 7` / `xref 0 7`. Final `startxref` is `216` in all three.

flpdf currently emits (the divergences this plan fixes):
- first-page xref `xref {param_slot} 1` (param dict only) — must cover the whole first-page section.
- main xref `xref 0 {total}` (all objects) — must cover only `0..param_slot`.
- main trailer `/Size {total} /Root R /Info R /ID` — must be `/Size {param_slot} /ID` only.
- final `startxref {main_off}` — must be `startxref 216` (the first-page xref).
- param-dict pad lands the first-page xref at 212 — must be 216.
- hint-stream dict serialized `/Filter /Length /S` (BTreeMap order) — qpdf is `/Filter /S /Length`.

## Diff-driven method (use for every "converge" task)

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc-13-10
cargo build -p flpdf-cli --features flpdf/qpdf-zlib-compat
target/debug/flpdf rewrite --linearize --deterministic-id \
    tests/fixtures/compat/<stem>.pdf /tmp/flpdf-<stem>-lin.pdf
python3 - <<'PY'
a=open("/tmp/flpdf-<stem>-lin.pdf","rb").read()
b=open("tests/golden/references/<stem>/linearize.pdf","rb").read()
n=min(len(a),len(b)); i=next((k for k in range(n) if a[k]!=b[k]), n)
print("lens",len(a),len(b),"first diff",i)
print("flpdf",a[max(0,i-24):i+48]); print("qpdf ",b[max(0,i-24):i+48])
PY
```

Fix the EARLIEST divergence, rebuild, repeat. Do NOT chase `/L /H /E /T` values
directly — they are downstream of the layout and the hint length, and converge
once the structure matches.

## Key files

- Modify: `crates/flpdf/src/linearization/writer.rs`
  - `write_part1_xref_and_trailer` (~line 448) — first-page xref + first trailer
  - `write_main_xref_and_trailer` (~line 542) — main xref + main trailer + final startxref
  - `build_hint_stream_object` (~line 1030) — hint-stream dict
  - `write_linearized` convergence/back-patch (~line 1441+) — wire new offsets
  - existing `patch_first_page_xref` / `FirstPageXrefPatch` (~line 606/914) — mirror the back-patch discipline
- Modify: `crates/flpdf/src/linearization/part1.rs` — param-dict pad target (offset 216)
- Create: `crates/flpdf/tests/cmp_linearize_tests.rs` — golden byte-parity test (model on `cmp_diff_zero_tests.rs`)
- Modify: `tests/golden/compat-matrix.md`, `docs/qpdf-compat-decisions.md` — registry update
- Regression (unchanged, must stay green): `crates/flpdf-cli/tests/cli_linearize_qpdf.rs`

---

### Task 1: Failing golden byte-parity test harness

**Files:**
- Create: `crates/flpdf/tests/cmp_linearize_tests.rs`

**Step 1: Write the test** — model on `crates/flpdf/tests/cmp_diff_zero_tests.rs`. Gate on `qpdf-zlib-compat`. Open the fixture, write with `WriteOptions { linearize: true, deterministic_id: true, ..default }` (mirror how the CLI sets linearized + deterministic options — check `crates/flpdf-cli` and `WriteOptions` field names), compare bytes against `tests/golden/references/<stem>/linearize.pdf` with a `first_diff` panic message identical in spirit to `assert_cmp_diff_zero`. Add three tests (`one_page`, `two_page`, `three_page`).

```rust
#![cfg(feature = "qpdf-zlib-compat")]
// ... open fixture, set opts.linearize = true; opts.deterministic_id = true; ...
// compare write output to tests/golden/references/<stem>/linearize.pdf, first_diff on mismatch
```

**Step 2: Run to verify it FAILS**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests one_page -- --nocapture`
Expected: FAIL, first diff at byte 44 (the param-dict `/L` value), flpdf longer than golden.

**Step 3: Commit the failing test** (it documents the target).

```bash
git add crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "test(flpdf): failing golden byte-parity test for linearized output (flpdf-9hc.13.10)"
```

> NOTE: This test stays red until Task 6/7/8. That is intentional and expected
> per the diff-driven method. Keep it in the suite so each structural fix is
> measured against it.

---

### Task 2: Hint-stream dict key order (layout-independent)

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` `build_hint_stream_object` (~line 1030)

**Step 1: Write a focused unit test** (in writer.rs `#[cfg(test)] mod tests`, or a new unit test) asserting the serialized hint-stream object dict emits keys in order `/Filter /S /Length` (qpdf order), e.g. assert the rendered bytes contain `/Filter /FlateDecode /S <n> /Length <m>` and that `/S` precedes `/Length`.

**Step 2: Run — FAIL** (current BTreeMap serialization yields `/Filter /Length /S`).

**Step 3: Implement** — emit the hint-stream object dict as raw bytes in qpdf key order (mirror the trailer raw-byte idiom used in `write_main_xref_and_trailer`), instead of building a `Dictionary` (which serializes alphabetically). Preserve `/Length` = compressed length and `/S` = shared-section offset.

**Step 4: Run — PASS.**

**Step 5: Commit.**

```bash
git commit -am "fix(flpdf): emit linearized hint-stream dict in qpdf key order /Filter /S /Length (flpdf-9hc.13.10)"
```

---

### Task 3: First-page xref covers the whole first-page section

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` `write_part1_xref_and_trailer` (~448) + its call site / back-patch in `do_write_pass` / `write_linearized`.

**Approach:** Change the Part-1 xref subsection from `xref {param_slot} 1` (param dict only) to `xref {param_slot} {total-param_slot}`, covering objects `param_slot..total` (the first-page section). The offsets of objects after the param dict are forward references not yet known when this section is emitted, so:
- reserve fixed-width 20-byte classic entries (`NNNNNNNNNN GGGGG n \n`) as a placeholder block whose byte length is `(total-param_slot)*20`, exactly like `write_first_page_xref_stream` reserves a deterministic payload;
- record the placeholder byte range (add a struct field analogous to `FirstPageXrefPatch.data_range`);
- back-patch the 20-byte entries in place after the final pass collects all `xref_offsets` (mirror `patch_first_page_xref`). Because the block byte length is invariant, no offset shifts and the hint stream stays the sole convergence variable.

Keep the first trailer's keys/format (`/Info /Root /Size {total} /Prev <padded> /ID`, `startxref 0`); `/Prev` continues to back-patch to the main-xref keyword offset.

**Steps:** Write/extend a unit test asserting the Part-1 xref header is `xref {param_slot} {total-param_slot}` and entry count matches; run (FAIL); implement; run (PASS); then run the Task-1 one-page golden test to observe the diff moving later. Commit.

```bash
git commit -am "feat(flpdf): linearized first-page xref covers the full first-page section (flpdf-9hc.13.10)"
```

---

### Task 4: Main xref + trailer + final startxref

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` `write_main_xref_and_trailer` (~542) + call site.

**Approach:**
- Emit `xref 0 {param_slot}` covering objects `0..param_slot` (free head + rest) instead of `0..total`.
- Main trailer becomes `trailer << /Size {param_slot} /ID [..] >>` — drop `/Root`, `/Info`, keep `/ID` (still file-scoped; the deterministic-`/ID` back-patch span must still cover it).
- Final `startxref` points at the **first-page xref offset** (the `xref` keyword written in Part 1 / the value the first trailer's reader chains from), not the main-xref offset. Thread the first-page xref offset out of `do_write_pass` to the startxref writer.

> The first trailer's `/Prev` already targets the main-xref keyword offset; keep
> that. The chain becomes: final startxref -> first-page xref -> /Prev -> main xref.

**Steps:** Unit test asserts main xref header `xref 0 {param_slot}`, main trailer == `/Size {param_slot} /ID ...` with no `/Root`/`/Info`, and final `startxref` == first-page xref offset; run (FAIL); implement; run (PASS). Commit.

```bash
git commit -am "feat(flpdf): linearized main xref covers rest only; trailer /Size param_slot /ID; startxref -> first-page xref (flpdf-9hc.13.10)"
```

---

### Task 5: Param-dict pad → first-page xref at offset 216

**Files:**
- Modify: `crates/flpdf/src/linearization/part1.rs` (pad reserve/target) and/or back_patch.

**Approach:** qpdf pads the param-dict region so the first-page `xref` keyword starts at offset 216 for the 15-byte `%PDF-1.3` header. flpdf currently lands at 212. Adjust the pad so the post-back-patch first-page xref keyword offset equals the golden's. Confirm the pad is computed from the header+dict width, not hard-coded, so it stays correct if the dict width shifts. (Verify the exact target against the golden with the diff method — it is a fixed 216 across one/two/three-page.)

**Steps:** Diff one-page (the first divergence after Tasks 3-4 should be at/around the pad or first-page xref start); adjust pad; rebuild + diff. Commit.

```bash
git commit -am "fix(flpdf): pad linearized param dict so first-page xref starts at qpdf offset (flpdf-9hc.13.10)"
```

---

### Task 6: Converge one-page to byte-equal

**Step 1:** Run the diff method on `one-page` repeatedly, fixing each earliest remaining divergence (residual padding, hint-table values once offsets align, any object-body or trailer spacing nits).

**Step 2:** `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests one_page` — Expected: PASS (byte-identical, `/ID` matches).

**Step 3: Commit** any residual fixes.

```bash
git commit -am "feat(flpdf): one-page linearized output byte-identical to qpdf (flpdf-9hc.13.10)"
```

---

### Task 7: Converge two-page

Run the diff method on `two-page` (more shared objects → more hint-table entries). Fix divergences. `cargo test ... two_page` PASS. Commit.

---

### Task 8: Converge three-page (acceptance)

Run the diff method on `three-page`. `cargo test ... three_page` PASS — this is the issue's acceptance (`/ID` byte-match). Verify the `/ID` explicitly:

```bash
target/debug/flpdf rewrite --linearize --deterministic-id tests/fixtures/compat/three-page.pdf /tmp/t.pdf
diff <(grep -ao '/ID \[[^]]*\]' /tmp/t.pdf | head -1) \
     <(grep -ao '/ID \[[^]]*\]' tests/golden/references/three-page/linearize.pdf | head -1)
```

Commit.

```bash
git commit -am "feat(flpdf): three-page linearized /ID byte-matches qpdf (flpdf-9hc.13.10)"
```

---

### Task 9: Guard the clean-split precondition

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` (where the two subsections are computed).

**Approach:** The contiguous two-subsection split (`[0,param_slot)` main, `[param_slot,total)` first-page) holds only when every object physically after `/E` is numbered `< param_slot` OR every first-page-section object is numbered `>= param_slot` contiguously — i.e. `part4_rest` is empty so no high-numbered object trails the rest. Assert this precondition; if violated, return `Error::Unsupported` (retain .13.8 self-stability for that input) rather than emitting a corrupt xref. Add a unit test that constructs a plan with a non-empty `part4_rest` high-number trailing case and asserts the `Unsupported` error.

**Steps:** test (FAIL→PASS), implement, commit.

```bash
git commit -am "feat(flpdf): reject non-contiguous linearized split, keep self-stability fallback (flpdf-9hc.13.10)"
```

---

### Task 10: Update compatibility registry

**Files:**
- Modify: `tests/golden/compat-matrix.md` — linearized row `byte-equal=diverge` → `byte-equal` (for the corpus), note the qpdf-zlib-compat gating.
- Modify: `docs/qpdf-compat-decisions.md` — update the `.13.3` "deterministic /ID self-stable but != qpdf" decision to reflect achieved linearized byte-parity (corpus, qpdf-zlib-compat).

Commit.

```bash
git commit -am "docs(flpdf): record linearized byte-parity in compat registry (flpdf-9hc.13.10)"
```

---

### Task 11: Final verification gate

**Step 1: Full relevant test run**

```bash
cargo test -p flpdf --features qpdf-zlib-compat
cargo test -p flpdf                         # default backend still green
cargo test -p flpdf-cli                     # cli_linearize_qpdf.rs regression (needs qpdf)
```
Expected: all PASS; `cli_linearize_qpdf.rs` qpdf `--check-linearization` zero-warning regression stays green.

**Step 2: fmt + clippy**

```bash
cargo fmt --all
cargo clippy --all-targets --features qpdf-zlib-compat -- -D warnings
```

**Step 3: Changed-line coverage (CLAUDE.md gate)**

```bash
scripts/patch-coverage.sh --base main
```
Expected: flpdf changed lines 100% covered. Add tests or `// cov:ignore: <reason>` for any genuinely untestable lines (note the reason in the PR description).

**Step 4:** Confirm `git status` clean, all work committed.

---

## Notes for the executor

- Work entirely in `/home/ubuntu/flpdf/.worktrees/flpdf-9hc-13-10` on branch `flpdf-9hc-13-10-lin-byte-parity` (base = main `d9a4b11`).
- The `qpdf-zlib-compat` feature is mandatory for the golden byte-parity tests; without it the deflate bytes differ and the tests cannot be byte-equal.
- The hint-stream convergence loop must remain the sole degree of freedom: every new xref structure must be emitted at a deterministic byte length (placeholder + in-place back-patch), never a length that depends on the offsets it records.
- @.claude/rules/pdf-rust-review-patterns.md — watch unsigned casts on `total`/`param_slot`, avoid needless clones in the back-patch paths.
