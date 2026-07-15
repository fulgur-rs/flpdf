# ADBE Extension INJECT byte-parity gates — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add three fixture-based byte-identity gates covering the `write_pdf` INJECT branch (`inject_adbe_extension` → Catalog `/Extensions /ADBE`), mirroring the STRIP gates added in PR #478. Prove flpdf is byte-identical to `qpdf` 11.9.0 for three source shapes: (1) no `/Extensions`, (2) direct non-ADBE developer prefix (`/XYZW`), (3) indirect `/Extensions` reference with existing `/ADBE` weak + `/ACRO`.

**Architecture:** Rename `crates/flpdf/tests/adbe_removal_qpdf_parity.rs` → `adbe_ext_qpdf_parity.rs` (git mv) and parametrise its helpers so STRIP and INJECT cases share one test file. Extend `tests/golden/regenerate.sh` with three new hand-crafted classic-1.3 fixtures (no content streams) and their qpdf-11.9.0 `--min-version=1.7.8 --static-id` goldens. No `flpdf/src` changes — the INJECT branch is already implemented; this is pure test coverage. Deflate is not exercised, so the file is NOT gated on `qpdf-zlib-compat`, matching the existing STRIP test.

**Tech Stack:** Rust, `qpdf` 11.9.0 (fixture normalisation + golden generation), Python 3 (fixture hand-crafting via `regenerate.sh`).

**Beads issue:** flpdf-9hc.16.16 (parent epic: flpdf-9hc.16 Overlay/underlay, root epic: flpdf-9hc qpdf-equivalent FLPDF).

**Commit strategy (mirrors PR #478):**
1. rename + parametrise helpers (baseline 2 STRIP tests still pass)
2. `regenerate.sh` — hand-craft 3 INJECT fixtures + goldens
3. commit the 3 fixture PDFs + 3 golden PDFs
4. add 3 INJECT byte-gate tests
5. (if needed) any doc/comment tidy-up

---

## Task 1: Rename test file and parametrise helpers

**Files:**
- Rename: `crates/flpdf/tests/adbe_removal_qpdf_parity.rs` → `crates/flpdf/tests/adbe_ext_qpdf_parity.rs`
- Modify: the same file (new name) — parametrise helpers so a follow-up commit can add INJECT tests

**Step 1: `git mv` the file**

Run:

```bash
git mv crates/flpdf/tests/adbe_removal_qpdf_parity.rs \
       crates/flpdf/tests/adbe_ext_qpdf_parity.rs
```

**Step 2: Update the module doc**

Replace the top-of-file `//!` block with:

```rust
//! Byte-identity: flpdf plain full-rewrite emits qpdf's Catalog /Extensions
//! /ADBE mutations (removal AND injection) byte-for-byte.
//!
//! REMOVAL (QPDFWriter.cc L1408 whole /Extensions removal, L1432 /ADBE-only
//! removal): proves `catalog_has_extensions_adbe` broadened trigger matches
//! qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0` (L1387) on inputs
//! whose source /ADBE dict lacks a valid `/ExtensionLevel`.
//!
//! INJECTION (`inject_adbe_extension` fired by `WriteOptions::min_extension_level`,
//! qpdf `--min-version=<v>.<ext>`): proves flpdf's injection reproduces qpdf's
//! Catalog /Extensions dict byte-for-byte across three shapes: (1) fresh
//! creation when source has no /Extensions, (2) direct /Extensions with a
//! non-ADBE developer prefix (/XYZW) preserved, (3) indirect /Extensions
//! reference with existing /ADBE weak + /ACRO — inlined onto the Catalog,
//! /ADBE overwritten, /ACRO preserved.
//!
//! Fixtures are content-stream-free, so byte-identity is independent of the
//! deflate backend — this file is NOT gated on `qpdf-zlib-compat`.
```

**Step 3: Parametrise the helpers**

Replace the helper functions with the parametrised form. The current file has:

```rust
fn adbe_removal_qpdf_equivalent(fixture: &str) -> Vec<u8> { … options fixed … }
fn golden(stem: &str) -> Vec<u8> { … "adbe-strip.pdf" fixed … }
fn assert_parity(fixture: &str, stem: &str) { … }
```

Replace with:

```rust
/// STRIP-side WriteOptions (plain full rewrite, qpdf-matching newline/id).
fn strip_options() -> WriteOptions {
    WriteOptions {
        full_rewrite: true,
        static_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriteOptions::default()
    }
}

/// INJECT-side WriteOptions: strip_options() + min-version 1.7 with extension
/// level 8 (mirrors `qpdf --min-version=1.7.8`).
fn inject_options() -> WriteOptions {
    WriteOptions {
        min_version: Some("1.7".into()),
        min_extension_level: Some(8),
        ..strip_options()
    }
}

/// Plain full-rewrite of `fixture` with the given options; return bytes.
fn write_qpdf_equivalent(fixture: &str, options: &WriteOptions) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, options).unwrap();
    out
}

/// Read golden `references/<stem>/<golden_name>`.
fn golden(stem: &str, golden_name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(golden_name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_parity(fixture: &str, stem: &str, golden_name: &str, options: &WriteOptions) {
    let actual = write_qpdf_equivalent(fixture, options);
    let expected = golden(stem, golden_name);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf golden {stem}/{golden_name} \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}
```

**Step 4: Update the two existing STRIP tests to use the new helper signature**

Replace both existing `#[test]` blocks with:

```rust
#[test]
fn whole_extensions_removed_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1408: /Extensions has only /ADBE and we don't want /ADBE → drop
    // whole /Extensions from Catalog.
    assert_parity(
        "one-page-stale-adbe-no-ext.pdf",
        "one-page-stale-adbe-no-ext",
        "adbe-strip.pdf",
        &strip_options(),
    );
}

#[test]
fn non_adbe_prefix_preserved_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1432: /Extensions has /ADBE + non-ADBE prefix and we don't want
    // /ADBE → remove /ADBE key only, keep /Extensions with other keys.
    assert_parity(
        "one-page-stale-adbe-no-ext-vendor.pdf",
        "one-page-stale-adbe-no-ext-vendor",
        "adbe-strip.pdf",
        &strip_options(),
    );
}
```

**Step 5: Run the test file — 2 STRIP tests must still pass**

Run:

```bash
cargo test -p flpdf --test adbe_ext_qpdf_parity
```

Expected: `test result: ok. 2 passed; 0 failed`.

Also make sure the old name no longer resolves:

```bash
cargo test -p flpdf --test adbe_removal_qpdf_parity 2>&1 | head -5
```

Expected: an error like "no test target named `adbe_removal_qpdf_parity`".

**Step 6: cargo fmt --check**

Run `cargo fmt --check`. Expected: no output (clean).

**Step 7: Commit**

```bash
git add crates/flpdf/tests/adbe_ext_qpdf_parity.rs \
        crates/flpdf/tests/adbe_removal_qpdf_parity.rs
git commit -m "test(flpdf): rename adbe_removal_qpdf_parity → adbe_ext_qpdf_parity + parametrise helpers"
```

(The `git add` on the deleted path is what stages the deletion side of the rename.)

---

## Task 2: Extend `tests/golden/regenerate.sh` with 3 INJECT fixture + golden blocks

**Files:**
- Modify: `tests/golden/regenerate.sh` — append after the existing "/ADBE strip fixtures + goldens" block near the end of the file

**Step 1: Locate the append point**

Run:

```bash
grep -n "adbe-strip\|one-page-stale-adbe" tests/golden/regenerate.sh | tail -20
```

You want to append AFTER the last `if [[ ! -f "$REF/one-page-stale-adbe-no-ext-vendor/adbe-strip.pdf" ]]; then ... fi` block (the second of the two strip blocks). The file ends with plain executable statements, so appending before its final line is fine.

**Step 2: Append the three INJECT blocks**

Add this to `tests/golden/regenerate.sh`, immediately after the last strip block:

```bash
# ---------------------------------------------------------------------------
# /ADBE INJECT fixtures + goldens (flpdf-9hc.16.16)
# ---------------------------------------------------------------------------
# Three hand-crafted classic-1.3 PDFs exercising `inject_adbe_extension` (the
# WriteOptions::min_extension_level path, mirroring qpdf --min-version=1.7.8):
#   - one-page-no-ext.pdf:       Catalog has no /Extensions → fresh injection.
#   - one-page-xyzw-only.pdf:    /Extensions << /XYZW >> (direct) → /ADBE added
#                                before /XYZW (alphabetical), /XYZW preserved.
#   - one-page-ext-indirect.pdf: /Extensions 3 0 R, obj 3 = << /ADBE weak /ACRO >>
#                                → inlined onto Catalog, /ADBE overwritten,
#                                /ACRO preserved; obj 3 removed from body.

if [[ ! -f "$FIX/one-page-no-ext.pdf" ]]; then
    echo "Generating one-page-no-ext.pdf ..."
    python3 - "$FIX/one-page-no-ext.pdf" <<'PY'
import sys
src = b"%PDF-1.3\n"
offs = []
offs.append(len(src))
src += b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"
offs.append(len(src))
src += b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n"
xr = len(src)
src += b"xref\n0 3\n0000000000 65535 f \n"
for o in offs:
    src += ("%010d 00000 n \n" % o).encode()
src += ("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n%d\n%%EOF\n" % xr).encode()
open(sys.argv[1], "wb").write(src)
PY
else
    echo "Skipping one-page-no-ext.pdf (already exists)"
fi

if [[ ! -f "$REF/one-page-no-ext/adbe-inject.pdf" ]]; then
    echo "Generating one-page-no-ext/adbe-inject.pdf golden ..."
    mkdir -p "$REF/one-page-no-ext"
    qpdf --min-version=1.7.8 --static-id --warning-exit-0 \
        "$FIX/one-page-no-ext.pdf" "$REF/one-page-no-ext/adbe-inject.pdf"
else
    echo "Skipping one-page-no-ext/adbe-inject.pdf golden (already exists)"
fi

if [[ ! -f "$FIX/one-page-xyzw-only.pdf" ]]; then
    echo "Generating one-page-xyzw-only.pdf ..."
    python3 - "$FIX/one-page-xyzw-only.pdf" <<'PY'
import sys
src = b"%PDF-1.3\n"
offs = []
offs.append(len(src))
src += (b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R "
        b"/Extensions << /XYZW << /BaseVersion /1.3 /ExtensionLevel 1 >> >> "
        b">>\nendobj\n")
offs.append(len(src))
src += b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n"
xr = len(src)
src += b"xref\n0 3\n0000000000 65535 f \n"
for o in offs:
    src += ("%010d 00000 n \n" % o).encode()
src += ("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n%d\n%%EOF\n" % xr).encode()
open(sys.argv[1], "wb").write(src)
PY
else
    echo "Skipping one-page-xyzw-only.pdf (already exists)"
fi

if [[ ! -f "$REF/one-page-xyzw-only/adbe-inject.pdf" ]]; then
    echo "Generating one-page-xyzw-only/adbe-inject.pdf golden ..."
    mkdir -p "$REF/one-page-xyzw-only"
    qpdf --min-version=1.7.8 --static-id --warning-exit-0 \
        "$FIX/one-page-xyzw-only.pdf" "$REF/one-page-xyzw-only/adbe-inject.pdf"
else
    echo "Skipping one-page-xyzw-only/adbe-inject.pdf golden (already exists)"
fi

if [[ ! -f "$FIX/one-page-ext-indirect.pdf" ]]; then
    echo "Generating one-page-ext-indirect.pdf ..."
    python3 - "$FIX/one-page-ext-indirect.pdf" <<'PY'
import sys
src = b"%PDF-1.3\n"
offs = []
offs.append(len(src))
src += b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 3 0 R >>\nendobj\n"
offs.append(len(src))
src += b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n"
offs.append(len(src))
src += (b"3 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> "
        b"/ACRO << /BaseVersion /1.7 /ExtensionLevel 1 >> >>\nendobj\n")
xr = len(src)
src += b"xref\n0 4\n0000000000 65535 f \n"
for o in offs:
    src += ("%010d 00000 n \n" % o).encode()
src += ("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n%d\n%%EOF\n" % xr).encode()
open(sys.argv[1], "wb").write(src)
PY
else
    echo "Skipping one-page-ext-indirect.pdf (already exists)"
fi

if [[ ! -f "$REF/one-page-ext-indirect/adbe-inject.pdf" ]]; then
    echo "Generating one-page-ext-indirect/adbe-inject.pdf golden ..."
    mkdir -p "$REF/one-page-ext-indirect"
    qpdf --min-version=1.7.8 --static-id --warning-exit-0 \
        "$FIX/one-page-ext-indirect.pdf" "$REF/one-page-ext-indirect/adbe-inject.pdf"
else
    echo "Skipping one-page-ext-indirect/adbe-inject.pdf golden (already exists)"
fi
```

**Step 3: Run the script — three fixtures + three goldens materialise**

Run:

```bash
bash tests/golden/regenerate.sh 2>&1 | grep -E "(one-page-no-ext|one-page-xyzw-only|one-page-ext-indirect)"
```

Expected: 6 "Generating …" lines (3 fixtures + 3 goldens), no errors.

**Step 4: Verify the files exist and look right**

Run:

```bash
ls -l tests/fixtures/compat/one-page-{no-ext,xyzw-only,ext-indirect}.pdf
ls -l tests/golden/references/one-page-{no-ext,xyzw-only,ext-indirect}/adbe-inject.pdf
```

Also spot-check one golden:

```bash
od -c tests/golden/references/one-page-xyzw-only/adbe-inject.pdf | head -12
```

Expected to see `%PDF-1.7\n%277 367 242 376\n` header and a Catalog containing `/Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> /XYZW << /BaseVersion /1.3 /ExtensionLevel 1 >> >>`.

**Step 5: Commit the `regenerate.sh` change alone (fixtures/goldens in the next task)**

```bash
git add tests/golden/regenerate.sh
git commit -m "test(flpdf): regenerate.sh — hand-craft /ADBE-inject fixtures + goldens"
```

---

## Task 3: Commit the 3 new fixtures + 3 new goldens

**Files:**
- Add: `tests/fixtures/compat/one-page-no-ext.pdf`
- Add: `tests/fixtures/compat/one-page-xyzw-only.pdf`
- Add: `tests/fixtures/compat/one-page-ext-indirect.pdf`
- Add: `tests/golden/references/one-page-no-ext/adbe-inject.pdf`
- Add: `tests/golden/references/one-page-xyzw-only/adbe-inject.pdf`
- Add: `tests/golden/references/one-page-ext-indirect/adbe-inject.pdf`

**Step 1: Stage the 6 files**

```bash
git add tests/fixtures/compat/one-page-no-ext.pdf \
        tests/fixtures/compat/one-page-xyzw-only.pdf \
        tests/fixtures/compat/one-page-ext-indirect.pdf \
        tests/golden/references/one-page-no-ext/adbe-inject.pdf \
        tests/golden/references/one-page-xyzw-only/adbe-inject.pdf \
        tests/golden/references/one-page-ext-indirect/adbe-inject.pdf
```

**Step 2: Verify `git status` shows only the 6 additions**

Run `git status`. Expected: 6 new files, no modifications.

**Step 3: Commit**

```bash
git commit -m "test(flpdf): fixtures + goldens for /ADBE inject parity (3 shapes)"
```

---

## Task 4: Add the 3 INJECT byte-gate tests

**Files:**
- Modify: `crates/flpdf/tests/adbe_ext_qpdf_parity.rs` — append 3 `#[test]` functions at the end

**Step 1: Add the 3 tests after the existing 2 STRIP tests**

Append immediately after the second STRIP test (`non_adbe_prefix_preserved_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf`):

```rust
#[test]
fn fresh_extensions_adbe_injected_when_source_has_none_byte_identical_to_qpdf() {
    // qpdf --min-version=1.7.8 on a Catalog with no /Extensions must emit
    // a fresh /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>.
    // Verifies inject_adbe_extension's fresh-creation branch byte-for-byte.
    assert_parity(
        "one-page-no-ext.pdf",
        "one-page-no-ext",
        "adbe-inject.pdf",
        &inject_options(),
    );
}

#[test]
fn non_adbe_prefix_preserved_when_source_lacks_adbe_and_min_ext_requests_injection_byte_identical_to_qpdf() {
    // qpdf --min-version=1.7.8 on a Catalog with /Extensions << /XYZW … >>
    // must add /ADBE before /XYZW (alphabetical) and preserve /XYZW verbatim.
    // The issue-focus case: non-ADBE developer prefix survives injection.
    assert_parity(
        "one-page-xyzw-only.pdf",
        "one-page-xyzw-only",
        "adbe-inject.pdf",
        &inject_options(),
    );
}

#[test]
fn indirect_extensions_inlined_and_adbe_overwritten_preserving_non_adbe_prefix_byte_identical_to_qpdf() {
    // qpdf --min-version=1.7.8 on a Catalog whose /Extensions is an indirect
    // reference (obj 3 = /ADBE weak + /ACRO) must inline obj 3 onto the
    // Catalog, overwrite /ADBE with (1.7, 8), and preserve /ACRO. Result key
    // order is alphabetical: /ACRO before /ADBE. obj 3 is dropped from the body.
    assert_parity(
        "one-page-ext-indirect.pdf",
        "one-page-ext-indirect",
        "adbe-inject.pdf",
        &inject_options(),
    );
}
```

**Step 2: Run the 5 tests together**

Run:

```bash
cargo test -p flpdf --test adbe_ext_qpdf_parity
```

Expected: `test result: ok. 5 passed; 0 failed`.

**Step 3: If any INJECT test FAILS**

The failure message reports `first diff at byte N` with ±16-byte context on both sides. This is a real flpdf-vs-qpdf byte divergence in the INJECT path that this issue has uncovered. Do NOT edit the golden; instead:

- Read the divergence context. Common causes are: Catalog key ordering, spacing around `<<`/`>>`, `/ADBE` sub-dict key order, indirect-ref inlining preserving vs dropping obj 3.
- File a follow-up beads issue titled "flpdf-9hc.16.16 follow-up: <divergence description>" with the first-diff context, then keep the failing test in place (do not comment out) — that's the point of a byte gate.
- STOP the plan and hand back to the user. Do not silently mask a divergence.

Do NOT proceed to Task 5 until all 5 tests pass or the user has explicitly acknowledged and deferred the divergence.

**Step 4: cargo fmt --check**

Run `cargo fmt --check`. Expected: no output.

**Step 5: Commit**

```bash
git add crates/flpdf/tests/adbe_ext_qpdf_parity.rs
git commit -m "test(flpdf): byte-gate for /ADBE inject parity vs qpdf 11.9.0 (3 shapes)"
```

---

## Task 5: Final verification gates and push

**Step 1: Patch-coverage gate (CLAUDE.md required)**

Run:

```bash
scripts/patch-coverage.sh --base main
```

Expected: `flpdf` diff shows 100% coverage of changed lines (the new tests execute themselves; the parametrised helpers are exercised by all 5 tests). Any uncovered line must be justified with `// cov:ignore: <reason>`.

If the script requires committed state and refuses "dirty tree" — fine, all 4 commits are already in. If `--allow-dirty` is ever needed, do NOT use it here; investigate first.

**Step 2: cargo fmt --check on the full workspace**

```bash
cargo fmt --check
```

Expected: no output.

**Step 3: Full flpdf test suite (regression check)**

```bash
cargo test -p flpdf 2>&1 | tail -20
```

Expected: all tests pass; no regressions introduced by the rename.

**Step 4: Update beads status and prepare PR**

- Confirm `bd show flpdf-9hc.16.16` still shows `in_progress`.
- Run `git log --oneline main..HEAD` — expected: 4 commits (rename+parametrise, regenerate.sh, fixtures+goldens, byte-gate tests).
- Push and open the PR:

```bash
git push -u origin feat/flpdf-9hc-16-16-adbe-inject-parity
gh pr create --title "test(flpdf): /ADBE inject byte-parity vs qpdf 11.9.0 (flpdf-9hc.16.16)" \
             --body "$(cat <<'BODY'
## Summary

- Adds 3 fixture-based byte-identity gates for the `write_pdf` INJECT branch
  (`inject_adbe_extension`, mirroring `qpdf --min-version=1.7.8`), mirroring
  the STRIP gates from PR #478.
- Renames `crates/flpdf/tests/adbe_removal_qpdf_parity.rs` →
  `adbe_ext_qpdf_parity.rs` so STRIP and INJECT parity cases live together.
- Fixtures: `one-page-no-ext.pdf` (fresh /Extensions creation),
  `one-page-xyzw-only.pdf` (non-ADBE developer prefix preserved — the issue
  focus), `one-page-ext-indirect.pdf` (indirect /Extensions inlined onto
  Catalog, /ADBE overwritten, /ACRO preserved).
- No `flpdf/src` changes; pure test-coverage work.
- Content-stream-free fixtures — no `qpdf-zlib-compat` gating required.

## Test plan

- [ ] `cargo test -p flpdf --test adbe_ext_qpdf_parity` — 5 tests pass
- [ ] `scripts/patch-coverage.sh --base main` — 100% on changed flpdf lines
- [ ] `cargo fmt --check` — clean
- [ ] `cargo test -p flpdf` — no regressions
BODY
)"
```

**Step 5: Close the beads issue**

Only after the PR is up and CI is green:

```bash
bd close flpdf-9hc.16.16 --reason "3 fixture-based INJECT byte-gate tests added; STRIP gates preserved under renamed adbe_ext_qpdf_parity.rs. See PR #<n>."
```

---

## Notes for the implementer

- **YAGNI check on fixture count**: three shapes were chosen deliberately to cover fresh injection, direct dict + prefix preservation, and indirect ref inlining + prefix preservation. Do NOT add a fourth without a distinct qpdf byte-layout justification.
- **Do NOT change `crates/flpdf/src/writer.rs`**. The INJECT branch is already implemented and covered by 3 writer unit tests; this issue adds only fixture-based end-to-end byte gates.
- **Reference**: PR #478 (commit `1aab4a0`, plan `docs/plans/2026-07-14-flpdf-9hc-16-15-adbe-removal-parity.md`) established the STRIP-side pattern this plan mirrors.
- **CLAUDE.md rules** — before implementing, re-read `.claude/rules/pdf-rust-review-patterns.md` for the four common review pitfalls. This plan touches tests only (no `.clone()` in hot paths, no PDF indirect-ref handling in Rust logic, no unsigned casts, no graph traversal), so most rules do not apply — but Rule 4 boundary is worth keeping in mind if a future extension does.
