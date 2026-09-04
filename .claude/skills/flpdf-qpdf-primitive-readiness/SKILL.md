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
- Keep phase 1 read-only for the audited issue: do not create or update
  issues, add or remove dependencies, edit notes, add labels, claim work,
  change status, or create an implementation worktree for it. This does not
  cover `scripts/fetch-qpdf-source.sh` installing the pinned qpdf oracle
  source — a separate, read-only, shared reference repository outside the
  flpdf tree, not an implementation worktree for the audited issue.
- Stop for explicit approval of the exact change set before phase 2.
- Never claim, close, reprioritize, implement, or create an implementation
  worktree for the audited issue. This skill audits and records readiness
  only.
- Treat qpdf source and observed behavior as authoritative over issue text,
  acceptance criteria, dependency records, and existing flpdf patterns.

## Phase 1: Read-Only Audit

After reading the rule file:

1. Run `bd show <target>` (`bd prime` already ran as the First Action).
   Record scope, acceptance criteria, parent, dependencies, labels, and
   existing notes.
2. Resolve the pinned source with
   `scripts/fetch-qpdf-source.sh --print-path`. If it exits 1, read its
   stderr message before reacting: a genuinely missing or off-pin tree
   says so and is safe to install with `scripts/fetch-qpdf-source.sh`
   (no arguments), then retry `--print-path`. A tree with local edits or
   unverifiable git metadata exits 1 too, but the plain install form
   refuses that exact same tree for the same reason — do not retry it in
   a loop. That tree is a shared reference repository outside the flpdf
   tree, and discarding its local edits (via `--force` or any other
   discard command the script names) destroys another developer's
   uncommitted qpdf work; never run a discard command unattended. Stop
   and ask the user for explicit approval of that specific destructive
   command before running it, or return `unknown` if approval is not
   available in this context.
3. Read qpdf first. Identify the relevant classes, fields, public contracts,
   ownership, call order, default implementations, errors, and consumer
   boundary. Many audits are fully conclusive from source alone — a live
   probe is not required by default.
4. Only when source alone does not settle observable behavior, use a
   focused live qpdf probe — but first confirm `qpdf --version` reports
   exactly `qpdf version 11.9.0`. `fetch-qpdf-source.sh` only warns on a
   missing or mismatched binary and keeps running; a probe against an
   unverified or wrong-version binary is not usable evidence. If the
   needed evidence genuinely requires a probe and no verified 11.9.0
   binary is available, return `unknown` — do not force `unknown` merely
   because the binary check step ran when no probe was actually needed.
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
| `missing` | Every required primitive has been conclusively evaluated, and at least one is absent, owns the wrong responsibility, collapses distinct qpdf concepts, or forces a special case. | Propose prerequisite reuse/creation, dependencies, notes, and label. |
| `unknown` | Source or observed behavior is insufficient for a safe responsibility or dependency decision for any required primitive. | None. Report the evidence gap and stop. |

`unknown` takes precedence over `missing` whenever a target has both a
conclusively absent primitive and a separately unresolved one: do not
report `missing` and mutate Beads for the known-absent part while another
required primitive's responsibility or behavior is still unevaluated. Every
required primitive must be conclusively assessed (as `equivalent`,
`divergent`, or `missing` in the flpdf correspondence table) before `ready`
or `missing` is a legal verdict; any unresolved primitive makes the whole
audit `unknown`.

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

- <one line per candidate, or `none`>: <ID>, <open or closed>; <why it is
  or is not the same responsibility>. A closed match is not reusable as an
  unresolved prerequisite while flpdf inspection still shows the primitive
  missing — state explicitly whether the plan reopens it or creates a new
  issue.

### Proposed Beads changes

- Create/reuse: <one line per primitive that needs prerequisite work —
  missing, wrong-responsibility, or otherwise divergent — never one for a
  primitive already assessed `equivalent` in the flpdf correspondence
  table above; `none` when every required primitive is already
  equivalent (including a plain `ready` audit with no prerequisite).
  Never invent a dependency to fill this slot, and never collapse two
  independent missing primitives into one entry; each keeps its own
  line: complete issue draft, or existing OPEN issue ID, or
  create-new/reopen-<ID> when the only match is closed>
- Dependency: <one `bd dep add <audited> <prerequisite>` line per created
  or reused prerequisite above, or `none`>
- Remove/reverse: <exact approved edge changes or none>
- Issue-text repairs: <exact acceptance-criteria/parent-epic/dependent-side
  description corrections required by
  .claude/rules/qpdf-port-design-patterns.md rule 4 when qpdf evidence
  reverses or contradicts recorded text, or none>
- Notes: <exact complete block to append, or `none` for an `unknown`
  verdict>
- Label: <for `ready` or `missing`: bd update <audited> --add-label
  primitive-audited, applied only after phase 2's pre-label readback and
  the first `bd dolt push` succeed. For `unknown`: `none` — the
  classification table requires no Beads mutation at all for `unknown`,
  so this line must never contain the literal `bd update` command in
  that case>

### Evidence gaps

- <none, or the exact reason the verdict is unknown>
```

For a new prerequisite draft, include its title, type, priority, parent,
labels, description, acceptance criteria, qpdf citations, independently
testable failure paths, and explicit non-goals.

After emitting the report, **stop**. Ask for approval. A request to investigate
or assess readiness is not approval to mutate Beads. If the proposed change
set changes materially after approval, present the revision and ask again.
An `unknown` verdict has no approved change set to apply — never enter
phase 2 for it; report the evidence gap and stop there.

## Phase 2: Apply Only the Approved Plan

Enter this phase only when the user explicitly approves the exact phase-1
report. If context does not contain that report and approval, rerun phase 1.

1. Re-read the target, every proposed prerequisite, and every issue named
   in an approved issue-text repair (parent epic, dependent side) — a
   stale re-read of only the target can apply step 6's approved text over
   newer content in one of those issues. Re-run the relevant phase-1
   correspondence checks before resuming, especially after any gap
   between approval and phase 2: the verdict was derived from flpdf's
   actual code, tests, and `docs/qpdf-correspondence.md`, not only the
   Beads issue text, and checking only a recorded revision hash would
   miss uncommitted working-tree edits to those tracked files. If
   anything changed that the approved plan does not already account for,
   stop and present a revised plan. A prior phase-2 attempt's own partial
   mutations — a prerequisite it already created, an edge it already
   applied, notes it already appended, matching this same approved plan —
   are not that kind of change: resuming into them is the retry this
   skill requires (see Partial Failure and Retry), not new drift to
   revise the plan for.
2. For each approved prerequisite (there may be zero, one, or several — one
   per missing qpdf responsibility unit, never collapsed into a single
   issue): reuse it only if it is open. If the only match is closed and
   flpdf inspection still shows the primitive missing, follow the approved
   plan's explicit choice: reopen that issue or create a new one — never
   treat a closed match as an unresolved prerequisite by default.
   Otherwise create only that approved prerequisite issue, one qpdf
   responsibility unit each; keep consumer migration in the audited issue.
   Skip this step entirely only when the approved plan creates or reuses
   no prerequisite at all.
3. Locate each created or reopened issue by its exact title and read it
   back with `bd show`. Never treat warning-laden `bd create` stdout as a
   bare ID. Skip this step when step 2 was skipped.
4. Check the current dependency tree and `bd dep cycles`. If this initial
   check already reports a cycle, stop here and report it instead of
   applying any graph change — a pre-existing cycle is not this audit's
   approved change to fix, and applying more edges on top of it risks
   compounding an already-broken graph. Otherwise apply every approved
   graph change — removals, reversals, and additions alike, not only
   additions for a newly created prerequisite. A `ready` audit whose only
   correction is an approved dependency reversal or removal (no new
   prerequisite at all) still applies that change here; do not skip this
   step just because there was no prerequisite to create in step 2. The
   audited consumer depends on each prerequisite:
   `bd dep add <audited> <prerequisite>`.
5. Run `bd dep cycles` again. If a cycle now exists that step 4's initial
   check did not show, the edges step 4 just applied introduced it: remove
   or reverse exactly those just-applied edges (never edges that already
   existed before step 4) to restore the prior, non-cyclic state, then
   stop and report the conflict — do not invent a different graph repair,
   and do not leave the newly cyclic graph in place for the mandatory
   session-close push to persist.
6. Inspect existing notes. Skip an identical audit. If evidence supersedes an
   older audit, name that entry explicitly. Append the approved block with
   `bd update <audited> --append-notes ...`; never overwrite unrelated notes.
   Apply any approved issue-text repairs now: correct acceptance criteria
   that qpdf evidence contradicts, and update the parent epic's and the
   dependent side's description, per
   `.claude/rules/qpdf-port-design-patterns.md` rule 4.
7. Before this step's push, read back every issue actually named in the
   approved plan (`<audited>`, each prerequisite, and each issue-text
   repair target) and confirm each one matches the plan exactly. For an
   issue whose ID the plan already states (`<audited>`, and any reused or
   reopened prerequisite) this means checking that ID against the plan
   character-for-character before running each `bd dep add`/`bd
   update`/`bd create` command in steps 2–6, since a mistyped-but-
   otherwise-valid ID silently mutates an unrelated issue that this
   readback would not otherwise think to inspect. A prerequisite the plan
   only gave as a complete issue draft has no ID to check against until
   step 3 establishes one by exact-title lookup — for that issue, confirm
   every later command in steps 4–6 targets the exact ID step 3 read
   back, not a guessed or re-derived one. If anything is missing,
   unexpected, or wrong, correct it and re-verify before proceeding — do
   not push unverified content. If a command already reached an
   unintended issue, revert that specific mutation before continuing.
8. Only after that local verification passes, run `bd dolt push` to
   persist the notes and dependency changes. If it fails, stop here: do
   not add `primitive-audited` (a label added before the notes it
   describes are even persisted would be worse than premature). Follow
   Partial Failure and Retry below.
9. Add `primitive-audited` with
   `bd update <audited> --add-label primitive-audited`.
10. Read back the label locally to confirm it was actually applied, before
    pushing it. If it is missing or wrong, correct it before proceeding —
    do not push an unverified label state.
11. Run `bd dolt push` again to persist the verified label. If this second
    push fails, the notes and dependencies are already persisted but the
    label is only local — report exactly this state; the only remaining
    retry step is re-running `bd dolt push` (idempotent) until it
    succeeds. Do not report the audit as complete until this push
    succeeds.

This skill's own phase-2 steps make no git-tracked changes, so they include
no `git push` of their own. That does not exempt the session from AGENTS.md's
session-close policy, which requires `git push` unconditionally at session
close regardless of whether this skill's own scope touched git-tracked
files — do not skip it because this audit was Beads-only. Do not close the
audited issue. A `missing` audit is complete even though its implementation
remains blocked by the recorded prerequisite.

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
2. Read back the target, any newly created or reopened issue, dependencies,
   notes, labels, and any issue whose text was repaired (parent epic,
   dependent side).
3. Correct or revert any mutation whose recorded content does not exactly
   match the approved plan — do not leave a known-wrong local state for a
   later retry to notice. The mandatory session-close `bd dolt push`
   (AGENTS.md's session-close policy) publishes whatever is locally
   recorded regardless of whether this audit ever retries, so this step
   runs even if no retry follows.
4. Report exactly what succeeded and failed.
5. On retry, reuse matching issues and existing edges, skip identical notes,
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
| “Unblock it quickly; create the issue now.” | Finish phase 1, show the exact plan, and wait for approval. |
| “The existing dependency probably has the right direction.” | Derive direction from qpdf responsibility and verify the graph. |
| “Search flpdf first to find the nearest shape.” | Read qpdf structure and call order first, then map flpdf. |
| “The audit is done, so close the target.” | Leave status and claim untouched; append notes and label only. |
| “A local special case is cheaper.” | Stop and isolate the missing prerequisite responsibility. |
| “Evidence is probably sufficient.” | Return `unknown`, make no Beads change, and state the missing evidence. |
