# Contributing

Welcome! flpdf is a Pure Rust PDF processing library aiming for qpdf
parity at the writer level.

## qpdf compatibility

flpdf's outputs are continuously compared against qpdf reference outputs.
Before making writer-level changes, read:

- `docs/qpdf-compat.md` — golden matrix workflow & divergence categories
- `docs/qpdf-compat-decisions.md` — registry of decision points where
  flpdf may deliberately diverge from qpdf
- `docs/pdfa.md` — PDF/A preservation scope and conformance-validation limits

When your change moves the matrix (re-blesses `tests/golden/compat-matrix.md`
or `tests/golden/baseline-static-id.md`), check the boxes in the PR
template's "Compat matrix" section.

## Release versioning and breaking changes

release-plz derives version bumps from Conventional Commits. While the
workspace is pre-1.0, non-breaking `feat` and `fix` commits bump the patch
version, and a breaking marker bumps the minor version. Removing or
incompatibly changing a published CLI command, flag, or public Rust API must
carry a breaking marker in the commit release-plz reads: use `!` after the
type/scope (for example, `chore(cli)!: remove a public subcommand`) or add a
`BREAKING CHANGE:` footer. A changelog entry or PR description alone does not
set the version bump.

The pre-1.0 compatibility policy does not promise that these surfaces stay
compatible, but the breaking marker still tells downstream users that the
release contains an incompatible change. The `show-stream` and `dump-object`
subcommands were removed in 0.6.0 without markers; the next intentional
breaking release must call out both removals in its release notes.

## Fuzzing

Start with [`docs/fuzzing.md`](docs/fuzzing.md) for local setup, running
targets, crash triage, regression fixtures, and adding a target.
[`fuzz/README.md`](fuzz/README.md) describes each harness, its input and seed
formats, and recorded fuzz runs. Pull requests exercise the registered targets
in the `Fuzz (short)` Actions job.

## Signed PDFs

flpdf preserves digital signatures by default and refuses operations that
would silently invalidate them. Before touching the writer or signature
handling, read [docs/signed-pdf.md](docs/signed-pdf.md) for the preserve /
refuse / opt-in-strip policy. Note that signature *generation* is intentionally out of scope.

## Issue tracking

We use [beads](https://github.com/steveyegge/beads) (`bd`) for issue
tracking. Run `bd ready` to see available work. The project-level
conventions are described in `CLAUDE.md`.
