# flpdf

A Pure Rust PDF toolkit modeled on the [qpdf](https://github.com/qpdf/qpdf)
workflow. flpdf aims for writer-level parity with qpdf — its outputs are
continuously compared against qpdf reference outputs through a golden matrix
(see [`docs/qpdf-compat.md`](docs/qpdf-compat.md)).

## Workspace layout

This repository is a Cargo workspace with two crates:

| Crate | Path | Purpose |
| --- | --- | --- |
| `flpdf` | `crates/flpdf` | Core PDF reader / writer library. |
| `flpdf-cli` | `crates/flpdf-cli` | CLI (`flpdf` binary) that wraps the library. |

Both crates are licensed under `MIT OR Apache-2.0`.

## Building

```bash
cargo build --workspace
cargo test  --workspace
```

The default build is Pure Rust. The optional `qpdf-zlib-compat` feature links
classic libz via `libz-sys` so flate2's deflate output matches qpdf's
`compress2()` byte-for-byte; it is used by bytes-identical tests only and is
not required for production builds.

```bash
cargo test -p flpdf --features qpdf-zlib-compat
```

## CLI usage

```bash
cargo build --release -p flpdf-cli
./target/release/flpdf --help
```

Common subcommands (`flpdf <subcommand> --help` for full options):

```bash
flpdf check input.pdf                       # validate structure / report diagnostics
flpdf pages input.pdf                       # show page structure
flpdf dump-object 7 0 input.pdf             # dump one indirect object
flpdf qdf      input.pdf  out.qdf           # qdf-style flat dump
flpdf rewrite  input.pdf  out.pdf           # incremental rewrite
flpdf rewrite --linearize    in.pdf out.pdf # produce a linearized PDF
flpdf rewrite --full-rewrite in.pdf out.pdf # decode + re-emit with FlateDecode
```

Encrypted inputs are supported via `--password`, `--password-file`, and
`--password-mode {auto,bytes,hex-bytes,unicode}` (the `unicode` mode applies
SASLprep for V=5 R=5/R=6 handlers). RC4-backed handlers and revision 5
encryption are gated behind `--allow-weak-crypto`.

`--static-id`, `--min-version`, `--force-version`, and the qpdf compatibility
flags (`--compress-streams`, `--linearize-pass1`) are accepted on `rewrite`
to keep qtest-style command lines parsing cleanly.

## Library usage

```rust
use std::fs::File;
use std::io::BufReader;
use flpdf::{pages, write_pdf, Pdf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = BufReader::new(File::open("input.pdf")?);
    let mut pdf = Pdf::open(file)?;

    for object_ref in pages::page_refs(&mut pdf)? {
        println!("page: {object_ref}");
    }

    let mut out = File::create("output.pdf")?;
    write_pdf(&mut pdf, &mut out)?;
    Ok(())
}
```

The crate is organized as a few small layers:

- `Pdf` — parsed-but-lazy document handle (`Pdf::open` reads the trailer and
  cross-reference table, then resolves objects on demand via `Pdf::resolve`).
- `Object`, `Dictionary`, `Stream`, `ObjectRef` — the data model.
- `pages`, `outline`, `fonts` — read-only traversal helpers that mirror
  `qpdf --show-pages`, `--show-outline`, and `--show-fonts`.
- `write_pdf` / `write_qdf` — incremental rewrite and qdf-style flat dump.
- `check_reader` — diagnostics gathered during parsing/repair, returning a
  `CheckReport` of `Diagnostic`s.

Errors flow through the unified `Error` enum and the crate-level `Result`
alias. See the rustdoc on `crates/flpdf/src/lib.rs` for the full API.

## Examples

Runnable examples live in [`crates/flpdf/examples`](crates/flpdf/examples).
Each one is a small, self-contained program; run any of them with:

```bash
cargo run --example <name> -p flpdf
```

| Example | Description |
| --- | --- |
| `inspect` | Inspect and dump a PDF's basic structure end to end. |
| `extract_page` | Extract a single page (0-based) into a new minimal PDF. |
| `extract_pages` | Extract a non-contiguous selection (pages 1, 3, 5) into a new file. |
| `extract_first_5_pages` | Extract the first 5 pages of a document into a new file. |
| `list_form_fields` | List every interactive form field with its type and value. |
| `walk_outline` | Walk the document outline (bookmarks) as an indented tree. |
| `pull_attachments` | Pull every embedded attachment out of a document to disk. |
| `reorder_pages` | Reorder a document's pages (here: reverse them) and write the result. |
| `merge_pdfs` | Merge two PDFs, preserving fonts shared between the merged-in pages. |
| `splice_pages` | Splice pages from one document into another at a given index. |

## qpdf compatibility

flpdf's writer-level outputs are compared against qpdf in two places:

- **Golden matrix** — `tests/golden/compat-matrix.md`, one row per
  (fixture, flag) tuple and one column per comparator strategy. Regenerated
  with `BLESS=1 cargo test -p flpdf-cli --test compat_matrix_baseline`.
- **Decisions registry** — [`docs/qpdf-compat-decisions.md`](docs/qpdf-compat-decisions.md)
  records every deliberate divergence point so contributors can tell
  intentional gaps from incidental ones.

The full workflow lives in [`docs/qpdf-compat.md`](docs/qpdf-compat.md). When
your change re-blesses the matrix or the static-ID baseline, check the PR
template's "Compat matrix" section.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the contributor guide and
[`AGENTS.md`](AGENTS.md) / [`CLAUDE.md`](CLAUDE.md) for AI-agent conventions.
Issue tracking uses [beads](https://github.com/steveyegge/beads); start with
`bd ready` to see available work.

## License

Licensed under either of

- Apache License, Version 2.0
- MIT License

at your option.
