# flpdf-9hc.16.13: overlay/underlay + --qdf + --no-original-object-ids byte-parity — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add 3 library-layer byte-parity gates in `overlay::byte_gate` for the `--qdf --no-original-object-ids` overlay/underlay recipe, so regressions in the QDF+NoOID writer path with overlay are caught locally.

**Architecture:** Reuse the existing byte_gate pattern in `crates/flpdf/src/overlay.rs`. Add a `write_qdf_nooid` helper alongside `write_static_id`, generate 3 goldens via `tests/golden/regenerate.sh` extension, and add 3 unit tests behind `#[cfg(all(test, feature = "qpdf-zlib-compat"))]`. Fixtures reuse the existing flpdf-authored `one/two/three-page.pdf` — no qtest content is imported (preserves flpdf-qtest's Artistic 2.0 isolation).

**Tech Stack:** Rust (flpdf crate), Bash (regenerate.sh), qpdf 11.9.0 (as tool for golden generation).

**Design source:** beads issue `flpdf-9hc.16.13` (design field).

---

## Prerequisites

- Worktree: `/home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-13-overlay-qdf-byte-gate` (branch `feat/flpdf-9hc-16-13-overlay-qdf-byte-gate`)
- qpdf 11.9.0 available on PATH (verified by `regenerate.sh` prelude)
- `cargo check --workspace --tests` currently green (verified at worktree creation)

---

## Task 1: Extend regenerate.sh with QDF variants and produce the 3 goldens

**Rationale:** Byte-parity tests reference concrete golden files. Producing them first via qpdf establishes ground truth; the tests written in Task 2 then verify flpdf writer output matches.

**Files:**
- Modify: `tests/golden/regenerate.sh` — insert a new section right after existing Phase 2b (overlay goldens, around line 1774 in the current file — anchor by comment `# Phase 2b: Overlay / underlay reference outputs`).

**Step 1: Add the new regeneration chunk**

Insert after the last existing overlay `qpdf ... $REF/overlay/*.pdf` block (grep for the last `overlay/` line in `regenerate.sh` to locate the exact insertion point). Insert:

```bash
# ---------------------------------------------------------------------------
# Phase 2b': QDF variants (flpdf-9hc.16.13)
#
# 3 representative overlay scenarios re-encoded with --qdf + --no-original-object-ids
# to cover the QDF writer path (uncompressed streams, object-number relaying, xref
# table form) in combination with overlay/underlay. Goldens serve as regression
# catchers for library-layer QDF+NoOID+overlay parity; uo-1..uo-8 exact byte
# parity is covered separately in flpdf-qtest (Artistic 2.0 isolation).
# ---------------------------------------------------------------------------

# QDF: three-page dest + one-page overlay (smallest QDF+overlay scenario).
qpdf --static-id --qdf --no-original-object-ids --warning-exit-0 \
    "$FIX/three-page.pdf" --overlay "$FIX/one-page.pdf" -- \
    "$REF/overlay/three-page-overlay-one-page-qdf.pdf"
echo "overlay/three-page-overlay-one-page-qdf.pdf"

# QDF: same page carries both overlay + underlay (order-preservation test).
qpdf --static-id --qdf --no-original-object-ids --warning-exit-0 \
    "$FIX/three-page.pdf" --overlay "$FIX/one-page.pdf" -- \
    --underlay "$FIX/two-page.pdf" -- \
    "$REF/overlay/three-page-overlay-and-underlay-qdf.pdf"
echo "overlay/three-page-overlay-and-underlay-qdf.pdf"

# QDF: two --overlay flags compose left-to-right (Fx0/Fx1 declaration order).
qpdf --static-id --qdf --no-original-object-ids --warning-exit-0 \
    "$FIX/three-page.pdf" --overlay "$FIX/one-page.pdf" -- \
    --overlay "$FIX/two-page.pdf" -- \
    "$REF/overlay/three-page-two-overlays-qdf.pdf"
echo "overlay/three-page-two-overlays-qdf.pdf"
```

**Step 2: Run regenerate.sh**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-13-overlay-qdf-byte-gate
bash tests/golden/regenerate.sh 2>&1 | tail -30
```

Expected: script exits 0, and the three `three-page-*-qdf.pdf` lines are echoed.

**Step 3: Verify goldens exist and are within the size policy**

```bash
ls -la tests/golden/references/overlay/three-page-*-qdf.pdf
```

Expected: 3 files, each < 100 KB (README size policy).

**Step 4: Commit**

```bash
git add tests/golden/regenerate.sh tests/golden/references/overlay/three-page-overlay-one-page-qdf.pdf \
        tests/golden/references/overlay/three-page-overlay-and-underlay-qdf.pdf \
        tests/golden/references/overlay/three-page-two-overlays-qdf.pdf
git commit -m "test(flpdf): add 3 QDF+NoOID overlay goldens (flpdf-9hc.16.13)"
```

---

## Task 2: Add `write_qdf_nooid` helper in `mod byte_gate`

**Rationale:** Isolates the QDF+NoOID `WriteOptions` recipe so the 3 subsequent tests are one-liners at the call site.

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` — insert into `mod byte_gate` right after `fn write_static_id` (search for `fn write_static_id`, insert after its closing brace).

**Step 1: Add the helper**

```rust
    /// Write `dest` through the `flpdf rewrite --static-id --qdf --no-original-object-ids`
    /// recipe. QDF mode internally promotes `newline_before_endstream` to
    /// [`NewlineBeforeEndstream::Yes`], so we rely on defaults for that field.
    fn write_qdf_nooid<R: std::io::Read + std::io::Seek>(dest: &mut Pdf<R>) -> Vec<u8> {
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        write_pdf_with_options(dest, &mut out, &opts).unwrap();
        out
    }
```

**Step 2: Verify it compiles**

```bash
cargo check --features qpdf-zlib-compat -p flpdf --lib --tests 2>&1 | tail -5
```

Expected: `Finished` line, no warnings. (No commit yet — this helper is unused until Task 3.)

---

## Task 3: Byte-parity test for overlay one-page (QDF)

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` — add test after existing `three_page_overlay_one_page_is_byte_identical`.

**Step 1: Write the failing test**

Insert into `mod byte_gate`:

```rust
    #[test]
    fn three_page_overlay_one_page_qdf_is_byte_identical() {
        // Same as three_page_overlay_one_page_is_byte_identical but written
        // through the QDF + --no-original-object-ids recipe.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-one-page-qdf.pdf");
    }
```

**Step 2: Run the test**

```bash
cargo test --features qpdf-zlib-compat -p flpdf --lib \
    overlay::byte_gate::three_page_overlay_one_page_qdf_is_byte_identical -- --nocapture 2>&1 | tail -30
```

Expected: test PASSES. If it fails with a `first diff at offset X` message, the failure output shows the diverging bytes — proceed to Step 2a below.

**Step 2a (only if Step 2 fails): Diagnose the divergence**

The scope of `flpdf-9hc.16.13` is verification of the existing writer path. If a real divergence surfaces, decide:

1. Is it a first-diff site the writer can fix in bounded scope? — fix it, re-run.
2. Is it a fundamental gap (e.g. QDF header comment format, xref formatting)? — pause, file a new bd issue for the underlying fix, mark this test `#[ignore = "blocked on flpdf-XXXX"]`, and continue with Task 4/5 to at least surface the gap.

Do NOT delete or weaken the golden — the whole point of the gate is to catch drift.

**Step 3: Commit**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "test(flpdf): overlay+QDF byte-gate for single-page overlay (flpdf-9hc.16.13)"
```

---

## Task 4: Byte-parity test for overlay+underlay same-page (QDF)

**Files:**
- Modify: `crates/flpdf/src/overlay.rs`.

**Step 1: Write the failing test**

Insert into `mod byte_gate` (right after Task 3's test):

```rust
    #[test]
    fn three_page_overlay_and_underlay_qdf_is_byte_identical() {
        // Overlay + underlay on the same dest pages, QDF recipe.
        // Matches the non-QDF three_page_overlay_and_underlay_is_byte_identical
        // scenario. Verifies Form XObject naming/order preservation under QDF.
        let mut dest = fixture("three-page.pdf");
        let mut overlay_source = fixture("one-page.pdf");
        let mut underlay_source = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut overlay_source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        apply_overlay_spec(
            &mut dest,
            &mut underlay_source,
            OverlayKind::Underlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-and-underlay-qdf.pdf");
    }
```

**Verification note:** the CLI-flag order in Task 1's regenerate chunk is `--overlay ... -- --underlay ... --`, and the byte_gate test must apply overlay THEN underlay in the same order so that the Form-XObject naming (Fx0=overlay, Fx1=underlay) matches. If the existing non-QDF golden was produced by the same order (grep in regenerate.sh — it is, per line ~1816), the ordering in this task is correct.

**Step 2: Run the test**

```bash
cargo test --features qpdf-zlib-compat -p flpdf --lib \
    overlay::byte_gate::three_page_overlay_and_underlay_qdf_is_byte_identical -- --nocapture 2>&1 | tail -30
```

Expected: PASS. (Step 2a from Task 3 applies here too.)

**Step 3: Commit**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "test(flpdf): overlay+underlay+QDF byte-gate same-page composition (flpdf-9hc.16.13)"
```

---

## Task 5: Byte-parity test for two overlays declaration order (QDF)

**Files:**
- Modify: `crates/flpdf/src/overlay.rs`.

**Step 1: Write the failing test**

```rust
    #[test]
    fn three_page_two_overlays_qdf_is_byte_identical() {
        // Two overlays compose left-to-right (Fx0/Fx1 declaration order), QDF.
        let mut dest = fixture("three-page.pdf");
        let mut source1 = fixture("one-page.pdf");
        let mut source2 = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source1,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        apply_overlay_spec(
            &mut dest,
            &mut source2,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "three-page-two-overlays-qdf.pdf");
    }
```

**Step 2: Run the test**

```bash
cargo test --features qpdf-zlib-compat -p flpdf --lib \
    overlay::byte_gate::three_page_two_overlays_qdf_is_byte_identical -- --nocapture 2>&1 | tail -30
```

Expected: PASS. (Same fallback as Task 3 Step 2a.)

**Step 3: Commit**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "test(flpdf): overlay+QDF byte-gate for two-overlay declaration order (flpdf-9hc.16.13)"
```

---

## Task 6: Document the -qdf variants in tests/golden/README.md

**Files:**
- Modify: `tests/golden/README.md`.

**Step 1: Locate the overlay section**

Search for the overlay reference documentation:

```bash
grep -n "overlay" tests/golden/README.md
```

If there is no dedicated overlay section (only the top-level Fixture × Flag Matrix), add a small subsection after that matrix.

**Step 2: Insert or amend the overlay documentation**

Add (or fold into an existing "Overlay References" subsection):

```markdown
### Overlay / underlay QDF variants (flpdf-9hc.16.13)

The following 3 goldens exercise the `--qdf --no-original-object-ids` writer path
in combination with overlay/underlay. They act as regression catchers for the
library-layer QDF+NoOID+overlay parity. Exact byte parity against qpdf's
`uo-1..uo-8` is *not* covered here; that is validated by `flpdf-qtest`'s
`compare-files` runtest steps, which live in a separate repository to isolate
the qtest test framework's Artistic 2.0 license from this tree.

| Golden                                         | Scenario                                             |
|------------------------------------------------|------------------------------------------------------|
| `overlay/three-page-overlay-one-page-qdf.pdf`  | single overlay onto page 1 (smallest QDF+overlay)    |
| `overlay/three-page-overlay-and-underlay-qdf.pdf` | overlay + underlay on the same dest pages         |
| `overlay/three-page-two-overlays-qdf.pdf`      | two `--overlay` flags composed left-to-right         |

qpdf command (all three): `qpdf --static-id --qdf --no-original-object-ids ...`.
```

**Step 3: Commit**

```bash
git add tests/golden/README.md
git commit -m "docs(flpdf): document -qdf overlay golden variants (flpdf-9hc.16.13)"
```

---

## Task 7: Verification

**Step 1: Full byte_gate under qpdf-zlib-compat**

```bash
cargo test --features qpdf-zlib-compat -p flpdf --lib overlay::byte_gate 2>&1 | tail -15
```

Expected: all tests pass, including the 3 new `*_qdf_is_byte_identical` cases and all pre-existing byte_gate tests (regression check).

**Step 2: cargo fmt and clippy**

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings 2>&1 | tail -10
```

Expected: no diffs, no warnings.

**Step 3: patch-coverage**

Per CLAUDE.md the coverage gate is mandatory before PR. Run from the worktree root:

```bash
bash scripts/patch-coverage.sh --base main
```

Expected: exit 0; flpdf changed-lines coverage 100%. If any changed line in `overlay.rs` is uncovered, the added tests do not cover it — investigate and either add a targeted test or apply `// cov:ignore` with justification (see CLAUDE.md coverage rules).

**Step 4: No commit needed — Task 7 is verification-only.** If any check fails, the fix goes into a new commit named for the failure (e.g., `fix(flpdf): clippy nit in write_qdf_nooid helper`).

---

## Task 8: Push and open PR

**Step 1: Rebase against main to pick up any late main-branch changes**

```bash
git fetch origin main
git rebase origin/main
```

Expected: no conflicts. If conflicts occur (unlikely for this small change), resolve and continue.

**Step 2: Push**

```bash
git push -u origin feat/flpdf-9hc-16-13-overlay-qdf-byte-gate
```

**Step 3: Open PR**

```bash
gh pr create --title "test(flpdf): overlay+QDF byte-gate (flpdf-9hc.16.13)" --body "$(cat <<'EOF'
## Summary

Adds 3 library-layer byte-parity gates for the `--qdf --no-original-object-ids` overlay/underlay writer path (flpdf-9hc.16.13).

- 3 new goldens under `tests/golden/references/overlay/*-qdf.pdf`, produced by qpdf 11.9.0 via `regenerate.sh`.
- 3 new `mod byte_gate` tests behind `qpdf-zlib-compat`.
- README documents the new variants.

## Why not uo-1..uo-8 directly?

qpdf-qtest lives in the separate `flpdf-qtest` repository specifically to isolate its Artistic 2.0 license from the fulgur-rs tree. This PR intentionally does *not* vendor `uo-*.pdf`. Exact `uo-*.pdf` byte parity is covered by `flpdf-qtest`'s `compare-files` runtest steps.

## Test plan

- [x] cargo test --features qpdf-zlib-compat -p flpdf --lib overlay::byte_gate
- [x] cargo fmt --check
- [x] cargo clippy --workspace -- -D warnings
- [x] scripts/patch-coverage.sh (flpdf 100%)
EOF
)"
```

Expected: PR URL returned. Note it for the beads issue close reason.

---

## Rollback / Recovery

- If Task 1 goldens turn out to be corrupted (qpdf drift), delete the 3 `.pdf` files and re-run `regenerate.sh` (it is idempotent).
- If a byte_gate test fails and the divergence is out of scope: mark the offending test `#[ignore = "flpdf-XXXX (new follow-up)"]`, do NOT delete the golden. File the follow-up in bd.
- If clippy adds new lints in main that fail here, `git rebase origin/main` and fix the specific lint. Do not blanket-`allow` warnings.
