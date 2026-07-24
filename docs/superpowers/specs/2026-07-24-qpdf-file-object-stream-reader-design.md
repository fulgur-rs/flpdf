# qpdf-style file-object stream reader design

## Problem

`flpdf --qdf` reports a spurious warning for qpdf 11.9.0's
`qpdf/qtest/qpdf/good14.pdf`:

```text
WARNING: good14.pdf: (object 7 0, offset 628): expected endobj
flpdf: operation succeeded with warnings
```

The command writes the expected QDF object bodies but exits 3. qpdf 11.9.0
writes the file without a warning and exits 0.

Object 7 has an indirect `/Length`, a non-EOL-ending payload, and an
`endstream` token adjacent to the final payload byte. flpdf's parser cannot
resolve the indirect length, so it returns an empty placeholder stream while
leaving its cursor at the payload start. The file-object parser then checks for
`endobj` at that provisional cursor and records the false warning. The reader
subsequently resolves `/Length`, finds the real `endstream` and `endobj`, and
completes the object, but it does not retract the earlier diagnostic.

This ordering differs from qpdf 11.9.0. qpdf's `QPDFParser` parses the direct
object first. `QPDF::readObject` then detects `stream`, calls
`QPDF::readStream`, resolves `/Length`, reads the payload and `endstream`, and
only then checks `endobj`.

## Goals

- Reproduce qpdf 11.9.0's file-object and stream-read ordering.
- Move file-object stream completion out of the direct-object parser.
- Use one stream completion model for normal objects, object-stream
  containers, and xref streams.
- Preserve the strict public direct-object parser contract.
- Preserve cache, encryption, object-window, recursion, and resource-limit
  safeguards.
- Make later qpdf parser parity work map to explicit, testable internal
  components rather than accumulating local warning exceptions.
- Make good14's QDF operation silent with exit 0.

## Non-goals

- Reimplement qpdf's tokenizer wholesale.
- Change content-stream tokenization or `--normalize-content`.
- Change QDF writer formatting, object numbering, stream filtering, or
  compression.
- Change object-stream member parsing. ObjStm members are direct objects and do
  not have `obj`, `stream`, or `endobj` framing.
- Remove existing denial-of-service limits in pursuit of source-level
  similarity.
- Vendor qpdf qtest fixtures into this repository.

## Chosen approach

Build a qpdf-shaped internal file-object reader bottom-up while the current
production path remains active. Add and verify syntax and stream-completion
components first, then switch consumers in layers. Delete the superseded path
only after every consumer uses the new implementation.

The new implementation lives in
`crates/flpdf/src/reader/file_object.rs`, declared as a private child module of
`reader.rs`. This keeps xref/cache/encryption ownership in `reader.rs` while
moving file-object framing and stream completion out of the already large
reader implementation.

## Component boundaries

### `parser.rs`: direct-object syntax

The parser owns PDF direct-object syntax: nulls, booleans, numbers, strings,
names, arrays, dictionaries, and indirect references nested in composite
objects.

An internal file-object entry point returns:

```rust
ParsedDirectObject {
    object: Object,
    next_offset: usize,
    empty_offset: Option<usize>,
}
```

`next_offset` is the first byte after the parsed direct object. This entry
point does not consume `stream`, stream data, `endstream`, or `endobj`.

The file-object mode retains the existing qpdf-compatible special cases:

- an empty object body followed by `endobj` becomes `Object::Null` and records
  the empty-object offset;
- a top-level bare `N G R` is parsed as integer `N`, leaving `G R` for the
  file-object layer to diagnose;
- references nested inside arrays and dictionaries remain references.

Public `parse_object`, strict indirect-object parsing, and ObjStm member parsing
retain their current behavior.

### `reader/file_object.rs`: file-object framing

The new module corresponds to qpdf's `QPDF::readObject` and
`QPDF::readStream`. It owns:

- indirect-object header framing;
- detection of `stream` after a parsed dictionary;
- stream line-ending validation;
- pending stream state;
- payload-boundary completion;
- `endstream` validation and recovery;
- final `endobj` validation;
- structured file-object diagnostics.

The syntax phase returns an explicitly incomplete value:

```rust
PendingFileObject {
    object_ref: ObjectRef,
    body: PendingBody,
    diagnostics: Vec<FileObjectDiagnostic>,
}

enum PendingBody {
    Direct {
        object: Object,
        next_offset: usize,
    },
    Stream {
        dict: Dictionary,
        data_start: usize,
        framing_eol: Option<RecoveredStreamEol>,
    },
}
```

An unresolved stream is never represented as a completed empty
`Object::Stream`. This prevents consumers from using provisional cursor or
payload state as final state.

The completion phase returns:

```rust
FileObjectRead {
    object_ref: ObjectRef,
    object: Object,
    diagnostics: Vec<FileObjectDiagnostic>,
    recovered_stream_eol: Option<RecoveredStreamEol>,
}
```

These fields are the required semantic contract. Private helper structs may
group them differently only when the public-to-the-crate result still exposes
the same information and preserves the distinction between pending and
completed stream state.

### `reader.rs`: resolution, cache, and decryption

`reader.rs` continues to own:

- xref lookup and bounded object windows;
- fallback-to-EOF budget;
- object cache and `Reserved` recursion guards;
- indirect `/Length` resolution;
- encryption state and object decryption;
- one-time registration of repair diagnostics.

It parses a pending file object, resolves `/Length` if required, and calls the
pure completion operation with the raw object bytes, resolved length, and
recovery policy. Stream payload boundaries are finalized before decryption.
The completed object is decrypted and cached only after stream and file-object
framing have been processed.

## Data flow

Normal uncompressed objects use this sequence:

```text
xref offset
  -> bounded raw object window
  -> parse indirect header
  -> parse direct object body
  -> detect optional stream token
  -> PendingFileObject
  -> resolve indirect /Length through Pdf cache
  -> finish stream payload and endstream
  -> check endobj
  -> FileObjectRead
  -> decrypt completed object
  -> cache object
  -> register diagnostics once
```

Object-stream containers use the same sequence. After the container has been
completed, decrypted, and decoded, its members continue to use the direct
object parser without file-object terminator checks.

Xref streams use the same syntax and completion operations with a bootstrap
recovery policy. Before a usable xref resolver exists, a direct integer
`/Length` is authoritative. If an indirect length cannot be resolved, the
bounded recovery algorithm is used. Xref streams do not get a separate stream
parser.

## Stream length and terminator behavior

The completion operation follows qpdf 11.9.0's order:

1. Save the payload start before resolving any indirect object.
2. Resolve `/Length`.
3. Seek logically to `payload_start + length`.
4. Require an `endstream` token at that position.
5. If length or `endstream` validation fails and recovery is enabled, run the
   bounded recovery path.
6. After stream completion, skip PDF whitespace and check `endobj`.

`endstream` and `endobj` are separate results. A valid `endstream` followed by
a missing or malformed `endobj` yields a completed stream and an
`ExpectedEndobj` warning. It is not converted into a generic stream-boundary
error.

The current combined `stream_end_boundary_at` predicate is replaced by
separate token-location operations that return the position following
`endstream` and the position at which `endobj` was expected.

## Diagnostics and recovery

Diagnostics are structured until they are registered in `Pdf`:

```rust
FileObjectDiagnostic {
    kind: FileObjectDiagnosticKind,
    relative_offset: usize,
}

enum FileObjectDiagnosticKind {
    EmptyObject,
    StreamLineEnding,
    InvalidStreamLength,
    ExpectedEndstream,
    AttemptingStreamLengthRecovery,
    RecoveredStreamLength { length: usize },
    EmptyRecoveredStream,
    ExpectedEndobj,
}
```

These diagnostic kinds are distinct semantic outcomes. An existing diagnostic
type may be reused as their storage representation, but each kind must remain
separately testable and distinct qpdf warnings must not be collapsed into one
generic parse error.

Behavioral rules:

- Empty file object: return `Null` and warn.
- Valid direct or resolved indirect `/Length`: use the exact payload boundary.
- Missing, non-integer, negative, cyclic, or otherwise unusable `/Length`:
  enter recovery when the active policy permits it.
- `endstream` mismatch at the authoritative boundary: warn and enter recovery
  when permitted.
- Recoverable `endstream` or `endobj`: return the recovered stream and matching
  warnings in qpdf order.
- Missing `endobj` after a completed stream: return the object and warn.
- Unrecoverable syntax or I/O/decryption failures: return an error.

Recovery remains bounded by the current object window and existing fallback
budget. It searches for token-bounded `endstream` or `endobj`, does not trust a
substring inside arbitrary payload bytes, and does not introduce an
unconditional scan of the rest of the file. Existing recursion guards remain
active while resolving indirect lengths.

Diagnostics are registered only when an object is first committed to the
cache. Re-resolving a cached object or reaching it through JSON inspection must
not duplicate warnings.

## Bottom-up stacked delivery

Implementation is split into four dependent Beads children and stacked PRs.
Each layer is independently green.

### Layer 1: syntax model

- Add the internal direct-object result with `next_offset`.
- Add pending file-object types and framing token helpers.
- Keep every production caller on the existing path.
- Prove existing parser behavior and new syntax results with unit tests.

### Layer 2: stream completion

- Add the pure stream/file-object completion operation.
- Add structured diagnostics and recovery policies.
- Exercise the complete direct/indirect/malformed stream matrix.
- Keep production routing unchanged.

### Layer 3: normal object switch

- Route normal uncompressed object reads through the new path.
- Preserve cache, indirect length, encryption, and diagnostic ordering.
- Add an flpdf-authored good14-shaped fixture.
- Require silent QDF exit 0 and qpdf-identical QDF object bodies.

### Layer 4: container and bootstrap switch

- Route ObjStm containers and xref streams through the new path.
- Keep ObjStm member parsing on the direct-object parser.
- Delete old stream placeholder, reslice, and combined-boundary helpers.
- Run final workspace, differential, qtest, and coverage gates.

The design document commit may be included in the bottom stack layer. Before
implementation, create and claim the four Beads children and record their
dependencies.

## Test matrix

Table-driven unit and qpdf 11.9.0 differential tests cover:

- direct and indirect `/Length`;
- LF, CRLF, CR, and malformed stream-start line endings;
- EOL-terminated and non-EOL-terminated payloads;
- payload-adjacent `endstream`;
- missing, non-integer, negative, dangling, self-referential, and cyclic
  `/Length`;
- stale lengths shorter and longer than the syntactic payload;
- missing, malformed, and misplaced `endstream`;
- missing and malformed `endobj`;
- token-like `endstream` and `endobj` byte sequences inside payloads;
- empty-object and top-level bare-reference recovery;
- encrypted streams;
- ObjStm containers and members;
- xref streams during bootstrap;
- first resolution versus cached resolution diagnostic counts.

The in-repository good14-shaped fixture is authored from scratch and contains
only the minimum graph needed to exercise an indirect length and adjacent
`endstream`. The upstream qtest fixture remains outside this repository.

## Verification gates

Every layer runs:

- `cargo fmt --all -- --check`;
- focused parser, reader, QDF, and CLI tests for its changed behavior;
- `cargo test -p flpdf`;
- `cargo test -p flpdf-cli`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`.

The final layer also runs:

- `cargo test`;
- strict workspace rustdoc with private items;
- qpdf 11.9.0 differential fixtures;
- flpdf-qtest `basic-parsing` test 41;
- final committed-HEAD patch coverage at 100%.

For byte comparison, the good14-shaped gate compares the QDF object body
region independently of already tracked generic header/trailer formatting
differences. This issue must not expand into unrelated QDF formatting changes.

## Risks and controls

- **Recovery drift:** Capture qpdf output, warning text/order, offsets, and exit
  status before implementing each malformed case.
- **Borrowing complexity:** Keep parsing/completion pure and let `Pdf` resolve
  indirect lengths between the two phases.
- **Cache recursion:** Preserve `Reserved` guards and restore unresolved cache
  entries on every error path.
- **Encryption ordering:** Complete raw stream boundaries before decryption and
  retain existing recovered-EOL inputs to the decryptor.
- **Bootstrap divergence:** Use the same completion engine with an explicit
  resolver/recovery policy rather than a second xref-stream parser.
- **Large review surface:** Keep routing changes out of the first two layers and
  delete old code only in the final layer.
