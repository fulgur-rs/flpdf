---
name: flpdf-qpdf-primitive-readiness
description: Use when inspecting a flpdf Beads issue before qpdf-port implementation, especially when deciding whether required primitives and dependencies are ready or a lower-level prerequisite issue is missing.
---

# flpdf qpdf Primitive Readiness

Claude Code port of the Codex skill at
`.agents/skills/flpdf-qpdf-primitive-readiness/SKILL.md`. Content is
identical; only the skill-directory convention differs between agents. Keep
both in sync — this file has no `agents/openai.yaml` counterpart because
Claude Code does not use that metadata mechanism.

## Overview

Audit an issue from qpdf's responsibility boundaries before implementation.
Keep discovery read-only, expose the complete proposed Beads update, and apply
it only after explicit user approval.

## First Action

**Run `bd prime` first** (the repository's mandatory task-tracking start,
required for every issue-tracking workflow), **then, before any other tool
call or file read, read `.claude/rules/qpdf-port-design-patterns.md`
completely.**

This includes resumed audits and seemingly obvious issues. If phase 2 resumes
without evidence that the rule was read in the current audit context, read it
again before inspecting or changing Beads.

## Non-Negotiable Boundaries

- Start from pinned qpdf 11.9.0. The existing flpdf shape is a change target,
  not a design constraint.
- Keep phase 1 read-only. Do not create or update issues, add or remove
  dependencies, edit notes, add labels, claim work, change status, or create a
  worktree.
- Stop for explicit approval of the exact change set before phase 2.
- Never claim, close, reprioritize, implement, or create a worktree for the
  audited issue. This skill audits and records readiness only.
- Treat qpdf source and observed behavior as authoritative over issue text,
  acceptance criteria, dependency records, and existing flpdf patterns.

## Phase 1: Read-Only Audit

After reading the rule file:

1. Run `bd show <target>` (`bd prime` already ran as the First Action).
   Record scope, acceptance criteria, parent, dependencies, labels, and
   existing notes.
2. Resolve the pinned source with
   `scripts/fetch-qpdf-source.sh --print-path`. If it exits 1 (the tree is
   missing or off the pin — it never clones), run
   `scripts/fetch-qpdf-source.sh` with no arguments to install the pinned
   worktree, then retry `--print-path`.
3. Confirm `qpdf --version` reports exactly `qpdf version 11.9.0` before
   treating any live probe as evidence. `fetch-qpdf-source.sh` only warns
   on a missing or mismatched binary and keeps running; a probe against an
   unverified or wrong-version binary is not usable evidence — return
   `unknown` instead if no verified 11.9.0 binary is available.
4. Read qpdf first. Identify the relevant classes, fields, public contracts,
   ownership, call order, default implementations, errors, and consumer
   boundary. Use a focused live qpdf probe (only against the verified 11.9.0
   binary from the previous step) when source alone does not settle
   observable behavior.
5. Write the required qpdf primitives as separate one-to-one responsibility
   units.
6. Only now inspect flpdf code and tests. Map every qpdf primitive to its flpdf
   counterpart or mark it absent/non-equivalent.
7. Read the complete matching row and annotations in
   `docs/qpdf-correspondence.md`. A classification marker alone is not
   evidence of an approved deviation.
8. Search Beads using qpdf symbols, flpdf symbols, responsibility names, and
   module names. Include closed issues with `bd search ... --status all`.
   Inspect every plausible match with `bd show`. Note separately which
   matches are open and which are closed.
9. Re-derive dependency direction from responsibility ownership. Do not trust
   the current graph merely because it already exists.
10. Classify the target and prepare the approval report below.

## Classification

| Result | Required evidence | Beads mutation |
|---|---|---|
| `ready` | Every required primitive exists with qpdf-equivalent responsibility, and the target is implementable and testable without a special case. A recorded dependency direction that conflicts with qpdf responsibility does not by itself block `ready`: if every primitive is otherwise qpdf-equivalent and the approval report proposes the exact corrected edge (and any required issue-text repairs, see below), the verdict is `ready` with an included dependency/text correction. | Propose notes, the dependency correction if any, any required issue-text repairs, plus `primitive-audited`. |
| `missing` | A primitive is absent, owns the wrong responsibility, collapses distinct qpdf concepts, or forces a special case. | Propose prerequisite reuse/creation, dependencies, notes, and label. |
| `unknown` | Source or observed behavior is insufficient for a safe responsibility or dependency decision. | None. Report the evidence gap and stop. |

These are mandatory stop signals; they cannot produce `ready`:

- sentinel values such as an empty buffer or zero standing for absence;
- a new panic or qpdf-incompatible error branch that fills a type hole;
- a qpdf-incompatible intermediate representation or concept split;
- material behavior that cannot be tested within the proposed boundary;
- reliance on an existing flpdf precedent without verifying its qpdf
  counterpart;
- a recorded dependency direction that conflicts with qpdf responsibility
  and leaves any primitive itself absent, wrong-responsibility, or
  special-cased (a dependency-only correction with every primitive
  otherwise qpdf-equivalent is `ready`, per the row above, not a stop
  signal).

When fixing a proposed prerequisite would still leave a special case in the
target, the prerequisite boundary is wrong. Return to qpdf source analysis.

## Approval Report

Return this structure with concrete content:

```markdown
## Primitive readiness audit

- Target: <issue ID and title>
- Verdict: ready | missing | unknown
- Target responsibility: <qpdf-owned responsibility>

### Required qpdf primitives

| Primitive | qpdf responsibility | Source/probe evidence |
|---|---|---|
| ... | ... | libqpdf/X.cc:NNN-NNN; command and observed result |

### flpdf correspondence

| Primitive | flpdf counterpart | Assessment |
|---|---|---|
| ... | file::symbol or absent | equivalent, divergent, or missing |

### Existing Beads candidates

- <ID or none>: <open or closed>; <why it is or is not the same
  responsibility>. A closed match is not reusable as an unresolved
  prerequisite while flpdf inspection still shows the primitive missing —
  state explicitly whether the plan reopens it or creates a new issue.

### Proposed Beads changes

- Create/reuse: <complete issue draft, or existing OPEN issue ID; if the
  only match is closed and the primitive remains missing, state
  create-new or reopen-<ID> explicitly>
- Dependency: bd dep add <audited> <prerequisite>
- Remove/reverse: <exact approved edge changes or none>
- Issue-text repairs: <exact acceptance-criteria/parent-epic/dependent-side
  description corrections required by
  .claude/rules/qpdf-port-design-patterns.md rule 4 when qpdf evidence
  reverses or contradicts recorded text, or none>
- Notes: <exact complete block to append>
- Label: bd update <audited> --add-label primitive-audited (only after
  `bd dolt push` in phase 2 succeeds)

### Evidence gaps

- <none, or the exact reason the verdict is unknown>
```

For a new prerequisite draft, include its title, type, priority, parent,
labels, description, acceptance criteria, qpdf citations, independently
testable failure paths, and explicit non-goals.

After emitting the report, **stop**. Ask for approval. A request to investigate
or assess readiness is not approval to mutate Beads. If the proposed change
set changes materially after approval, present the revision and ask again.

## Phase 2: Apply Only the Approved Plan

Enter this phase only when the user explicitly approves the exact phase-1
report. If context does not contain that report and approval, rerun phase 1.

1. Re-read the target and every proposed prerequisite. If relevant state
   changed since the phase-1 evidence was gathered — including flpdf code,
   tests, or `docs/qpdf-correspondence.md`, not only the Beads issue text —
   stop and present a revised plan. Re-checking only the Beads records is
   not sufficient: the verdict was derived from flpdf's actual code and
   docs, so re-run the relevant phase-1 correspondence checks (or confirm
   the audited flpdf revision is unchanged) before resuming, especially
   after any gap between approval and phase 2.
2. Reuse a matching issue only if it is open. If the only match is closed
   and flpdf inspection still shows the primitive missing, follow the
   approved plan's explicit choice: reopen that issue or create a new one
   — never treat a closed match as an unresolved prerequisite by default.
   Otherwise create only the approved prerequisite issue. Make it one qpdf
   responsibility unit; keep consumer migration in the audited issue.
3. Locate the created or reopened issue by its exact title and read it back
   with `bd show`. Never treat warning-laden `bd create` stdout as a bare
   ID.
4. Check the current dependency tree and `bd dep cycles`. Apply only approved
   removals, reversals, and additions. The audited consumer depends on the
   prerequisite:
   `bd dep add <audited> <prerequisite>`.
5. Run `bd dep cycles` again. Stop on a cycle; do not invent a graph repair.
6. Inspect existing notes. Skip an identical audit. If evidence supersedes an
   older audit, name that entry explicitly. Append the approved block with
   `bd update <audited> --append-notes ...`; never overwrite unrelated notes.
   Apply any approved issue-text repairs now: correct acceptance criteria
   that qpdf evidence contradicts, and update the parent epic's and the
   dependent side's description, per
   `.claude/rules/qpdf-port-design-patterns.md` rule 4.
7. Run `bd dolt push` and report its result. If it fails, stop here: do not
   add `primitive-audited` (a label added before successful persistence
   would mark an unpersisted audit complete). Follow Partial Failure and
   Retry below.
8. Only after `bd dolt push` succeeds, add `primitive-audited` with
   `bd update <audited> --add-label primitive-audited`.
9. Read back the audited issue, prerequisite issues, dependency tree, notes,
   and labels.

This skill's own Beads mutations do not by themselves require `git push` —
they change no git-tracked implementation files. That does not override the
repository's mandatory session-close policy: if the current session has any
other git-tracked changes, `git push` for those remains required at session
close regardless of this skill's own scope. Do not close the audited issue.
A `missing` audit is complete even though its implementation remains blocked
by the recorded prerequisite.

## Notes Contract

Append this block for a conclusive `ready` or `missing` audit:

```markdown
## Primitive readiness audit — YYYY-MM-DD

- Result: ready | missing
- Target responsibility: ...
- Required qpdf primitives:
  - ... (`libqpdf/X.cc:NNN`)
- flpdf correspondence:
  - ...
- Existing deviations:
  - ...
- Dependency decision:
  - ...
- Evidence:
  - source/probe/test commands and results
```

`primitive-audited` means only that a conclusive audit was approved,
recorded, read back, and persisted. The notes and dependency graph carry the
actual readiness result.

## Partial Failure and Retry

On any phase-2 failure:

1. Stop further mutations.
2. Read back the target, any newly created issue, dependencies, notes, and
   labels.
3. Report exactly what succeeded and failed.
4. On retry, reuse matching issues and existing edges, skip identical notes,
   and add an existing label idempotently.

Do not label an inconclusive or partially recorded audit complete.

## Red Flags

Stop if you are about to:

- inspect flpdf code before deriving the qpdf model;
- say the issue is detailed enough to skip source verification;
- trust an existing dependency direction without responsibility analysis;
- create a prerequisite before showing its full draft and receiving approval;
- work around a missing primitive inside the consumer;
- claim or close the audited issue because the audit itself is complete;
- mark `unknown` or a partial update as `primitive-audited`.

## Common Mistakes

| Temptation | Required response |
|---|---|
| "Unblock it quickly; create the issue now." | Finish phase 1, show the exact plan, and wait for approval. |
| "The existing dependency probably has the right direction." | Derive direction from qpdf responsibility and verify the graph. |
| "Search flpdf first to find the nearest shape." | Read qpdf structure and call order first, then map flpdf. |
| "The audit is done, so close the target." | Leave status and claim untouched; append notes and label only. |
| "A local special case is cheaper." | Stop and isolate the missing prerequisite responsibility. |
| "Evidence is probably sufficient." | Return `unknown`, make no Beads change, and state the missing evidence. |
