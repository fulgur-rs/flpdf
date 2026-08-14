# flpdf-3yn9.13 qpdf content primitives implementation plan

> **For the agent executing this plan:** implement each step in order, keep each
> stack layer independently green, and stop at the first missing qpdf primitive.

**Goal:** Port the qpdf 11.9.0 `QPDFObjectHandle` page/Form content family into
flpdf and establish one canonical ObjectHandle route for the later
`PageObjectHelper` and page-content consumer cutovers.

**Architecture:** Keep object identity, indirect resolution, stream source
dispatch, and filter construction in `ObjectHandle` and its existing document
owned pipeline. Page content methods only normalize handles and orchestrate the
qpdf pipeline. Content parsing exposes qpdf-shaped callbacks at the
ObjectHandle boundary; it must not materialize a page into a caller-owned
`Vec<u8>` as a substitute for a provider or add an ObjectHandle-to-Object
compatibility bridge.

**Oracle:** qpdf 11.9.0 from
`scripts/fetch-qpdf-source.sh --print-path`, currently
`/home/ubuntu/.cache/flpdf/qpdf-11.9.0`. Source citations and probes outrank
the existing flpdf shape. The relevant qpdf boundaries are
`QPDFObjectHandle.hh:129-227,421-473,837-850,1242-1255,1328-1334` and
`QPDFObjectHandle.cc:1488-1571,1702-1859,2340-2352`.

## Scope and invariants

- Implement `get_page_contents`, `add_page_contents`, `rotate_page`,
  `coalesce_content_streams`, `pipe_content_streams`, `pipe_page_contents`,
  `parse_page_contents`, `parse_as_contents`, `filter_page_contents`,
  `filter_as_contents`, `add_content_token_filter`, `add_token_filter`,
  `is_form_xobject`, `is_image`, and `get_unique_resource_name` at the
  ObjectHandle boundary.
- Preserve qpdf's missing/null/single-stream/array normalization, warning and
  error timing, stream separator rule, pipeline `finish` lifecycle, relative
  rotation traversal, and ImageMask exclusion.
- Reuse the completed stream provider and filter pipeline. The page route must
  pass an actual stream handle through that pipeline and must not use
  `pages::page_content_bytes`, an empty-buffer sentinel, or an eager aggregate
  as a provider replacement.
- Keep `PageObjectHelper`, resource pruning, page/Form dictionary construction,
  placement/flattening, overlay orchestration, and writer cutover out of this
  issue. They consume these primitives in later Beads.
- Preserve the existing public raw-parser surface only until its separately
  owned consumer cutovers are complete; the new ObjectHandle callback surface
  must be canonical and must not call back into a raw page-content route.

## Stacked PRs

Each layer is based on the immediately preceding branch. Each layer gets its
own focused tests, qpdf probe evidence, review, per-PR patch coverage, CI, and
Beads readback before the next layer is started.

### PR 1 — ObjectHandle classification and page-content normalization

Branch: `feature/flpdf-3yn9-13-objecthandle-content-shape`
Base: `main`
Beads: child of `flpdf-3yn9.13`

1. Add RED tests for direct and indirect Form/Image classification, ImageMask
   exclusion, unique resource suffix selection, and absent/null/single/array
   `/Contents` normalization.
2. Implement qpdf-shaped `is_form_xobject`, `is_image`, and
   `get_unique_resource_name`, resolving only at the same ObjectHandle boundary
   as qpdf.
3. Implement the stream-list normalization used by all later page methods.
   A null outer value is an empty content list; an array dereferences each
   member, keeps stream members, and reports/skips non-stream members with the
   qpdf description and damage state. Do not decode bytes here.
4. Add source correspondence documentation and focused tests for direct,
   indirect, nested, malformed, and inherited dictionary cases.

### PR 2 — Page content mutation and streaming pipeline

Branch: `feature/flpdf-3yn9-13-page-content-pipeline`
Base: PR 1

1. Add RED tests for `add_page_contents` prepend/append behavior, relative and
   absolute rotation (including inherited `/Rotate`, visited-parent protection,
   and invalid angles), and provider-backed coalescing without provider
   execution at registration time.
2. Implement `add_page_contents`, `rotate_page`, and
   `coalesce_content_streams` using the existing owned indirect stream factory
   and provider boundary.
3. Implement `pipe_content_streams` and `pipe_page_contents` over the existing
   `pipe_stream_data` route. Normalize stream/array contents, preserve decoded
   bytes, insert a separator only when the prior decoded stream lacks LF, and
   finish the downstream pipeline at qpdf's boundary. Propagate source,
   filter, and sink failures without broad fallback conversion.
4. Add differential tests for empty, raw, Flate, multiple-stream, no-final-LF,
   null, malformed, and failed-decode cases using `/usr/bin/qpdf` 11.9.0 where
   observable.

### PR 3 — ObjectHandle parser and token-filter entry points

Branch: `feature/flpdf-3yn9-13-content-parser-entrypoints`
Base: PR 2

1. Add RED tests for parser callback ObjectHandle identity, offset/length,
   inline-image events, normal EOF, early termination, diagnostic propagation,
   and callback failure. Add token-filter tests for output, discard, EOF,
   repeated invocation, and stream ownership.
2. Define the qpdf-shaped `ParserCallbacks`/`TokenFilter` boundary required by
   ObjectHandle content methods. Parsed tokens must arrive as ObjectHandles;
   the callback receives qpdf's token span and EOF lifecycle. A callback's
   early termination must stop parsing without a second EOF event, matching
   qpdf's `TerminateParsing` handling.
3. Implement `parse_page_contents`, `parse_as_contents`,
   `filter_page_contents`, `filter_as_contents`, and
   `add_content_token_filter` through the canonical stream pipeline and
   tokenizer. Keep parse/filter errors distinct from pipeline sink errors.
4. Migrate only the directly owned production adapters needed by this API;
   leave PageObjectHelper and unrelated raw parser consumers for their own
   Beads. Remove any temporary duplicate route introduced by this layer before
   review.

### PR 4 — Correspondence, consumer handoff, and closure evidence

Branch: `feature/flpdf-3yn9-13-content-primitives`
Base: PR 3

1. Add module documentation mapping every in-scope qpdf declaration and
   implementation range to the final Rust method and test.
2. Verify no caller in the new page/Form route performs raw Object resolution,
   page-wide eager materialization, or a second filter implementation.
3. Run the complete focused suite, all-features clippy, strict private-item
   rustdoc, workspace tests, qpdf differential probes, and changed-line
   coverage against each PR's actual parent.
4. Request review, classify every comment against pinned qpdf source and live
   behavior, fix only validated gaps, then push and merge the stack in order.
   Close the child/parent Beads only after merge readback, `bd dep cycles`, and
   successful `bd dolt push`.

## Verification gates

For every PR, run the relevant RED/GREEN focused test first, then:

```text
cargo fmt --all -- --check
cargo test -p flpdf --test <focused-test>
cargo test -p flpdf --lib
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail
scripts/patch-coverage.sh --base <actual-parent-branch> --lcov
```

The qpdf differential tests must invoke the pinned 11.9.0 binary/source
helpers, use real fixture PDFs, and record exit status, warnings, and output
bytes where the behavior is observable. No PR is considered complete merely
because the tip of the stack has coverage; coverage is measured per PR.
