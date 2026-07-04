# release-plz Migration Design (flpdf-l2ug)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this design task-by-task.

**Goal:** Replace the hand-written two-workflow release automation
(`release-prepare.yml` + `release.yml`) with [release-plz], modeled on the
sibling repos `fulgur` and `fulgur-chart`, reduced to flpdf's reality (2 crates,
pure Rust, no bindings, no prebuilt binaries, lockstep workspace version).

**Motivation (user-stated):** reduce maintenance of the bespoke workflows,
automate the release cadence, and standardize on the ecosystem-standard tool.

**Models:** `fulgur` (release-plz.yml structure, dual-gate, App token) and
`fulgur-chart` (pure-release-plz library shape, commit-based changelog, tiny
config). flpdf's final shape is a reconciliation of the two, adapted to a
lockstep single-version workspace.

---

## Locked decisions (dialogue + empirical probes)

| Axis | Decision | Rationale |
|---|---|---|
| Changelog | **commit-based** (git-cliff native), **lib owns** root `CHANGELOG.md` (`flpdf` `changelog_path=CHANGELOG.md`; `flpdf-cli` `changelog_update=false`) | Serves "standardize / low-maintenance". PR-label hybrid dropped. Single section per release. |
| Versioning | **lockstep** (`version.workspace = true`), single `v{{version}}` tag | flpdf-cli is a thin wrapper over flpdf; one version number is simplest for users. Matches current scheme. |
| Version bump | **conventional default** (no `custom_minor_increment_regex`) | At 0.x, release-plz applies SemVer 0.x rules: `feat` and `fix` both → **patch**, breaking → minor. flpdf keeps its 0.1.x patch cadence naturally. |
| Publish gate | **dual gate**: ① `release-pr-approval` status check + ② `environment: release` required reviewers | User explicitly wanted the double gate. `check-releases` if-gate keeps ordinary merges from pausing on the release approval. |
| GH Release body | **release-plz generates it from the changelog** (no body-set / un-draft step, no tag-triggered `release.yml`) | commit-based changelog means release-plz's native release body is correct. Binaries deferred (flpdf-ift.7). |
| Environment name | keep existing **`release`** (not renamed to `crates-io`) | crates.io Trusted Publisher already registered to `release`; flpdf has no npm, one environment suffices. |
| App token | keep **`RELEASE_APP`** | Release PR must be App-authored so CI runs on it (GITHUB_TOKEN-authored PRs don't trigger workflows); also for tag/release push. |

### Rejected alternatives
- **(a) full fulgur-chart (decoupled per-crate versions):** clean commit-based
  output + all commits recorded, but abandons lockstep/single-tag; lib & cli
  version numbers diverge. Rejected — user wants lockstep.
- **(c) full fulgur (PR-based hybrid):** complete changelog + lockstep, but keeps
  `changelog_update=false` + aux-sync + `.github/release.yml` + `pr-labeler`
  (more config, against "reduce maintenance"). Rejected — user wants commit-based.

---

## Empirical findings (release-plz 0.3.159, `release-plz update` in isolated worktrees off `origin/main` @ v0.1.7)

Probing method: temp worktree, non-tracking branch, `TMPDIR` outside `/tmp`
(a stray `/tmp/Cargo.toml` breaks `cargo metadata`'s workspace walk-up),
`release-plz update` (no GITHUB_TOKEN needed; reads crates.io baseline).

1. **Workspace-inherited version is handled.** `version.workspace = true` →
   release-plz bumps `[workspace.package].version` (0.1.7 → 0.1.8), both crates
   follow, and flpdf-cli's `flpdf = { path=…, version="…" }` requirement (both
   `[dependencies]` and `[dev-dependencies]`) is auto-updated. **No `version_group`
   needed** (fulgur needs one only because it uses explicit per-crate versions
   for its bindings).
2. **0.x bump = patch for feat and fix.** A `fix` and a `feat` on 0.1.7 both
   produced 0.1.8. Only breaking changes reach 0.2.0. (Corrects an earlier
   assumption that `feat` → 0.2.0.)
3. **lockstep + commit-based + shared `changelog_path` = duplicate version
   headers.** With both crates pointing at `CHANGELOG.md`, release-plz writes one
   `## [0.1.8]` section per crate (cli's compare link `flpdf-cli-v0.1.7…` would
   404 since we do not create per-crate tags). This is why lib-owns-changelog
   (`flpdf-cli` `changelog_update=false`) is required — it yields a single clean
   `## [0.1.8](…/compare/v0.1.7...v0.1.8)` section.
4. **cli-only commit does NOT hold back the release.** A cli-only `feat` (no lib
   edit) still bumped the shared `workspace.version` to 0.1.8 and marked
   flpdf-cli for publish (`flpdf: already up to date` → `flpdf: next version is
   0.1.8`). The cli-only change ships; only the changelog omits it (a cli-only
   cycle yields an empty `## [x]` section). Functionally correct.

---

## Final `release-plz.toml`

```toml
# Single canonical `v{{version}}` tag/release for the lockstep workspace, instead
# of release-plz's default per-package tags (flpdf-v* / flpdf-cli-v*). Both crates
# share [workspace.package].version (version.workspace = true), so a per-package
# scheme would emit two identical-SHA tags. Disable at the workspace level, then
# opt `flpdf` back in below as the canonical tag/release.
[workspace]
git_tag_enable = false
git_release_enable = false

# flpdf owns the single root CHANGELOG.md (commit-based, git-cliff). It also
# carries the canonical version tag/release for the lockstep workspace.
[[package]]
name = "flpdf"
changelog_path = "CHANGELOG.md"
git_tag_enable = true
git_tag_name = "v{{ version }}"
git_release_enable = true
git_release_name = "v{{ version }}"

# flpdf-cli is published (publish=true default) and version-locked to flpdf via
# the shared workspace version, but does NOT emit its own changelog section:
# a lockstep workspace with a shared changelog_path would otherwise produce a
# duplicate `## [x]` header per crate. Trade-off: cli-only commits are absent
# from the changelog narrative (they still trigger the release). See flpdf-l2ug.
[[package]]
name = "flpdf-cli"
changelog_update = false
```

---

## `.github/workflows/release-plz.yml` (3 jobs)

Structure mirrors `fulgur`/`fulgur-chart`. Top-level `permissions: contents: read`;
each job grants only what it needs.

- **`check-releases`** — on every push to `main`, decide whether the push is an
  actual release (so the gated `release` job's environment approval appears only
  on real releases, not on every merge). `has_releases=true` when:
  - `github.ref == refs/heads/main` AND (`workflow_dispatch` OR head commit
    subject matches `release-plz-\d{4}-\d{2}-\d{2}` OR any pushed commit subject
    matches `^chore: release( v[0-9]|$)`).
  - Detection strings verified against sibling repos' real git history (release-plz
    output): the Release PR branch is `release-plz-<ISO8601>` (→ the "Merge pull
    request … from …release-plz-…" subject), and the release commit subject is
    `chore: release vX.Y.Z` when all released packages share one version (fulgur,
    version_group) or bare `chore: release` when versions differ (fulgur-chart,
    decoupled). flpdf is lockstep single-version → versioned form; the regex
    accepts both shapes and rejects `chore: release docs`-style false positives.
- **`release-pr`** — App token → `release-plz/action` `command: release-pr`.
  Creates/updates the Release PR (version bump + Cargo.lock + CHANGELOG.md). **No
  aux-sync step** (unlike fulgur: commit-based changelog is native, and the
  flpdf-cli path-dep version is auto-updated by release-plz). `permissions:
  contents: write, pull-requests: write`; `persist-credentials: false`.
- **`release`** — `needs: [release-pr, check-releases]`,
  `if: needs.check-releases.outputs.has_releases == 'true'`,
  `environment: release` (gate ②), `concurrency: release-plz-publish`.
  Steps: App token → `rust-lang/crates-io-auth-action` (OIDC) →
  `release-plz/action` `command: release` with `GITHUB_TOKEN` (App) +
  `CARGO_REGISTRY_TOKEN` (OIDC). release-plz publishes flpdf then flpdf-cli,
  creates the `v{{version}}` tag, and creates the GitHub Release with the
  changelog body. `permissions: contents: write, id-token: write`.

Pin all actions by SHA (repo convention). Reuse the pinned SHAs already in
`release.yml`/`fulgur` (create-github-app-token v3.2.0, crates-io-auth-action
v1.0.5, release-plz/action v0.5.130, dtolnay/rust-toolchain, Swatinem/rust-cache).

## `.github/workflows/release-pr-approval-gate.yml`

Port fulgur's gate verbatim (it is repo-agnostic): on `pull_request_target`
(opened/reopened/synchronize) + `pull_request_review` (submitted/dismissed),
write a `release-pr-approval` commit status. Non-`release-plz-*` PRs get immediate
`success`; `release-plz-*` PRs get `success` only when the latest review on the
current head SHA, by a non-author OWNER/MEMBER/COLLABORATOR, is APPROVED, else
`pending`. `permissions: contents: read, pull-requests: read, statuses: write`.
Never checks out PR code.

---

## Gate reality — the parts YAML can't enforce (→ `docs/RELEASE_SETUP.md`)

The two gates are only *reported* by workflows; they are *enforced* by repo
settings. Document and set up:

1. **① Release PR content approval:** register `release-pr-approval` as a
   **required status check** on the `main` branch ruleset. Without this the gate
   workflow writes a status nobody enforces and the Release PR can merge
   unreviewed.
2. **② Release-execution approval:** the `release` environment must actually
   carry **required reviewers** (already configured for the current pipeline).
3. **crates.io Trusted Publisher:** registered with **Environment = `release`**
   (keep — do not rename).
4. **`RELEASE_APP`** installed with Pull requests + Contents (and Workflows)
   Read/write, secrets `RELEASE_APP_ID` / `RELEASE_APP_PRIVATE_KEY` present.

## Files

- **Add:** `release-plz.toml`, `.github/workflows/release-plz.yml`,
  `.github/workflows/release-pr-approval-gate.yml`, `docs/RELEASE_SETUP.md`
  (satisfies flpdf-ift.3; supersedes its scope).
- **Delete:** `.github/workflows/release-prepare.yml`,
  `.github/workflows/release.yml`.
- **Keep (unchanged):** `pr-labeler.yml`, `.github/release.yml`. These become
  dead config under commit-based changelog but are left in place — they interact
  with dependabot's `release-notes:internal` self-labeling. Remove in a separate
  follow-up issue, not here.

## Known limitations (accepted)

- **cli-only changelog gap:** commits touching only `crates/flpdf-cli/` are
  absent from `CHANGELOG.md`; a purely-cli release cycle yields an empty
  `## [x]` section (the release still happens correctly).
- **Mixed changelog format:** new git-cliff sections stack on top of the existing
  PR-based entries. History is not reformatted.

## Deferred (separate issues)

- Prebuilt binary matrix + `release:published` cascade → **flpdf-ift.7**. When it
  lands: set `git_release_draft = true` on `flpdf`, add a `v*`-tag-triggered
  `release.yml` (build → attach → un-draft), and add a `RestrictReleaseTag` tag
  ruleset (restrict `v*` creation to the release App) to close the direct-tag
  bypass.
- Remove dead `pr-labeler.yml` / `.github/release.yml` (with dependabot label
  migration) → new follow-up issue.

## Verification

- Pre-merge: `actionlint` on the new workflows; `release-plz update` dry-run in a
  worktree reproduces the single-section CHANGELOG + lockstep bump.
- First real release: the changelog-derived **GitHub Release body** is only
  exercised on the `release` command path (not `update`); verify it visually on
  the first post-migration release.
