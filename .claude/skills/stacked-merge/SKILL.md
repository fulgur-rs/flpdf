---
name: stacked-merge
description: Use when merging a chain of dependent PRs (epic/<id>/sub-* branches) into main in this repo, when resuming a half-finished stacked merge after a rate limit or session end, or when CodeRabbit is paused / rate-limited / requesting changes on such a PR and you need to drive it to merge with minimal human intervention.
---

# Stacked PR Merge (flpdf)

## Overview

Merge a chain of dependent PRs (`epic/<id>/sub-1..N`) into `main`
**bottom-to-top, one at a time**, fully autonomously. The full design and
rationale live in `docs/plans/2026-05-16-resilient-stacked-merge-design.md`
— **read it before first use**. This skill is the *how*; the design doc is
the *why*. The gaps below are exactly the ones a capable agent gets wrong
without this skill.

**This repo does NOT use gh-stack tooling for these PRs.** They are plain
chained PRs (base pointers only); `gh stack view` errors with "not part of
a stack". **Never use `gh stack merge` / `gh stack sync`** — they will fail
or operate on the wrong thing. Merge as **normal PRs** with `gh pr merge`
and do the cascade rebase **by hand** (below).

`main` is PR-only (GitHub ruleset). Never `git push origin main` — merge
only via `gh pr merge` (goes through the API, respects the ruleset).

## Resume / state

No state file. Source of truth = GitHub + git + CodeRabbit comments.
At session start derive: merged set (PR state==MERGED), `cursor` = lowest
sub-K that is **unmerged with its branch still ahead of main**, cascade-pending for the new cursor (`git merge-base
--is-ancestor origin/main origin/<cursor-branch>` is false ⇒ needs the §5
rebase). Also check for an interrupted manual rebase: if
`.git/rebase-merge` or `.git/rebase-apply` exists, a previous cascade was
cut off mid-conflict → resolve or `git rebase --abort` then **stop →
human** (see Stop section).

**Do NOT compute cursor as "lowest `state==OPEN` sub".** A middle sub can
be **CLOSED-but-not-MERGED** with its branch still ahead of `origin/main`
(work stranded; the next PR's base still points at it). Picking the next
OPEN sub would lose that work and carry its ungated commits onto main via
the child PR. So: for each sub bottom-up, if it is not MERGED **and**
`git merge-base --is-ancestor origin/<sub-branch> origin/main` is false
(branch has commits not on main), that sub is the cursor — even if its PR
is CLOSED or draft. Recovery: `gh pr reopen <pr>` → `gh pr ready <pr>` →
`gh pr edit <pr> --base main` if its old base branch was deleted (no
auto-retarget happens while a PR is closed) → re-derive. If reopen fails
→ **stop → human**.

Every phase is idempotent — re-running is safe.

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
4. **Merge** (normal PR, NOT gh-stack): cursor's base must be `main`
   (it is, since cursor is the lowest open sub; if not, a lower PR didn't
   merge — re-derive). Gate fully green →
   `gh pr merge <cursor> --squash --delete-branch`.
   - Use `--squash` (one commit per PR on main; consistent with the PR
     template's per-PR change unit). `--delete-branch` removes the remote
     branch, which makes GitHub **auto-retarget the next PR's base to
     `main`**.
5. **Cascade by hand** (replaces `gh stack sync`): only the **new cursor**
   (next sub) needs rebasing now — PRs further up still point at branches
   that still exist, so rebase them lazily when they each become cursor.
   For the new cursor branch `B`:
   - `git fetch origin --prune`
   - First commit belonging to the PR (not the parent's):
     `FIRST=$(gh pr view <B-pr> --json commits --jq '.commits[0].oid')`
     — derived from GitHub, so no ledger needed.
   - `git checkout B && git rebase --onto origin/main "$FIRST^" B`
     (replays only B's own commits onto new main; drops the parent's
     now-merged commits even though squash changed their hashes).
   - `git push --force-with-lease origin B`
   - Conflict during rebase → `git rebase --abort`, **stop → human**
     (no auto conflict resolution; the manual rebase is NOT atomic, so
     leave B abandoned-but-restored, not half-rebased).
   - This force-push changes B's head SHA ⇒ CodeRabbit re-review +
     roborev re-kick needed — handled by step 1 gate / step 2 next loop.
6. Loop to the new cursor.

## Stop → human (only these)

Resolve non-convergence · CI failure not fixable · **manual cascade rebase
conflict** (after `git rebase --abort`) · Compat-matrix drift needing a
decision · "defer-safety unknown" finding · **CLOSED-not-MERGED middle sub
whose `gh pr reopen` fails**.
Everything else (pause, rate-limit, CHANGES_REQUESTED) is automatic.

## Common mistakes (from baseline testing)

- Treating CodeRabbit as non-blocking once it's paused/rate-limited — it is
  a **required** gate here.
- Using `reviewDecision==APPROVED` without the head-SHA-match check — a
  cascade force-push leaves a stale approval that must not count.
- Inventing `@coderabbitai resolve` for pause — pause is cleared by the
  `Resume review` **checkbox**, not a command.
- Using `gh stack merge` / `gh stack sync` — this repo has NO registered
  gh-stack stack; those commands fail. Use `gh pr merge` + manual rebase.
- Plain `git rebase origin/main` on the next branch instead of
  `git rebase --onto origin/main "$FIRST^"` — a plain rebase replays the
  parent's already-merged commits and explodes into conflicts / a bloated
  diff. Always use `--onto` with the PR's first commit.
- Omitting the qpdf byte-identical / Compat-matrix gate entirely.
- Stopping on CHANGES_REQUESTED instead of triage→fix/followup.

## Alternative mode

If direct-to-main has friction, see design doc §9: insert an
`epic/<id>` acceptance branch, merge the stack into it, then one final
acceptance→main PR. Switch criteria are in §9.
