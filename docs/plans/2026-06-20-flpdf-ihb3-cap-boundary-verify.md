# flpdf-ihb.3: cap-boundary stranded-container — verify & close

> For Claude: this plan executes the bd issue `flpdf-ihb.3` (P0). Read its
> `design` field first (`bd show flpdf-ihb.3`) — it has the re-investigation
> conclusion this plan is built on. This is a **verify-and-close** task
> (same shape as flpdf-vvjr.3 / flpdf-699x), NOT the original 3-part fix:
> the consistency bug it describes no longer reproduces on current code.

## Goal

Prove that the ihb.3 inconsistency (page-0 object count vs shared-object hint
table disagreement at the cap boundary → `qpdf --check-linearization` not clean)
is resolved on current code, lock it in with a regression fixture + byte test,
and close the issue. If a residual *byte-parity* gap (not a consistency gap)
remains at the exact boundary, file a separate ihb-family follow-up — do NOT
reopen ihb.3's consistency scope.

## Background (verified this session, default-features probe)

- `gen_shared_fonts.py N` emits N first-page-shared font dicts (all eligible).
- N = 96, 98, 99, **100 (= exact cap)**, 101 → `qpdf --check-linearization`
  reports **"no linearization errors"** (0 WARNING). The cap-boundary case is
  clean. Structural fields (`flpdf --show-linearization`: H_offset, page
  nobjects, first_shared_obj) match qpdf at N=100.
- The 5-byte `first_page_offset` diff seen in the probe was a miniz-vs-zlib
  deflate artifact (default features), NOT a layout gap — re-check under
  `qpdf-zlib-compat`.

## Tasks

### Task 1 — Pin the exact-cap-boundary fixture

- Determine the fixture whose **eligible first-page-shared count is an exact
  multiple of cap (100)** so `canonicalise_first_half_batch` spills the extras
  (`/Info` + `/Pages`) into a separate first-half ObjStm container — the precise
  ihb.3 trigger. Start from `gen_shared_fonts.py` and find the N (the generator
  emits N fonts; confirm the eligible count, which may include `/Pages`).
- ihb.3 names `/Info` + `/Pages`; the current generator emits **no `/Info`**.
  Either add an `/Info` dict to the generator (so the stranded container holds
  both, matching the original bug shape) or confirm `/Pages`-only still forms
  the stranded container. Decide by what qpdf actually does — generate the qpdf
  golden and inspect.
- Verify the stranded container forms: `flpdf --show-linearization` →
  count first-half ObjStm containers; confirm one holds only the extras.
- Commit the fixture as `tests/fixtures/compat/objstm-lin-cap-boundary-NNN.pdf`
  and add its generation to `tests/golden/regenerate.sh`.

### Task 2 — Byte + consistency regression test

- Generate the qpdf golden:
  `qpdf --linearize --object-streams=generate --deterministic-id <fixture>
  tests/golden/references/<stem>/linearize-objstm.pdf`; add to regenerate.sh.
- Build `qpdf-zlib-compat` and assert flpdf is **byte-identical** to the golden;
  add **strict + structural** tests to `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`
  (already in `ci.yml`'s qpdf-zlib-compat byte-test list).
- Assert `qpdf --check-linearization` is clean for the fixture (a CLI/structural
  test, no zlib-compat needed) — this is the direct ihb.3 regression guard.

### Task 3 — Close (or file residual)

- **Byte-identical + check-lin clean** → ihb.3 verified-resolved. Close it.
  Then confirm `flpdf-ihb` (which depends on this) has no other open consistency
  work, unblocking `flpdf-vvjr`.
- **Residual byte gap at the exact boundary** (a real Part-3-packing divergence,
  but check-lin still clean) → file a new ihb-family follow-up describing it;
  still close ihb.3 (its *consistency* scope is met).

### Task 4 — Gates

- `cargo fmt --all -- --check`, `cargo clippy -p flpdf --all-targets -- -D warnings`.
- objstm byte suite (incl `sharedfonts-100`) stays green; classic suite unchanged.
- `scripts/patch-coverage.sh --base origin/main` = 100% (mostly test-only change).
- doc-link gate.

## Out of scope

- The broader ihb Part-3-packing member-set parity (separate epic work).
- ihb.3's original 3-part coordinated fix — the consistency bug is already gone.
