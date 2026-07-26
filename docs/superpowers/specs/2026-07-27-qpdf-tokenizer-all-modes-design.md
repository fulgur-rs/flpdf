# qpdf 11.9.0 tokenizer all-modes design

**Issue:** `flpdf-n9t0.1`

**Date:** 2026-07-27

**Oracle:** qpdf 11.9.0 (`v11.9.0`)

**Source:** `scripts/fetch-qpdf-source.sh --print-path`

## Goal

Replace flpdf's partial pull-only tokenizer and its independent content-stream
lexer with one component that mirrors qpdf 11.9.0's
`QPDFTokenizer.hh`/`QPDFTokenizer.cc`.

Completion means:

- all 18 qpdf token types and every tokenizer operation mode are implemented;
- push and pull tokenization use the same state machine;
- object parsing, content parsing, and the future token-filter pipeline consume
  that state machine rather than scanning bytes independently;
- content parsing follows qpdf's object-event and callback model;
- `content_stream.rs` no longer owns a second PDF lexer;
- qpdf source, live differential probes, Rust tests, and the external qtest
  harness agree on the observable behavior.

Pre-v1.0 qpdf parity is more important than preserving flpdf's current public
`ContentStreamParser`/`ContentToken` API. Those APIs may be removed.

## Scope

### In scope

- `QPDFTokenizer`'s token model, state machine, push mode, pull mode, EOF
  policy, ignorable tokens, bad-token recovery, raw values, maximum token
  length, `betweenTokens`, and inline-image mode.
- The `QPDFParser` content-stream mode needed to consume the completed
  tokenizer without recreating lexical rules.
- qpdf's `ParserCallbacks`-shaped content event flow.
- `Operator` and `InlineImage` object values required by content parsing.
- Migration of all current `ContentStreamParser`/`ContentToken` consumers.
- Removal of the independent lexer in `content_stream.rs`.

### Out of scope

- `Pl_QPDFTokenizer` and `ContentNormalizer`; these belong to
  `flpdf-qxba.7`, which depends on this issue.
- General `QPDFObjectHandle` parity beyond the two content-only object values.
- A general qpdf `InputSource` port. flpdf's tokenizer only needs a seekable
  byte cursor with current and last-token offsets.
- Importing qpdf qtest fixtures into this repository.

The existing `normalize_content_stream` implementation may remain temporarily
as an adapter over the new shared tokenizer/content-event path so the CLI
continues to build. It must not retain lexical logic. `flpdf-qxba.7` will
replace its non-qpdf normalization policy with `ContentNormalizer`.

## Oracle correspondence

| qpdf 11.9.0 | flpdf target |
|---|---|
| `include/qpdf/QPDFTokenizer.hh` | `crates/flpdf/src/tokenizer.rs` public shape within the crate |
| `libqpdf/QPDFTokenizer.cc` | `crates/flpdf/src/tokenizer.rs` state and behavior |
| `libqpdf/QPDFParser.cc` content-stream branch | `crates/flpdf/src/parser.rs` content mode |
| `QPDFObjectHandle.cc:1770-1847` | `content_stream.rs` content-event orchestration and callback lifecycle |
| `QPDF_Operator.cc` | `Object::Operator` |
| `QPDF_InlineImage.cc` | `Object::InlineImage` |
| `QPDFObjectHandle::ParserCallbacks` | Rust callback trait/control flow |
| `Pl_QPDFTokenizer.cc` | follow-up `flpdf-qxba.7`, not this issue |

`Tokenizer` and `Token` remain `pub(crate)`. This is an explicit Rust API
visibility decision; it does not permit omitting any qpdf token type, state, or
operation mode.

`tokenizer.rs` starts with the D4 correspondence line:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/QPDFTokenizer.cc.
```

Every non-obvious recovery branch cites its corresponding qpdf source line.
There are no flpdf-only lexical policy branches.

## Architecture

qpdf has two consumers of the same tokenizer:

```text
bytes + shared cursor
        |
        v
Tokenizer
   |------------------------------|
   v                              v
Parser(content_stream = true)     raw Token stream
   |                              |
   v                              v
Object events                  Pl_QPDFTokenizer
   |                              |
   v                              v
ParserCallbacks                ContentNormalizer
```

This issue completes the left side and the shared tokenizer. The right side is
the next issue.

flpdf must not retain a third route that recognizes whitespace, comments,
names, strings, numbers, or inline-image boundaries in `content_stream.rs`.

## Token model

`TokenType` contains the qpdf set in the same semantic categories:

- `Bad`
- `ArrayClose`, `ArrayOpen`
- `BraceClose`, `BraceOpen`
- `DictClose`, `DictOpen`
- `Integer`, `Name`, `Real`, `String`
- `Null`, `Bool`, `Word`
- `Eof`, `Space`, `Comment`, `InlineImage`

`Token` owns its byte values. An owned representation lets push and pull mode
return the same type and matches qpdf's `std::string` ownership.

Each token carries:

- type;
- canonical value;
- raw input value;
- optional error message as owned raw bytes, matching qpdf's `std::string`;
- pull-mode start and end offsets.

Tokenizer error bytes are converted with a lossy UTF-8 display policy only at
the human-facing `Error::parse` / parser-diagnostic boundary. Keeping the token
contract byte-preserving is required for high-bit invalid input; qpdf inserts
the offending byte directly into its error `std::string`
(`QPDFTokenizer.cc:640-680`).

For `Name` and `String`, canonical value and raw value are distinct:

- a name's canonical value includes the leading `/` and decodes `#xx`;
- a string's canonical value excludes delimiters and resolves escapes;
- raw value preserves the exact input representation.

For other token types, canonical and raw values are identical.

Constructing a new `Name` or `String` token from a canonical value creates a
valid canonical PDF raw representation. This is required by the next
`ContentNormalizer` layer.

Token equality follows qpdf:

- compare only type and canonical value;
- a `Bad` token is never equal, including to another `Bad` token;
- raw value, error text, and offsets do not participate.

The qpdf convenience predicates (`isInteger`, `isWord`, and word-value
comparison) have Rust equivalents.

## Tokenizer state and modes

The Rust state machine corresponds to qpdf's states rather than keeping the
current function-per-token pull scanner:

- before token / top / token ready;
- space and comment;
- literal string, escape, octal character, and post-CR;
- name and the two name-hex states;
- `<`, `>`, hex string, and second hex nibble;
- sign, decimal, integer, real, and generic literal;
- inline image.

The pull configuration defaults match qpdf:

- EOF is not allowed;
- ignorable tokens are not included.

`allow_eof` changes pull EOF from `Bad("unexpected EOF")` to `Eof`.
It does not affect direct push mode: qpdf's `presentEOF` always produces
`Eof`, while the policy check exists only in pull `nextToken`
(`QPDFTokenizer.cc:723-762,933-939`).
`include_ignorable` returns contiguous PDF whitespace as `Space` and comments
without their terminating line ending as `Comment`.

### Push mode

Push mode provides the semantic equivalents of:

- `presentCharacter`;
- `presentEOF`;
- `getToken`;
- `betweenTokens`.

`get_token` returns a ready token plus qpdf's unread-character indication.
`between_tokens` reports qpdf's `before_token` state, including whitespace or
comment input that is outside a token.

Presenting input while a token is waiting, or requesting inline-image mode
from an invalid state, is a tokenizer logic-state error rather than a damaged
PDF parse error.

### Pull mode

Pull mode feeds the same state machine one byte at a time from a shared cursor.
It records the first non-ignorable byte as the token start, performs qpdf's
single-character unread, and records the cursor's last-token offset.

The pull operation accepts qpdf's two per-read policies:

- `allow_bad`: return a `Bad` token instead of a parse error;
- `max_len`: if nonzero, stop an unfinished token when its raw length reaches
  the limit and report
  `exceeded allowable length while reading token`.

The object parser may use the lower-level result without allocating a second
token copy, but this optimization must not create a second lexical path.

## EOF and recovery

EOF behavior follows the active state:

- appendable name, number, sign, decimal, real, or literal states are completed
  by presenting qpdf's synthetic delimiter;
- top/before-token becomes `Eof` in push mode or in pull mode when allowed;
- disallowed pull EOF becomes `Bad("unexpected EOF")`;
- ignorable whitespace becomes `Space` when requested, otherwise EOF;
- a final comment becomes `Comment` when ignorable tokens are requested and
  `Bad` otherwise;
- unfinished strings, hex strings, and inline images become
  `Bad("EOF while reading token")`.

Name recovery, string escapes, odd hex nibbles, unexpected delimiters, and bad
number-to-word transitions remain source-compatible with qpdf 11.9.0. Parser
policy decides whether a returned `Bad` token becomes a warning/null recovery
or a hard error.

## Inline-image mode

The caller enters inline-image mode only after it has emitted the `ID`
operator and consumed exactly one following byte, as qpdf does in
`QPDFObjectHandle.cc:1820-1826` and `Pl_QPDFTokenizer.cc:49-57`.

`expect_inline_image` performs qpdf's `findEI` behavior:

1. Find an `EI` candidate at a nonzero absolute offset with a delimiter or EOF
   after it. Although the qpdf source comment says “preceded by a delimiter,”
   qpdf 11.9.0 does not inspect the preceding byte and accepts `EI` embedded
   after other bytes when its following boundary is valid
   (`QPDFTokenizer.cc:45-72`, confirmed by the live probe).
2. Tokenize up to ten tokens following the candidate.
3. Reject the candidate if that lookahead encounters a bad token.
4. For word tokens, reject candidates whose lookahead contains non-printable
   non-space bytes, or mixes alphabetic/`*` characters with other characters.
5. Continue searching after a rejected candidate from the cursor reached by
   the lookahead tokenizer, not immediately after the candidate. This prevents
   an `EI` embedded inside the rejected token from being reconsidered
   (`QPDFTokenizer.cc:799-855`).
6. Use the first candidate that passes; if the scan ends after candidates were
   seen, retain qpdf's last-candidate fallback behavior.
7. Restore the shared cursor to the beginning of image data.
8. Feed the precomputed byte count through the normal state machine and emit
   one `InlineImage` token that excludes the terminating `EI`.

If no candidate is usable, scanning reaches EOF and produces a bad token.

This replaces flpdf's current first-boundary match. Inline image bytes may
contain arbitrary binary data and false `EI` sequences.

## Content object model

Add:

```rust
Object::Operator(Vec<u8>)
Object::InlineImage(Vec<u8>)
```

They mirror qpdf's `QPDF_Operator` and `QPDF_InlineImage`:

- clone/copy preserves the stored bytes;
- PDF unparse writes those bytes verbatim;
- JSON representation is `null`;
- reference traversal treats both as terminal scalar values;
- content parsing may produce them, but file-object parsing does not.

No `Reserved` object work is included in this issue.

## Parser content mode

`Parser` receives the same tokenizer and cursor used by the content
orchestrator. In content mode:

- EOF returns no object rather than a PDF null;
- a word becomes `Object::Operator`;
- integers are never combined into indirect references;
- arrays and dictionaries use the normal object parser;
- token diagnostics use qpdf's bad-token recovery policy and offsets.

This is a parser mode, not a second content lexer.

## Content callbacks

Replace `ContentStreamParser`/`ContentToken` with a callback flow corresponding
to `QPDFObjectHandle::parseContentStream_data` and `ParserCallbacks`.

The callback surface supports:

- `content_size(size)` before the first object;
- `handle_object(object, offset, length)` for every parsed object;
- `handle_eof()` after natural completion;
- immediate early termination.

Early termination returns without calling `handle_eof`, matching qpdf's
`TerminateParsing` catch path.

For each object, the orchestrator:

1. reads ahead to locate the next non-ignorable token;
2. seeks back to that token start;
3. asks `Parser` for one content object;
4. reports its byte offset and consumed length;
5. after reporting `Operator("ID")`, consumes one separator byte, enters
   inline-image mode, and reports the resulting `InlineImage` object as a
   separate event.

The callback sequence exposes `BI`, its dictionary entries, `ID`, and the image
payload as qpdf object events. It does not reconstruct flpdf's former
`ContentToken::InlineImage { dict, data }` aggregate.

Consumers that need operands accumulate object events until an operator in
their own callback, as qpdf consumers do. A shared non-lexical accumulator is
allowed, but it must consume object events and must not inspect source bytes.

## Consumer migration and deletion

All production and test consumers of `ContentStreamParser`/`ContentToken` must
move to callbacks or an event-only adapter:

- resource discovery and pruning;
- default appearance parsing;
- appearance generation/inspection;
- page-object helper content inspection;
- CLI optimization and normalization tests;
- coalesce and content-stream tests.

After migration:

- remove the `ContentToken` public enum;
- remove the `ContentStreamParser` iterator;
- remove `skip_ws_collect_comment`, `read_keyword`, `at_operand_start`,
  the independent inline-image scanner, and related byte-position state;
- remove their re-exports from `lib.rs`;
- verify by search that only `tokenizer.rs` recognizes lexical token
  boundaries.

`normalize_content_stream` may use a callback accumulator temporarily, but
must not scan or tokenize bytes itself.

## Delivery stack

Deliver three dependency-ordered PRs. Each PR has its own tests and patch
coverage gate.

### Layer 1: tokenizer core

- token model and custom equality;
- qpdf state machine for all non-inline-image states;
- push and pull modes;
- EOF, ignorable, raw values, bad-token policy, max length, offsets;
- existing object-parser consumers cut over to the new pull path.

Layer 1 must not add an adapter that preserves the old pull scanner.

### Layer 2: inline image

- `expect_inline_image`;
- `findEI` candidate validation and cursor restoration;
- inline-image state and token;
- false-candidate, lookahead, delimiter, binary, and EOF tests.

After Layer 2, `tokenizer.rs` has full `QPDFTokenizer` mode coverage.

### Layer 3: content cutover

- `Object::Operator` and `Object::InlineImage`;
- parser content mode;
- callback orchestration and early termination;
- all consumer migrations;
- old `ContentStreamParser`/`ContentToken` and lexer deletion;
- correspondence documentation update.

`flpdf-qxba.7` branches from Layer 3.

## Verification strategy

### TDD

Every behavior change starts with a focused failing test. Tests must fail
because the qpdf behavior is absent, not because the test harness is broken.
Implementation follows red/green/refactor within each layer.

### Checked-in Rust tests

Normal Rust tests do not require qpdf. They use flpdf-authored byte sequences
whose expected token types, canonical values, raw values, errors, unread
characters, and offsets were confirmed against qpdf 11.9.0.

Coverage includes:

- every token type;
- every state transition family;
- push/pull equivalence;
- both EOF policies;
- ignorable space/comment grouping;
- name and string canonical/raw divergence;
- bad-token return versus error;
- maximum length at, below, and above the boundary;
- `between_tokens` and unread behavior;
- all inline-image candidate decisions;
- content callback offsets, lengths, EOF, and early termination;
- `Operator`/`InlineImage` unparse and JSON behavior;
- all migrated production consumers.

### Live differential oracle

Provide an ignored live differential test or repository script that:

1. resolves the read-only pinned qpdf 11.9.0 source;
2. builds or uses a tokenizer probe outside the repository worktree;
3. feeds identical flpdf-authored inputs to qpdf and flpdf;
4. compares type, canonical value, raw value, error, pull offsets, and push
   unread decisions.

The probe and its inputs must not copy qpdf qtest fixtures into flpdf.

### External qtest

Run the separate `/home/ubuntu/flpdf-qtest` harness before Layer 1 and after
Layer 3 for:

- `tokenizer`;
- `token-filters`;
- `basic-parsing`;
- `inline-images`.

Record the exact before/after subtest states. Pass count is a completion metric,
not a reason to change layer order or add unrelated fixes.

### Per-layer quality gates

For each layer:

```text
cargo fmt --all -- --check
focused crate/integration tests
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links \
              -D rustdoc::private_intra_doc_links \
              -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
scripts/patch-coverage.sh --base <parent-branch> --lcov <layer-report>
```

Patch coverage is measured against each PR's own parent and must be 100%.
Existing qpdf byte baselines must remain unchanged unless a live qpdf 11.9.0
oracle demonstrates that the baseline encoded the old flpdf-only behavior.

## Completion criteria

`flpdf-n9t0.1` closes only when:

- the qpdf header/API and operation-mode correspondence table has no gaps;
- `tokenizer.rs` has the D4 module correspondence line and source-backed
  recovery branches;
- all 18 token types are reachable and tested;
- push/pull, EOF, ignorable, max-length, recovery, raw-value,
  `betweenTokens`, and inline-image behavior match qpdf 11.9.0;
- content parsing produces qpdf-shaped object events and callback lifecycle;
- no production byte lexer remains in `content_stream.rs`;
- all former content parser consumers use the shared tokenizer path;
- existing qpdf byte baselines pass or are changed only with recorded qpdf
  11.9.0 oracle evidence;
- live differential evidence is recorded;
- relevant qtest before/after evidence is recorded;
- all three PRs pass their independent quality and 100% patch-coverage gates.
