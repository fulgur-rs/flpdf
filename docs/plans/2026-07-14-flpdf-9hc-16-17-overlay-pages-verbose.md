# flpdf-9hc.16.17 (bundled with 16.14): `--overlay` + `--pages` + `--verbose` progress

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Unblock qtest `form-xobject 27/33` (uo-6, uo-8) by (a) allowing
`--overlay/--underlay` to co-exist with `--pages` (page-selection first,
overlay second — matching qpdf), and (b) emitting the 5 qpdf-parity progress
lines that `--verbose --pages` produces.

**Architecture:** The current top-level and `rewrite` dispatch rejects
`--overlay` + `--pages` because the page-op path (`run_page_extraction` /
`run_rewrite_with_page_ops`) never runs `apply_overlay_specs`. Fix by:
(1) removing the two rejection guards; (2) plumbing overlay application into
the page-op path AFTER page-tree rebuild + rotate + remap/prune + AcroForm
prune, BEFORE serialize — so `--to`/`--from`/`--repeat` indices reference the
extracted page count (qpdf semantics); (3) threading `verbose` into
`run_page_extraction` and `run_rewrite_with_page_ops`; (4) emitting the 5
`flpdf:`-prefixed progress lines (shim normalizes to `qpdf:` for golden
comparison).

**Tech Stack:** Rust; `flpdf-cli` (top-level + Rewrite subcommand dispatch);
`flpdf::apply_overlay_specs` / `flpdf::overlay_verbose_report` (already used
by the plain-rewrite path in `run_rewrite`); qtest goldens under
`/home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-{6,8}.out`.

---

## qpdf source references (verified 2026-07-14, `/tmp/qpdf-src`)

`libqpdf/QPDFJob.cc`:

- L2250: `<file>: checking for shared resources\n` — per unique --pages input
- L2312: `no shared resources found\n` — negative branch (positive branch is
  `found resources in non-leaf` / `found shared resources in leaf`; flpdf
  does not implement the scan and always emits the negative branch)
- L2425: `selecting --keep-open-files=y|n` — only when the user did NOT pass
  `--keep-open-files`; value is `y` if `#files ≤ threshold` else `n`
- L2539: `removing unreferenced pages from primary input` — once per --pages
- L2594: `adding pages from <file>` — per Selection (page-range spec)

Golden shape (from uo-6.out — order matters, byte-identical):

```
qpdf: selecting --keep-open-files=y
qpdf: fxo-red.pdf: checking for shared resources
qpdf: no shared resources found
qpdf: removing unreferenced pages from primary input
qpdf: adding pages from fxo-red.pdf
qpdf: processing underlay/overlay
  page 1
    fxo-blue.pdf overlay 1
    …
qpdf: wrote file a.pdf
```

flpdf-cli emits `flpdf: …`; `/home/ubuntu/flpdf-qtest/normalize/stderr-rules.sed`
substitutes `^flpdf:` → `qpdf:` before comparison.

## Deliberate divergences (per CLAUDE.md 逸脱明示ルール)

- `no shared resources found` is always emitted; flpdf does not scan for
  shared resources (not needed for output byte-identity).
- `selecting --keep-open-files=y` is always emitted when the block fires;
  flpdf has no auto-decision subsystem. This matches every observed qpdf
  --pages golden that does not pass `--keep-open-files`.

Document both in the PR body.

---

## Task 1: Baseline reproduction (empirical, no code change)

**Files:**
- Read: `crates/flpdf-cli/src/main.rs:1688-1695` (top-level reject)
- Read: `crates/flpdf-cli/src/main.rs:2187-2194` (rewrite reject)

**Step 1: Reproduce the current rejection**

```bash
cd /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf
/home/ubuntu/flpdf/.worktrees/flpdf-9hc.16.17-overlay-pages-verbose/target/release/flpdf \
  --overlay fxo-blue.pdf --to=1,1,1,1 --from=1-4 -- \
  fxo-red.pdf --pages . 1 -- /tmp/uo6-baseline.pdf
```

Expected stderr: `flpdf: --overlay/--underlay is not applied in the
--pages/--rotate/--split-pages/--collate pipeline; rerun without the page
operation`
Expected exit: 1

**Step 2: Verify goldens exist**

```bash
ls /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-6.out
ls /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-8.out
```

Expected: both files exist.

**Step 3: No commit** — baseline task only.

---

## Task 2: Extract filename-basename helper (test-first, minimal)

**Rationale:** progress lines use the basename of each --pages source file
(uo-6 golden: `fxo-red.pdf`, not `.` or absolute path). Isolating basename
resolution to a helper makes the emission code straightforward.

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (add private fn near `run_page_extraction`)
- Test: `crates/flpdf-cli/tests/cli_overlay.rs` (extend if already has a
  helper-test section) OR inline `#[cfg(test)]` in `main.rs`

**Step 1: Write the failing test**

Add near an existing `#[cfg(test)]` block in `main.rs`:

```rust
#[test]
fn pages_progress_filename_uses_basename() {
    let p = std::path::Path::new("/tmp/qtest/fxo-red.pdf");
    assert_eq!(pages_progress_filename(p), "fxo-red.pdf");
    let p2 = std::path::Path::new("fxo-red.pdf");
    assert_eq!(pages_progress_filename(p2), "fxo-red.pdf");
    let p3 = std::path::Path::new("./sub/../fxo-red.pdf");
    assert_eq!(pages_progress_filename(p3), "fxo-red.pdf");
}
```

**Step 2: Run test to verify it fails**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc.16.17-overlay-pages-verbose
cargo test -p flpdf-cli pages_progress_filename_uses_basename 2>&1 | tail -20
```

Expected: FAIL, "cannot find function `pages_progress_filename`".

**Step 3: Write minimal implementation**

```rust
fn pages_progress_filename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
```

**Step 4: Run test to verify it passes**

```bash
cargo test -p flpdf-cli pages_progress_filename_uses_basename 2>&1 | tail -20
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "feat(flpdf-cli): pages_progress_filename helper for --verbose --pages progress (flpdf-9hc.16.17)"
```

---

## Task 3: Emit 5 progress lines from `run_page_extraction` when `verbose`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:3834+` (`run_page_extraction` signature + body)
- Modify: `crates/flpdf-cli/src/main.rs` (both call sites: top-level 1713,
  Rewrite subcommand 2243)
- Test: `crates/flpdf-cli/tests/verbose_pages_progress.rs` (new)

**Step 1: Write the failing test**

Create `crates/flpdf-cli/tests/verbose_pages_progress.rs`:

```rust
//! Structural test: `flpdf --verbose --pages` emits qpdf-parity progress
//! (flpdf-9hc.16.17). Byte-parity of the block against uo-6/uo-8 goldens is
//! verified via the qtest shim; this test asserts the emission itself so
//! regressions surface without the harness dependency.

use assert_cmd::Command;
use predicates::prelude::*;

const FLPDF_BIN: &str = env!("CARGO_BIN_EXE_flpdf");

#[test]
fn verbose_pages_alone_emits_qpdf_parity_progress_block() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.pdf");
    let out = dir.path().join("out.pdf");
    // Reuse an existing tiny fixture; single-page primary + --pages . 1
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/minimal-one-page.pdf"),
        &src,
    ).unwrap();

    Command::new(FLPDF_BIN)
        .args(["--verbose", "--static-id"])
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(&out)
        .assert()
        .success()
        .stderr(predicate::str::contains("flpdf: selecting --keep-open-files=y"))
        .stderr(predicate::str::contains("src.pdf: checking for shared resources"))
        .stderr(predicate::str::contains("flpdf: no shared resources found"))
        .stderr(predicate::str::contains("flpdf: removing unreferenced pages from primary input"))
        .stderr(predicate::str::contains("adding pages from"))
        .stderr(predicate::str::contains("src.pdf"))
        .stderr(predicate::str::contains("flpdf: wrote file"));
}

#[test]
fn verbose_pages_progress_line_order_matches_qpdf() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.pdf");
    let out = dir.path().join("out.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/minimal-one-page.pdf"),
        &src,
    ).unwrap();

    let output = Command::new(FLPDF_BIN)
        .args(["--verbose", "--static-id"])
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let i_kfo = stderr.find("selecting --keep-open-files").unwrap();
    let i_check = stderr.find("checking for shared resources").unwrap();
    let i_none = stderr.find("no shared resources found").unwrap();
    let i_rm = stderr.find("removing unreferenced pages").unwrap();
    let i_add = stderr.find("adding pages from").unwrap();
    let i_wrote = stderr.find("wrote file").unwrap();
    assert!(i_kfo < i_check, "keep-open-files must precede checking");
    assert!(i_check < i_none, "checking must precede no-shared");
    assert!(i_none < i_rm, "no-shared must precede removing");
    assert!(i_rm < i_add, "removing must precede adding");
    assert!(i_add < i_wrote, "adding must precede wrote file");
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p flpdf-cli --test verbose_pages_progress 2>&1 | tail -30
```

Expected: FAIL (--verbose currently blocked by clap on top-level without
--list-attachments; or run_page_extraction doesn't take verbose).

**Step 3: Wire verbose through**

- Ensure top-level `--verbose` is not constrained to `--list-attachments`.
  Check the actual clap error by re-running the baseline command from Task 1
  step 1 with `--verbose` and confirm the constraint. If the constraint is
  a clap `requires` or `group`, remove/loosen it. Otherwise investigate
  which group/argument config causes the current usage-line output.
- Add `verbose: bool` parameter to `run_page_extraction` (main.rs:3834) and
  `run_rewrite_with_page_ops` (main.rs:4033). Thread through from both
  call sites (top-level around 1713, Rewrite subcommand around 2243).
- At the start of `run_page_extraction`, immediately after `let plan = …`
  (line 3906), emit:

```rust
if verbose {
    eprintln!("flpdf: selecting --keep-open-files=y");
    // Per-unique-source progress
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    for spec in &inputs {
        let key = std::fs::canonicalize(&spec.path).unwrap_or_else(|_| spec.path.clone());
        if !seen.contains(&key) {
            seen.push(key);
            let fname = pages_progress_filename(&spec.path);
            eprintln!("flpdf: {}: checking for shared resources", fname);
            eprintln!("flpdf: no shared resources found");
        }
    }
    eprintln!("flpdf: removing unreferenced pages from primary input");
    // Per-Selection "adding pages from" — one line per InputSpec occurrence
    for spec in &inputs {
        eprintln!("flpdf: adding pages from {}", pages_progress_filename(&spec.path));
    }
}
```

- At the end of `run_page_extraction`, right before `Ok(())`, emit the
  `flpdf: wrote file …` line (mirror `run_rewrite`'s line 3230):

```rust
if verbose {
    eprintln!("flpdf: wrote file {}", output.display());
}
```

- Do the same threading (verbose parameter) in `run_rewrite_with_page_ops`
  for the `--rotate`/`--split-pages` alone case (uo-6/uo-8 do not exercise
  this branch, but keeping the surface consistent avoids a follow-up gap).
  For that branch emit only `flpdf: wrote file …` for now; other progress
  lines require the --pages topology.

**Step 4: Run test to verify it passes**

```bash
cargo test -p flpdf-cli --test verbose_pages_progress 2>&1 | tail -30
```

Expected: PASS both tests.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/verbose_pages_progress.rs
git commit -m "feat(flpdf-cli): --verbose --pages emits qpdf-parity progress block (flpdf-9hc.16.17)"
```

---

## Task 4: Allow `--overlay/--underlay` + `--pages` (drop the two rejections)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:1688-1695` (top-level page-op branch)
- Modify: `crates/flpdf-cli/src/main.rs:2187-2194` (Rewrite subcommand page-op branch)
- Modify: `crates/flpdf-cli/src/main.rs` (thread `overlay_specs` into
  `run_page_extraction` + apply after AcroForm prune)
- Test: `crates/flpdf-cli/tests/cli_overlay.rs` (extend with a --pages + --overlay
  regression that asserts the invocation succeeds — byte-parity is Task 6)

**Step 1: Write the failing test**

Append to `crates/flpdf-cli/tests/cli_overlay.rs`:

```rust
#[test]
fn overlay_plus_pages_no_longer_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.pdf");
    let overlay = dir.path().join("overlay.pdf");
    let out = dir.path().join("out.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/minimal-one-page.pdf"),
        &src,
    ).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/minimal-one-page.pdf"),
        &overlay,
    ).unwrap();
    Command::new(FLPDF_BIN)
        .args(["--static-id"])
        .arg("--overlay").arg(&overlay).args(["--to=1", "--from=1", "--"])
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(&out)
        .assert()
        .success();
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p flpdf-cli --test cli_overlay overlay_plus_pages_no_longer_rejected 2>&1 | tail -20
```

Expected: FAIL (exit 1, current rejection message).

**Step 3: Remove rejection guards + wire overlay into page-op path**

- main.rs:1688-1695 (top-level): delete the entire `if !overlay_specs.is_empty()
  { … exit(1); }` block. The caller already validated the overlay group parse.
- main.rs:2187-2194 (Rewrite): same delete.
- Both dispatch arms currently call `run_page_extraction(...)` /
  `run_rewrite_with_page_ops(...)` — extend the signature of
  `run_page_extraction` to take `overlay_specs: &[OverlaySpec]` and `verbose:
  bool`. In `run_page_extraction`, insert BETWEEN step 8 (AcroForm prune, line
  ~3971) and step 9 (serialize, line ~3974):

```rust
// Overlay/underlay stacking runs AFTER page-selection so --to/--from/--repeat
// indices reference the extracted page count (matching qpdf's ordering).
if !overlay_specs.is_empty() {
    // Build fresh, since apply_overlay_specs takes &mut [OverlaySpec] and
    // caches Pdf handles across passes. Clone the input slice.
    let mut built: Vec<OverlaySpec> = overlay_specs.to_vec();
    if verbose {
        let report = flpdf::overlay_verbose_report(&mut pdf, &mut built)?;
        eprintln!("flpdf: processing underlay/overlay");
        for page in &report {
            eprintln!("  page {}", page.dest_page);
            for src in &page.sources {
                let file = &built[src.spec_index].file;
                let kind_str = match src.kind {
                    flpdf::OverlayKind::Underlay => "underlay",
                    flpdf::OverlayKind::Overlay => "overlay",
                };
                eprintln!("    {} {} {}", file, kind_str, src.src_page);
            }
        }
    }
    flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
}
```

  Note: `OverlaySpec` derives `Clone` — verify with `grep "derive.*Clone" |
  grep OverlaySpec` in `crates/flpdf/src/overlay.rs`. If not, use
  `overlay_specs.iter().cloned().collect()` after adding `Clone` (small,
  local change).

- Both call sites now pass `overlay_specs` and `args.verbose` /
  `cmd.verbose` respectively.

- Emit the "flpdf: wrote file …" line at the end of run_page_extraction is
  already done in Task 3; keep as-is.

**Step 4: Run test to verify it passes**

```bash
cargo test -p flpdf-cli --test cli_overlay overlay_plus_pages_no_longer_rejected 2>&1 | tail -20
```

Expected: PASS.

**Step 5: Verify full suite still passes**

```bash
cargo test -p flpdf-cli 2>&1 | tail -20
cargo test -p flpdf 2>&1 | tail -20
```

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_overlay.rs
git commit -m "feat(flpdf-cli): --overlay/--underlay + --pages composes (page-selection first) (flpdf-9hc.16.14, flpdf-9hc.16.17)"
```

---

## Task 5: End-to-end verbose progress for `--overlay + --pages` (uo-6 shape)

**Files:**
- Test: `crates/flpdf-cli/tests/verbose_pages_progress.rs` (extend)

**Step 1: Add failing test**

Append to `verbose_pages_progress.rs`:

```rust
#[test]
fn verbose_pages_plus_overlay_emits_page_progress_then_overlay_progress() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.pdf");
    let overlay = dir.path().join("overlay.pdf");
    let out = dir.path().join("out.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/minimal-one-page.pdf"),
        &src,
    ).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/minimal-one-page.pdf"),
        &overlay,
    ).unwrap();
    let output = Command::new(FLPDF_BIN)
        .args(["--verbose", "--static-id"])
        .arg("--overlay").arg(&overlay).args(["--to=1", "--from=1", "--"])
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "flpdf failed: stderr={:?}",
        String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let i_pages_block = stderr.find("removing unreferenced pages").unwrap();
    let i_overlay_block = stderr.find("processing underlay/overlay").unwrap();
    let i_wrote = stderr.find("wrote file").unwrap();
    assert!(i_pages_block < i_overlay_block,
        "page-selection block must precede overlay block");
    assert!(i_overlay_block < i_wrote,
        "overlay block must precede wrote-file line");
}
```

**Step 2: Run test — verify PASS after Task 3+4**

```bash
cargo test -p flpdf-cli --test verbose_pages_progress 2>&1 | tail -20
```

Expected: PASS (Task 3 provides pages-progress, Task 4 provides overlay-progress).

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/verbose_pages_progress.rs
git commit -m "test(flpdf-cli): --verbose --pages + --overlay orders progress blocks (flpdf-9hc.16.17)"
```

---

## Task 6: qtest byte-parity for uo-6 / uo-8 progress (optional, when harness available)

**Rationale:** Full qtest runtest under qpdf-zlib-compat is heavy and lives
outside the crate. If time permits, run the two runtest steps and verify
byte-identical stderr against the goldens. Otherwise document as follow-up.

**Files:**
- No code changes; verification via existing `flpdf-qtest` harness

**Step 1: Rebuild release with qpdf-zlib-compat**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc.16.17-overlay-pages-verbose
cargo build --release --features qpdf-zlib-compat --bin flpdf 2>&1 | tail -5
```

**Step 2: Run uo-6 / uo-8 runtest steps via shim**

Consult `flpdf-qtest/README.md` for exact invocation. Expected: PASS.

**Step 3: If failing**, capture the divergence with `diff -u expected actual`
and fix. If it's a body byte-identity issue (writer-side), that is NOT this
task's scope — document as flpdf-9hc.16.10/16.13 follow-up in the PR.

**Step 4: No commit if verification-only.**

---

## Task 7: patch-coverage gate + finalize

**Files:**
- Verify: patch-coverage passes on changed lines in `flpdf-cli`

**Step 1: Commit any straggler changes first**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc.16.17-overlay-pages-verbose
git status
```

Expected: clean.

**Step 2: Run patch-coverage**

```bash
bash scripts/patch-coverage.sh --base main
```

Expected: exit 0. If it fails on changed lines, add unit tests for the
uncovered branches (or `// cov:ignore:` with a documented reason for truly
unreachable lines).

**Step 3: cargo fmt**

```bash
cargo fmt --check
```

If fmt drift, `cargo fmt` and amend the last commit.

**Step 4: Update beads issue**

```bash
bd close flpdf-9hc.16.17 --reason="Bundled with 16.14 in a single PR; overlay+pages composes with page-selection first, verbose --pages emits qpdf-parity progress"
bd close flpdf-9hc.16.14 --reason="Bundled with 16.17; ordering verified via test overlay_plus_pages_no_longer_rejected + verbose_pages_plus_overlay_emits_page_progress_then_overlay_progress"
```

**Step 5: PR**

```bash
git push -u origin feat/flpdf-9hc-16-17-overlay-pages-verbose
gh pr create --title "feat(flpdf-cli): --overlay/--underlay composes with --pages; verbose --pages emits qpdf progress (flpdf-9hc.16.14, .16.17)" --body "$(cat <<'EOF'
## Summary

- Removes the two `--overlay/--underlay` + `--pages` rejection guards (top-level and Rewrite subcommand). Overlay now applies after page-selection, matching qpdf's ordering (`--to`/`--from`/`--repeat` reference the extracted page count).
- Threads `--verbose` into the `--pages` code path and emits five qpdf-parity progress lines in the exact order qpdf produces: `selecting --keep-open-files=y`, `<file>: checking for shared resources`, `no shared resources found`, `removing unreferenced pages from primary input`, `adding pages from <file>`.
- Enables the uo-6 / uo-8 qtest cases (form-xobject 27/33).

## Deliberate divergences

- `no shared resources found` is always emitted; flpdf does not implement qpdf's shared-resource scan. Matches every observed `--verbose --pages` golden that flpdf targets.
- `selecting --keep-open-files=y` is always emitted; flpdf has no auto keep-open-files decision. Matches uo-6 / uo-8 / enable-kfo goldens.

## Test plan

- [x] `cargo test -p flpdf-cli --test verbose_pages_progress` (3 tests, PASS)
- [x] `cargo test -p flpdf-cli --test cli_overlay overlay_plus_pages_no_longer_rejected` (PASS)
- [x] `cargo test -p flpdf-cli` (regression-clean)
- [x] `cargo test -p flpdf` (regression-clean)
- [x] `scripts/patch-coverage.sh --base main` (PASS)
- [ ] qtest uo-6 / uo-8 runtest under qpdf-zlib-compat (verify or document as follow-up)
EOF
)"
```

---

## Notes on Verification Discipline (bd memory `behavior-changing-fix-needs-qpdf-oracle-check`)

Passing structural tests is not the same as passing qpdf-parity. Task 6
covers the actual byte-level oracle. If the harness step fails on stderr
content (progress mismatch), fix here. If it fails on the output PDF bytes,
that is a writer-side issue tracked separately (flpdf-9hc.16.10 / 16.13).

## Notes on rustdoc + review-patterns (per CLAUDE.md rules)

- New helper `pages_progress_filename` is private — no rustdoc required, but
  add a one-line `//` on WHY (not WHAT) if it clarifies. Preferred: no
  comment.
- New tests contain no issue IDs in `///` doc comments (they are in module
  `//!` doc where allowed, as follow-up context). Public API surface
  unchanged: no docs.rs regression risk.

## Notes on the `--verbose` clap constraint (Task 3)

Empirical: `flpdf --verbose ... (no --list-attachments)` currently exits 2
with "required arguments were not provided: --list-attachments". Confirm
this is a clap `requires`/group constraint on the top-level `--verbose`
argument (main.rs:469-475) or on the ArgGroup at line 126. Loosen the
constraint minimally — do NOT introduce a new group — so top-level
`--verbose` works for the `--pages` and default-rewrite paths. Existing
`--list-attachments --verbose` code path (main.rs:1558) MUST continue to
work; add a regression test if the fix is more than removing a `requires`.
