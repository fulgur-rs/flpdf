# qpdf Module Doc Correspondence Design

**Issue:** `flpdf-qxba.3`

**Goal:** Make every Rust module in `crates/flpdf/src` declare its qpdf
correspondence at the top of its module documentation, generate a deterministic
machine-readable index from those declarations, and reject missing or stale
correspondence metadata in CI.

## Scope

The checker covers every `*.rs` file below `crates/flpdf/src`, including
`lib.rs`, nested modules, and `mod.rs` files.

`crates/flpdf-cli` is outside the checker. D4 describes correspondence with
qpdf's library components, while the CLI is primarily a `QPDFJob`/argument
surface rather than a set of `libqpdf` component modules.

The existing `docs/qpdf-correspondence.md` remains the human-maintained design
and backlog document. The generated index is a compact inventory; it does not
replace the explanations, status classifications, or implementation guidance in
that document.

## Source Annotation Grammar

Each covered file must contain exactly one classification line in its leading
inner-doc block.

A module that is a completed qpdf component mirror uses D4's required wording:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
```

More than one qpdf implementation file may be listed on the same line, separated
by commas:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/JSON.cc, libqpdf/JSONHandler.cc.
```

Every path on a `Mirrors` line must be a relative `libqpdf/*.cc` path. The
version is exactly `11.9.0`. This prevents annotations from silently drifting
away from the repository's pinned oracle.

A module that is not a completed one-to-one component mirror uses an explicit
correspondence classification:

```rust
//! qpdf correspondence: flpdf-only standard font metrics.
```

This free-text form covers flpdf-only modules, Rust/ecosystem substitutions,
responsibilities smeared across multiple qpdf components, and incomplete
component extractions. It must contain a non-empty reason and must not claim
that the module satisfies D4.

The classification must appear before the first Rust item. Other leading module
documentation and inner attributes are preserved.

## Checker and Generator

Add `scripts/qpdf-module-docs.py`, implemented with the Python standard library.
It has two public modes:

- `--write` scans the source tree and writes
  `docs/qpdf-module-doc-index.md`.
- `--check` scans the source tree, validates every classification, renders the
  expected index in memory, and fails if the committed index differs.

The scan is sorted by repository-relative source path. Diagnostics name the
offending file and one concrete reason:

- classification missing from the leading inner-doc block;
- multiple classification lines;
- empty `qpdf correspondence` reason;
- wrong qpdf version;
- invalid or non-`libqpdf/*.cc` path;
- generated index missing or stale.

The generated index contains its generator command, a warning not to edit it
manually, and a table with source path, classification kind, and qpdf
correspondence text. Markdown table cells are escaped by the generator.

## Module Classification Pass

Annotate every currently covered module. A `Mirrors` line is added only when the
existing source, `docs/qpdf-correspondence.md`, and pinned qpdf component
boundary support a completed component claim. Existing `pdf_version.rs` remains
the first D4 example.

All other modules receive a truthful `qpdf correspondence` reason. This task
does not upgrade a `smeared`, incomplete, substituted, or flpdf-only module to a
completed mirror merely to satisfy the checker.

The bulk edit is documentation-only. It must not move implementation, rename
modules, change visibility, or alter runtime behavior.

## Tests and CI

Add Python `unittest` coverage under
`scripts/tests/test_qpdf_module_docs.py`. Tests use temporary source trees and
exercise:

- a valid single-file mirror;
- a valid multiple-file mirror;
- a valid explicit non-mirror classification;
- missing and duplicate classifications;
- wrong version and invalid qpdf paths;
- an empty non-mirror reason;
- deterministic path ordering and Markdown escaping;
- stale generated-index detection.

The tests are written and observed failing before the checker implementation.

Add a Quality-job step in `.github/workflows/ci.yml` that runs:

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

The focused test command, checker, formatting, documentation links, workspace
clippy, workspace tests, and changed-line coverage gate are run before
completion. Because the production Rust changes are module documentation only,
the checker tests provide the behavioral coverage for this task.

## Review Hardening

The source scanner must not depend on Python's Unicode identifier tables to
decide whether an apostrophe starts a Rust lifetime. Rust may accept an XID
start character that the Python runtime does not yet recognize. If the scanner
then treats the apostrophe as the start of a character literal, a later
apostrophe in ordinary text can hide the closing `]` of an inner attribute and
make the scanner miss a real module-doc classification.

Instead, the scanner recognizes a character literal only when the apostrophe is
followed by exactly one Rust character-literal payload (one source character or
one escape) and an immediate closing apostrophe. An apostrophe that does not
have that shape remains ordinary scanner input, so brackets after a newer Rust
lifetime stay visible. This keeps the checker self-contained and avoids
vendoring Rust's evolving XID tables or invoking `rustc` during the Python-only
CI step. The regression fixture uses U+088F, which Rust 1.96 accepts as a
lifetime start while Python 3.12 does not recognize it as an identifier start,
and places a real classification after the affected inner attribute.

The CI command contract must account for inherited workflow configuration
before it narrows inspection to the `quality` job. A workflow-level
`defaults.run.shell` can alter every unqualified `run` step, so the contract
first rejects a non-default workflow-level run shell and then applies the
existing quality-job and step-level exact-command checks. Parsing the workflow
with a new YAML dependency or requiring repeated explicit `shell: bash` entries
would add broader dependency or configuration churn without improving this
specific invariant.

The Rust regression constructs a workflow with
`defaults.run.shell: echo {0}` and an otherwise exact Quality command. The
contract must reject it. Existing character-literal scanner coverage remains
green, proving that a real character literal containing a bracket is still
consumed without changing attribute depth.

## Non-goals

- Replacing or mechanically regenerating `docs/qpdf-correspondence.md`.
- Declaring every existing module a completed qpdf mirror.
- Moving code to make current `smeared` modules satisfy D1 or D2.
- Checking `crates/flpdf-cli`, test modules, examples, or build artifacts.
- Verifying that a named qpdf source file exists by downloading or modifying the
  oracle tree during CI; the grammar and pinned version are checked locally.
