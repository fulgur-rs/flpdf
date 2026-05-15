---
name: stacked-merge
description: Use when merging a stack of dependent PRs (gh-stack / epic sub-* branches) into main in this repo, when resuming a half-finished stack merge after a rate limit or session end, or when CodeRabbit is paused / rate-limited / requesting changes on a stack PR and you need to drive it to merge with minimal human intervention.
---

# Stacked PR Merge (flpdf)

## Overview

Merge a stack (`epic/<id>/sub-1..N`) into `main` **bottom-to-top, one at a
time**, fully autonomously. The full design and rationale live in
`docs/plans/2026-05-16-resilient-stacked-merge-design.md` — **read it before
first use**. This skill is the *how*; the design doc is the *why*. The gaps
below are exactly the ones a capable agent gets wrong without this skill.

`main` is PR-only (GitHub ruleset). Never `git push origin main`.

## Resume / state

No state file. Source of truth = GitHub + git + CodeRabbit comments.
At session start derive: merged set (PR state==MERGED), `cursor` = lowest
OPEN `sub-K`, cascade-pending (`git merge-base --is-ancestor origin/main
<upstack>` is false). Every phase is idempotent — re-running is safe.

## Per-PR loop (cursor = lowest OPEN sub)

1. **Gate** (all must hold; one false → stop with reason printed):
   - CI: `gh pr checks <cursor> --required` all success. The `test` job
     **is the qpdf byte-identical safety net** (compat_matrix /
     zlib-compat / compat baselines) — it is the real gate, not optional.
   - **CodeRabbit (REQUIRED, self-imposed)**: either latest `coderabbitai`
     review == `APPROVED`, **or** every actionable finding is dispositioned
     (fixed, or followup issue filed + replied on its comment thread).
     **AND** the deciding commit (approval commit, or last fix push) ==
     current PR head SHA. CodeRabbit is **never** "skip after timeout".
   - roborev: pass **for the current head SHA**. roborev is a local
     CLI/daemon (`roborev`), **not** a GitHub check, and does **not**
     auto-fire on force-push. If head SHA changed (cascade / fix push),
     the old result is void — explicitly re-kick `roborev review <HEAD>`
     (or the `roborev-review-branch` skill) and wait for a fresh
     `completed` pass (lands in beads via `.roborev.toml` hooks).
   - Compat matrix: tick the PR template's 2 boxes automatically **iff**
     `tests/golden/compat-matrix.md` / `tests/golden/baseline-static-id.md`
     have no drift vs `origin/main`; if drifted, do NOT auto-tick → human
     flag (intentional re-bless + possible `docs/qpdf-compat-decisions.md`
     entry).
2. **CodeRabbit state machine** (when gate's CR condition is not yet met):
   - `paused` (comment has an unchecked `Resume review` task box): fetch
     the comment via `gh api .../issues/comments/{id}`, flip **only** the
     line labelled `Resume review` from `- [ ]` to `- [x]`, PATCH it back,
     re-fetch to confirm. Re-evaluate.
   - `rate-limited` (comment states a retry time): wait until that reset
     time, then `gh pr comment <cursor> --body "@coderabbitai review"`.
     Re-evaluate. (Both at once → clear rate-limit first.)
   - `CHANGES_REQUESTED`: enter Resolve sub-loop (below). Do not stop.
   - `in-progress`/none: short poll.
3. **Resolve sub-loop** (CHANGES_REQUESTED): triage each finding,
   conservative bias —
   - clear & safe → fix now, commit, push to the PR branch;
   - needs judgement / out of scope → `bd create` a followup issue and
     reply **on that finding's comment thread** with the issue ref;
   - unsure if safe to defer → fix it, or human flag (never auto-defer).
   Then `@coderabbitai review`, re-enter step 2. Bounded iterations N;
   non-convergence → human flag. **No finding silently dropped.**
4. **Merge**: gate fully green → `gh stack merge <cursor>` (bottom-only;
   `delete_branch_on_merge` removes the remote branch).
5. **Cascade**: `gh stack sync` (atomic; rebases upstack, re-points PR
   bases). Conflict → it restores all branches and you **stop → human**
   (no auto conflict resolution). Cascade force-pushes upstack ⇒ CodeRabbit
   re-review + roborev re-kick fire — handled by step 2 / gate next loop.
6. Loop to the new cursor.

## Stop → human (only these)

Resolve non-convergence · CI failure not fixable · `gh stack sync` conflict ·
Compat-matrix drift needing a decision · "defer-safety unknown" finding.
Everything else (pause, rate-limit, CHANGES_REQUESTED) is automatic.

## Common mistakes (from baseline testing)

- Treating CodeRabbit as non-blocking once it's paused/rate-limited — it is
  a **required** gate here.
- Using `reviewDecision==APPROVED` without the head-SHA-match check — a
  cascade force-push leaves a stale approval that must not count.
- Inventing `@coderabbitai resolve` for pause — pause is cleared by the
  `Resume review` **checkbox**, not a command.
- Raw `gh pr merge` instead of `gh stack merge` / skipping `gh stack sync`.
- Omitting the qpdf byte-identical / Compat-matrix gate entirely.
- Stopping on CHANGES_REQUESTED instead of triage→fix/followup.

## Alternative mode

If direct-to-main has friction, see design doc §9: insert an
`epic/<id>` acceptance branch, merge the stack into it, then one final
acceptance→main PR. Switch criteria are in §9.
