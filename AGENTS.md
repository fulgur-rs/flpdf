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
  - `cargo test -p flpdf --test check_tests`
  - `cargo test -p flpdf --test writer_tests`
  - `cargo test -p flpdf-cli --test cli_tests`
  - `cargo test -p flpdf-cli --test compat_matrix_tests` (skips if `qpdf` is not installed)
- Quick integration smoke:
  - `cargo run --bin flpdf -- --check tests/fixtures/minimal.pdf`
  - `cargo run --bin flpdf -- tests/fixtures/minimal.pdf /tmp/out.pdf`

## 5) Test fixtures / helpers
- Use real fixtures under `tests/fixtures/` and compatibility data under `tests/fixtures/compat` + `tests/fixtures/compat/golden`.
- Temporary files in tests/fixtures are generally built as tiny synthetic PDFs with explicit xref+trailer offsets, so verify offsets and `/Root` when editing.

## 6) Repo conventions
- Use non-interactive shell flags (`cp -f`, `mv -f`, `rm -f`, recursive `-rf`) to avoid hangs.
- Do not edit `AGENTS.md`/`CLAUDE.md`/`docs/superpowers/...` unless instruction updates are needed.
- `.beads/issues.jsonl` is tracked by Beads tooling and `.gitignore`d; avoid manual edits unless explicitly requested by issue workflow.

## 7) Session close
- Before finishing, ensure quality gates ran for changed code, then push both Beads and git:
  - `bd dolt push`
  - `git pull --rebase` (optional if already synced)
  - `git push`
- Do not hand off before remote push succeeds.
