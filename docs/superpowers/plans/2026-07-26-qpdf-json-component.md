# qpdf JSON Component Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's owned, whole-document JSON path with a complete qpdf 11.9.0-compatible shared JSON component and incremental CLI output.

**Architecture:** Build the component in four dependent branches: shared values and writers, parser and Reactor, schema and handler, then production cutover. The first three branches retain the current `JsonValue` as an explicitly temporary legacy module; the fourth migrates every consumer and deletes it.

**Tech Stack:** Rust 1.87, `std::rc::Rc`, `std::cell::RefCell`, `std::io::{Read, Write}`, `BTreeMap`, `BTreeSet`, `base64` 0.22, Cargo workspace tests, qpdf 11.9.0 oracle.

## Global Constraints

- Oracle source is the read-only tree returned by `scripts/fetch-qpdf-source.sh --print-path`.
- The source contract is qpdf 11.9.0 `JSON.hh`, `JSON.cc`, `JSONHandler.hh`, and `JSONHandler.cc`.
- pre-v1.0 uses qpdf-complete behavior; do not substitute `serde_json`.
- Preserve encoded number tokens, shared-handle mutation, parse offsets, Reactor event order, schema errors, handler paths, string bytes, and output bytes.
- The only approved substitutions are `Pl_Base64` to `base64::engine::general_purpose::STANDARD`, and `Pl_Concatenate`/`Pl_String` to `Write`/`Vec<u8>`.
- Each production behavior starts with a failing test and follows red-green-refactor.
- Every stacked PR measures changed-line coverage against its immediate parent and must reach 100%.
- Do not close `flpdf-qxba.6` until `.6.4` deletes every legacy production implementation.

## Stack and branch bases

| Bead | Branch | Base |
|---|---|---|
| `flpdf-qxba.6.1` | `feature/flpdf-qxba-6-1-json-core` | `main` |
| `flpdf-qxba.6.2` | `feature/flpdf-qxba-6-2-json-parser` | `.6.1` branch |
| `flpdf-qxba.6.3` | `feature/flpdf-qxba-6-3-json-validation` | `.6.2` branch |
| `flpdf-qxba.6.4` | `feature/flpdf-qxba-6-4-json-integration` | `.6.3` branch |

---

### Task 1: Create the shared handle and scalar values (`flpdf-qxba.6.1`)

**Files:**
- Move: `crates/flpdf/src/json.rs` → `crates/flpdf/src/json/legacy.rs`
- Create: `crates/flpdf/src/json/mod.rs`
- Create: `crates/flpdf/src/json/value.rs`
- Test: `crates/flpdf/tests/json_tests.rs`

**Interfaces:**
- Consumes: `std::io::{self, Write}` and the approved `base64` dependency.
- Produces: `Json`, `JsonError`, scalar constructors, typed accessors, and offset accessors used by every later task.

- [ ] **Step 1: Claim the bottom bead and confirm the branch**

```sh
bd update flpdf-qxba.6.1 --claim
git branch --show-current
git status --short --branch
bash /home/ubuntu/flpdf-qtest/scripts/run.sh 2>&1 | tee /tmp/flpdf-qxba-6-qtest-before.log
```

Expected branch: `feature/flpdf-qxba-6-1-json-core`. Expected worktree: clean.
Record the baseline pass count in `flpdf-qxba.6` without using it to change
scope or priority.

- [ ] **Step 2: Write failing tests for default and scalar behavior**

Create `crates/flpdf/tests/json_tests.rs`:

```rust
use flpdf::json::Json;

#[test]
fn default_handle_writes_null_but_is_not_initialized_null() {
    let value = Json::default();
    assert_eq!(value.unparse().unwrap(), b"null");
    assert!(!value.is_null());
    assert_eq!(value.start(), 0);
    assert_eq!(value.end(), 0);
}

#[test]
fn encoded_number_is_not_normalized() {
    let value = Json::make_number(b"2.1e5");
    assert_eq!(value.get_number().as_deref(), Some(b"2.1e5".as_slice()));
    assert_eq!(value.unparse().unwrap(), b"2.1e5");
}

#[test]
fn scalar_accessors_reject_other_types_without_mutating_output() {
    let value = Json::make_bool(true);
    assert_eq!(value.get_bool(), Some(true));
    assert_eq!(value.get_string(), None);
    assert_eq!(value.get_number(), None);
}
```

- [ ] **Step 3: Run the new test and verify RED**

```sh
cargo test -p flpdf --test json_tests
```

Expected: compile failure because `flpdf::json::Json` does not exist.

- [ ] **Step 4: Move the legacy module without changing its behavior**

Move the current file with `git mv`, add `pub use legacy::{write, JsonValue};`
to `json/mod.rs`, and keep `pub mod json;` in `lib.rs` unchanged:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/JSON.cc and libqpdf/JSONHandler.cc.
//! Public APIs: qpdf 11.9.0 include/qpdf/JSON.hh and libqpdf/qpdf/JSONHandler.hh.
//!
//! qpdf Pipeline substitutions: `Pl_Base64` is the standard `base64` engine;
//! `Pl_Concatenate` and `Pl_String` are `Write` and `Vec<u8>`.

mod legacy;
mod value;

pub use legacy::{write, JsonValue};
pub use value::{Json, JsonError};
```

- [ ] **Step 5: Implement the scalar handle**

Add the concrete core to `json/value.rs`:

```rust
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::rc::Rc;

#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    #[error("{0}")]
    Type(String),
    #[error("{0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Default)]
pub struct Json(Option<Rc<RefCell<Members>>>);

impl std::fmt::Debug for Json {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Json")
            .field(&self.0.as_ref().map(|_| "<initialized>"))
            .finish()
    }
}

pub(crate) struct Members {
    pub(crate) value: Value,
    pub(crate) start: i64,
    pub(crate) end: i64,
}

pub(crate) enum Value {
    Dictionary {
        members: BTreeMap<Vec<u8>, Json>,
        parsed_keys: BTreeSet<Vec<u8>>,
    },
    Array(Vec<Json>),
    String { original: Vec<u8>, encoded: Vec<u8> },
    Number(Vec<u8>),
    Bool(bool),
    Null,
    Blob(Rc<RefCell<Box<dyn FnMut(&mut dyn io::Write) -> io::Result<()>>>>),
}

pub(crate) enum ValueSnapshot {
    Dictionary(Vec<(Vec<u8>, Json)>),
    Array(Vec<Json>),
    String(Vec<u8>),
    Number(Vec<u8>),
    Bool(bool),
    Null,
    Blob(Rc<RefCell<Box<dyn FnMut(&mut dyn io::Write) -> io::Result<()>>>>),
}
```

Implement `make_string`, `make_int`, `make_real`, `make_number`, `make_bool`,
`make_null`, `get_string`, `get_number`, `get_bool`, `is_null`, `set_start`,
`set_end`, `start`, and `end`. `make_real` uses qpdf's
`QUtil::double_to_string(value, 6)` rule: six fractional digits, trim trailing
zeros and the trailing point, and preserve a single `0` for zero.

Use this public surface:

```rust
impl Json {
    pub const LATEST: i32 = 2;
    pub fn make_string(value: impl AsRef<[u8]>) -> Self;
    pub fn make_int(value: i64) -> Self;
    pub fn make_real(value: f64) -> Self;
    pub fn make_number(encoded: impl AsRef<[u8]>) -> Self;
    pub fn make_bool(value: bool) -> Self;
    pub fn make_null() -> Self;
    pub fn get_string(&self) -> Option<Vec<u8>>;
    pub fn get_number(&self) -> Option<Vec<u8>>;
    pub fn get_bool(&self) -> Option<bool>;
    pub fn is_null(&self) -> bool;
    pub fn set_start(&self, start: i64);
    pub fn set_end(&self, end: i64);
    pub fn start(&self) -> i64;
    pub fn end(&self) -> i64;
}
```

- [ ] **Step 6: Run RED again and confirm the remaining failure is writer-only**

```sh
cargo test -p flpdf --test json_tests
```

Expected: compile failure for missing `Json::unparse`, while scalar accessors compile.

- [ ] **Step 7: Add the minimal scalar writer entry points**

In `json/value.rs`, expose crate-private borrowing helpers. In
`json/writer.rs`, implement:

```rust
impl Json {
    pub fn write(&self, out: &mut (impl Write + ?Sized), depth: usize) -> io::Result<()> {
        match self.value_snapshot() {
            None => out.write_all(b"null"),
            Some(ValueSnapshot::Number(value)) => out.write_all(&value),
            Some(ValueSnapshot::Bool(value)) => {
                out.write_all(if value { b"true" } else { b"false" })
            }
            Some(ValueSnapshot::Null) => out.write_all(b"null"),
            Some(ValueSnapshot::String(encoded)) => {
                out.write_all(b"\"")?;
                out.write_all(&encoded)?;
                out.write_all(b"\"")
            }
            Some(other) => write_container_or_blob(other, out, depth),
        }
    }

    pub fn unparse(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write(&mut out, 0)?;
        Ok(out)
    }
}
```

Register `mod writer;` in `json/mod.rs`.

- [ ] **Step 8: Verify GREEN and commit**

```sh
cargo fmt
cargo test -p flpdf --test json_tests
cargo test -p flpdf json::
git add crates/flpdf/src/json crates/flpdf/tests/json_tests.rs
git commit -m "feat(json): add qpdf shared scalar model"
```

Expected: all focused tests pass.

---

### Task 2: Add shared containers, encoded keys, and accessors

**Files:**
- Modify: `crates/flpdf/src/json/value.rs`
- Modify: `crates/flpdf/tests/json_tests.rs`

**Interfaces:**
- Consumes: `Json` scalar API from Task 1.
- Produces: dictionary/array construction, shared mutation, duplicate-key tracking, and iteration required by parser, schema, handler, and inspection code.

- [ ] **Step 1: Add failing shared-container tests**

```rust
#[test]
fn cloned_dictionary_handles_share_mutation_and_sort_encoded_keys() {
    let dictionary = Json::make_dictionary();
    let alias = dictionary.clone();
    dictionary
        .add_dictionary_member(b"b", Json::make_int(2))
        .unwrap();
    alias
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    assert_eq!(
        dictionary.unparse().unwrap(),
        b"{\n  \"a\": 1,\n  \"b\": 2\n}"
    );
}

#[test]
fn uninitialized_children_become_initialized_null() {
    let array = Json::make_array();
    let stored = array.add_array_element(Json::default()).unwrap();
    assert!(stored.is_null());
}

#[test]
fn parser_key_seen_set_is_separate_from_encoded_members() {
    let dictionary = Json::make_dictionary();
    assert!(!dictionary.check_dictionary_key_seen(b"a\n").unwrap());
    assert!(dictionary.check_dictionary_key_seen(b"a\n").unwrap());
}
```

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf --test json_tests cloned_dictionary_handles_share_mutation
```

Expected: compile failure for missing container APIs.

- [ ] **Step 3: Implement all container APIs**

Use these exact public signatures:

```rust
pub fn make_dictionary() -> Self;
pub fn add_dictionary_member(
    &self,
    key: impl AsRef<[u8]>,
    value: Json,
) -> Result<Json, JsonError>;
pub fn make_array() -> Self;
pub fn add_array_element(&self, value: Json) -> Result<Json, JsonError>;
pub fn is_array(&self) -> bool;
pub fn is_dictionary(&self) -> bool;
pub fn check_dictionary_key_seen(
    &self,
    key: impl AsRef<[u8]>,
) -> Result<bool, JsonError>;
pub fn get_dict_item(&self, encoded_key: impl AsRef<[u8]>) -> Json;
pub fn for_each_dict_item(&self, callback: impl FnMut(&[u8], Json)) -> bool;
pub fn for_each_array_item(&self, callback: impl FnMut(Json)) -> bool;
```

`add_dictionary_member` encodes the key before insertion.
`check_dictionary_key_seen` stores the decoded key. Wrong-type mutations return
`JsonError::Type` with qpdf's method-specific message. A missing dictionary
item returns `Json::make_null()`.

- [ ] **Step 4: Verify GREEN and commit**

```sh
cargo fmt
cargo test -p flpdf --test json_tests
git add crates/flpdf/src/json/value.rs crates/flpdf/tests/json_tests.rs
git commit -m "feat(json): add qpdf shared containers"
```

---

### Task 3: Complete tree writing, escaping, real formatting, and blobs

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/flpdf/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/flpdf/src/json/writer.rs`
- Modify: `crates/flpdf/tests/json_tests.rs`

**Interfaces:**
- Consumes: `Json` value snapshots from Tasks 1–2.
- Produces: byte-exact tree serialization and `make_blob`.

- [ ] **Step 1: Add upstream byte-lock tests**

```rust
#[test]
fn qpdf_string_escape_bytes_are_exact() {
    let value = Json::make_string(
        b"<1>\xcf\x80<2>\xf0\x9f\xa5\x94\\\"<3>\x03\t\x08\r\n<4>",
    );
    assert_eq!(
        value.unparse().unwrap(),
        b"\"<1>\xcf\x80<2>\xf0\x9f\xa5\x94\\\\\\\"<3>\\u0003\\t\\b\\r\\n<4>\""
    );
}

#[test]
fn qpdf_blob_uses_standard_base64_without_newlines() {
    let blob = Json::make_blob(|out| {
        out.write_all(b"\x01\x02\x03\x04\x05\xff\xfe\xfd\xfc\xfb")
    });
    assert_eq!(blob.unparse().unwrap(), b"\"AQIDBAX//v38+w==\"");
}

#[test]
fn qpdf_real_uses_six_digit_trimmed_format() {
    assert_eq!(Json::make_real(3.14159).unparse().unwrap(), b"3.14159");
    assert_eq!(Json::make_real(-0.0).unparse().unwrap(), b"-0");
}
```

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf --test json_tests qpdf_blob_uses_standard_base64_without_newlines
```

Expected: compile failure for missing `make_blob`.

- [ ] **Step 3: Add the approved dependency**

Add `base64 = "0.22"` to `[workspace.dependencies]` and
`base64.workspace = true` to `crates/flpdf/Cargo.toml`.
Import `base64::Engine` where `STANDARD.encode` is called.

- [ ] **Step 4: Port `JSON::Writer::encode_string` and blob writing**

Implement byte escaping exactly:

```rust
pub(crate) fn encode_string(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for &byte in input {
        match byte {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\x08' => out.extend_from_slice(b"\\b"),
            b'\x0c' => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x0f => out.extend_from_slice(format!("\\u000{byte:x}").as_bytes()),
            0x10..=0x1f => out.extend_from_slice(format!("\\u001{:x}", byte & 0x0f).as_bytes()),
            _ => out.push(byte),
        }
    }
    out
}
```

For a blob, invoke the callback into a temporary `Vec<u8>`, encode it with
`STANDARD.encode`, and write quotes around the result. Do not append LF.
Expose it as:

```rust
impl Json {
    pub fn make_blob(
        callback: impl FnMut(&mut dyn Write) -> io::Result<()> + 'static,
    ) -> Json;
}
```

- [ ] **Step 5: Verify GREEN and commit**

```sh
cargo fmt
cargo test -p flpdf --test json_tests
cargo test -p flpdf json::
git add Cargo.toml Cargo.lock crates/flpdf/Cargo.toml crates/flpdf/src/json/writer.rs crates/flpdf/tests/json_tests.rs
git commit -m "feat(json): match qpdf tree and blob writing"
```

---

### Task 4: Add the complete incremental writer and close `.6.1`

**Files:**
- Modify: `crates/flpdf/src/json/writer.rs`
- Modify: `crates/flpdf/tests/json_tests.rs`

**Interfaces:**
- Consumes: complete tree writer from Task 3.
- Produces: the eight qpdf incremental writer methods used by the final CLI integration.

- [ ] **Step 1: Add a failing nested incremental-output test**

```rust
#[test]
fn incremental_writer_matches_qpdf_nested_bytes() {
    let mut out = Vec::new();
    let mut top_first = true;
    Json::write_dictionary_open(&mut out, &mut top_first, 0).unwrap();
    Json::write_dictionary_item(
        &mut out,
        &mut top_first,
        b"version",
        &Json::make_int(2),
        1,
    )
    .unwrap();
    Json::write_dictionary_key(&mut out, &mut top_first, b"items", 1).unwrap();
    let mut array_first = true;
    Json::write_array_open(&mut out, &mut array_first, 1).unwrap();
    Json::write_array_item(&mut out, &mut array_first, &Json::make_bool(true), 2).unwrap();
    Json::write_array_close(&mut out, array_first, 1).unwrap();
    Json::write_dictionary_close(&mut out, top_first, 0).unwrap();
    assert_eq!(
        out,
        b"{\n  \"version\": 2,\n  \"items\": [\n    true\n  ]\n}"
    );
}
```

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf --test json_tests incremental_writer_matches_qpdf_nested_bytes
```

Expected: compile failure for missing incremental methods.

- [ ] **Step 3: Port all incremental methods**

Implement the exact signatures:

```rust
pub fn write_dictionary_open(out: &mut (impl Write + ?Sized), first: &mut bool, depth: usize) -> io::Result<()>;
pub fn write_array_open(out: &mut (impl Write + ?Sized), first: &mut bool, depth: usize) -> io::Result<()>;
pub fn write_dictionary_close(out: &mut (impl Write + ?Sized), first: bool, depth: usize) -> io::Result<()>;
pub fn write_array_close(out: &mut (impl Write + ?Sized), first: bool, depth: usize) -> io::Result<()>;
pub fn write_dictionary_item(out: &mut (impl Write + ?Sized), first: &mut bool, key: &[u8], value: &Json, depth: usize) -> io::Result<()>;
pub fn write_dictionary_key(out: &mut (impl Write + ?Sized), first: &mut bool, encoded_key: &[u8], depth: usize) -> io::Result<()>;
pub fn write_array_item(out: &mut (impl Write + ?Sized), first: &mut bool, value: &Json, depth: usize) -> io::Result<()>;
pub fn write_next(out: &mut (impl Write + ?Sized), first: &mut bool, depth: usize) -> io::Result<()>;
```

Open methods set `first = true`. `write_next` sets it false and emits either
`\n` or `,\n` followed by `2 * depth` spaces. Empty close emits only the closing
delimiter.

- [ ] **Step 4: Run the bottom-layer gates**

```sh
cargo fmt
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_tests
cargo test -p flpdf
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

- [ ] **Step 5: Commit, measure the bottom PR, and push**

```sh
git add crates/flpdf/src/json/writer.rs crates/flpdf/tests/json_tests.rs
git commit -m "feat(json): add qpdf incremental writer"
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/llvm-cov/lcov.info
scripts/patch-coverage.sh --base origin/main --lcov target/llvm-cov/lcov.info
bd close flpdf-qxba.6.1
bd dolt push
git push -u origin feature/flpdf-qxba-6-1-json-core
```

Expected patch coverage: 100%.

---

### Task 5: Port scalar parsing, offsets, and lexical errors (`flpdf-qxba.6.2`)

**Files:**
- Create: `crates/flpdf/src/json/parser.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Test: `crates/flpdf/tests/json_parse_tests.rs`

**Interfaces:**
- Consumes: `Json` constructors and offsets from `.6.1`.
- Produces: `Json::parse`, `Json::parse_reader`, and qpdf-compatible parser errors.

- [ ] **Step 1: Create and claim the parser branch**

```sh
git switch -c feature/flpdf-qxba-6-2-json-parser
bd update flpdf-qxba.6.2 --claim
```

- [ ] **Step 2: Add failing scalar and offset tests**

```rust
use flpdf::json::Json;

#[test]
fn parser_preserves_number_token_and_offsets() {
    let value = Json::parse(b" \n-2.10E+05\t").unwrap();
    assert_eq!(value.get_number().as_deref(), Some(b"-2.10E+05".as_slice()));
    assert_eq!((value.start(), value.end()), (2, 12));
}

#[test]
fn parser_rejects_material_after_top_level_value() {
    let error = Json::parse(b"null true").unwrap_err();
    assert_eq!(
        error.to_string(),
        "JSON: offset 9: material follows end of object: true"
    );
}
```

- [ ] **Step 3: Verify RED**

```sh
cargo fmt
cargo test -p flpdf --test json_parse_tests
```

Expected: compile failure for missing `Json::parse`.

- [ ] **Step 4: Implement the lexer and scalar parser**

Port every qpdf lexer state into:

```rust
enum LexState {
    Top,
    NumberMinus,
    NumberLeadingZero,
    NumberBeforePoint,
    NumberPoint,
    NumberAfterPoint,
    NumberE,
    NumberESign,
    Number,
    Alpha,
    String,
    Backslash,
    U4,
    AfterString,
    BeginDictionary,
    EndDictionary,
    BeginArray,
    EndArray,
    Colon,
    Comma,
}
```

Implement `parse(&[u8])` through a `Cursor` and
`parse_reader<R: Read>(reader: &mut R, reactor: Option<&mut dyn Reactor>)`.
Implement these terminal rules explicitly: leading zero accepts only `.`, `e`,
or `E`; a decimal point requires a following digit; an exponent accepts one
optional sign and requires a digit; alpha tokens accept only `true`, `false`,
and `null`; whitespace or a structural delimiter terminates a complete token;
and any token after `ParseState::Done` reports material after the top-level
object. Use the exact error strings from `JSON.cc:706-949`.

The public signatures are:

```rust
pub fn parse(input: &[u8]) -> Result<Json, JsonError>;
pub fn parse_reader<R: Read>(
    reader: &mut R,
    reactor: Option<&mut dyn Reactor>,
) -> Result<Json, JsonError>;
```

- [ ] **Step 5: Verify GREEN and commit**

```sh
cargo fmt
cargo test -p flpdf --test json_parse_tests
git add crates/flpdf/src/json/mod.rs crates/flpdf/src/json/parser.rs crates/flpdf/tests/json_parse_tests.rs
git commit -m "feat(json): parse qpdf scalar JSON"
```

---

### Task 6: Port containers, escapes, surrogates, and duplicate keys

**Files:**
- Modify: `crates/flpdf/src/json/parser.rs`
- Modify: `crates/flpdf/tests/json_parse_tests.rs`

**Interfaces:**
- Consumes: scalar parser from Task 5 and container APIs from Task 2.
- Produces: complete tree parsing without Reactor consumption.

- [ ] **Step 1: Add failing container tests**

```rust
#[test]
fn parser_decodes_escapes_and_utf16_surrogate_pairs() {
    let value = Json::parse(br#"{"x":"\u03c0 \ud83e\udd54"}"#).unwrap();
    let string = value.get_dict_item(b"x").get_string().unwrap();
    assert_eq!(string, "π 🥔".as_bytes());
}

#[test]
fn parser_rejects_duplicate_key_even_when_spelling_uses_escape() {
    let error = Json::parse(br#"{"a":1,"\u0061":2}"#).unwrap_err();
    assert_eq!(
        error.to_string(),
        "JSON: offset 7: duplicated dictionary key"
    );
}
```

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf --test json_parse_tests parser_decodes_escapes_and_utf16_surrogate_pairs
```

Expected: parse error because container states are not implemented.

- [ ] **Step 3: Implement container parser states**

Use a stack entry that retains the parent state and current container:

```rust
struct StackEntry {
    state: ParseState,
    item: Json,
}

enum ParseState {
    Top,
    DictionaryBegin,
    DictionaryAfterKey,
    DictionaryAfterColon,
    DictionaryAfterItem,
    DictionaryAfterComma,
    ArrayBegin,
    ArrayAfterItem,
    ArrayAfterComma,
    Done,
}
```

Port `handle_u_code`, including high/low surrogate errors and UTF-8 emission.
Call `check_dictionary_key_seen` on the decoded key before insertion. Set the
container end offset after reading `}` or `]`.

- [ ] **Step 4: Verify GREEN and commit**

```sh
cargo test -p flpdf --test json_parse_tests
git add crates/flpdf/src/json/parser.rs crates/flpdf/tests/json_parse_tests.rs
git commit -m "feat(json): parse qpdf JSON containers"
```

---

### Task 7: Add Reactor consumption and close `.6.2`

**Files:**
- Modify: `crates/flpdf/src/json/parser.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Modify: `crates/flpdf/tests/json_parse_tests.rs`

**Interfaces:**
- Consumes: complete parser from Tasks 5–6.
- Produces: public `Reactor` trait and event sequencing.

- [ ] **Step 1: Add a recording Reactor test**

```rust
#[derive(Default)]
struct RecordingReactor {
    events: Vec<String>,
}

impl flpdf::json::Reactor for RecordingReactor {
    fn dictionary_start(&mut self) { self.events.push("dict-start".into()); }
    fn array_start(&mut self) { self.events.push("array-start".into()); }
    fn container_end(&mut self, value: &Json) {
        self.events.push(format!("end:{}", value.end()));
    }
    fn top_level_scalar(&mut self) { self.events.push("scalar".into()); }
    fn dictionary_item(&mut self, key: &[u8], value: &Json) -> bool {
        self.events.push(format!(
            "dict-item:{}:{}",
            String::from_utf8_lossy(key),
            value.unparse().map(|v| String::from_utf8_lossy(&v).into_owned()).unwrap()
        ));
        key != b"keep"
    }
    fn array_item(&mut self, value: &Json) -> bool {
        self.events.push(format!(
            "array-item:{}",
            String::from_utf8_lossy(&value.unparse().unwrap())
        ));
        true
    }
}
```

Parse `{"drop":[1],"keep":2}` and assert the parent receives
`dict-item:drop:[]` before `array-start`, consumed children are absent, and the
`keep` value remains.

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf --test json_parse_tests reactor_parent_sees_empty_child_before_child_start
```

Expected: compile failure for missing `Reactor`.

- [ ] **Step 3: Implement the public trait and exact event order**

```rust
pub trait Reactor {
    fn dictionary_start(&mut self);
    fn array_start(&mut self);
    fn container_end(&mut self, value: &Json);
    fn top_level_scalar(&mut self);
    fn dictionary_item(&mut self, key: &[u8], value: &Json) -> bool;
    fn array_item(&mut self, value: &Json) -> bool;
}
```

For a child container: construct it, set its start, invoke the parent item
callback, conditionally insert it, then invoke its start callback. Duplicate
key detection happens before the item callback, including consumed items.

- [ ] **Step 4: Run parser-layer gates and push**

```sh
cargo fmt
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_tests
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf
git add crates/flpdf/src/json/mod.rs crates/flpdf/src/json/parser.rs crates/flpdf/tests/json_parse_tests.rs
git commit -m "feat(json): add qpdf parser Reactor"
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/llvm-cov/lcov.info
scripts/patch-coverage.sh --base origin/feature/flpdf-qxba-6-1-json-core --lcov target/llvm-cov/lcov.info
bd close flpdf-qxba.6.2
bd dolt push
git push -u origin feature/flpdf-qxba-6-2-json-parser
```

Expected patch coverage: 100%.

---

### Task 8: Port qpdf schema checking (`flpdf-qxba.6.3`)

**Files:**
- Create: `crates/flpdf/src/json/schema.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Test: `crates/flpdf/tests/json_schema_tests.rs`

**Interfaces:**
- Consumes: parsed `Json`, dictionary/array iteration.
- Produces: `SchemaFlags`, `Json::check_schema`, and `check_schema_with_flags`.

- [ ] **Step 1: Create and claim the validation branch**

```sh
git switch -c feature/flpdf-qxba-6-3-json-validation
bd update flpdf-qxba.6.3 --claim
```

- [ ] **Step 2: Add failing schema tests**

```rust
use flpdf::json::{Json, SchemaFlags};

#[test]
fn optional_flag_allows_missing_but_not_extra_keys() {
    let schema = Json::parse(br#"{"a":"value","b":"value"}"#).unwrap();
    let value = Json::parse(br#"{"a":1}"#).unwrap();
    let mut errors = Vec::new();
    assert!(value.check_schema_with_flags(&schema, SchemaFlags::OPTIONAL, &mut errors));

    let extra = Json::parse(br#"{"a":1,"x":2}"#).unwrap();
    assert!(!extra.check_schema_with_flags(&schema, SchemaFlags::OPTIONAL, &mut errors));
    assert!(errors.last().unwrap().contains("not present in schema"));
}

#[test]
fn pattern_key_validates_every_dictionary_value() {
    let schema = Json::parse(br#"{"<objid>":{"n":"number"}}"#).unwrap();
    let value = Json::parse(br#"{"one":{"n":1},"two":{"n":2}}"#).unwrap();
    let mut errors = Vec::new();
    assert!(value.check_schema(&schema, &mut errors));
}
```

- [ ] **Step 3: Verify RED**

```sh
cargo fmt
cargo test -p flpdf --test json_schema_tests
```

Expected: unresolved imports for `SchemaFlags`.

- [ ] **Step 4: Implement every qpdf schema branch**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaFlags(u64);

impl SchemaFlags {
    pub const NONE: Self = Self(0);
    pub const OPTIONAL: Self = Self(1);
    pub(crate) fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for SchemaFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl Json {
    pub fn check_schema(&self, schema: &Json, errors: &mut Vec<String>) -> bool;
    pub fn check_schema_with_flags(
        &self,
        schema: &Json,
        flags: SchemaFlags,
        errors: &mut Vec<String>,
    ) -> bool;
}
```

Implement the recursive match in this order: dictionary type check; one
angle-bracket pattern key; exact dictionary keys and optional missing keys;
unknown checked-object keys; one-element schema array; fixed-length schema
array; schema string wildcard; invalid schema type. Preserve the qpdf error
prefixes `top-level object` and `json key "<path>"` and the exact messages from
`JSON.cc:450-581`.

- [ ] **Step 5: Verify GREEN and commit**

```sh
cargo test -p flpdf --test json_schema_tests
git add crates/flpdf/src/json/mod.rs crates/flpdf/src/json/schema.rs crates/flpdf/tests/json_schema_tests.rs
git commit -m "feat(json): port qpdf schema checking"
```

---

### Task 9: Port `JsonHandler` and close `.6.3`

**Files:**
- Create: `crates/flpdf/src/json/handler.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Test: `crates/flpdf/tests/json_handler_tests.rs`

**Interfaces:**
- Consumes: `Json` typed accessors and encoded dictionary iteration.
- Produces: `JsonHandler`, `SharedJsonHandler`, and `JsonHandlerError`.

- [ ] **Step 1: Add failing dispatch and fallback tests**

```rust
use flpdf::json::{Json, JsonHandler};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn dictionary_handler_uses_exact_key_then_unknown_key_fallback() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let exact = JsonHandler::shared();
    exact.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let fallback = JsonHandler::shared();
    fallback.borrow_mut().add_any_handler({
        let seen = seen.clone();
        move |path, _| seen.borrow_mut().push(format!("fallback:{}", String::from_utf8_lossy(path)))
    });

    let mut root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});
    root.add_dictionary_key_handler(b"known", exact);
    root.add_fallback_dictionary_handler(fallback);
    root.handle(b".", Json::parse(br#"{"known":1,"other":null}"#).unwrap()).unwrap();

    assert_eq!(&*seen.borrow(), &[".known=1", "fallback:.other"]);
}
```

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf --test json_handler_tests
```

Expected: unresolved imports for `JsonHandler`.

- [ ] **Step 3: Implement the complete public handler surface**

Define:

```rust
pub type SharedJsonHandler = Rc<RefCell<JsonHandler>>;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("{0}")]
pub struct JsonHandlerError(pub String);

pub struct JsonHandler {
    any: Option<Box<dyn FnMut(&[u8], Json)>>,
    null: Option<Box<dyn FnMut(&[u8])>>,
    string: Option<Box<dyn FnMut(&[u8], &[u8])>>,
    number: Option<Box<dyn FnMut(&[u8], &[u8])>>,
    boolean: Option<Box<dyn FnMut(&[u8], bool)>>,
    dictionary_start: Option<Box<dyn FnMut(&[u8], Json)>>,
    dictionary_end: Option<Box<dyn FnMut(&[u8])>>,
    array_start: Option<Box<dyn FnMut(&[u8], Json)>>,
    array_end: Option<Box<dyn FnMut(&[u8])>>,
    dictionary_keys: BTreeMap<Vec<u8>, SharedJsonHandler>,
    fallback_dictionary: Option<SharedJsonHandler>,
    array_item: Option<SharedJsonHandler>,
    fallback: Option<SharedJsonHandler>,
}
```

Implement these exact public methods:

```rust
pub fn new() -> Self;
pub fn shared() -> SharedJsonHandler;
pub fn add_any_handler(&mut self, callback: impl FnMut(&[u8], Json) + 'static);
pub fn add_null_handler(&mut self, callback: impl FnMut(&[u8]) + 'static);
pub fn add_string_handler(&mut self, callback: impl FnMut(&[u8], &[u8]) + 'static);
pub fn add_number_handler(&mut self, callback: impl FnMut(&[u8], &[u8]) + 'static);
pub fn add_bool_handler(&mut self, callback: impl FnMut(&[u8], bool) + 'static);
pub fn add_dictionary_handlers(
    &mut self,
    start: impl FnMut(&[u8], Json) + 'static,
    end: impl FnMut(&[u8]) + 'static,
);
pub fn add_dictionary_key_handler(&mut self, key: impl AsRef<[u8]>, handler: SharedJsonHandler);
pub fn add_fallback_dictionary_handler(&mut self, handler: SharedJsonHandler);
pub fn add_array_handlers(
    &mut self,
    start: impl FnMut(&[u8], Json) + 'static,
    end: impl FnMut(&[u8]) + 'static,
    item: SharedJsonHandler,
);
pub fn add_fallback_handler(&mut self, handler: SharedJsonHandler);
pub fn handle(&mut self, path: &[u8], value: Json) -> Result<(), JsonHandlerError>;
```

Preserve early return after any/scalar handling, encoded path joining, indexed
array paths, unexpected-key errors, and final unexpected-type errors from
`JSONHandler.cc:120-189`.

- [ ] **Step 4: Run validation-layer gates and push**

```sh
cargo fmt
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_tests
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf --test json_schema_tests
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf
git add crates/flpdf/src/json/mod.rs crates/flpdf/src/json/handler.rs crates/flpdf/tests/json_handler_tests.rs
git commit -m "feat(json): port qpdf JSON handler"
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/llvm-cov/lcov.info
scripts/patch-coverage.sh --base origin/feature/flpdf-qxba-6-2-json-parser --lcov target/llvm-cov/lcov.info
bd close flpdf-qxba.6.3
bd dolt push
git push -u origin feature/flpdf-qxba-6-3-json-validation
```

Expected patch coverage: 100%.

---

### Task 10: Migrate JSON inspection values and delete duplicate Base64 (`flpdf-qxba.6.4`)

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: tests inside `crates/flpdf/src/json_inspect.rs`

**Interfaces:**
- Consumes: `Json` constructors, mutation, accessors, and blob writer.
- Produces: every section builder and PDF-object converter returning `Json`.

- [ ] **Step 1: Create and claim the integration branch**

```sh
git switch -c feature/flpdf-qxba-6-4-json-integration
bd update flpdf-qxba.6.4 --claim
```

- [ ] **Step 2: Change one conversion test to require the new type**

Add a type assertion to the existing `pdf_object_to_json` tests:

```rust
fn assert_json(_: &crate::json::Json) {}

let converted = pdf_object_to_json(&Object::Integer(42)).unwrap();
assert_json(&converted);
assert_eq!(converted.get_number().as_deref(), Some(b"42".as_slice()));
```

- [ ] **Step 3: Verify RED**

```sh
cargo test -p flpdf json_inspect::tests::pdf_object
```

Expected: type mismatch because the function returns legacy `JsonValue`.

- [ ] **Step 4: Convert builders mechanically and preserve behavior**

Replace variants with constructors:

```rust
JsonValue::Null                => Json::make_null()
JsonValue::Bool(value)         => Json::make_bool(value)
JsonValue::Integer(value)      => Json::make_int(value)
JsonValue::Float(value)        => Json::make_real(value)
JsonValue::String(value)       => Json::make_string(value.as_bytes())
JsonValue::Array(values)       => json_array(values)?
JsonValue::Object(pairs)       => json_dictionary(pairs)?
```

Add builders that propagate wrong-container errors:

```rust
fn json_array(values: impl IntoIterator<Item = Json>) -> Result<Json, ConvertError> {
    let array = Json::make_array();
    for value in values {
        array
            .add_array_element(value)
            .map_err(|error| ConvertError::JsonError(error.to_string()))?;
    }
    Ok(array)
}
```

Add `ConvertError::JsonError(String)` with display prefix `JSON error: `.
Change `pdf_object_to_json`, every `build_*_section`, `build_qpdf_key*`, and
their helpers to return `Json`. Replace inline stream data with
`Json::make_blob`; delete `base64_encode` and its unit tests.

- [ ] **Step 5: Run inspection tests and commit**

```sh
cargo fmt
cargo test -p flpdf json_inspect
git add crates/flpdf/src/json_inspect.rs
git commit -m "refactor(json): migrate inspection values"
```

---

### Task 11: Replace whole-document materialization with sink output

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`

**Interfaces:**
- Consumes: incremental `Json` writer and migrated section builders.
- Produces: `JsonOutputSummary`, `JsonOutputError`, and
  `write_qpdf_json_v2_selected_objects_with_options`.

- [ ] **Step 1: Add a failing sink API test**

```rust
#[test]
fn selected_sink_writer_emits_envelope_then_selected_section() {
    let mut pdf = empty_pdf();
    let mut out = Vec::new();
    let summary = write_qpdf_json_v2_selected_objects_with_options(
        &mut pdf,
        DecodeLevel::Generalized,
        &StreamDataMode::None,
        &[JsonKey::Pages],
        &[],
        &mut out,
    )
    .unwrap();
    assert!(summary.datafile_objects.is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&out).is_ok());
    let version = out.windows(b"\"version\"".len()).position(|w| w == b"\"version\"").unwrap();
    let parameters = out.windows(b"\"parameters\"".len()).position(|w| w == b"\"parameters\"").unwrap();
    let pages = out.windows(b"\"pages\"".len()).position(|w| w == b"\"pages\"").unwrap();
    assert!(version < parameters && parameters < pages);
}
```

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf selected_sink_writer_emits_envelope_then_selected_section
```

Expected: missing sink-oriented function.

- [ ] **Step 3: Define concrete output types**

```rust
#[derive(Debug, Default, Eq, PartialEq)]
pub struct JsonOutputSummary {
    pub datafile_objects: Vec<ObjectRef>,
}

#[derive(Debug, thiserror::Error)]
pub enum JsonOutputError {
    #[error(transparent)]
    Convert(#[from] ConvertError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 4: Implement ordered section emission**

Add:

```rust
fn build_parameters(decode_level: DecodeLevel) -> Result<Json, ConvertError> {
    let parameters = Json::make_dictionary();
    parameters
        .add_dictionary_member(
            b"decodelevel",
            Json::make_string(decode_level.as_qpdf_str().as_bytes()),
        )
        .map_err(|error| ConvertError::JsonError(error.to_string()))?;
    Ok(parameters)
}
```

Open the top-level dictionary, emit `version`, `parameters`, then selected
sections in this exact order:

```rust
let mut first = true;
Json::write_dictionary_open(out, &mut first, 0)?;
Json::write_dictionary_item(out, &mut first, b"version", &Json::make_int(2), 1)?;
Json::write_dictionary_item(out, &mut first, b"parameters", &build_parameters(decode_level)?, 1)?;
emit_section(out, &mut first, b"pages", keys, JsonKey::Pages, || build_pages_section(pdf))?;
emit_section(out, &mut first, b"pagelabels", keys, JsonKey::Pagelabels, || build_pagelabels_section(pdf))?;
emit_section(out, &mut first, b"acroform", keys, JsonKey::Acroform, || build_acroform_section(pdf))?;
emit_section(out, &mut first, b"attachments", keys, JsonKey::Attachments, || build_attachments_section(pdf))?;
emit_section(out, &mut first, b"encrypt", keys, JsonKey::Encrypt, || build_encrypt_section(pdf))?;
emit_section(out, &mut first, b"outlines", keys, JsonKey::Outlines, || build_outlines_section(pdf))?;
```

Emit the qpdf metadata and object map last. Prepare all qpdf objects once, but
write each selected object immediately and record its `ObjectRef` when file
stream mode emits `datafile`. Close the top-level dictionary and write one LF.

- [ ] **Step 5: Remove post-build selection**

Delete `filter_json_keys`, `filter_json_objects`, the four
`build_qpdf_json_v2* -> JsonValue` APIs, and tests that exist only for malformed
completed-tree filter inputs. Retain selector parsing tests and move selection
assertions to sink-output tests.

- [ ] **Step 6: Verify GREEN and commit**

```sh
cargo fmt
cargo test -p flpdf json_inspect
cargo test -p flpdf-cli --test cli_json
git add crates/flpdf/src/json_inspect.rs crates/flpdf-cli/tests/cli_json.rs
git commit -m "refactor(json): stream qpdf JSON document output"
```

---

### Task 12: Cut over the CLI, prove partial output, delete legacy, and close the stack

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`
- Modify: `docs/qpdf-correspondence.md`
- Delete: `crates/flpdf/src/json/legacy.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`

**Interfaces:**
- Consumes: sink-oriented integration from Task 11.
- Produces: the sole production JSON path and qpdf-compatible fatal partial output.

- [ ] **Step 1: Add failing stdout and output-file partial tests**

Copy the minimal short `/Names /Dests /Kids` fixture shape from
`outline_document_helper_tests.rs` into `cli_json.rs` as
`short_name_tree_pair_pdf`, then invoke the flpdf binary:

```rust
let input = write_temp_pdf(&short_name_tree_pair_pdf());
let output = Command::cargo_bin("flpdf")
    .unwrap()
    .args(["--json=2", "--json-key=outlines", input.path().to_str().unwrap()])
    .output()
    .unwrap();
assert!(!output.status.success());
assert!(output.stdout.starts_with(b"{\n  \"version\": 2,"));
assert!(!output.stdout.ends_with(b"}\n"));
assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err());
```

Repeat with `--json-output <path>`, assert stdout is empty, and assert the file
contains the same incomplete prefix.

- [ ] **Step 2: Verify RED**

```sh
cargo test -p flpdf-cli --test cli_json json_fatal_preserves_partial_stdout
```

Expected: flpdf output is empty because the CLI still builds before opening the sink.

- [ ] **Step 3: Open the sink before JSON construction**

Replace the completed-tree block in `run_json_mode` with:

```rust
let json_result = if let Some(ref path) = cli.json_output {
    let mut file = File::create(path)?;
    write_qpdf_json_v2_selected_objects_with_options(
        &mut pdf,
        decode_level,
        &stream_mode,
        &json_keys,
        &json_objects,
        &mut file,
    )
} else {
    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    let write_result: Result<JsonOutputSummary, JsonOutputError> =
        write_qpdf_json_v2_selected_objects_with_options(
        &mut pdf,
        decode_level,
        &stream_mode,
        &json_keys,
        &json_objects,
        &mut locked,
    );
    let flush_result = locked.flush().map_err(JsonOutputError::from);
    match (write_result, flush_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(summary), Ok(())) => Ok(summary),
    }
};
```

On error, emit warnings and return without reopening, truncating, or replacing
the sink.

- [ ] **Step 4: Use `JsonOutputSummary` for side files**

On successful JSON output, iterate `summary.datafile_objects`; use the same
`pdf`, decode level, and `format_json_side_file_path`. Delete
`collect_datafile_object_refs`.

- [ ] **Step 5: Delete every legacy production symbol**

Delete `json/legacy.rs` and remove its re-exports. Confirm:

```sh
rg -n "JsonValue|json::write|fn base64_encode|filter_json_keys|filter_json_objects|collect_datafile_object_refs|build_qpdf_json_v2_with_options" crates/flpdf/src crates/flpdf-cli/src
```

Expected: no matches.

- [ ] **Step 6: Mark the completed component in the correspondence table**

Change the `JSON.cc` and `JSONHandler.cc` rows to `json/` with status `✅`.
Recompute the summary counts:

```text
✅ mirrors: 2,944
🔀 smeared: 28,493
❌ missing: 623
```

The total remains 41,459.

- [ ] **Step 7: Run all focused and workspace gates**

```sh
cargo fmt
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_tests
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf --test json_schema_tests
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_json
cargo test -p flpdf-cli --test json_schema_diff
cargo test
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

- [ ] **Step 8: Commit the verified integration**

```sh
git add crates/flpdf/src/json crates/flpdf/src/json_inspect.rs crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_json.rs docs/qpdf-correspondence.md
git commit -m "refactor(json): cut CLI over to qpdf streaming"
```

- [ ] **Step 9: Prove per-PR coverage and qpdf output**

```sh
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/llvm-cov/lcov.info
scripts/patch-coverage.sh --base origin/feature/flpdf-qxba-6-3-json-validation --lcov target/llvm-cov/lcov.info
cargo test -p flpdf-cli --test compat_matrix_tests
bash /home/ubuntu/flpdf-qtest/scripts/run.sh 2>&1 | tee /tmp/flpdf-qxba-6-qtest-after.log
```

Expected patch coverage: 100%. Record qtest before/after counts in
`flpdf-qxba.6` as an outcome metric; do not change implementation to chase its
count.

- [ ] **Step 10: Close beads and push**

```sh
bd close flpdf-qxba.6.4
bd close flpdf-qxba.6
bd dolt push
git push -u origin feature/flpdf-qxba-6-4-json-integration
```

Before closing the parent, verify `.6.1`–`.6.4` are all closed and
`bd dep cycles` reports no cycles.
