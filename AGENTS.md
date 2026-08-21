# Repository Instructions

## 1) Project shape
- This is a Rust workspace with two crates:
  - `crates/flpdf` (core PDF reader/writer library)
  - `crates/flpdf-cli` (CLI that wraps the library)
- `Cargo.toml` has workspace dependencies; `cargo` runs at repo root to affect both crates.

## 2) Entry points
- Core API is exported from `crates/flpdf/src/lib.rs`.
- CLI flags and commands are implemented in `crates/flpdf-cli/src/main.rs`.
- `be aware`: CLI uses the same reader/writer paths as library, so regressions often surface in both `flpdf` and `flpdf-cli` test suites.

## 3) Task tracking (mandatory)
- Use `bd` for issue work. Start with `bd prime`.
- Common workflow:
  - `bd ready`
  - `bd show <id>`
  - `bd update <id> --claim`
  - `bd close <id>`
- At session end, push Beads state and git before handing off.
- If a task should be split, use stacked PR flow (smaller dependent branches) instead of one large branch.

## 4) Development commands
- Build/verify order that usually saves time:
  - `cargo fmt -- --check`
  - `cargo test -p <crate> --test <name>`
  - `cargo test -p <crate>`
  - `cargo test` (workspace)
- High-signal focused checks:
  - `cargo test -p flpdf --test reader_tests`
  - `cargo test -p flpdf --test xref_tests`
  - `cargo test -p flpdf job::check::tests`
  - `cargo test -p flpdf --test writer_tests`
  - `cargo test -p flpdf-cli --test cli_tests`
  - `cargo test -p flpdf-cli --test compat_matrix_tests` (skips if `qpdf` is not installed)
- Quick integration smoke:
  - `cargo run --bin flpdf -- --check tests/fixtures/minimal.pdf`
  - `cargo run --bin flpdf -- tests/fixtures/minimal.pdf /tmp/out.pdf`
- qpdf oracle source (for `libqpdf/X.cc:NNN` citations in docs and module docs):
  - `scripts/fetch-qpdf-source.sh` installs qpdf pinned at the 11.9.0 commit — the version
    that matches the packaged `/usr/bin/qpdf` used as the behavioural oracle.
  - `scripts/fetch-qpdf-source.sh --print-path` resolves it; do not re-clone into `/tmp`.
  - Layout is a shared mirror plus a worktree per pinned version, with full history, so
    `git log`/`git blame` over `libqpdf/` are available for establishing *why* qpdf
    behaves a given way. Inspect other revisions without moving HEAD (`git show
    v12.0.0:libqpdf/X.cc`), or the worktree falls off the pin.
  - The tree is treated as read-only: a tracked-file edit makes both forms refuse, since
    citations against an edited tree are wrong. `--force` is the only path that discards.

## 5) Test fixtures / helpers
- Use real fixtures under `tests/fixtures/` and compatibility data under `tests/fixtures/compat` + `tests/fixtures/compat/golden`.
- Temporary files in tests/fixtures are generally built as tiny synthetic PDFs with explicit xref+trailer offsets, so verify offsets and `/Root` when editing.

## 6) Repo conventions
- Use non-interactive shell flags (`cp -f`, `mv -f`, `rm -f`, recursive `-rf`) to avoid hangs.
- Do not edit `AGENTS.md`/`CLAUDE.md` unless instruction updates are needed.
- `.beads/issues.jsonl` is tracked by Beads tooling and `.gitignore`d; avoid manual edits unless explicitly requested by issue workflow.

## 7) Design-doc review scope (`docs/plans/*.md`)
- A design doc mixes content that needs different review treatment:
  - **Durable decisions** — crate/module layout, dependency ordering, scope boundaries, qpdf oracle facts verified against real source/output. Stable once checked; review these for correctness like any other claim.
  - **Acceptance criteria** — fixture lists, golden-test assertions (exit code, stderr, stdout), negative/failure-path coverage. Always fully in scope, even inside a section that also contains provisional algorithm content: this is where thorough review actually pays off, since it's what will catch an implementation bug later. Never mark this tier provisional.
  - **Implementation-detail sketches** — exact algorithms, branch-by-branch pseudocode, resolution order, edge-case enumeration for code that does not exist yet. These are provisional, and the authority for their correctness is qpdf's own behavior (the oracle) — not this document's prose, and not a reviewer's read of the prose. A claim here can only be confirmed or refuted by running real code against the oracle (TDD), which this document cannot do. Precision here is fragile: sketch-vs-sketch review rounds have produced as many new errors as they removed (see PR #585's history — a "fix" in one round introduced the bug the next round caught).
- Content between these markers is in the third tier:
  > **[provisional — settled by TDD, not by this document]**
  >
  > *(implementation-detail sketch)*
  >
  > **[/provisional]**
- For marked content: the oracle stays authoritative even inside the marker.
  - **Do** flag a claim that misdescribes qpdf's actual behavior (a wrong oracle fact, a citation that doesn't match the cited source) — that's a factual error, checkable independently of whether the surrounding sketch is complete.
  - **Do not** flag missing edge cases, unhandled branches, or "this needs more precision" in how the sketch itself is worded — those are expected to stay open until the corresponding code and tests exist and are checked against the oracle. Re-litigating prose precision is the failure mode this convention exists to avoid.
- Unmarked content — including everything under Acceptance criteria — has no such exemption; review it normally and thoroughly.

## 8) Session close
- Before finishing, ensure quality gates ran for changed code, then push both Beads and git:
  - `bd dolt push`
  - `git pull --rebase` (optional if already synced)
  - `git push`
- Do not hand off before remote push succeeds.
