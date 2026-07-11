# Overlay source: implicit weak-crypto open (qpdf-parity) — flpdf-9hc.16.19

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `flpdf-cli` accept an RC4-encrypted overlay source (with correct `--password`) without the top-level `--allow-weak-crypto` flag, matching qpdf's silent-accept behavior and unblocking qtest `form-xobject` test 31 (uo-7).

**Architecture:** Inside `build_overlay_specs` (`crates/flpdf-cli/src/main.rs`), force `PdfOpenOptions.allow_weak_crypto = true` for every overlay source. Drop the now-vestigial `allow_weak_crypto` parameter and update the single call site plus in-file unit tests. Mirrors the same treatment `run_check` gives its read-only inspection open (line 2280-2282).

**Tech Stack:** Rust, cargo, flpdf-cli bin crate, flpdf lib (`Pdf::open_with_options`, `PdfOpenOptions.allow_weak_crypto`).

---

### Anchor references

- Function to modify: `crates/flpdf-cli/src/main.rs:3703` (`fn build_overlay_specs`)
- Call site: `crates/flpdf-cli/src/main.rs:3134`
- Existing tests to update: `crates/flpdf-cli/src/main.rs:5719, 5738, 5758, 5794, 5797, 5800, 5821, 5824, 5827, 5848, 5851, 5857` (drop 3rd `false` arg from `build_overlay_specs(...)` calls)
- Existing test to rewrite: `crates/flpdf-cli/src/main.rs:5864` (`build_overlay_specs_threads_allow_weak_crypto_to_source` — premise inverts)
- qpdf oracle output: `/home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-7.out`
- qpdf oracle PDF: `/home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-7.pdf`

---

### Task 1: Rewrite the existing unit test to assert the new behavior (RED)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:5864-5888`

**Step 1: Rewrite the test in place**

Replace `build_overlay_specs_threads_allow_weak_crypto_to_source` (5864-5888) with a test that asserts the new implicit-allow behavior. Rename it to `build_overlay_specs_opens_rc4_source_without_allow_weak_crypto`.

```rust
#[test]
fn build_overlay_specs_opens_rc4_source_without_allow_weak_crypto() {
    // qpdf accepts RC4 (weak-crypto) source opens unconditionally: the flag
    // gates weak-crypto WRITES, not reads. Overlay sources are read-only,
    // so `build_overlay_specs` opens them with `allow_weak_crypto = true`
    // regardless of the top-level flag — the same treatment `run_check`
    // gives its inspection open. Verifies flpdf-cli matches qpdf uo-7.
    let cli_specs = vec![OverlaySpec {
        kind: OverlayKind::Overlay,
        file: encrypted_fixture("v2-rc4-128-r3.pdf"),
        password: Some("user-v2".into()),
        from: None,
        to: None,
        repeat: None,
    }];
    // No `allow_weak_crypto` parameter after this change; the open succeeds
    // on the RC4 source without the caller opting in.
    let built = build_overlay_specs(&cli_specs, false).unwrap();
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].kind, flpdf::OverlayKind::Overlay);
}
```

**Step 2: Run test to confirm it fails (compile error is a valid RED here)**

Run: `cargo test -p flpdf-cli --bin flpdf build_overlay_specs_opens_rc4_source_without_allow_weak_crypto 2>&1 | tail -20`

Expected: compile error — `build_overlay_specs` still takes 3 args on `main`. This is the failing state for TDD's RED step.

**Step 3: Do NOT commit yet.** The signature change lands in Task 2; committing a RED build here would break bisect.

---

### Task 2: Change `build_overlay_specs` signature and force implicit weak-crypto (GREEN for Task 1)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:3703-3721` (function signature + body)
- Modify: `crates/flpdf-cli/src/main.rs:3134` (call site — drop 3rd arg)
- Modify: `crates/flpdf-cli/src/main.rs` unit test call sites: 5719, 5738, 5758, 5794, 5797, 5800, 5821, 5824, 5827, 5848, 5851, 5857 (drop `, false` from `build_overlay_specs(..., false, false)`)

**Step 1: Update `build_overlay_specs` signature and body**

Change (line 3703):

```rust
fn build_overlay_specs(
    specs: &[OverlaySpec],
    repair: bool,
    allow_weak_crypto: bool,
) -> CliResult<Vec<flpdf::OverlaySpec<BufReader<File>>>> {
```

to:

```rust
fn build_overlay_specs(
    specs: &[OverlaySpec],
    repair: bool,
) -> CliResult<Vec<flpdf::OverlaySpec<BufReader<File>>>> {
```

Inside the loop (line 3712-3721), change:

```rust
        let options = PdfOpenOptions {
            repair,
            allow_weak_crypto,
            password: spec
                .password
                .as_ref()
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..Default::default()
        };
```

to:

```rust
        // Overlay sources are read-only; qpdf accepts weak-crypto opens
        // unconditionally (the flag only gates weak-crypto WRITES). Match
        // qpdf and unblock RC4 overlays — same pattern `run_check` uses
        // for its inspection open.
        let options = PdfOpenOptions {
            repair,
            allow_weak_crypto: true,
            password: spec
                .password
                .as_ref()
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..Default::default()
        };
```

**Step 2: Update docstring**

At line 3699 (the current `# Errors` section mentions authentication only; keep it accurate — no change needed unless the paragraph references `allow_weak_crypto`). Read the docstring block starting at ~3686 and remove any mention of `allow_weak_crypto` as a caller-supplied gate; add one sentence stating overlay sources are opened with `allow_weak_crypto = true` unconditionally (qpdf-parity). Keep the doc English-only per `.claude/rules/pdf-rust-doc-review-patterns.md` rule 5.

**Step 3: Update the sole non-test call site**

Line 3134:

```rust
let mut built = build_overlay_specs(overlay_specs, repair, password.allow_weak_crypto)?;
```

becomes:

```rust
let mut built = build_overlay_specs(overlay_specs, repair)?;
```

**Step 4: Update in-file test call sites**

For each of these lines, drop the final `, false` / `, true`:

- 5719, 5738, 5758 (build_overlay_specs_opens_source_and_maps_fields / defaults_ranges_when_absent / missing_file_errors)
- 5794, 5797, 5800 (distinguishes_absent_and_empty_from)
- 5821, 5824, 5827 (distinguishes_absent_and_empty_to)
- 5848, 5851, 5857 (distinguishes_absent_and_empty_repeat)

Each `build_overlay_specs(&x, false, false)` → `build_overlay_specs(&x, false)`.
The rewritten test from Task 1 already uses the new signature.

**Step 5: Run all `build_overlay_specs` tests**

Run: `cargo test -p flpdf-cli --bin flpdf build_overlay_specs 2>&1 | tail -25`

Expected: 7 tests pass (the six existing structural tests + the rewritten RC4 test). No compile errors.

**Step 6: Run the full `flpdf-cli` test suite for regressions**

Run: `cargo test -p flpdf-cli 2>&1 | tail -40`

Expected: 0 failures. Any failure surfaces a call site or contract we missed.

**Step 7: Commit**

```bash
git add crates/flpdf-cli/src/main.rs docs/plans/2026-07-12-flpdf-9hc-16-19-overlay-rc4-implicit-weak-crypto.md
git commit -m "fix(flpdf-cli): implicit weak-crypto for overlay source open (qpdf-parity)

Overlay sources are read-only; qpdf accepts weak-crypto opens unconditionally.
Force allow_weak_crypto=true inside build_overlay_specs and drop the vestigial
parameter. Unblocks qtest form-xobject test 31 (uo-7): an RC4-encrypted overlay
source with --password now opens without --allow-weak-crypto.

flpdf-9hc.16.19"
```

---

### Task 3: Add a CLI integration test that reproduces the uo-7 invocation shape

**Files:**
- Modify (or create): pick the existing overlay-focused CLI integration test file. Check first with:

  ```bash
  ls crates/flpdf-cli/tests/ | grep -i overlay
  ```

  Add the new test to the most closely-scoped file (likely `cli_overlay.rs` or similar). If no overlay CLI test file exists, add it to `cli_overlay_rc4.rs` (new file).

**Step 1: Write the failing integration test**

Use `assert_cmd::Command` (the harness already used elsewhere in the same directory) and a fixture path. Prefer the qtest fixture `20-pages.pdf` if it's already vendored under `crates/flpdf-cli/tests/fixtures/` or `tests/fixtures/`; otherwise use `crates/flpdf/tests/fixtures/encrypted/v2-rc4-128-r3.pdf` as the RC4 source (the same fixture Task 1 uses). Look for prior overlay CLI test files first and copy the fixture-lookup helper they use.

```rust
#[test]
fn overlay_from_rc4_source_succeeds_without_allow_weak_crypto() {
    // qpdf-parity for qtest form-xobject 31 (uo-7): RC4-encrypted overlay
    // source with correct --password must open without the top-level
    // --allow-weak-crypto flag.
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.pdf");
    let dest = <path to a plain destination PDF, e.g. compat one-page.pdf>;
    let rc4_src = <path to RC4-encrypted overlay source with a known password>;

    let assert = Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--static-id")
        .arg(dest)
        .arg(&out)
        .arg("--overlay")
        .arg(rc4_src)
        .arg("--password=<known password>")
        .arg("--")
        .assert();

    assert.success();
    // The output PDF must exist (non-zero size); byte-identity vs qpdf golden
    // is covered by the qtest harness / .16.13 byte gate, not this test.
    let bytes = std::fs::read(&out).unwrap();
    assert!(!bytes.is_empty(), "output PDF must be non-empty");
}
```

Adapt the destination and source fixture paths to what the repo actually has (grep `crates/flpdf-cli/tests/*.rs` for existing usage of these fixtures and reuse the helper). Do NOT hand-craft new fixtures unless nothing suitable exists.

**Step 2: Run it — expect it to pass because Task 2 already changed the behavior**

Run: `cargo test -p flpdf-cli --test <test_file_basename> overlay_from_rc4_source_succeeds_without_allow_weak_crypto 2>&1 | tail -10`

Expected: PASS. If it fails with a "encrypted PDF: encryption uses weak crypto" error, Task 2 is incomplete — go back and verify the call site was updated.

**Step 3: Run the full test suite one more time to confirm no regression from the new test**

Run: `cargo test -p flpdf-cli 2>&1 | tail -20`

Expected: all pass.

**Step 4: Commit**

```bash
git add crates/flpdf-cli/tests/<test_file>.rs
git commit -m "test(flpdf-cli): overlay from RC4 source succeeds without --allow-weak-crypto

CLI integration test mirroring qtest form-xobject 31 (uo-7).

flpdf-9hc.16.19"
```

---

### Task 4: Verify against the qpdf uo-7 golden directly

**Files:** (no code changes; verification only)

**Step 1: Build a release-ish debug binary in the worktree**

Run: `cargo build -p flpdf-cli 2>&1 | tail -3`

Expected: `Finished`.

**Step 2: Reproduce the exact uo-7 command using worktree binary**

```bash
cd $(mktemp -d)
cp /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/fxo-red.pdf .
cp /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/20-pages.pdf .
/home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-19-overlay-rc4/target/debug/flpdf \
  --static-id --qdf --no-original-object-ids --verbose \
  fxo-red.pdf a.pdf \
  --overlay 20-pages.pdf --password=user -- 2>uo7.stderr
echo "exit=$?"
```

Expected: `exit=0`.

**Step 3: Compare stderr to qpdf golden (after shim `flpdf:` → `qpdf:` normalization)**

```bash
sed 's/^flpdf:/qpdf:/' uo7.stderr > uo7.qpdf.stderr
diff uo7.qpdf.stderr /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-7.out
```

Expected: no diff (except possibly the `wrote file a.pdf` line if the target path differs from `a.pdf` — the qtest driver uses `a.pdf` as target, so passing that same name gives zero diff).

**Step 4: Compare output PDF to qpdf golden byte-for-byte**

```bash
cmp a.pdf /home/ubuntu/flpdf-qtest/vendor/qpdf-qtest/qpdf/uo-7.pdf; echo "cmp exit=$?"
```

Expected: `cmp exit=0` (byte-identical).

**Step 5: Record the verification result**

If both diff and cmp are clean, no commit is needed here — verification is a check, not a code change. If either fails, that surfaces a regression; go back to Task 2/3 and diagnose.

Optionally, capture the verification result to the beads issue:

```bash
bd update flpdf-9hc.16.19 --notes "uo-7 direct verification: stderr matches uo-7.out, PDF byte-identical to uo-7.pdf (verified $(date -I))"
```

*(Skip `date` if it's blocked; write the date literally.)*

---

### Task 5: Coverage gate

**Files:** (no code changes; gate only)

**Step 1: Run the patch-coverage script**

Run: `scripts/patch-coverage.sh --base origin/main 2>&1 | tail -40`

Expected: no uncovered changed lines in `crates/flpdf` (there aren't any — all changes are in `flpdf-cli`). `flpdf-cli` uncovered lines are report-only per CLAUDE.md but should still be near-zero on this diff (it's a signature/literal change).

**Step 2: If any changed line is uncovered, decide**

- If the uncovered line is genuinely reachable but unexercised, add a test (extend Task 3's integration test or add a variant).
- If it's a signature-only change with no runtime path (e.g. an updated docstring line), leave it — the gate reports but does not block for `flpdf-cli`.
- Do NOT use `// cov:ignore` for this change; there is no genuinely untestable line here.

**Step 3: Commit any additional test if added**

```bash
git add crates/flpdf-cli/tests/<file>.rs
git commit -m "test(flpdf-cli): extend coverage on overlay implicit weak-crypto path

flpdf-9hc.16.19"
```

---

### Task 6: Final checks and PR handoff

**Files:** (no code changes)

**Step 1: Sanity — full workspace test**

Run: `cargo test 2>&1 | tail -30`

Expected: all pass.

**Step 2: Fmt check (per bd memory: fmt is a CI gate)**

Run: `cargo fmt --check 2>&1 | tail -5`

Expected: no output (clean). If diffs surface, run `cargo fmt` and amend the last commit with `git commit --amend --no-edit` (do NOT use `--no-verify`).

**Step 3: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`

Expected: no warnings/errors.

**Step 4: Hand off to `superpowers:finishing-a-development-branch`**

Once all three checks are clean, follow `superpowers:finishing-a-development-branch` to open the PR against `main`. PR title: `fix(flpdf-cli): implicit weak-crypto for overlay source open (qpdf-parity)`.

PR body must reference:
- `flpdf-9hc.16.19`
- The uo-7 direct-verification result (stderr + PDF byte-identical)
- The design rationale (overlay source is read-only, qpdf accepts unconditionally, mirrors `run_check`'s treatment)

---

### Acceptance recap

- New / updated unit test `build_overlay_specs_opens_rc4_source_without_allow_weak_crypto` green
- New CLI integration test `overlay_from_rc4_source_succeeds_without_allow_weak_crypto` green
- Direct uo-7 invocation: exit 0, stderr matches golden, output PDF byte-identical to `uo-7.pdf`
- `cargo fmt --check` clean, `cargo clippy` clean, workspace tests green
- `scripts/patch-coverage.sh` passes for `crates/flpdf` (no changes there); `crates/flpdf-cli` uncovered changed lines minimal
