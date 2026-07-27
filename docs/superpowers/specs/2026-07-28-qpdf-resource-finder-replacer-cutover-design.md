# qpdf ResourceFinder / ResourceReplacer cutover design

**Issue:** `flpdf-qynx.3`
**Date:** 2026-07-28
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd`)
**Oracle source:** `scripts/fetch-qpdf-source.sh --print-path`
**Parent design:** [qpdf Phase 2 foundations design](2026-07-27-qpdf-phase-2-foundations-design.md)

## Problem

flpdf already reproduces parts of qpdf's resource-name discovery and replacement behavior, but
the responsibility is split across independent handwritten routes:

- `content_normalizer.rs` owns a private `TokenFilter` and whole-buffer token runner that mirrors
  `Pl_QPDFTokenizer` without using the crate's `Pipeline` contract;
- `overlay_appearance_stream.rs` has a second byte scanner, operator table, inline-image finder,
  and resource-name rewriter;
- `overlay_annotations.rs` has a third scanner and operator table for copied fields' `/DA`
  strings;
- `resources.rs` has a fourth resource-operator classifier inside the recursive
  unreferenced-resource pruning walk.

qpdf 11.9.0 keeps three distinct responsibilities:

1. `Pl_QPDFTokenizer` buffers pipeline writes, emits every byte in exactly one token at `finish`,
   gives those tokens to a `TokenFilter`, and optionally forwards filter output;
2. `ResourceFinder` is a `ParserCallbacks` implementation that records the last parsed name,
   resource operator, and raw byte offset;
3. `ResourceReplacer` is a `TokenFilter` that rewrites only name tokens at offsets selected by a
   prior `ResourceFinder` pass.

Adding these components beside the existing routes would not complete the issue. The production
consumers must move and the duplicate scanners, operator tables, and replacement algorithms must
be deleted.

## Goals

1. Complete the crate-private `Pl_QPDFTokenizer` responsibility on top of `Pipeline`.
2. Preserve qpdf's separate `ParserCallbacks` and `TokenFilter` boundaries.
3. Add one `ResourceFinder` and one `ResourceReplacer` implementation.
4. Migrate `ContentNormalizer`, copied-field `/DA`, copied appearance streams, and
   unreferenced-resource pruning.
5. Preserve qpdf byte output, token offsets, inline-image opacity, and lifecycle/error timing.
6. Delete every superseded scanner, resource-operator table, and rewrite path in the declared
   consumer scope.
7. Reach fresh 100% patch coverage for changed executable lines.

## Non-goals

- Replacing `content_stream.rs`'s general `ParserCallbacks` orchestration.
- Replacing resource-pruning scope resolution, Form XObject recursion, `(Form, scope owner)`
  deduplication, cycle handling, or conservative retain policy.
- Adding deferred stream token filters to the PDF object model. The current consumers transform
  decoded bytes immediately.
- Making `TokenFilter`, `ResourceFinder`, or `ResourceReplacer` public API.
- Introducing a generic resource-transformation framework or combining finder and replacer into
  one abstraction.
- Migrating unrelated default-appearance parsing and appearance generation.

## Responsibility boundaries

### TokenFilter output

`token_filter.rs` owns the crate-private filter callback contract and the output helper corresponding
to qpdf's protected `TokenFilter::write` and `writeToken` methods.

The output helper wraps `Option<&mut dyn Pipeline>`:

- `Some(next)` forwards non-empty byte writes to `next`;
- `None` discards output successfully;
- `write_token` forwards a token's raw bytes;
- constructing a canonical string or name token remains the filter's responsibility.

The filter receives the output helper explicitly for each callback. `QpdfTokenizer` separately
tracks the retained downstream borrow and whether filter output is still attached, reproducing
qpdf's hidden pipeline pointer without a shared-ownership graph.

### QpdfTokenizer pipeline stage

`pipeline/qpdf_tokenizer.rs` owns the `Pl_QPDFTokenizer` lifecycle:

- `write` appends chunks to an internal byte buffer without tokenizing them;
- `finish` tokenizes the complete buffered input with EOF and ignorable tokens enabled;
- every byte is covered exactly once by a delivered token;
- an `ID` token is followed by one synthetic space token containing the single consumed separator,
  then one opaque inline-image token;
- the EOF token is delivered before `handle_eof`;
- filter output is forwarded or discarded through the output helper;
- the first successful `handle_eof` permanently detaches filter output before the retained
  downstream pipeline is finished;
- repeated finish and write-after-finish still deliver callbacks and finish downstream, but their
  filter output is discarded;
- callback/tokenization failures are returned unchanged and do not finish downstream;
- token callback and `handle_eof` failures retain attachment, while downstream-finish failure
  occurs after detachment and remains detached on retry;
- chunk boundaries do not affect tokens, offsets, or output.

The stage follows observed qpdf reuse behavior rather than adding a global Pipeline state machine.
A live qpdf lifecycle probe fixes repeated `finish`, write-after-finish, and downstream-finish retry
behavior, including the distinction between downstream ownership and filter-output attachment.

### ResourceFinder

`resource_finder.rs` owns qpdf's operator-to-resource table and implements
`content_stream::ParserCallbacks`.

It records:

- `resource type -> name -> raw start offsets`;
- whether parser recovery diagnostics occurred.

The flat `getNames()` view needed by the live qpdf oracle is the categorized map's key union and is
derived only in tests; production does not retain a duplicate set.

It tracks the most recently observed name and its offset. Like qpdf, unrelated operands do not clear
that name, and a resource operator consults it without clearing it. The resource types are:

| Operators | Resource type |
|---|---|
| `CS`, `cs` | `ColorSpace` |
| `gs` | `ExtGState` |
| `Tf` | `Font` |
| `SCN`, `scn` | `Pattern` |
| `BDC`, `DP` | `Properties` |
| `sh` | `Shading` |
| `Do` | `XObject` |

Names use flpdf's decoded representation without the leading slash; offsets always refer to the
original raw content bytes.

### ResourceReplacer

`resource_replacer.rs` owns qpdf's offset-indexed replacement algorithm and implements
`TokenFilter`.

Construction crosses:

```text
dr_map[resource type][old name] = new name
finder[resource type][old name] = raw offsets
```

into:

```text
replacement[old name][raw offset] = new name
```

For each token, it:

1. checks a name token against the replacement table at the current raw offset;
2. converts the replacement's decoded name body to an unambiguous canonical name value and writes
   its escaped token, including when the decoded body itself begins with `/`;
3. otherwise writes the original raw token;
4. increments its offset by the original token's raw length.

Whitespace, comments, malformed tokens retained by the tokenizer, and opaque inline-image bytes pass
through exactly. Replacement length never changes the source-offset counter.
Matching qpdf's error ordering, a selected replacement advances after its write succeeds, while a
non-replacement advances before attempting its downstream write.

## Consumer cutover

### ContentNormalizer

`content_normalizer.rs` keeps the public `ContentNormalization` result and normalization policy.
Its private `TokenFilter` and `run_token_filter` are deleted. The normalizer implements the shared
filter trait and writes through `QpdfTokenizer -> Buffer`.

Existing qpdf 11.9.0 differential cases remain byte-identical, including bad-token state, CR/CRLF,
canonical string/name forms, `ID` separators, false `EI` candidates, EOF order, and inline binary
data.

### Copied field default appearances

`overlay_annotations.rs::adjust_default_appearance` performs:

```text
parse_content_stream_data -> ResourceFinder
QpdfTokenizer -> ResourceReplacer -> Buffer
```

The current inline scanner, local operator table, raw-span splice logic, and local-resources
presence guard are deleted. qpdf's replacer selects offsets only by crossing `dr_map` with finder
results; it does not require the old name to exist in a separately resolved resource dictionary.
The caller no longer builds that dictionary solely for rewriting.

If parsing is incomplete, the original `/DA` bytes are retained. Structural errors continue through
the caller's existing `Result` channel.

### Copied appearance streams

`overlay_appearance_stream.rs` uses the same finder/replacer path for decoded stream bytes. Its
private tokenizer, numeric/operator discrimination, inline-image `EI` lookahead, operator table,
and byte-splice replacement implementation are deleted.

Resource-dictionary privatization, conflict renaming, second-order `DrMap` updates, stream decoding,
and stream re-encoding remain in `overlay_appearance_stream.rs`.

If parsing is incomplete, the decoded content is retained unchanged while the existing surrounding
resource-dictionary adjustment remains deterministic.

### Unreferenced-resource pruning

`resources.rs` uses `ResourceFinder` for lexical resource discovery in each page or Form stream.
The pruning module retains:

- inherited resource-scope resolution;
- page versus own-resources Form attribution;
- `(Form ref, scope owner)` traversal deduplication;
- recursive `Do` traversal;
- inline-image header handling required by its pruning policy;
- builtin color-space exclusions;
- decode failure and malformed-content conservative retain behavior;
- structural error propagation.

The finder supplies qpdf's name/operator classification and `Do` names. The pruning adapter decides
which discovered names belong to the current page scope and which `Do` names resolve to Form
XObjects that must be traversed. Parser diagnostics or errors make collection incomplete and retain
the affected resource group.

Form traversal follows the callback-order `Do` prefix observed before the first diagnostic, so an
earlier Form/object structural error still propagates even when later content is malformed. A
diagnostic permanently closes that prefix: later `Do` operators, operators inside an invalid inline
header, and `Do`-looking bytes inside opaque inline-image payloads are never traversed.

Inline-image header color spaces remain pruning-specific because qpdf's `ResourceFinder` does not
classify the `ID` operator as a color-space consumer. This is not a duplicate general operator
table.

## Error and lifecycle behavior

- Pipeline and callback errors keep their original `PipelineError` category and message.
- The first filter/tokenization failure is returned.
- Downstream `finish` is called only after token delivery and `handle_eof` succeed.
- Successful `handle_eof` permanently detaches filter output before downstream `finish`.
- Token callback and `handle_eof` failures retain attachment; a downstream finish failure is
  returned unchanged with output already detached.
- No finish is triggered from `Drop`.
- Finder parse diagnostics are data-quality state, not structural Rust errors.
- Overlay transforms retain original bytes when finder results are incomplete.
- Resource pruning retains instead of pruning from partial results.
- Errors resolving PDF objects, resource dictionaries, or Form streams preserve the existing
  public `flpdf::Error` behavior.

## Test strategy

### QpdfTokenizer contract tests

- multiple input chunkings produce identical token records and output;
- optional downstream forwards output or discards it;
- EOF token precedes `handle_eof`;
- every raw input byte belongs to exactly one delivered token;
- `ID` separator and inline image behavior match qpdf;
- empty input, malformed tokens, and terminal `ID` match qpdf;
- filter callback failure and downstream finish failure occur at qpdf-compatible times;
- repeated finish, write-after-finish, and downstream-finish retry/detachment are fixed by a live
  qpdf lifecycle probe.

### ResourceFinder / ResourceReplacer tests

- all ten operators map to the seven resource types;
- unrelated operators do not record resources;
- the last name survives unrelated operands and resource-operator callbacks;
- `BDC` and `DP` select the final name;
- decoded names and raw offsets include comments, whitespace, and escaped names correctly;
- repeated names record every offset;
- replacements are selected by both name and offset;
- canonical escaping is used for replacement names;
- decoded replacement bodies beginning with `/` keep that byte as escaped name content;
- replacement and non-replacement write failures preserve qpdf's offset-update ordering;
- non-selected tokens and inline-image payloads remain byte-identical;
- malformed-token and parser-diagnostic behavior is conservative.

### Consumer tests

- existing ContentNormalizer oracle records remain unchanged;
- `/DA` and appearance-stream overlay byte gates remain qpdf-identical;
- names absent from a local appearance `/Resources` dictionary still rewrite like qpdf;
- resource pruning keeps its page/Form scope, DAG, cycle, malformed-content, and inline-image
  regressions;
- tests prove each old helper and operator table has no production callsite before deletion.

### Quality gates

Run, in order:

1. focused unit and integration tests for each RED/GREEN slice;
2. `cargo fmt --all -- --check`;
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
4. `cargo test`;
5. live qpdf 11.9.0 differential probes;
6. fresh `cargo llvm-cov` and `scripts/patch-coverage.sh` with 100% changed executable lines.

## Delivery shape

The issue is one vertical cutover on one feature branch. Commits remain independently reviewable:

1. complete `TokenFilter` and `QpdfTokenizer` with oracle-backed lifecycle tests;
2. add `ResourceFinder` and `ResourceReplacer` with oracle-backed behavior tests;
3. migrate ContentNormalizer and overlay consumers, then delete their old routes;
4. migrate resource pruning's lexical classification and delete its duplicate general table;
5. update correspondence documentation and coverage exclusions only where fresh evidence requires
   them.

The branch is not complete while any declared consumer still calls an old route. No compatibility
alias or forwarding wrapper remains.
