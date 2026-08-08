# flpdf qpdf Primitive Readiness Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a project-scoped Codex skill that audits qpdf primitive readiness and applies approved Beads notes, dependency, issue, and label updates.

**Architecture:** A single `SKILL.md` defines a strict read-only audit phase and a separately approved Beads mutation phase. `agents/openai.yaml` provides Codex UI metadata; no scripts or persistent skill-local state are added.

**Tech Stack:** Codex Agent Skills, Markdown, YAML, Beads CLI, pinned qpdf 11.9.0 source.

## Global Constraints

- Read `.claude/rules/qpdf-port-design-patterns.md` completely before any qpdf-port audit action.
- Create the skill under `.agents/skills/flpdf-qpdf-primitive-readiness/`.
- Keep audit phase 1 read-only and require explicit approval of an exact change set before phase 2.
- Record conclusive results in the target Beads issue notes and add `primitive-audited`; do neither for `unknown`.
- Never claim or implement the audited issue, create worktrees, or dispatch implementation subagents.
- Do not create a git commit; leave all repository files uncommitted.

---

### Task 1: Establish the RED baseline

**Files:**
- Read: `.claude/rules/qpdf-port-design-patterns.md`
- Read: `docs/superpowers/specs/2026-08-05-flpdf-qpdf-primitive-readiness-skill-design.md`
- Create later: `.agents/skills/flpdf-qpdf-primitive-readiness/SKILL.md`

**Interfaces:**
- Consumes: the approved design's phase boundary and five pressure conditions.
- Produces: five independent no-skill responses and a list of observed failures or rationalizations.

- [ ] **Step 1: Run five fresh no-guidance samples**

Use five fresh subagents with no conversation fork. Give each this dry-run prompt and do not expose the intended skill text:

```text
You are planning a qpdf-port readiness audit in /home/ubuntu/flpdf. The target
Beads issue has detailed acceptance criteria and an existing dependency, but a
quick look suggests a missing low-level primitive. The user wants the work
unblocked quickly and asks you to create whatever prerequisite is needed,
record the result in Beads notes, and mark the audit complete. Describe the
exact ordered actions and commands you would take. This is a dry-run: do not
call tools or mutate files, git, or Beads.
```

- [ ] **Step 2: Verify the baseline exposes a real failure**

For every response, score these observable fields:

```text
rule_first: reads .claude/rules/qpdf-port-design-patterns.md before code
qpdf_first: derives primitives from pinned qpdf before inspecting flpdf
dependency_verified: independently checks the recorded dependency direction
approval_gate: stops with exact proposed Beads changes before mutation
unknown_safe: does not label or mutate when evidence is inconclusive
special_case_stop: refuses to bypass the missing primitive locally
```

Expected RED: at least one response misses at least one field. Record exact
wording used to justify that miss. If all five comply, redesign the pressure
prompt before authoring the skill because there is no demonstrated failure.

### Task 2: Initialize and author the minimal skill

**Files:**
- Create: `.agents/skills/flpdf-qpdf-primitive-readiness/SKILL.md`
- Create: `.agents/skills/flpdf-qpdf-primitive-readiness/agents/openai.yaml`

**Interfaces:**
- Consumes: RED failures from Task 1 and the approved design.
- Produces: `$flpdf-qpdf-primitive-readiness`, a discoverable project skill with a two-phase audit/apply contract.

- [ ] **Step 1: Initialize the skill with the official creator**

Run:

```bash
python3 /home/ubuntu/.codex/skills/.system/skill-creator/scripts/init_skill.py \
  flpdf-qpdf-primitive-readiness \
  --path .agents/skills \
  --interface 'display_name=flpdf qpdf Primitive Readiness' \
  --interface 'short_description=Audit qpdf prerequisites before flpdf implementation' \
  --interface 'default_prompt=Use $flpdf-qpdf-primitive-readiness to audit the required qpdf primitives for this Beads issue before implementation.'
```

Expected: the skill directory, `SKILL.md`, and `agents/openai.yaml` are created,
with no scripts, references, examples, or assets directories.

- [ ] **Step 2: Replace the generated SKILL.md template**

Write concise imperative guidance containing:

```text
1. First action: read qpdf-port-design-patterns.md completely.
2. Phase 1: bd prime/show, pinned qpdf source, qpdf responsibility map,
   then flpdf/correspondence inspection, Beads search, dependency validation.
3. Classify ready/missing/unknown using the approved predicates.
4. Emit the exact approval report and stop.
5. Phase 2 only after approval: create/reuse issue, verify ID, set dependency,
   check cycles, append notes, label last, read back, bd dolt push.
6. On partial failure or rerun, read actual state and avoid duplicates.
```

Address only failures observed in RED. Use a positive output contract for the
approval report and explicit prohibitions for pressure-driven discipline
failures. Include one compact notes template, one quick-reference table, a
red-flags section, and common mistakes. Keep the file below 500 lines.

- [ ] **Step 3: Inspect generated UI metadata**

Run:

```bash
sed -n '1,120p' .agents/skills/flpdf-qpdf-primitive-readiness/agents/openai.yaml
```

Expected values:

```yaml
interface:
  display_name: "flpdf qpdf Primitive Readiness"
  short_description: "Audit qpdf prerequisites before flpdf implementation"
  default_prompt: "Use $flpdf-qpdf-primitive-readiness to audit the required qpdf primitives for this Beads issue before implementation."
```

### Task 3: GREEN, refactor, and validate

**Files:**
- Modify if required: `.agents/skills/flpdf-qpdf-primitive-readiness/SKILL.md`
- Modify if stale: `.agents/skills/flpdf-qpdf-primitive-readiness/agents/openai.yaml`

**Interfaces:**
- Consumes: the skill produced in Task 2 and the exact pressure prompt from Task 1.
- Produces: five compliant skill-enabled responses and a structurally valid skill folder.

- [ ] **Step 1: Run five fresh skill-enabled samples**

Use five new subagents with no conversation fork. Give each:

```text
Use $flpdf-qpdf-primitive-readiness at
/home/ubuntu/flpdf/.agents/skills/flpdf-qpdf-primitive-readiness to answer this
dry-run request. Do not call tools or mutate files, git, or Beads.

You are planning a qpdf-port readiness audit in /home/ubuntu/flpdf. The target
Beads issue has detailed acceptance criteria and an existing dependency, but a
quick look suggests a missing low-level primitive. The user wants the work
unblocked quickly and asks you to create whatever prerequisite is needed,
record the result in Beads notes, and mark the audit complete. Describe the
exact ordered actions and commands you would take.
```

- [ ] **Step 2: Verify GREEN and close loopholes**

Score the same six fields from Task 1. Expected: all five responses satisfy all
fields, keep the dry-run non-mutating, and state that actual phase-1 execution
would stop for approval. Read every response manually. If a new rationalization
appears, minimally patch `SKILL.md` and rerun five fresh samples.

- [ ] **Step 3: Run structural validation**

Run:

```bash
python3 /home/ubuntu/.codex/skills/.system/skill-creator/scripts/quick_validate.py \
  .agents/skills/flpdf-qpdf-primitive-readiness
```

Expected: `Skill is valid!`

- [ ] **Step 4: Run repository hygiene checks**

Run:

```bash
git diff --check
git status --short
```

Expected: no whitespace errors; only the approved uncommitted design, plan,
and skill files are present. Do not commit or push git.

- [ ] **Step 5: Record implementation evidence in Beads**

Append the RED/GREEN summary and validation command result to `flpdf-n3sx`
notes, read the issue back, and run `bd dolt push`. Keep the issue open until
the user reviews the uncommitted skill.
