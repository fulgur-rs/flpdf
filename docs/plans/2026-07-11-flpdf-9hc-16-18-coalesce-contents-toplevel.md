# flpdf-9hc.16.18: top-level `--coalesce-contents` alias — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Expose `--coalesce-contents` as a top-level qpdf-shaped alias so `flpdf --coalesce-contents INPUT OUTPUT` and the trailing-position form used by qtest form-xobject uo-3 both work, mirroring the existing `--qdf` / `--no-original-object-ids` pattern.

**Architecture:** Add one `bool` field to the top-level `Cli` struct with clap `conflicts_with_all` mirroring `--decrypt`; replace the two hardcoded `false, // coalesce_contents` positional arguments in the dispatch chain with `args.coalesce_contents`; also list the new field in `--json`'s existing `conflicts_with_all`. No changes to `extract_overlay_groups` (the token already flows to clap correctly — the trailing-position bug lives in clap, not in the raw-argv splitter). No new coalesce semantics: reuse the existing `Rewrite` subcommand path.

**Tech Stack:** Rust, clap derive, assert_cmd for CLI e2e tests.

**Design source:** beads issue `flpdf-9hc.16.18` (design field).

---

## Prerequisites

- Worktree: `/home/ubuntu/flpdf/.worktrees/fix/flpdf-9hc-16-18-coalesce-contents-toplevel` (branch `fix/flpdf-9hc-16-18-coalesce-contents-toplevel`)
- Baseline: `cargo build --bin flpdf` clean; `rewrite_coalesce_contents_accepted_and_produces_valid_output` passes (verified at worktree creation).

**Key file references** (line numbers as of `main` @ `926d4f4`):
- `crates/flpdf-cli/src/main.rs`
  - Top-level `Cli` struct: L136
  - `--json` `conflicts_with_all`: L189–199
  - `--qdf` field (insertion anchor for the new field): L362–363
  - `--linearize` branch `run_rewrite` call: L1603–1620 (the `false, // coalesce_contents` line at L1612)
  - Default rewrite branch `run_rewrite` call: L1765–1782 (the `false, // coalesce_contents` line at L1774)
  - `extract_overlay_groups`: L3604
  - `extract_*` unit tests: L5468 onward (in the same file, `#[cfg(test)] mod tests`)
- `crates/flpdf-cli/tests/cli_tests.rs`
  - `rewrite_coalesce_contents_accepted_and_produces_valid_output`: L1662 (template for the new test)
  - `two_page_pdf_with_multi_contents` helper: L1504

---

## Task 1: Regression test — extract_overlay_groups already passes trailing top-level flag through

**Rationale:** The issue's original diagnosis pointed at `extract_overlay_groups`. It's a false lead — the function is correct — but the test anchors the invariant so a future refactor doesn't reintroduce the confusion.

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` — add one `#[test]` next to the existing extract tests around L5559.

**Step 1: Add the failing/passing test**

Insert this after `extract_captures_sub_flags_per_group` (~L5559):

```rust
    #[test]
    fn extract_leaves_trailing_top_level_flag_after_group_terminator() {
        // qtest form-xobject uo-3 style: a top-level flag appears AFTER the
        // overlay/underlay group's `--` terminator. The extractor must place
        // that trailing flag verbatim into the residual argv so clap sees it.
        // A regression here would reintroduce the flpdf-9hc.16.18 diagnosis
        // trap ("blame the extractor when the top-level flag is missing from
        // clap's schema").
        let argv = strs(&[
            "flpdf",
            "in.pdf",
            "out.pdf",
            "--overlay",
            "src.pdf",
            "--",
            "--coalesce-contents",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(
            residual,
            strs(&["flpdf", "in.pdf", "out.pdf", "--coalesce-contents"])
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].file, "src.pdf");
    }
```

**Step 2: Run — expect PASS (invariant already holds)**

```bash
cd /home/ubuntu/flpdf/.worktrees/fix/flpdf-9hc-16-18-coalesce-contents-toplevel
cargo test --package flpdf-cli --bin flpdf extract_leaves_trailing_top_level_flag_after_group_terminator -- --nocapture
```

Expected: `test result: ok. 1 passed`. If it fails, stop — the extractor is not what we thought.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "test(flpdf-cli): anchor extract_overlay_groups trailing-flag invariant (flpdf-9hc.16.18)"
```

---

## Task 2: Failing test — top-level `--coalesce-contents` accepted

**Rationale:** Red step for the actual fix. Mirrors the existing subcommand-level test one-for-one, differing only by dropping the `"rewrite"` argv token.

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs` — add a `#[test]` right after `rewrite_coalesce_contents_accepted_and_produces_valid_output` (L1682).

**Step 1: Add the failing test**

Insert after L1682:

```rust
#[test]
fn top_level_coalesce_contents_accepted_and_produces_valid_output() {
    // Top-level alias of `flpdf rewrite --coalesce-contents` (qpdf-shape).
    // Mirrors rewrite_coalesce_contents_accepted_and_produces_valid_output,
    // dropping only the "rewrite" argv token.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--coalesce-contents"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", output.to_str().unwrap()])
        .assert()
        .success();
}
```

**Step 2: Run — expect FAIL with clap "unexpected argument"**

```bash
cargo test --package flpdf-cli --test cli_tests top_level_coalesce_contents_accepted_and_produces_valid_output -- --nocapture 2>&1 | tail -20
```

Expected: `unexpected argument '--coalesce-contents' found`, test fails. Do NOT commit yet.

---

## Task 3: Add top-level `coalesce_contents` field + wire dispatch chain

**Rationale:** Green step. Two mechanical edits: add the field, replace two hardcoded `false` values.

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` L189–199 (extend `--json` conflicts_with_all), L362–363 (add the new field after `--qdf`), L1612 and L1774 (dispatch calls).

**Step 1: Extend `--json` `conflicts_with_all`**

Find (~L189):
```rust
          conflicts_with_all = [
              "check", "linearize", "static_id", "deterministic_id", "static_aes_iv",
              "dump_object",
              "show_info", "show_catalog", "show_metadata", "show_outline",
              "show_fonts", "show_npages", "show_pages", "show_linearization", "output",
              "compress_streams", "linearize_pass1", "remove_restrictions",
              "decrypt", "encrypt", "copy_encryption_from",
              "add_attachment", "remove_attachment", "list_attachments",
              "show_attachment", "copy_attachments_from",
              "no_original_object_ids", "qdf",
          ],
```

Add `"coalesce_contents",` on the same last line as `"no_original_object_ids", "qdf",`:

```rust
              "no_original_object_ids", "qdf", "coalesce_contents",
```

**Step 2: Add the new field right after `qdf`**

Find (~L362):
```rust
    #[arg(long = "qdf")]
    qdf: bool,
```

Insert immediately after `qdf: bool,` (before the blank line and the `// ── Page-operation flags ...` comment at L365):

```rust
    /// Coalesce multiple /Contents streams into a single stream per page
    /// (top-level alias of `flpdf rewrite --coalesce-contents`; qpdf
    /// `--coalesce-contents` equivalent). Requires a full rewrite of the
    /// document. Rejected against inspection / attachment modes so a
    /// silently-ignored flag surfaces as a usage error instead.
    #[arg(long = "coalesce-contents",
          conflicts_with_all = [
              "check", "dump_object", "show_info", "show_catalog",
              "show_metadata", "show_outline", "show_fonts",
              "show_npages", "show_pages", "show_linearization",
              "list_attachments", "show_attachment", "remove_attachment",
          ])]
    coalesce_contents: bool,
```

**Step 3: Wire the `--linearize` branch**

Find (~L1612):
```rust
            false,                              // normalize_content
            false,                              // coalesce_contents
```

Replace the second line:
```rust
            false,                              // normalize_content
            args.coalesce_contents,
```

Drop the trailing `// coalesce_contents` comment — the identifier name now speaks for itself.

**Step 4: Wire the default rewrite branch**

Find (~L1774):
```rust
            false,                              // normalize_content
            false,                              // coalesce_contents
```

Replace the second line the same way:
```rust
            false,                              // normalize_content
            args.coalesce_contents,
```

**Step 5: Run Task 2's test — expect PASS**

```bash
cargo test --package flpdf-cli --test cli_tests top_level_coalesce_contents_accepted_and_produces_valid_output -- --nocapture
```

Expected: PASS.

**Step 6: Sanity — subcommand path unchanged**

```bash
cargo test --package flpdf-cli --test cli_tests rewrite_coalesce_contents_accepted_and_produces_valid_output -- --nocapture
```

Expected: PASS (no regression on the subcommand path).

**Step 7: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "feat(flpdf-cli): --coalesce-contents as top-level qpdf-shaped alias (flpdf-9hc.16.18)"
```

---

## Task 4: Failing test — clap conflicts reject inspection combination

**Rationale:** Red step for the conflicts_with_all list. Ensures a silent-ignore regression can't slip past by verifying `flpdf --check --coalesce-contents` exits 2.

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs` — add one `#[test]` next to the previous one.

**Step 1: Add the test**

Insert right after `top_level_coalesce_contents_accepted_and_produces_valid_output`:

```rust
#[test]
fn top_level_coalesce_contents_conflicts_with_check() {
    // A silent-ignore combination (--check would win the dispatch chain over
    // any rewrite modifier) would produce wrong output. clap must surface it
    // as a usage error, exit 2 (qpdf convention). Mirrors how --decrypt /
    // --remove-restrictions are gated.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--check", "--coalesce-contents", "in.pdf"])
        .assert()
        .failure()
        .code(2);
}
```

**Step 2: Run — expect PASS if Task 3 wired conflicts correctly**

```bash
cargo test --package flpdf-cli --test cli_tests top_level_coalesce_contents_conflicts_with_check -- --nocapture
```

Expected: PASS. If it fails with exit code 3 or 1, the clap `conflicts_with_all` is missing entries.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_tests.rs
git commit -m "test(flpdf-cli): --coalesce-contents rejects inspection combos (flpdf-9hc.16.18)"
```

---

## Task 5: Reproducer test — the exact issue command exits 0

**Rationale:** The issue's headline reproducer must pass. This is the acceptance-linked test: it's the shape qtest form-xobject uo-3 emits under the PATH-shim.

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_tests.rs` — add after the conflicts test.

**Step 1: Add the test**

Insert:

```rust
#[test]
fn top_level_coalesce_contents_with_overlay_underlay_trailing_position() {
    // The exact shape qtest form-xobject uo-3 emits (via the PATH-shim
    // qpdf→flpdf): --coalesce-contents at the very end of argv, after
    // TWO overlay/underlay groups each terminated by `--`. The parser
    // must let the trailing top-level flag through to clap, and clap
    // must accept it (see flpdf-9hc.16.18). We only assert exit 0 —
    // byte-parity of the output is a separate concern.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("in.pdf");
    let overlay = temp.path().join("over.pdf");
    let underlay = temp.path().join("under.pdf");
    let output = temp.path().join("out.pdf");
    std::fs::write(&input, two_page_pdf_with_multi_contents()).unwrap();
    std::fs::write(&overlay, two_page_pdf_with_multi_contents()).unwrap();
    std::fs::write(&underlay, two_page_pdf_with_multi_contents()).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "--static-id",
            "--qdf",
            "--no-original-object-ids",
            "--verbose",
        ])
        .arg(&input)
        .arg(&output)
        .arg("--overlay")
        .arg(&overlay)
        .args(["--from=", "--repeat=r2,r1", "--"])
        .arg("--underlay")
        .arg(&underlay)
        .args(["--from=z-1", "--", "--coalesce-contents"])
        .assert()
        .success();

    assert!(output.exists());
}
```

**Note on the FLPDF_STATIC_ID_QUIET env var:** the flpdf-cli static-id warning writes to stderr; assert_cmd's `.success()` doesn't check stderr, but suppressing the warning keeps the test output tidy under `--nocapture`. Justification: matches memory `flpdf-cli-e2e-byte-compare-needs-static-id` guidance.

**Step 2: Run — expect PASS**

```bash
cargo test --package flpdf-cli --test cli_tests top_level_coalesce_contents_with_overlay_underlay_trailing_position -- --nocapture
```

Expected: PASS.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_tests.rs
git commit -m "test(flpdf-cli): reproducer for --coalesce-contents trailing after overlay group (flpdf-9hc.16.18)"
```

---

## Task 6: Full local quality gates

**Files:** none (verification only).

**Step 1: cargo fmt --check**

```bash
cargo fmt --all -- --check
```

Expected: no diff. If diff, `cargo fmt --all` then commit as `style(flpdf-cli): cargo fmt (flpdf-9hc.16.18)`.

**Step 2: cargo clippy**

```bash
cargo clippy --workspace --all-targets --no-default-features -- -D warnings 2>&1 | tail -20
```

Expected: no warnings/errors.

**Step 3: Full flpdf-cli test suite**

```bash
cargo test --package flpdf-cli 2>&1 | tail -20
```

Expected: all pass. Existing subcommand paths must not regress.

**Step 4: workspace test suite (default features)**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all pass.

**Step 5: patch-coverage**

```bash
scripts/patch-coverage.sh --base main
```

Expected:
- flpdf: 100% patch coverage (there are no changes under `crates/flpdf/`, so this is a no-op for the strict gate).
- flpdf-cli: report only; strive for coverage on the new lines (the tests in Tasks 1–5 cover the added field, dispatch wiring, and conflicts list). No exit≠0 required from flpdf-cli.

If any commit is missing, patch-coverage errors on dirty tree — commit before running per memory `llvm-cov-no-qpdf-zlib-compat`.

---

## Task 7: PR

**Files:** none.

**Step 1: Push**

```bash
git push -u origin fix/flpdf-9hc-16-18-coalesce-contents-toplevel
```

**Step 2: gh pr create**

```bash
gh pr create --title "fix(flpdf-cli): --coalesce-contents as top-level qpdf-shaped alias (flpdf-9hc.16.18)" --body "$(cat <<'EOF'
## Summary

Expose `--coalesce-contents` as a top-level flag on the qpdf-shaped CLI surface, mirroring the existing `--qdf` / `--no-original-object-ids` / `--decrypt` pattern. Wires the flag into both dispatch branches that can run the rewrite path (the default rewrite branch and the `--linearize` branch) so the existing `Rewrite` subcommand semantics reach the top-level surface — no new coalesce logic.

Unblocks qtest form-xobject test 23 (overlay/underlay 3 / uo-3), whose command shape places `--coalesce-contents` as the trailing token after two `--`-terminated overlay/underlay groups.

## Root cause (correcting the issue's diagnosis)

The issue attributed the failure to `extract_overlay_groups`. It's a false lead: the extractor already forwards the trailing `--coalesce-contents` token into the residual argv (verified by a new regression test); clap then rejects it because `--coalesce-contents` was only defined on `RewriteCommand`. The fix is entirely at the `Cli` struct + dispatch level. `extract_overlay_groups` is untouched.

## Changes

- Add `coalesce_contents: bool` to the top-level `Cli` struct with `conflicts_with_all` that rejects silent-shadow combinations against inspection / attachment modes.
- Add `"coalesce_contents"` to `--json`'s existing `conflicts_with_all`, matching how `--qdf` etc. are listed there.
- Replace two `false, // coalesce_contents` hardcoded arguments in the dispatch chain with `args.coalesce_contents` (default rewrite branch + `--linearize` branch).

## Tests

- `extract_leaves_trailing_top_level_flag_after_group_terminator` — regression net anchoring the invariant the issue's diagnosis assumed was broken.
- `top_level_coalesce_contents_accepted_and_produces_valid_output` — top-level surface parity with the existing subcommand test.
- `top_level_coalesce_contents_conflicts_with_check` — silent-shadow guard.
- `top_level_coalesce_contents_with_overlay_underlay_trailing_position` — the exact issue reproducer.

Existing `rewrite --coalesce-contents` tests untouched.

## Test plan

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets --no-default-features -- -D warnings`
- [x] `cargo test --workspace`
- [x] `scripts/patch-coverage.sh --base main`

Closes flpdf-9hc.16.18.
EOF
)"
```

**Step 3: bd close on merge (post-PR)**

Do NOT close the bd issue yet — leave it in_progress until the PR merges. The Blueprint:impl completion step (Step 6) will handle the close prompt after `verification-before-completion` and `finishing-a-development-branch`.
