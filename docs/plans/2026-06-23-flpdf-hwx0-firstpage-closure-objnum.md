# flpdf-hwx0: linearized first-page closure + ordering byte-parity

For Claude: implementation plan for flpdf-hwx0. Full root-cause analysis lives in
the beads issue `design` field (`bd show flpdf-hwx0`). This file is the executable
step list; the issue is the source of truth for *why*.

## Goal (done-signal)

`docs/plans/tools/gen_kept_indirect_length.py` → linearized generate output
(`rewrite --linearize --object-streams=generate --deterministic-id`) byte-identical
to qpdf 11.9.0. Two independent divergences both rooted in
`crates/flpdf/src/linearization/plan.rs`; neither fix alone reaches byte-identical.

## Steps (TDD: failing test first, then each fix, verify after each)

### Step 1 — pin the failing target (test-first)
- Add a structural (miniz) byte test in `crates/flpdf/tests/cmp_linearize_tests.rs`
  for the kept fixture's linearized generate output, plus a `qpdf-zlib-compat`
  gated byte-identical test (mirror existing `assert_linearize_*` helpers / golden
  pattern). Regenerate/commit a qpdf golden if the harness needs one.
- Confirm it FAILS at byte 16 before any fix (object-numbering divergence).

### Step 2 — Fix 1: stop following `/Length` in the linearization closure
- In `collect_direct_refs` (plan.rs:~109) skip the `/Length` key in the
  `Object::Stream` arm ONLY. Keep `/ColorSpace`, `/SMask`, `/Mask`, `/Alternates`.
- Rationale: qpdf directizes `/Length` before computing obj_user, so a kept
  integer holder is never page-reachable; it must route to part9/rest (second
  half) via its catalog edge, not `ContainerPart::FirstPage`.
- Confirm the retain filters (plan.rs:610,642) are now no-ops (orphans never enter
  the closure) — keep them; orphan GC happens via the separate all_refs filter.
- Verify: kept fixture now matches qpdf on object numbering (byte-16 gone);
  orphan control (gen_od) stays byte-identical.

### Step 3 — Fix 2: first-page section ordering = page-dict-first, then source number
- After Step-5 partition (plan.rs:~702), sort `part2_objects` by
  `(r != first_page_ref, r.number)` — page dict pinned first, rest ascending by
  source object number (qpdf part6 order, proven in generate AND disable mode).
- Leave the `/Resources` DFS membership walk intact (only ordering is overridden).
- Verify: kept fixture + MIN-A style fixtures now byte-identical.

### Step 4 — regression + gates
- Full linearize suite green: `cargo test -p flpdf --features qpdf-zlib-compat
  --test cmp_linearize_tests --test kept_indirect_length_holder_tests
  --test cmp_diff_zero_tests` (+ broader `cargo test -p flpdf`).
- `cargo fmt`, clippy.
- `scripts/patch-coverage.sh` (100% changed-line coverage in `flpdf`).
- Add the new `qpdf-zlib-compat` byte test to `ci.yml`'s explicit byte-test list
  (memory: flpdf-ci-bytes-identical-explicit-test-list).

## Risk
Fix 2 touches all first-page ordering — the full linearize byte suite is the
safety net. Fix 1 is surgical (only ever affects `/Length` integer holders).
