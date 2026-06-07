# Contributing

Welcome! flpdf is a Pure Rust PDF processing library aiming for qpdf
parity at the writer level.

## qpdf compatibility

flpdf's outputs are continuously compared against qpdf reference outputs.
Before making writer-level changes, read:

- `docs/qpdf-compat.md` — golden matrix workflow & divergence categories
- `docs/qpdf-compat-decisions.md` — registry of decision points where
  flpdf may deliberately diverge from qpdf

When your change moves the matrix (re-blesses `tests/golden/compat-matrix.md`
or `tests/golden/baseline-static-id.md`), check the boxes in the PR
template's "Compat matrix" section.

## Signed PDFs

flpdf preserves digital signatures by default and refuses operations that
would silently invalidate them. Before touching the writer or signature
handling, read `docs/signed-pdf.md` for the preserve / refuse / opt-in-strip
policy. Note that signature *generation* is intentionally out of scope.

## Issue tracking

We use [beads](https://github.com/steveyegge/beads) (`bd`) for issue
tracking. Run `bd ready` to see available work. The project-level
conventions are described in `CLAUDE.md`.
