# flpdf qpdf Primitive Readiness Skill Design

**Beads issue:** `flpdf-n3sx`

## Goal

Create a project-scoped Codex skill, `flpdf-qpdf-primitive-readiness`, that
audits whether a qpdf-port issue has all prerequisite qpdf-equivalent
primitives before implementation begins. The skill records conclusive results
in Beads, but performs no mutation until the user approves an exact change
plan.

This first slice does not dispatch implementation subagents or create
worktrees. A later orchestration skill may require this skill as its readiness
gate.

## Location and files

The skill lives at:

```text
.agents/skills/flpdf-qpdf-primitive-readiness/
├── SKILL.md
└── agents/openai.yaml
```

No script or private state file is needed. The audit depends on judgment about
qpdf responsibility boundaries, while Beads already provides the persistent
record.

## Trigger and responsibility

The skill applies when a user asks whether the primitives or prerequisites for
a flpdf qpdf-port issue are ready, asks for a pre-implementation dependency
audit, or asks to identify and record missing qpdf-equivalent primitives.

The skill owns:

- qpdf-first primitive discovery;
- one-to-one qpdf-to-flpdf correspondence checks;
- existing Beads prerequisite searches;
- dependency-direction validation;
- a `ready`, `missing`, or `unknown` conclusion;
- an approval-gated Beads update for conclusive audits.

It does not claim or implement the audited issue, create worktrees, or dispatch
implementation subagents.

## Workflow

### Phase 1: read-only audit

1. Read `.claude/rules/qpdf-port-design-patterns.md` completely before opening
   qpdf or flpdf implementation files.
2. Run `bd prime` and inspect the target with `bd show <id>`. Record its scope,
   acceptance criteria, parent, current dependencies, and notes.
3. Resolve the pinned qpdf 11.9.0 source with
   `scripts/fetch-qpdf-source.sh --print-path`.
4. Read the qpdf data structures, fields, public contracts, call order, and
   responsibility boundaries needed by the target issue. Use a live qpdf probe
   when observable behavior is material or source evidence alone is ambiguous.
5. Only after the qpdf model is understood, inspect the corresponding flpdf
   implementation and the full matching row and annotations in
   `docs/qpdf-correspondence.md`.
6. Write a one-to-one mapping for every required qpdf primitive. Do not use an
   unverified flpdf precedent as design evidence.
7. Search open and closed Beads issues by relevant class, method, responsibility,
   and module names. Inspect plausible matches with `bd show`.
8. Validate dependency direction from the qpdf responsibility boundary rather
   than trusting the recorded graph.
9. Classify the target and prepare an exact proposed Beads change set.
10. Present the complete audit and proposed changes, then stop for explicit
    user approval.

Phase 1 must not create or update issues, add or remove dependencies, edit
notes, add labels, claim work, or create a worktree.

### Phase 2: approved Beads update

After explicit approval of the displayed change set:

1. Create each approved prerequisite issue that has no valid existing match,
   or select the approved existing issue.
2. Read created issues back with `bd show`; never infer an ID from warning-laden
   `bd create` output.
3. Add or repair dependencies so the audited issue depends on its prerequisite:
   `bd dep add <audited> <prerequisite>`.
4. Run `bd dep cycles` and stop on any cycle rather than attempting an ad hoc
   graph repair.
5. Append the approved audit block to the audited issue's existing notes.
6. Add the `primitive-audited` label to the audited issue only after all other
   local audit updates succeed.
7. Read back the audited issue, prerequisite issues, dependency tree, notes,
   and labels.
8. Persist the Beads state with `bd dolt push`.

If a command fails, read back actual state and report the partial update. A
retry must detect existing issues, dependencies, notes, and labels before
writing, so it does not create duplicates.

## Classification contract

| Result | Meaning |
|---|---|
| `ready` | Every required primitive exists with qpdf-equivalent responsibility, and the target can be implemented and tested without a special case. |
| `missing` | A primitive is absent, has the wrong responsibility, collapses distinct qpdf concepts, or forces a special case in the target. |
| `unknown` | Available qpdf source or observed behavior is insufficient for a safe responsibility or dependency decision. |

The following are mandatory stop signals and prevent a `ready` conclusion:

- a sentinel value is needed to represent a missing state;
- a qpdf-incompatible panic or new error branch is needed to fill a type hole;
- a qpdf-incompatible intermediate representation or split is needed;
- material behavior cannot be tested within the proposed responsibility
  boundary;
- the only justification is an existing flpdf pattern whose qpdf counterpart
  has not been verified;
- the recorded Beads dependency direction conflicts with the qpdf
  responsibility boundary.

An `unknown` result makes no Beads changes and receives no
`primitive-audited` label.

## Approval report

Before Phase 2, present:

- target issue;
- `ready`, `missing`, or `unknown` verdict;
- required qpdf primitives and exact source citations;
- flpdf correspondence and known deviations;
- source and probe evidence;
- reused prerequisite issues;
- complete drafts of new prerequisite issues;
- dependency additions, removals, or reversals;
- the exact notes block to append;
- the issue that will receive `primitive-audited`.

Approval applies only to this displayed change set. If further investigation
changes it materially, present the revised set and obtain approval again.

## Notes format

Conclusive audits append, rather than replace, the existing notes:

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

Before appending, inspect existing notes for an audit covering the same qpdf
responsibility and evidence. If present, propose a replacement or explicit
superseding entry rather than silently duplicating it.

`primitive-audited` means only that a conclusive audit was approved, recorded,
and persisted. It does not mean the audited issue is ready to implement; the
notes and dependencies carry that result.

## New prerequisite issue contract

A new prerequisite issue must be one qpdf responsibility unit and include:

- why the primitive is required;
- the qpdf class, method, fields, and call boundary being ported;
- exact pinned-source citations and relevant observed behavior;
- the required qpdf-to-flpdf mapping;
- acceptance criteria that make the primitive independently testable;
- explicit exclusions that keep consumer migration in the original issue;
- parent, priority, type, and labels justified by the existing issue graph.

If fixing the primitive would still leave the target dependent on special
cases, the proposed issue boundary is wrong and the audit returns to Phase 1.

## Validation strategy

Treat the skill as discipline-enforcing process documentation and validate it
with RED-GREEN-REFACTOR.

RED uses fresh subagents without the skill on read-only pressure scenarios:

1. a detailed issue tempts the agent to inspect flpdf before qpdf;
2. an existing dependency tempts the agent to trust its direction;
3. a missing primitive tempts the agent to create an issue before approval;
4. a local special case appears cheaper than fixing the prerequisite;
5. incomplete qpdf evidence tempts the agent to label the audit complete.

Record the actual baseline failures and rationalizations. GREEN reruns the same
scenarios with the skill and requires the agent to:

- read the design-pattern rule first;
- start from qpdf responsibility;
- stop on special-case signals;
- keep Phase 1 read-only;
- emit the required approval report;
- withhold `primitive-audited` for `unknown`.

After closing loopholes, validate the folder with the official skill validator
and confirm `agents/openai.yaml` matches `SKILL.md`. All pressure scenarios stay
read-only and must not mutate live Beads state.

## Acceptance

- Codex discovers the project-scoped skill for primitive-readiness and
  pre-implementation qpdf dependency-audit requests.
- The first task action is always reading
  `.claude/rules/qpdf-port-design-patterns.md`.
- No Beads mutation occurs before explicit approval.
- `ready`, `missing`, and `unknown` follow the documented evidence contract.
- Approved `missing` audits reuse or create the correct prerequisite issue and
  install an acyclic dependency in the correct direction.
- Conclusive approved audits append evidence to the target notes, add
  `primitive-audited`, read back state, and run `bd dolt push`.
- RED baseline and GREEN skill-enabled pressure scenarios demonstrate that the
  skill changes agent behavior.
- The official skill validator passes.
