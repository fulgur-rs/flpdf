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

## Fuzzing

Pull requests run the `Fuzz (short)` Actions job. It seeds the gitignored
corpora with `tools/seed-corpus.sh`, then runs `roundtrip`, `xref`,
`filter_pipeline`, `primitive_parser`, and `objstm` for 60 seconds each. Each
input has a 10-second timeout and the job caps RSS at 2 GiB. Local setup,
target-specific commands, and longer acceptance runs are documented in
[`fuzz/README.md`](fuzz/README.md).

A crash or timeout makes the target exit nonzero and fails the job. Crash logs
show libFuzzer's stack trace, and the reproducer is written under
`fuzz/artifacts/<target>/`. For example, replay a crash with:

```bash
cargo +nightly-2026-05-24 fuzz run --target x86_64-unknown-linux-gnu roundtrip \
  fuzz/artifacts/roundtrip/crash-<hash>
```

An example crash log (addresses and intermediate frames abbreviated) looks
like this:

```text
==<pid>== ERROR: libFuzzer: deadly signal
    #0 ... in <fuzz target frame>
    #1 ... in <caller>
SUMMARY: libFuzzer: deadly signal
Test unit written to fuzz/artifacts/<target>/crash-<hash>
```

## Signed PDFs

flpdf preserves digital signatures by default and refuses operations that
would silently invalidate them. Before touching the writer or signature
handling, read [docs/signed-pdf.md](docs/signed-pdf.md) for the preserve /
refuse / opt-in-strip policy. Note that signature *generation* is intentionally out of scope.

## Issue tracking

We use [beads](https://github.com/steveyegge/beads) (`bd`) for issue
tracking. Run `bd ready` to see available work. The project-level
conventions are described in `CLAUDE.md`.
