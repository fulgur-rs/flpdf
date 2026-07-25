# qpdf JSON component design

**Issue:** `flpdf-qxba.6`
**Subtasks:** `flpdf-qxba.6.1`–`flpdf-qxba.6.4`
**Date:** 2026-07-26
**Oracle:** qpdf 11.9.0 (`v11.9.0`) — `include/qpdf/JSON.hh`,
`libqpdf/JSON.cc`, `libqpdf/qpdf/JSONHandler.hh`, and
`libqpdf/JSONHandler.cc`
**Oracle path:** Resolve with `scripts/fetch-qpdf-source.sh --print-path`.

## Context

flpdf's current JSON implementation is split between:

- `crates/flpdf/src/json.rs`, which contains an owned `JsonValue` enum and a
  tree-only serializer;
- `crates/flpdf/src/json_inspect.rs`, which constructs a complete qpdf JSON v2
  tree, filters that completed tree, and has a separate Base64 encoder; and
- `crates/flpdf-cli/src/main.rs`, which writes the completed tree and walks it a
  second time to discover stream side files.

This split does not correspond to qpdf. qpdf's `JSON` component owns the value
model, parser, schema checker, blob encoding, tree writer, and incremental
writer. `QPDFJob::doJSON` opens the output dictionary before it constructs
individual sections and writes sections in order. Consequently, a fatal error
can leave an intentionally incomplete JSON prefix in the output.

For pre-v1.0, flpdf will completely port this component instead of substituting
`serde_json`. This is a public API change: the owned `JsonValue` enum will be
replaced by a shared handle named `Json`.

## Goals

1. Port every public member of qpdf 11.9.0 `JSON.hh`.
2. Port every public member of qpdf 11.9.0 `JSONHandler.hh`.
3. Preserve qpdf's encoded number tokens, shared-handle mutation, parse
   offsets, Reactor event order, schema rules, handler fallback rules, string
   escaping, and incremental formatting.
4. Make every production qpdf JSON output path use the new component.
5. Remove whole-document materialization from the CLI path so a fatal error
   preserves the bytes emitted before the error.
6. Remove the old JSON value model, old tree writer, post-build filters,
   duplicate Base64 implementation, and post-build side-file scan.

## Non-goals

- JSON v1 output is not added. qpdf JSON v2 remains the supported schema.
- `--json-input` is not wired into the CLI in this stack. The parser and
  `JsonHandler` APIs unlock that separate work.
- `serde_json` test dependencies are not removed.
- Generic qpdf Pipeline types are not introduced.
- JSON behavior after flpdf v1.0 is not decided here.

## Definition of done

The parent issue is complete only when all of the following hold:

- The public API inventory from `JSON.hh` and `JSONHandler.hh` has a Rust
  counterpart.
- Production contains one JSON value model and one JSON writer implementation.
- The CLI writes a JSON document incrementally and appends exactly one final
  LF only after a successful top-level close.
- A conversion failure leaves the already-written prefix in stdout or the
  requested output file.
- Existing output that already matches qpdf remains byte-identical.
- Any changed bytes are locked to observed qpdf 11.9.0 output.
- Every stacked PR has 100% changed-line coverage against its own base.
- Formatting, workspace clippy, focused tests, workspace tests, and the strict
  private-item rustdoc link check pass.

## D2 inventory

The production definitions and consumers that must be migrated or removed are:

| Responsibility | Current location |
|---|---|
| Owned `JsonValue` model | `crates/flpdf/src/json.rs` |
| Tree serializer and escaping | `crates/flpdf/src/json.rs` |
| PDF-object conversion | `crates/flpdf/src/json_inspect.rs::pdf_object_to_json` |
| Section builders | `crates/flpdf/src/json_inspect.rs::build_*_section` |
| Whole-document builders | `build_qpdf_json_v2*` in `json_inspect.rs` |
| Completed-tree key filters | `filter_json_keys`, `filter_json_objects` |
| Duplicate Base64 encoder | `json_inspect.rs::base64_encode` |
| CLI tree write | `flpdf::json::write` calls in `flpdf-cli/src/main.rs` |
| CLI completed-tree scan | `collect_datafile_object_refs` |

The existing public section builders may continue to return a JSON value, but
their return type changes from `JsonValue` to `Json`. The whole-document
builders change from returning a completed value to writing into a sink.

## Module structure

`crates/flpdf/src/json.rs` becomes the `crates/flpdf/src/json/` module:

| File | Responsibility |
|---|---|
| `json/mod.rs` | Public API, re-exports, qpdf source correspondence, permitted Pipeline substitutions |
| `json/value.rs` | Shared `Json` handle, private value kinds, constructors, mutation, accessors, iteration, offsets |
| `json/writer.rs` | String encoding, tree output, unparse, blob output, incremental writer methods |
| `json/parser.rs` | Tokenization, recursive parse state, duplicate-key detection, offsets, Reactor integration |
| `json/schema.rs` | qpdf template-schema validation and error collection |
| `json/handler.rs` | Handler registration, recursive dispatch, fallback, path construction, usage errors |
| `json/legacy.rs` | Temporary old `JsonValue` implementation, present only in the first three stacked layers |

The final layer deletes `json/legacy.rs` and its re-exports. Temporary
duplication is therefore explicit, bounded to the stack, and mechanically
removed before the parent issue can close.

## Shared value model

`Json` is a cloneable optional `Rc<RefCell<Members>>` handle. `Members` stores
a private value kind plus inclusive start and non-inclusive end offsets.
Cloning an initialized `Json` clones the handle, not the value. Mutating a
dictionary or array through one clone is visible through every clone, matching
qpdf's `std::shared_ptr<Members>`. A default-constructed value has no handle,
matching qpdf's null `m`.

The private value kinds are:

- dictionary backed by `BTreeMap<Vec<u8>, Json>` plus a separate set of
  decoded keys seen by the parser;
- array backed by `Vec<Json>`;
- string with both original bytes and pre-encoded output bytes;
- number stored as an encoded `String`;
- boolean;
- null; and
- blob backed by a shared callback that writes raw bytes.

An uninitialized `Json::default()` writes and unparses as JSON null, but it is
not an initialized null value: `is_null` is false and typed accessors return no
value. Adding an uninitialized value to a dictionary or array stores an
initialized null value. Offset setters are no-ops and offset getters return
zero. These distinctions are locked by the upstream default-construction
tests.

Dictionary ordering is lexical over encoded key bytes because qpdf stores
members in `std::map` after applying `Writer::encode_string` in
`addDictionaryMember`. Iteration exposes those encoded keys, while duplicate
detection uses the decoded parser key in a separate set. Fixed qpdf JSON v2
section order is not represented by dictionary insertion order; it is
controlled by the incremental writer in the integration layer.

JSON strings and dictionary keys use byte vectors internally. This preserves
qpdf's `std::string` behavior for non-ASCII and non-UTF-8 bytes. Public
constructors accept byte slices, with string conveniences for ordinary UTF-8
callers. String accessors and Reactor dictionary-key callbacks expose bytes
without lossy conversion.

## Public `Json` API

Rust names use snake case while preserving qpdf's responsibility boundaries.
The public surface includes:

- `LATEST`;
- `unparse` and `write`;
- `write_dictionary_open`, `write_dictionary_close`, `write_array_open`,
  `write_array_close`, `write_dictionary_item`, `write_dictionary_key`,
  `write_array_item`, and `write_next`;
- `make_dictionary`, `add_dictionary_member`, `make_array`,
  `add_array_element`, `make_string`, `make_int`, `make_real`, `make_number`,
  `make_bool`, `make_null`, and `make_blob`;
- `is_array`, `is_dictionary`, `check_dictionary_key_seen`, `get_string`,
  `get_number`, `get_bool`, `is_null`, `get_dict_item`,
  `for_each_dict_item`, and `for_each_array_item`;
- `check_schema` with no flags and `check_schema_with_flags`;
- `parse` and `parse_reader`; and
- `set_start`, `set_end`, `start`, and `end`.

`SchemaFlags` is a small bitmask type with `NONE` and `OPTIONAL`, matching
qpdf's `f_none` and `f_optional` without adding a bitflags dependency.

`Json::write` and `Json::unparse` do not add a trailing LF. Document-producing
callers own that delimiter, matching `QPDFJob::doJSON`.

## Writer behavior

The writer ports `JSON.cc:25-273` and retains these byte rules:

- two spaces per indentation level;
- empty containers close on the same line;
- non-empty containers close on a new line at the parent depth;
- `write_next` emits either `LF + indentation` or `comma + LF + indentation`;
- strings are escaped byte by byte;
- control escapes use qpdf's lowercase hexadecimal form;
- `make_number` writes its encoded string unchanged;
- `make_real` follows qpdf's six-digit double formatting behavior; and
- blobs are quoted standard-alphabet Base64 without inserted newlines.

Dictionary member keys are encoded when inserted and written verbatim by the
incremental dictionary-key method, matching qpdf's division of
responsibility. Callers of the incremental method supply an already-safe JSON
key, as qpdf's `QPDFJob` does.

The permitted internal substitutions are:

- `Pl_Base64` becomes the `base64` crate's `STANDARD` engine; and
- `Pl_Concatenate` and `Pl_String` become `Write` and `Vec<u8>`.

These substitutions do not change output bytes and are documented at the top
of `json/mod.rs`.

## Parser and Reactor

The parser ports the state machine from `JSON.cc` rather than delegating to
`serde_json`. It accepts a byte slice or a byte reader and returns a `Json`; a
UTF-8 string convenience delegates without conversion.

The parser:

- retains the exact number token for `make_number`;
- rejects the same malformed strings, numbers, delimiters, and trailing data
  as qpdf;
- records each value's inclusive start and non-inclusive end offsets;
- detects duplicate dictionary keys even when a Reactor consumes the earlier
  item; and
- builds nested containers with the same event order as qpdf.

The public `Reactor` trait contains dictionary start, array start, container
end, top-level scalar, dictionary item, and array item callbacks. For a child
container, the parent item callback receives the initially empty child before
the child's start callback. Returning `true` from an item callback consumes
the item and prevents storage in the parent tree.

## Schema validation

`check_schema` ports `JSON.cc:450-581`. The schema is qpdf's template format,
not JSON Schema:

- a schema dictionary constrains allowed keys and required keys;
- a single schema key wrapped in angle brackets is a pattern key whose value
  validates every key in the checked dictionary;
- the optional flag allows missing schema keys but never unknown object keys;
- a one-element schema array accepts one value or an arbitrary-length array;
- a longer schema array requires the same length and validates positionally;
- a schema string describes a value of any JSON type; and
- invalid schema shapes produce qpdf-compatible collected errors.

Validation returns a boolean and appends human-readable errors to a caller
owned list, preserving qpdf's non-throwing validation contract.

## `JsonHandler`

`JsonHandler` ports the full API and dispatch order from `JSONHandler.cc`.
Callbacks use `FnMut`; nested handlers use shared, interior-mutable handles so
the recursive configuration graph has the same sharing behavior as qpdf.

The public methods are `add_any_handler`, `add_null_handler`,
`add_string_handler`, `add_number_handler`, `add_bool_handler`,
`add_dictionary_handlers`, `add_dictionary_key_handler`,
`add_fallback_dictionary_handler`, `add_array_handlers`,
`add_fallback_handler`, and `handle`.

Dispatch follows this order:

1. an any-value handler, if registered;
2. the first registered matching scalar handler, followed by immediate return;
3. a registered dictionary-start handler, then each exact dictionary-key
   handler or the unknown-key fallback, then the dictionary-end handler;
4. a registered array-start handler, then the item handler for each element,
   then the array-end handler;
5. a general fallback handler; and
6. a usage error when no handler accepts the value or a handled dictionary has
   an unexpected key without an unknown-key fallback.

Paths and type mismatch messages match qpdf. Rust reports usage exceptions as
a typed `JsonHandlerError`.

## Incremental qpdf JSON v2 integration

The CLI opens stdout or the requested output file before building the JSON
document and calls a sink-oriented function:

```rust
write_qpdf_json_v2_selected_objects_with_options(
    pdf,
    decode_level,
    stream_mode,
    keys,
    objects,
    out,
) -> Result<JsonOutputSummary, JsonOutputError>
```

The function performs:

1. top-level dictionary open;
2. `version` and `parameters`;
3. `pages` and `pagelabels`, preserving their page-tree repair side effects;
4. `acroform`, `attachments`, `encrypt`, and `outlines`;
5. the qpdf metadata and raw-object map;
6. top-level dictionary close; and
7. one final LF.

Selectors are applied before a section or raw object is constructed.
`filter_json_keys` and `filter_json_objects` are deleted.

Bounded sections may be constructed as individual `Json` values before their
dictionary item is emitted. The whole document is never retained. The raw
object map is emitted one object at a time because it can grow with the input
file.

`JsonOutputSummary` records the object references whose emitted stream entries
contain `datafile`. The CLI writes those side files only after the JSON body
finishes successfully. This removes the completed-tree scan and preserves the
current rule that failed JSON output creates no side files.

## Error and partial-output behavior

The component keeps three error domains distinct:

- `JsonError` for parsing and JSON value errors;
- `ConvertError` for PDF-to-JSON conversion; and
- `io::Error` for sink failures.

The sink-oriented integration error retains whether the source was conversion
or I/O. Existing CLI warning emission and exit-code behavior remains in
control of `main.rs`.

The top-level dictionary is opened before any section construction. If a
selected section fails, previously emitted bytes remain in the sink and the
top-level dictionary is not closed. The CLI emits accumulated warnings and
the fatal diagnostic without truncating or replacing that prefix. Output-file
creation follows the same rule as stdout.

## Stacked delivery

### `flpdf-qxba.6.1` — core and writer

- Move the module root to `json/mod.rs`.
- Add the shared model and writer.
- Keep the old model in `json/legacy.rs`.
- Port the value, writer, shared-mutation, number, string, and blob tests from
  `libtests/json.cc`.

### `flpdf-qxba.6.2` — parser and Reactor

- Add parser state and public parse APIs.
- Port `libtests/json_parse.cc`, including offsets and Reactor event order.
- Depend on `.6.1`.

### `flpdf-qxba.6.3` — schema and handler

- Add schema checking and `JsonHandler`.
- Port the schema cases from `libtests/json.cc`.
- Port `libtests/json_handler.cc`.
- Depend on `.6.2`.

### `flpdf-qxba.6.4` — production cutover

- Change JSON inspection builders to return `Json`.
- Replace whole-document builders with sink-oriented writers.
- Apply selectors during construction.
- Collect side-file references during raw-object emission.
- Open the CLI sink before document construction.
- Delete `legacy.rs`, the old Base64 encoder, post-build filters, completed
  tree scan, and old whole-document APIs.
- Add CLI byte and partial-output tests.
- Depend on `.6.3`.

## Testing and gates

Each layer follows red-green-refactor and first ports the relevant upstream
test so the missing behavior fails before production code is added.

Focused suites:

- `crates/flpdf/tests/json_tests.rs`;
- `crates/flpdf/tests/json_parse_tests.rs`;
- `crates/flpdf/tests/json_handler_tests.rs`;
- existing `crates/flpdf-cli/tests/cli_json.rs`;
- existing `crates/flpdf-cli/tests/json_schema_diff.rs`; and
- the existing fatal outline JSON tests.

Each stacked branch runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_tests
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_json
cargo test -p flpdf-cli --test json_schema_diff
cargo test
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Changed-line coverage is measured from the committed branch head against that
PR's parent branch, not against the bottom of the entire stack. The final
layer also records qtest before and after counts as a result metric.

## Rejected approaches

### Immediate API-first cutover

Changing every `json_inspect.rs` and CLI consumer in the first layer would
combine a public model rewrite with the largest integration edit. That makes
the core writer difficult to review independently.

### Section-by-section CLI cutover

Routing different sections through old and new writers would require
qpdf-independent selection branches and make byte ordering depend on
migration state.

### Permanent compatibility model

Keeping `JsonValue` as a second public tree and converting between it and
`Json` would violate the single-implementation requirement and retain the
whole-document architecture that prevents partial output.
