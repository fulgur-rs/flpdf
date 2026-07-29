# qpdf Observable Parity Beads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Materialize the approved qpdf 11.9.0 observable-parity roadmap as a dependency-ordered Beads graph without duplicating existing work.

**Architecture:** Create one convergence root epic, seven phase epics, ten new gap issues, and four coordination-only child completion gates. Existing qtest, repair, Pipeline, CLI, and writer issues remain authoritative and are linked through type-compatible blocking dependencies; only `flpdf-egzr` is rewritten to reflect the approved C-API exclusion.

**Tech Stack:** Beads/Dolt (`bd`), Git, Markdown design and plan documents.

## Global Constraints

- qpdf 11.9.0 is the source and behavioral oracle.
- The official parity gate targets Linux x86_64.
- C and C++ ABI or symbol compatibility is out of scope.
- `qpdf-ctest` is not ported; its underlying PDF behavior is mapped to Rust oracle tests.
- `qpdf-zlib-compat` requires byte identity; the Pure Rust default requires semantic and structural identity.
- Existing Beads remain authoritative and must not be duplicated or reparented.
- Beads permits epic-to-epic blocking but rejects non-epic blockers on an epic.
  Non-epic tasks, bugs, and features may use mixed blocking types. Required
  non-epic work blocks a child completion-gate task; `relates-to` is reserved
  for non-blocking roadmap associations.
- New implementation slices use consumer RED, the smallest lower-level primitive, production wiring, old-route deletion, differential verification, and 100% changed executable-line coverage.
- Beads state and Git commits must both be pushed before handoff.

## State Responsibilities

- `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`: approved scope and roadmap authority.
- `docs/superpowers/plans/2026-07-29-qpdf-observable-parity-beads.md`: reproducible Beads mutation and verification procedure.
- Beads root epic: owns pre-1.0 observable-parity closure.
- Beads phase epics: group dependencies and expose readiness without reparenting existing issues.
- Beads gap tasks: define only implementation work absent from the current tracker.
- Beads completion-gate tasks: add no implementation scope and prevent phase
  closure while required independently owned non-epic work remains open.
- `flpdf-egzr`: retains the helper inventory while excluding direct `qpdf-ctest` porting.

---

### Task 1: Create the convergence and phase epics

**State:**
- Create: one P1 root epic with label `qpdf-parity`
- Create: seven child epics under the root
- Consume: approved design spec path as `--spec-id`
- Produce: stable issue IDs recorded for Tasks 2–5

- [ ] **Step 1: Recheck duplicate titles**

Search every proposed exact title:

```bash
for title in \
  "pre-v1.0 qpdf 11.9.0 observable parity" \
  "qpdf parity P0: authoritative measurement" \
  "qpdf parity P1: unlock upstream observation" \
  "qpdf parity P2: object model, parser, and repair convergence" \
  "qpdf parity P3: stream, filter, and crypto convergence" \
  "qpdf parity P4: QPDFJob, CLI, and transform convergence" \
  "qpdf parity P5: writer convergence" \
  "qpdf parity P6: applicable parity closure" \
  "qtest: applicable manifest and exclusion registry" \
  "qtest survey: reconcile 2790 parsed results with 2762 reported" \
  "qpdf-ctest: map C-API-only coverage to Rust oracle tests" \
  "test_driver: inventory remaining IDs by qpdf responsibility" \
  "QPDFObjectHandle consumers: audit eager and lazy resolution contracts" \
  "Pl_TIFFPredictor: qpdf 11.9.0 production cutover" \
  "Pl_DCT and decode-level: qpdf 11.9.0 production cutover" \
  "writer parity: encrypted-output semantic and byte gate" \
  "writer parity: incremental-output semantic and append-invariant gate" \
  "qpdf parity closure: applicable qtest and three-build stability" \
  "qpdf parity P2: existing work completion gate" \
  "qpdf parity P3: existing work completion gate" \
  "qpdf parity P4: existing work completion gate" \
  "qpdf parity P5: existing work completion gate"
do
  bd search "$title"
done
```

Expected: each of the 22 searches has no exact match before first creation. If
an exact issue exists anywhere in the tracker, reuse it and verify its scope
rather than creating another. Repeat the relevant exact-title search
immediately before each create so a concurrent worker cannot introduce a
duplicate between this inventory and creation.

- [ ] **Step 2: Create the root epic**

Create a P1 epic titled:

```text
pre-v1.0 qpdf 11.9.0 observable parity
```

Description:

```text
Convergence epic for observable parity with pinned qpdf 11.9.0 on Linux
x86_64. Covers CLI behavior, Rust API semantics, applicable upstream qtest,
and byte-identical supported writer routes under qpdf-zlib-compat. C/C++ ABI,
symbols, and Windows-only behavior are excluded. Completion is defined by the
approved design spec, not raw qtest pass count.
```

Acceptance:

```text
All seven phase epics are closed; every applicable qtest passes for three
independent builds; represented C-API/platform behavior has a passing Rust
oracle test; qpdf-produced qpdf-zlib-compat writer tuples are byte-identical;
Pure Rust tuples are semantically and structurally identical; incremental
tuples satisfy their appended-revision invariants; no parity-scoped Bead other
than this root remains open.
```

Use:

```text
--type=epic --priority=P1 --labels=qpdf-parity,pre-v1
--spec-id=docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md
```

Capture the returned ID as `ROOT_ID`.

- [ ] **Step 3: Create the seven phase epics**

Create these children with `--parent "$ROOT_ID"`, label `qpdf-parity`, and the
same `--spec-id`:

| Phase | Priority | Exact title | Exact acceptance |
|---|---:|---|---|
| P0 | P1 | qpdf parity P0: authoritative measurement | Every qtest-reported subtest has one non-overlapping composite state and its state-specific ownership or replacement fields; the applicable denominator equals applicable + blocked + passing + failing; classified counts equal qtest's total; the survey parser count equals qtest's total on two runs; one reproducible command records pins, denominator, passes, clusters, and regressions. |
| P1 | P1 | qpdf parity P1: unlock upstream observation | Applicable test-driver IDs and non-C helpers have pinned merged-output/exit-status differentials; qpdf-ctest behaviors map to Rust tests or explicit ABI-only exclusions; survey output separates infrastructure and behavior failures. |
| P2 | P1 | qpdf parity P2: object model, parser, and repair convergence | Object consumers have oracle-backed resolution contracts; known parser/xref/page-tree recovery gaps are closed; warning timing, text, attribution, order, and fatality match qpdf on the applicable malformed corpus. |
| P3 | P1 | qpdf parity P3: stream, filter, and crypto convergence | TIFF, DCT/decode-level, remaining Flate, AES, MD5/SHA/count/discard consumers are wired through qpdf-shaped production paths; replaced routes are deleted; specialized-filter/decode/encryption matrices pass. |
| P4 | P2 | qpdf parity P4: QPDFJob, CLI, and transform convergence | Applicable argument forms, options, logger routing, warning lifecycle, content concatenation, copy/Form/attachment/annotation transforms match pinned qpdf and no replaced orchestration route remains. |
| P5 | P1 | qpdf parity P5: writer convergence | qpdf-produced plain, QDF, ObjStm/xref-stream, linearized, encrypted, and copy-encryption tuples pass Pure Rust semantic/structural comparison and qpdf-zlib-compat byte comparison; incremental tuples pass final-document semantic/structural comparison and appended-revision invariant checks and are excluded from qpdf byte identity. |
| P6 | P1 | qpdf parity P6: applicable parity closure | Every applicable qtest passes for three independent builds; represented behavior points to passing Rust tests; exclusions are approved; divergence allowlists are empty; writer gates pass; no parity-scoped implementation Bead outside the root, this Phase 6 epic, and its closure task remains open. |

Use each acceptance cell verbatim as the corresponding epic's
`--acceptance`. Its description states that it owns that phase of the approved
design. Capture IDs as `P0_ID` through `P6_ID`.

- [ ] **Step 4: Establish phase ordering**

Add blocking dependencies:

```text
P1_ID depends on P0_ID
P2_ID depends on P0_ID
P3_ID depends on P0_ID
P4_ID depends on P0_ID
P5_ID depends on P0_ID
P6_ID depends on P1_ID, P2_ID, P3_ID, P4_ID, and P5_ID
```

Do not make P2, P3, P4, and P5 depend on one another.
Do not make `ROOT_ID` depend on a child phase: Beads propagates a blocked
parent's state to its children, so that edge would prevent P0 from becoming
ready. Root closure is governed by child progress and root acceptance.

- [ ] **Step 5: Read back the hierarchy**

Run:

```bash
bd show "$ROOT_ID"
bd list --parent "$ROOT_ID" --limit 20
```

Expected: exactly seven phase children; P0 and the root are open; P1–P5 are
blocked by P0; P6 is blocked by P1–P5.

---

### Task 2: Create the ten missing gap issues

**State:**
- Consume: `P0_ID` through `P6_ID`
- Produce: ten non-duplicative tasks with exact acceptance criteria

- [ ] **Step 1: Create Phase 0 measurement tasks**

Create under `P0_ID`:

1. P1 task `qtest: applicable manifest and exclusion registry`
   - Give every qtest-reported subtest exactly one non-overlapping composite
     state: applicable, excluded, represented, blocked, passing, or failing.
     Derive the applicable denominator as applicable + blocked + passing +
     failing, and derive the direct-pass count from passing.
   - Record rationale for every excluded entry and a concrete Rust-test
     reference for every represented entry. A true ABI-only exclusion requires
     rationale but no replacement test. An excluded direct qpdf-ctest entry
     that may exercise portable behavior may use `bead:flpdf-25kg.2.1`
     provisionally during Phase 0; the Phase 1 mapping task must replace every
     such reference with a Rust test, narrower follow-up Bead, or ABI-only
     scope rationale.
   - Own the reproducible full-survey command that records qpdf, flpdf, and
     qtest pins, applicable denominator, passes, failure clusters, and
     allowlist regressions.
   - Emit separate machine-readable blocked/infrastructure and
     failing/behavior counts in the survey manifest summary.
   - Acceptance: counts sum exactly to qtest's reported total; the applicable
     denominator and direct-pass count follow the formulas above; no entry
     lacks its state-specific rationale, owner, Bead, or replacement reference;
     the recorded command reproduces both stable surveys and the
     infrastructure/behavior split.
2. P1 bug `qtest survey: reconcile 2790 parsed results with 2762 reported`
   - Reproduce the 2026-07-29 mismatch without weakening comparison.
   - Acceptance: parser count equals qtest summary count on two independent
     runs and existing 39/39 allowlist status remains unchanged.

The manifest task depends on the result-accounting bug so its recorded command,
denominator, and failure categories are generated from the authoritative
2,762-result set.

- [ ] **Step 2: Create Phase 1 observation tasks**

Create under `P1_ID`:

1. P2 task `qpdf-ctest: map C-API-only coverage to Rust oracle tests`
   - Inventory all 53 invocations by underlying behavior.
   - Acceptance: every invocation points to an existing passing Rust oracle
     test, a new follow-up Bead, or a documented ABI-only exclusion, and every
     provisional `bead:flpdf-25kg.2.1` manifest reference is replaced.
2. P1 task `test_driver: inventory remaining IDs by qpdf responsibility`
   - Cover IDs 0, 2, 3, 28, 33–36, 39, 52–71, 76–77, 80, 81, and 85.
   - Acceptance: each ID has source range, qtest invocations, consumer
     contract, dependency order, and follow-up Bead IDs.

- [ ] **Step 3: Create the Phase 2 lazy/eager audit**

Create under `P2_ID`:

1. P1 task `QPDFObjectHandle consumers: audit eager and lazy resolution contracts`
   - Classify each consumer operation as raw identity inspection, one-value
     resolution, chain resolution, or graph traversal.
   - Acceptance: every discovered mismatch has an oracle probe and a bounded
     follow-up issue; no broad lazy-object rewrite is proposed without a real
     consumer.

- [ ] **Step 4: Create Phase 3 codec tasks**

Create under `P3_ID`:

1. P1 feature `Pl_TIFFPredictor: qpdf 11.9.0 production cutover`
   - Acceptance includes streaming chunk boundaries, constructor errors,
     finish behavior, DecodeParms, production decode wiring, old-route
     deletion, and a pinned live differential.
2. P1 feature `Pl_DCT and decode-level: qpdf 11.9.0 production cutover`
   - Acceptance includes specialized/generalized/all decode levels, DCT
     warning/error behavior, JSON/CLI consumers, old fallback deletion, and a
     pinned live differential.

- [ ] **Step 5: Create Phase 5 byte-gate tasks**

Create under `P5_ID`:

1. P2 task `writer parity: encrypted-output semantic and byte gate`
   - Pure Rust semantic comparison and `qpdf-zlib-compat` byte comparison.
   - Include deterministic crypto inputs and supported encryption revisions.
2. P2 task `writer parity: incremental-output semantic and append-invariant gate`
   - Compare appended objects, `/Prev`, xref form, generations, trailer/ID,
     warning status, and final-document semantics and structure under
     deterministic inputs.
   - Do not require qpdf byte identity: qpdf 11.9.0 opens output with `wb+` and
     writes a new header (`libqpdf/QPDFWriter.cc:83-98,2991-3001`), producing no
     corresponding incremental byte stream.

- [ ] **Step 6: Create the Phase 6 closure task**

Create under `P6_ID`:

1. P1 task `qpdf parity closure: applicable qtest and three-build stability`
   - Acceptance copies every Phase 6 criterion from the design spec and
     exempts the root, Phase 6 epic, and the closure task itself from the
     zero-open condition.
   - The task depends on the Phase 0 manifest task so the denominator cannot
     change silently.

- [ ] **Step 7: Read back all new gap issues**

Before readback, add task-to-task blocking dependencies:

```text
the Phase 0 manifest task depends on the Phase 0 result-accounting bug
the qpdf-ctest mapping, test-driver inventory, eager/lazy audit, TIFF, DCT,
encrypted gate, and incremental gate tasks each depend on both P0 tasks
the closure task depends on both P0 tasks and all seven preceding gap tasks
```

These non-epic mixed-type blocking edges are deliberate: Beads accepts them,
and they encode real implementation readiness. Do not replace them with
`relates-to`, which is non-blocking. Phase completion is governed by child
progress plus the epic acceptance criteria.

Run:

```bash
bd list --parent "$P0_ID" --limit 20
bd list --parent "$P1_ID" --limit 20
bd list --parent "$P2_ID" --limit 20
bd list --parent "$P3_ID" --limit 20
bd list --parent "$P5_ID" --limit 20
bd list --parent "$P6_ID" --limit 20
```

Expected: 2 + 2 + 1 + 2 + 2 + 1 = 10 new gap tasks, with no unexpected
duplicate title.

---

### Task 3: Create non-epic completion gates

**State:**
- Create: four coordination-only child tasks under `P2_ID` through `P5_ID`
- Consume: required independently owned non-epic parity issues
- Produce: type-compatible blocking paths that prevent premature phase closure

- [ ] **Step 1: Recheck the four gate titles**

Run `bd search` for each exact gate title from Task 1 Step 1 immediately before
creation. Reuse an exact match rather than creating a duplicate.

- [ ] **Step 2: Create the four child gates**

Create:

| Parent | Priority | Exact title | Exact acceptance |
|---|---:|---|---|
| `P2_ID` | P1 | qpdf parity P2: existing work completion gate | flpdf-ud7r, flpdf-xm72, flpdf-fmb9, and flpdf-4zt3 are closed; this task adds no implementation scope. |
| `P3_ID` | P1 | qpdf parity P3: existing work completion gate | flpdf-qynx.8, flpdf-qynx.9, and flpdf-qynx.10 are closed; this task adds no implementation scope. |
| `P4_ID` | P2 | qpdf parity P4: existing work completion gate | flpdf-qynx.4, flpdf-qynx.7, flpdf-w5ny, and flpdf-w1cs are closed; this task adds no implementation scope. |
| `P5_ID` | P1 | qpdf parity P5: existing work completion gate | flpdf-9hc.42, flpdf-9hc.29, flpdf-cecz, and flpdf-j4ph are closed; this task adds no implementation scope. |

Use the same labels and `--spec-id` as the phase epics. Each description states
that Beads permits only epic blockers on epics, so this child gate translates
required non-epic completion into parent-child phase progress. Capture the IDs
as `P2_GATE_ID` through `P5_GATE_ID`.

- [ ] **Step 3: Add gate blockers**

Add blocking dependencies:

```text
P2_GATE_ID depends on flpdf-ud7r, flpdf-xm72, flpdf-fmb9, and flpdf-4zt3
P3_GATE_ID depends on flpdf-qynx.8, flpdf-qynx.9, and flpdf-qynx.10
P4_GATE_ID depends on flpdf-qynx.4, flpdf-qynx.7, flpdf-w5ny, and flpdf-w1cs
P5_GATE_ID depends on flpdf-9hc.42, flpdf-9hc.29, flpdf-cecz, and flpdf-j4ph
```

These edges are valid mixed non-epic blockers. Do not attach the existing work
as direct blockers of the phase epic; Beads rejects non-epic blockers on epics.

- [ ] **Step 4: Read back the gates**

Run `bd show` for all four gate IDs.

Expected: each gate is a child of its phase, blocked by exactly the listed
existing issues, and contains no implementation acceptance beyond their
closure.

---

### Task 4: Link existing work and correct the helper inventory

**State:**
- Modify: Bead `flpdf-egzr`
- Consume: phase epic IDs and existing authoritative Beads
- Produce: dependency edges without reparenting existing issues

- [ ] **Step 1: Update `flpdf-egzr` for the C-API exclusion**

Preserve the full original helper table as historical evidence, but change the
implementation scope to:

```text
Direct qpdf-ctest porting is excluded because C API/ABI compatibility is out
of scope. Its 53 invocations are tracked by the qpdf-ctest behavior-mapping
task. Windows-only test_shell_glob is also excluded from the Linux x86_64
gate. The implementation backlog is the remaining 11 helpers and 34
invocations. Existing counts of 13/88 remain documented as the raw upstream
inventory.
```

Update its next steps to point to the Phase 1 mapping task and require
responsibility-based child issues.

- [ ] **Step 2: Link Phase 1 existing work**

Add blocking dependencies:

```text
P1_ID depends on flpdf-n9t0
P1_ID depends on flpdf-egzr
```

- [ ] **Step 3: Link Phase 2 existing work**

Add the type-compatible phase-level blocking dependency:

```text
flpdf-9hc.17
```

Add one non-blocking `relates-to` edge:

```text
flpdf-mfir
```

`flpdf-mfir` is accessor deduplication with no observable parity acceptance,
so it does not gate Phase 2 closure.

- [ ] **Step 4: Link Phase 3 existing work**

Add the type-compatible phase-level blocking dependency:

```text
flpdf-qynx.5
```

- [ ] **Step 5: Link Phase 4 existing work**

Add the type-compatible phase-level blocking dependency:

```text
flpdf-9hc.23
```

- [ ] **Step 6: Link Phase 5 existing work**

Add the type-compatible phase-level blocking dependency:

```text
flpdf-9hc.20
```

`flpdf-9hc.20` is retained as the historical byte-identical floor even though
all children are complete; closing it remains its owner's lifecycle decision.

- [ ] **Step 7: Add non-blocking roadmap relations**

Add `relates-to` edges:

```text
ROOT_ID relates to flpdf-9hc
ROOT_ID relates to flpdf-qynx
```

Do not make the root depend on `flpdf-9hc`; it contains non-parity work such as
fuzzing that is not part of observable closure.

---

### Task 5: Validate the tracker graph

**State:**
- Consume: complete new and updated Beads graph
- Produce: evidence that the graph is complete, non-duplicative, and healthy

- [ ] **Step 1: Validate exact issue content**

Run `bd show` for the root, all seven phases, all ten new gaps, all four
completion gates, and `flpdf-egzr`.

Expected:

- exact qpdf pin and Linux scope;
- C API/ABI exclusion appears consistently;
- `qpdf-zlib-compat` and Pure Rust gates are distinct;
- every implementation issue names consumer wiring and old-route deletion;
- no issue promises raw pass-count gains as its primary goal.

- [ ] **Step 2: Validate dependencies**

Run:

```bash
bd blocked
bd list --parent "$ROOT_ID" --limit 20
bd orphans
```

Expected:

- P0 is ready unless another explicit dependency exists;
- the Phase 0 manifest waits for result-accounting and owns the reproducible
  command plus machine-readable infrastructure/behavior split;
- P1–P5 wait on P0;
- P6 waits on P1–P5;
- root remains the open parent container and closes only after all seven child
  phases satisfy their acceptance criteria;
- non-P0 gap tasks wait on both P0 tasks, and the closure task waits on all
  preceding gap tasks;
- the C-API mapping runs after the initial manifest and replaces every
  provisional mapping-Bead reference before Phase 1 closes;
- required existing parity issues block their owning phase; only explicitly
  non-observable roadmap associations such as `flpdf-mfir` use `relates-to`;
- each Phase 2–5 completion gate is blocked by the exact required non-epic
  issues, and its parent phase cannot close while the gate remains open;
- the closure task, P6 epic, and root close in that order, and each zero-open
  check exempts the still-open items in that closure chain;
- existing issues retain their original parents;
- no broken dependency or orphan is introduced.

- [ ] **Step 3: Run Beads quality checks**

Run:

```bash
bd lint
bd doctor --check=conventions
bd preflight
```

Any warning caused by a new or modified issue is fixed before push. Pre-existing
warnings are recorded with their issue IDs and left unchanged.

- [ ] **Step 4: Persist tracker state**

Announce that the qpdf roadmap Beads graph is being pushed, then run:

```bash
bd dolt push
```

Expected: remote persistence succeeds. Benign auto-export `git add failed`
warnings do not override a successful Dolt push.

---

### Task 6: Commit and push the plan

**Files:**
- Modify: `docs/superpowers/plans/2026-07-29-qpdf-observable-parity-beads.md`
- Modify: `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`

- [ ] **Step 1: Review plan against the approved design**

Run:

```bash
rg -n 'T''BD|T''ODO|PLACE''HOLDER|X''XX' \
  docs/superpowers/plans/2026-07-29-qpdf-observable-parity-beads.md
git diff --check
```

Expected: placeholder scan has no matches and `git diff --check` has no
output.

- [ ] **Step 2: Commit the plan**

Run:

```bash
git add docs/superpowers/plans/2026-07-29-qpdf-observable-parity-beads.md
git commit -m "docs: plan qpdf parity roadmap tracking"
```

- [ ] **Step 3: Rebase and push Git**

Announce the exact branch and commits, then run:

```bash
git pull --rebase
git push
```

Expected: `main` and `origin/main` point to the same commit containing both the
approved design and this execution plan.

- [ ] **Step 4: Final readback**

Run:

```bash
git status --short --branch
bd show "$ROOT_ID"
bd dolt push
```

Expected: clean Git worktree, `main...origin/main` with no ahead/behind count,
root roadmap epic present, and final Dolt push successful.
