# qpdf Observable Parity Beads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Materialize the approved qpdf 11.9.0 observable-parity roadmap as a dependency-ordered Beads graph without duplicating existing work.

**Architecture:** Create one convergence root epic, seven phase epics, and ten new gap issues. Existing qtest, repair, Pipeline, CLI, and writer issues remain authoritative and are linked as dependencies; only `flpdf-egzr` is rewritten to reflect the approved C-API exclusion.

**Tech Stack:** Beads/Dolt (`bd`), Git, Markdown design and plan documents.

## Global Constraints

- qpdf 11.9.0 is the source and behavioral oracle.
- The official parity gate targets Linux x86_64.
- C and C++ ABI or symbol compatibility is out of scope.
- `qpdf-ctest` is not ported; its underlying PDF behavior is mapped to Rust oracle tests.
- `qpdf-zlib-compat` requires byte identity; the Pure Rust default requires semantic and structural identity.
- Existing Beads remain authoritative and must not be duplicated or reparented.
- Beads blocking edges connect issues of the same type: epic-to-epic phase
  ordering and task-to-task implementation readiness are maintained as
  separate layers; cross-type roadmap links use `relates-to`.
- New implementation slices use consumer RED, the smallest lower-level primitive, production wiring, old-route deletion, differential verification, and 100% changed executable-line coverage.
- Beads state and Git commits must both be pushed before handoff.

## State Responsibilities

- `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`: approved scope and roadmap authority.
- `docs/superpowers/plans/2026-07-29-qpdf-observable-parity-beads.md`: reproducible Beads mutation and verification procedure.
- Beads root epic: owns pre-1.0 observable-parity closure.
- Beads phase epics: group dependencies and expose readiness without reparenting existing issues.
- Beads gap tasks: define only work absent from the current tracker.
- `flpdf-egzr`: retains the helper inventory while excluding direct `qpdf-ctest` porting.

---

### Task 1: Create the convergence and phase epics

**State:**
- Create: one P1 root epic with label `qpdf-parity`
- Create: seven child epics under the root
- Consume: approved design spec path as `--spec-id`
- Produce: stable issue IDs recorded for Tasks 2–4

- [ ] **Step 1: Recheck duplicate titles**

Run:

```bash
bd search "pre-v1.0 qpdf 11.9.0 observable parity"
bd search "Authoritative parity measurement"
bd search "Applicable parity closure"
```

Expected: no existing issue with the exact new title. If an exact issue exists,
reuse it and verify its scope rather than creating another.

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
oracle test; qpdf-zlib-compat writer tuples are byte-identical; Pure Rust
tuples are semantically and structurally identical; no parity-scoped Bead
remains open.
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
| P0 | P1 | qpdf parity P0: authoritative measurement | Every qtest-reported subtest has one machine-readable state and owner; classified counts equal qtest's total; the survey parser count equals qtest's total on two runs; one reproducible command records pins, denominator, passes, clusters, and regressions. |
| P1 | P1 | qpdf parity P1: unlock upstream observation | Applicable test-driver IDs and non-C helpers have pinned merged-output/exit-status differentials; qpdf-ctest behaviors map to Rust tests or explicit ABI-only exclusions; survey output separates infrastructure and behavior failures. |
| P2 | P1 | qpdf parity P2: object model, parser, and repair convergence | Object consumers have oracle-backed resolution contracts; known parser/xref/page-tree recovery gaps are closed; warning timing, text, attribution, order, and fatality match qpdf on the applicable malformed corpus. |
| P3 | P1 | qpdf parity P3: stream, filter, and crypto convergence | TIFF, DCT/decode-level, remaining Flate, AES, MD5/SHA/count/discard consumers are wired through qpdf-shaped production paths; replaced routes are deleted; specialized-filter/decode/encryption matrices pass. |
| P4 | P2 | qpdf parity P4: QPDFJob, CLI, and transform convergence | Applicable argument forms, options, logger routing, warning lifecycle, content concatenation, copy/Form/attachment/annotation transforms match pinned qpdf and no replaced orchestration route remains. |
| P5 | P1 | qpdf parity P5: writer convergence | Plain, QDF, ObjStm/xref-stream, linearized, encrypted, copy-encryption, and incremental tuples pass Pure Rust semantic/structural comparison and qpdf-zlib-compat byte comparison where supported. |
| P6 | P1 | qpdf parity P6: applicable parity closure | Every applicable qtest passes for three independent builds; represented behavior points to passing Rust tests; exclusions are approved; divergence allowlists are empty; writer gates pass; no parity-scoped Bead remains open. |

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
   - Classify every qtest-reported subtest as applicable, excluded,
     represented, blocked, passing, or failing.
   - Record rationale and replacement Rust test for excluded/represented
     entries.
   - Acceptance: counts sum exactly to qtest's reported total; no entry lacks
     a reason, owner, or Bead link.
2. P1 bug `qtest survey: reconcile 2790 parsed results with 2762 reported`
   - Reproduce the 2026-07-29 mismatch without weakening comparison.
   - Acceptance: parser count equals qtest summary count on two independent
     runs and existing 39/39 allowlist status remains unchanged.

- [ ] **Step 2: Create Phase 1 observation tasks**

Create under `P1_ID`:

1. P2 task `qpdf-ctest: map C-API-only coverage to Rust oracle tests`
   - Inventory all 53 invocations by underlying behavior.
   - Acceptance: every invocation points to an existing passing Rust oracle
     test, a new follow-up Bead, or a documented ABI-only exclusion.
2. P1 task `test_driver: inventory remaining IDs by qpdf responsibility`
   - Cover IDs 0, 3, 28, 33, 34, 39, 52–71, 76–77, 80, 81, and 85.
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
2. P2 task `writer parity: incremental-output semantic and byte gate`
   - Compare appended objects, `/Prev`, xref form, generations, trailer/ID,
     warning status, and final bytes under deterministic inputs.

- [ ] **Step 6: Create the Phase 6 closure task**

Create under `P6_ID`:

1. P1 task `qpdf parity closure: applicable qtest and three-build stability`
   - Acceptance copies every Phase 6 criterion from the design spec.
   - The task depends on the Phase 0 manifest task so the denominator cannot
     change silently.

- [ ] **Step 7: Read back all new gap issues**

Before readback, add task-to-task blocking dependencies:

```text
the qpdf-ctest mapping, test-driver inventory, eager/lazy audit, TIFF, DCT,
encrypted gate, and incremental gate tasks each depend on both P0 tasks
the closure task depends on both P0 tasks and all seven preceding gap tasks
```

Do not add phase-epic-to-child-task blocking edges: Beads permits blocking
edges only between issues of the same type. Phase completion is governed by
child progress plus the epic acceptance criteria.

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

### Task 3: Link existing work and correct the helper inventory

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

Add a blocking dependency from `P2_ID` to the existing epic:

```text
flpdf-9hc.17
```

Add `relates-to` edges from `P2_ID` to the existing tasks:

```text
flpdf-mfir
flpdf-ud7r
flpdf-xm72
flpdf-fmb9
flpdf-4zt3
```

- [ ] **Step 4: Link Phase 3 existing work**

Add a blocking dependency from `P3_ID` to the existing epic:

```text
flpdf-qynx.5
```

Add `relates-to` edges from `P3_ID` to the existing tasks:

```text
flpdf-qynx.8
flpdf-qynx.9
flpdf-qynx.10
```

- [ ] **Step 5: Link Phase 4 existing work**

Add a blocking dependency from `P4_ID` to the existing epic:

```text
flpdf-9hc.23
```

Add `relates-to` edges from `P4_ID` to the existing tasks:

```text
flpdf-qynx.4
flpdf-qynx.7
flpdf-w5ny
flpdf-w1cs
```

- [ ] **Step 6: Link Phase 5 existing work**

Add a blocking dependency from `P5_ID` to the existing epic:

```text
flpdf-9hc.20
```

Add `relates-to` edges from `P5_ID` to the existing tasks:

```text
flpdf-9hc.42
flpdf-9hc.29
flpdf-cecz
flpdf-j4ph
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

### Task 4: Validate the tracker graph

**State:**
- Consume: complete new and updated Beads graph
- Produce: evidence that the graph is complete, non-duplicative, and healthy

- [ ] **Step 1: Validate exact issue content**

Run `bd show` for the root, all seven phases, all ten new gaps, and
`flpdf-egzr`.

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
- P1–P5 wait on P0;
- P6 waits on P1–P5;
- root remains the open parent container and closes only after all seven child
  phases satisfy their acceptance criteria;
- non-P0 gap tasks wait on both P0 tasks, and the closure task waits on all
  preceding gap tasks;
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

### Task 5: Commit and push the plan

**Files:**
- Create: `docs/superpowers/plans/2026-07-29-qpdf-observable-parity-beads.md`
- Preserve: `docs/superpowers/specs/2026-07-29-qpdf-observable-parity-roadmap-design.md`

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
