# Overlay `/DR` Merge Differential Fixture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a qpdf 11.9.0 oracle fixture and gated diagnostic test that proves the hidden `/DR/Font` collision diverges from flpdf's current name scan.

**Architecture:** Keep the existing `overlay::byte_gate` harness and writer recipe. Add one minimal destination PDF fixture and one qpdf QDF golden; the test will assert the known divergence plus the copied-field `/DA` and `/DR`/AP resource mappings in their QDF object dictionaries. Do not alter `unique_dr_name` or the doc comments owned by `flpdf-l5in`.

**Tech Stack:** Rust unit tests in `crates/flpdf/src/overlay.rs`, qpdf 11.9.0, PDF/QDF fixtures under `tests/fixtures/compat` and `tests/golden/references/overlay`, feature `qpdf-zlib-compat`.

---

### Task 1: Add the diagnostic test before changing implementation behavior

**Files:**
- Modify: `crates/flpdf/src/overlay.rs:1423-1684`
- Test data referenced: `tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf`
- Oracle referenced: `tests/golden/references/overlay/overlay-dr-merge-hidden-collision.pdf`

- [ ] **Step 1: Add one gated test beside the existing populated-`/AcroForm` overlay gates.**

Use the established `fixture`, `apply_overlay_specs`, `write_qpdf`, and `golden`
helpers. The test must perform the same overlay recipe as qpdf and assert:

```rust
let expected = golden("overlay-dr-merge-hidden-collision.pdf");
assert_ne!(actual, expected);
// Parse the copied widget dictionaries and assert qpdf `/DA` uses `/F1_1`
// while flpdf's recorded divergence uses `/F1_2`.
// Parse the `/DR` and copied AP `/Resources` dictionaries and assert the
// corresponding Helvetica/Courier object mappings in both outputs.
```

Name it
`overlay_copy_annotations_indirect_font_hidden_collision_records_qpdf_divergence`
and gate it with the module's existing `#[cfg(all(test, feature =
"qpdf-zlib-compat"))]` boundary.

- [ ] **Step 2: Run the focused test and observe the expected missing-fixture failure.**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --lib \
  overlay::byte_gate::overlay_copy_annotations_indirect_font_hidden_collision_records_qpdf_divergence
```

Expected: the test reaches the new fixture/golden lookup and fails because the
new data files do not exist yet. No production code is changed.

### Task 2: Create the discriminating destination fixture

**Files:**
- Create: `tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf`

- [ ] **Step 1: Add a minimal one-page destination PDF.**

The document must contain a catalog, one page, an `/AcroForm`, and a direct
`/AcroForm/DR/Font` dictionary with this semantic graph:

```text
/AcroForm/DR/Font <<
  /F0 42 0 R
  /F1 22 0 R
  /F1_1 22 0 R
>>
42 0 obj << /BaseFont /Helvetica /Inner 22 0 R /Subtype /Type1 /Type /Font >>
22 0 obj << /BaseFont /Helvetica /Subtype /Type1 /Type /Font >>
```

The page may use an empty content stream and must be valid for overlaying the
existing one-page annotated source. Verify it with:

```bash
qpdf --check tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf
```

- [ ] **Step 2: Confirm the source path and the hidden collision.**

Run:

```bash
qpdf --json --json-output=2 --json-key=qpdf \
  tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf -
```

Confirm `/Font` is direct, `/F0` resolves to a dictionary containing no
`/F1_1`, and `/F1_1` already exists directly in `/Font`.

### Task 3: Generate and inspect the pinned qpdf golden

**Files:**
- Create: `tests/golden/references/overlay/overlay-dr-merge-hidden-collision.pdf`

- [ ] **Step 1: Generate the oracle with qpdf 11.9.0.**

Run the exact command from the design document:

```bash
qpdf --qdf --static-id --no-original-object-ids --min-version=1.6 \
  tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf \
  --overlay tests/fixtures/compat/form-fields-and-annotations.pdf \
  --repeat=1 -- tests/golden/references/overlay/overlay-dr-merge-hidden-collision.pdf
```

- [ ] **Step 2: Inspect the oracle dictionaries.**

Verify that the destination DR contains the source font at `/F1_1`, copied
field `/DA` strings use `/F1_1`, no copied field `/DA` uses `/F1_2`, and the
copied AP `/Resources` dictionary maps the renamed operand to the Courier font.

- [ ] **Step 3: Run the diagnostic test and make it pass.**

Run the focused command from Task 1. Expected: PASS while asserting that the
current flpdf output is intentionally different from the qpdf golden.

### Task 4: Verify neighboring parity and repository gates

**Files:**
- No additional source files.

- [ ] **Step 1: Run all overlay byte gates.**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
```

Expected: all existing parity gates plus the new diagnostic gate pass.

- [ ] **Step 2: Run formatting and the relevant crate tests.**

```bash
cargo fmt --all -- --check
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_byte_identical_overlay
```

- [ ] **Step 3: Run changed-line coverage and inspect status.**

Use the repository's qpdf-zlib-compatible coverage command and
`scripts/patch-coverage.sh`; then run `git diff --check` and `git status
--short`. Existing unrelated worktree files must not be staged.

- [ ] **Step 4: Record the confirmed divergence and follow-up boundary.**

The implementation issue records fixture/oracle evidence only. The follow-up
must change the collision-name implementation toward qpdf's
`getResourceNames()` scope and then convert this diagnostic assertion into a
byte-identical gate. Do not change `unique_dr_name` in this branch.

- [ ] **Step 5: Commit the focused changes.**

```bash
git add crates/flpdf/src/overlay.rs \
  tests/fixtures/compat/overlay-dr-merge-hidden-collision.pdf \
  tests/golden/references/overlay/overlay-dr-merge-hidden-collision.pdf \
  docs/superpowers/specs/2026-08-11-overlay-dr-merge-fixture-design.md \
  docs/superpowers/plans/2026-08-11-overlay-dr-merge-fixture.md
git commit -m "test: record overlay DR collision divergence"
```
