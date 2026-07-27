# qpdf JSON Definitive Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining defined qpdf 11.9.0 JSON value, parser, handler, diagnostic, and side-file parity gaps in the existing #559–#562 stack.

**Architecture:** Make `JsonHandler` a cloneable shared handle that clones only the exact live callback or child target before invocation. Make the JSON writer obtain container state after opening delimiters and use an explicitly finalized Base64 adapter. Carry diagnostic bytes in `JsonMessage`, and isolate qpdf's measured 4 KiB stdio behavior in a side-file-only adapter.

**Tech Stack:** Rust 2021 workspace; `Rc`, `Weak`, `RefCell`, `Cell`, `BTreeMap`, and `std::io::Write`; existing `base64` crate; qpdf 11.9.0 at `scripts/fetch-qpdf-source.sh --print-path`; Beads; four existing `gh stack` branches; Cargo, Clippy, strict rustdoc, `scripts/qpdf-module-docs.py`, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 is the behavioral oracle. Cite and probe the pinned source; do not move or edit its worktree.
- Preserve the single-threaded `Rc` model. Do not add `Send`, `Sync`, threads, or unsafe code.
- `JsonHandler` callbacks become `Fn`; callers with mutable state use explicit interior mutability.
- Remove `SharedJsonHandler`, `HandlerSnapshot`, `ActiveHandler`, `DispatchContext`, and `handle_shared`; do not retain a compatibility dispatch path.
- Blob Base64 tail bytes, padding, and the closing quote are emitted only after callback success. Drop emits nothing.
- Parser, schema, and handler diagnostics preserve arbitrary bytes. `Display` may be lossy, but exact tests and boundaries use `as_bytes`.
- The pinned Linux `/dev/full` boundary is 4,096 bytes: 4,095 bytes can fail only at final `fflush`, while a 4,096-byte `fwrite` fails during ordinary writing.
- Keep qpdf's relaxed final-flush rule inside the JSON side-file adapter. General `Write` and ordinary write failures stay strict.
- The missing-`/Pages` plus `--json-key=pages` divergence remains separate; do not alter it in this stack.
- Every PR must report 100% patch coverage from its clean committed `HEAD` against its direct parent branch.
- Use only non-interactive stack commands. Final publication is `gh stack submit --auto --remote origin`.

## File Structure

| File | Responsibility in this plan | Stack layer |
|---|---|---|
| `crates/flpdf/src/json/value.rs` | Shared value tags, live container access, shared `Fn` blob producer | #559 |
| `crates/flpdf/src/json/writer.rs` | Live dictionary/array timing and explicit-success Base64 adapter | #559 |
| `crates/flpdf/tests/json_tests.rs` | Writer mutation, callback re-entry, and partial blob regressions | #559 |
| `crates/flpdf/src/json/message.rs` | Exact diagnostic byte container and lossy human display | #560 |
| `crates/flpdf/src/json/parser.rs` | Byte-native parser error construction | #560 |
| `crates/flpdf/src/json/mod.rs` | Public exports and private module wiring | #560, #561, #562 |
| `crates/flpdf/tests/json_parse_tests.rs` | Exact high-bit parser diagnostics | #560 |
| `crates/flpdf/src/json/handler.rs` | Cloneable live shared handler and byte-native handler errors | #561 |
| `crates/flpdf/src/json/schema.rs` | Byte-native schema paths and error collection | #561 |
| `crates/flpdf/tests/json_handler_tests.rs` | Same-callback re-entry, live replacement lifetime, byte errors | #561 |
| `crates/flpdf/tests/json_schema_tests.rs` | Exact non-UTF-8 schema messages and migrated error assertions | #561 |
| `crates/flpdf/src/json/stdio.rs` | Measured 4 KiB qpdf stdio-compatible side-file adapter | #562 |
| `crates/flpdf/src/json_inspect.rs` | Side-file adapter integration and finish timing | #562 |
| `crates/flpdf-cli/tests/cli_json.rs` | Linux `/dev/full` live qpdf comparison | #562 |
| `docs/qpdf-module-doc-index.md` | Generated entries for `json/message.rs` and `json/stdio.rs` | #560, #562 |

---

### Task 1: Reopen the Beads work and validate the existing stack

**Files:**
- Inspect: `docs/superpowers/specs/2026-07-27-qpdf-json-definitive-parity-design.md`
- Inspect: `crates/flpdf/src/json/`
- Inspect: `crates/flpdf/tests/json_*`

**Interfaces:**
- Consumes: the existing stack `main → core → parser → validation → integration`.
- Produces: a clean, claimed bottom-layer starting point without changing stack topology.

- [ ] **Step 1: Verify the worktree and stack before mutation**

Run:

```bash
git status --short --branch
gh stack view --json
git branch --format='%(refname:short) %(objectname:short)' \
  | rg 'feature/flpdf-qxba-6|^main '
```

Expected: the worktree is clean; the stack is exactly:

```text
main
feature/flpdf-qxba-6-1-json-core
feature/flpdf-qxba-6-2-json-parser
feature/flpdf-qxba-6-3-json-validation
feature/flpdf-qxba-6-4-json-integration
```

The top branch contains the approved design and this plan. Do not push yet.

- [ ] **Step 2: Reopen and claim the component Beads**

Run:

```bash
bd reopen flpdf-qxba.6 flpdf-qxba.6.1 flpdf-qxba.6.2 flpdf-qxba.6.3 flpdf-qxba.6.4 \
  --reason="Definitive qpdf 11.9.0 differential probes found live-dispatch, raw-byte diagnostic, blob-finalization, and side-file parity gaps in open PRs #559-#562."
bd update flpdf-qxba.6 --claim
bd update flpdf-qxba.6.1 --claim
```

Expected: the parent and four layer tasks are open; `.6` and `.6.1` are assigned for active work.

- [ ] **Step 3: Check out the bottom branch non-interactively**

Run:

```bash
gh stack checkout feature/flpdf-qxba-6-1-json-core
git status --short --branch
```

Expected: current branch is `feature/flpdf-qxba-6-1-json-core`, with no uncommitted files.

- [ ] **Step 4: Establish the focused baseline**

Run:

```bash
cargo test -p flpdf --test json_tests
```

Expected: the pre-change suite passes. Record the count in the Bead notes; the new RED tests start from this known-green head.

---

### Task 2: Make dictionary and array writer timing live

**Files:**
- Modify: `crates/flpdf/tests/json_tests.rs`
- Modify: `crates/flpdf/src/json/value.rs`
- Modify: `crates/flpdf/src/json/writer.rs`

**Interfaces:**
- Consumes: `Json`, `next_dictionary_item_after`, `Write`, and the existing incremental writer methods.
- Produces: `Json::array_items_snapshot() -> Option<Vec<Json>>` and `Json::dictionary_item_for_write(&[u8]) -> Option<Json>` for borrow-free writer traversal.

- [ ] **Step 1: Add a sink that mutates after observing exact output bytes**

Add this test helper near `MaxWriteSink`:

```rust
struct CallbackSink<F> {
    bytes: Vec<u8>,
    callback: F,
}

impl<F: FnMut(&[u8])> io::Write for CallbackSink<F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        (self.callback)(&self.bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
```

Add three regressions:

```rust
#[test]
fn dictionary_writer_rereads_value_after_key_output() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    let alias = dictionary.clone();
    let replaced = Rc::new(Cell::new(false));
    let mut sink = CallbackSink {
        bytes: Vec::new(),
        callback: {
            let replaced = replaced.clone();
            move |bytes: &[u8]| {
                if !replaced.get() && bytes.ends_with(b"\"a\": ") {
                    replaced.set(true);
                    alias
                        .add_dictionary_member(b"a", Json::make_int(99))
                        .unwrap();
                }
            }
        },
    };

    dictionary.write(&mut sink, 0).unwrap();

    assert!(replaced.get());
    assert_eq!(sink.bytes, b"{\n  \"a\": 99\n}");
}

#[test]
fn dictionary_writer_starts_iteration_after_opening_brace() {
    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"a", Json::make_int(1))
        .unwrap();
    let alias = dictionary.clone();
    let inserted = Rc::new(Cell::new(false));
    let mut sink = CallbackSink {
        bytes: Vec::new(),
        callback: {
            let inserted = inserted.clone();
            move |bytes: &[u8]| {
                if !inserted.get() && bytes == b"{" {
                    inserted.set(true);
                    alias
                        .add_dictionary_member(b"b", Json::make_int(2))
                        .unwrap();
                }
            }
        },
    };

    dictionary.write(&mut sink, 0).unwrap();

    assert_eq!(sink.bytes, b"{\n  \"a\": 1,\n  \"b\": 2\n}");
}

#[test]
fn array_writer_snapshots_elements_after_opening_bracket() {
    let array = Json::make_array();
    array.add_array_element(Json::make_int(1)).unwrap();
    let alias = array.clone();
    let inserted = Rc::new(Cell::new(false));
    let mut sink = CallbackSink {
        bytes: Vec::new(),
        callback: {
            let inserted = inserted.clone();
            move |bytes: &[u8]| {
                if !inserted.get() && bytes == b"[" {
                    inserted.set(true);
                    alias.add_array_element(Json::make_int(2)).unwrap();
                }
            }
        },
    };

    array.write(&mut sink, 0).unwrap();

    assert_eq!(sink.bytes, b"[\n  1,\n  2\n]");
}
```

- [ ] **Step 2: Run the RED tests**

Run:

```bash
cargo test -p flpdf --test json_tests \
  dictionary_writer_rereads_value_after_key_output
cargo test -p flpdf --test json_tests \
  array_writer_snapshots_elements_after_opening_bracket
```

Expected:

- dictionary test fails with `1` where `99` is expected;
- array test fails with `[1]` where `[1,2]` is expected.

The opening-brace test may already pass because dictionary traversal is partly live; keep it as the timing contract.

- [ ] **Step 3: Replace pre-open array contents with a container tag**

In `value.rs`, change both snapshot enums so `Array` has no payload:

```rust
pub(crate) enum ValueSnapshot {
    Dictionary,
    Array,
    String(Vec<u8>),
    Number(Vec<u8>),
    Bool(bool),
    Null,
    Blob(BlobWriter),
}

pub(crate) enum ContainerOrBlobSnapshot {
    Dictionary,
    Array,
    Blob(BlobWriter),
}
```

Update `into_container_or_blob` and `value_snapshot` accordingly:

```rust
impl ValueSnapshot {
    pub(crate) fn into_container_or_blob(self) -> Option<ContainerOrBlobSnapshot> {
        match self {
            Self::Dictionary => Some(ContainerOrBlobSnapshot::Dictionary),
            Self::Array => Some(ContainerOrBlobSnapshot::Array),
            Self::Blob(writer) => Some(ContainerOrBlobSnapshot::Blob(writer)),
            Self::String(_) | Self::Number(_) | Self::Bool(_) | Self::Null => None,
        }
    }
}

// Inside `value_snapshot`:
Value::Dictionary { parsed_keys, .. } => {
    let _ = parsed_keys;
    ValueSnapshot::Dictionary
},
Value::Array(_) => ValueSnapshot::Array,
```

Add short-borrow accessors:

```rust
pub(crate) fn dictionary_item_for_write(&self, encoded_key: &[u8]) -> Option<Json> {
    let members = self.0.as_ref()?.borrow();
    let Value::Dictionary {
        members: dictionary,
        ..
    } = &members.value
    else {
        return None;
    };
    dictionary.get(encoded_key).cloned()
}

pub(crate) fn array_items_snapshot(&self) -> Option<Vec<Json>> {
    let members = self.0.as_ref()?.borrow();
    let Value::Array(values) = &members.value else {
        return None;
    };
    Some(values.clone())
}
```

- [ ] **Step 4: Re-read dictionary values and obtain arrays after `[`**

In `writer.rs`, replace the dictionary item call with:

```rust
let selected = value;
Json::write_dictionary_key(out, &mut first, &key, depth + 1)?;
let value = owner
    .dictionary_item_for_write(&key)
    .unwrap_or(selected);
value.write(out, depth + 1)?;
previous_key = Some(key);
```

Replace the array arm with:

```rust
ContainerOrBlobSnapshot::Array => {
    let mut first = true;
    Json::write_array_open(out, &mut first, depth)?;
    let values = owner
        .array_items_snapshot()
        .expect("array tag was obtained from the same Json handle");
    for value in &values {
        Json::write_array_item(out, &mut first, value, depth + 1)?;
    }
    Json::write_array_close(out, first, depth)
}
```

Do not hold a `Members` borrow while any `write_*` method touches the sink.

- [ ] **Step 5: Run the focused suite**

Run:

```bash
cargo test -p flpdf --test json_tests
```

Expected: all writer tests pass, including the three new timing contracts and the existing future-key mutation test.

- [ ] **Step 6: Commit the live-container writer change**

Run:

```bash
git add crates/flpdf/src/json/value.rs crates/flpdf/src/json/writer.rs \
  crates/flpdf/tests/json_tests.rs
git commit -m "fix(json): observe live writer mutations"
```

Expected: one focused #559 commit; worktree clean.

---

### Task 3: Make blob callbacks re-entrant and finalize Base64 only on success

**Files:**
- Modify: `crates/flpdf/tests/json_tests.rs`
- Modify: `crates/flpdf/src/json/value.rs`
- Modify: `crates/flpdf/src/json/writer.rs`

**Interfaces:**
- Consumes: the existing `Json::make_blob` and `Json::write`.
- Produces: `BlobWriter = Rc<dyn Fn(&mut dyn Write) -> io::Result<()>>` and a private `Base64Writer` whose `finish` is the only tail-emission path.

- [ ] **Step 1: Add same-callback re-entry and partial-output regressions**

Add:

```rust
#[test]
fn blob_callback_can_reenter_the_same_callback() {
    let holder = Rc::new(RefCell::new(None::<Json>));
    let weak_holder = Rc::downgrade(&holder);
    let nested = Rc::new(Cell::new(false));
    let blob = Json::make_blob({
        let nested = nested.clone();
        move |out| {
            if nested.replace(true) {
                out.write_all(b"x")?;
            } else {
                let blob = weak_holder
                    .upgrade()
                    .expect("holder is alive")
                    .borrow()
                    .as_ref()
                    .expect("blob is installed")
                    .clone();
                blob.write(out, 0)?;
            }
            Ok(())
        }
    });
    *holder.borrow_mut() = Some(blob.clone());

    assert_eq!(blob.unparse().unwrap(), b"\"ImVBPT0i\"");
    holder.borrow_mut().take();
}

#[test]
fn blob_error_does_not_finalize_a_partial_base64_group() {
    for (raw, expected) in [
        (b"x".as_slice(), b"\"".as_slice()),
        (b"abcd".as_slice(), b"\"YWJj".as_slice()),
    ] {
        let raw = raw.to_vec();
        let blob = Json::make_blob(move |out| {
            out.write_all(&raw)?;
            Err(io::Error::other("producer failed"))
        });
        let mut bytes = Vec::new();

        let error = blob.write(&mut bytes, 0).unwrap_err();

        assert_eq!(error.to_string(), "producer failed");
        assert_eq!(bytes, expected);
    }
}
```

- [ ] **Step 2: Run the RED tests**

Run:

```bash
cargo test -p flpdf --test json_tests blob_callback_can_reenter_the_same_callback
cargo test -p flpdf --test json_tests \
  blob_error_does_not_finalize_a_partial_base64_group
```

Expected:

- re-entry panics with `already borrowed: BorrowMutError`;
- the one-byte failure contains unwanted `eA==`, and the four-byte failure contains unwanted tail padding.

- [ ] **Step 3: Change blob storage from `FnMut` to shared `Fn`**

In `value.rs`, use:

```rust
type BlobWriter = Rc<dyn Fn(&mut dyn io::Write) -> io::Result<()>>;
```

Change the constructor to:

```rust
pub fn make_blob(callback: impl Fn(&mut dyn io::Write) -> io::Result<()> + 'static) -> Self {
    Self::with_value(Value::Blob(Rc::new(callback)))
}
```

Update test-only direct `Value::Blob` construction in `value.rs` to use `Rc::new(callback)` without `RefCell<Box<_>>`.

- [ ] **Step 4: Replace `EncoderWriter` with an explicit-success adapter**

Remove `base64::write::EncoderWriter` and retain:

```rust
use base64::{engine::general_purpose::STANDARD, Engine as _};
```

Add this private adapter in `writer.rs`:

```rust
struct Base64Writer<'a, W: Write + ?Sized> {
    out: &'a mut W,
    pending: [u8; 3],
    pending_len: usize,
}

impl<'a, W: Write + ?Sized> Base64Writer<'a, W> {
    fn new(out: &'a mut W) -> Self {
        Self {
            out,
            pending: [0; 3],
            pending_len: 0,
        }
    }

    fn write_group(&mut self, group: &[u8; 3]) -> io::Result<()> {
        let mut encoded = [0; 4];
        STANDARD
            .encode_slice(group, &mut encoded)
            .expect("three input bytes always encode into four output bytes");
        self.out.write_all(&encoded)
    }

    fn finish(self) -> io::Result<()> {
        if self.pending_len != 0 {
            let encoded = STANDARD.encode(&self.pending[..self.pending_len]);
            self.out.write_all(encoded.as_bytes())?;
        }
        Ok(())
    }
}
```

Implement `Write` so it:

1. fills and emits an existing pending group;
2. emits every complete three-byte group with `write_group`;
3. stores only the final zero-to-two bytes;
4. returns the original input length after successful output; and
5. delegates `flush` to `self.out.flush()`.

Use this exact structure:

```rust
impl<W: Write + ?Sized> Write for Base64Writer<'_, W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let input_len = input.len();

        if self.pending_len != 0 {
            let needed = 3 - self.pending_len;
            let copied = needed.min(input.len());
            self.pending[self.pending_len..self.pending_len + copied]
                .copy_from_slice(&input[..copied]);
            self.pending_len += copied;
            input = &input[copied..];
            if self.pending_len == 3 {
                let group = self.pending;
                self.write_group(&group)?;
                self.pending_len = 0;
            }
        }

        while input.len() >= 3 {
            let group: [u8; 3] = input[..3]
                .try_into()
                .expect("three-byte slice has fixed length");
            self.write_group(&group)?;
            input = &input[3..];
        }

        self.pending[..input.len()].copy_from_slice(input);
        self.pending_len = input.len();
        Ok(input_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
```

Change the blob arm to:

```rust
out.write_all(b"\"")?;
let mut encoder = Base64Writer::new(&mut *out);
writer(&mut encoder)?;
encoder.finish()?;
out.write_all(b"\"")
```

Do not add a `Drop` implementation.

- [ ] **Step 5: Run blob and complete core tests**

Run:

```bash
cargo test -p flpdf --test json_tests blob_
cargo test -p flpdf --test json_tests
cargo test -p flpdf
```

Expected: all pass; the existing large streaming test proves the adapter does not issue one full encoded write.

- [ ] **Step 6: Commit the blob boundary change**

Run:

```bash
git add crates/flpdf/src/json/value.rs crates/flpdf/src/json/writer.rs \
  crates/flpdf/tests/json_tests.rs
git commit -m "fix(json): match qpdf blob callback boundaries"
```

Expected: clean #559 branch with two new commits.

---

### Task 4: Gate #559 and restack onto the parser layer

**Files:**
- Verify: all #559 changes
- Update through Beads only: `flpdf-qxba.6.1`

**Interfaces:**
- Consumes: Tasks 2–3 committed on `feature/flpdf-qxba-6-1-json-core`.
- Produces: a 100%-covered core layer and rebased upper branches.

- [ ] **Step 1: Run #559 quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_tests
cargo test -p flpdf
python3 scripts/qpdf-module-docs.py --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: every command exits zero.

- [ ] **Step 2: Measure committed-HEAD patch coverage against `main`**

Run from a clean tree:

```bash
git status --short
scripts/patch-coverage.sh --base main
```

Expected: changed executable lines for #559 report 100%. If a reachable line is uncovered, add a focused behavioral assertion, commit it as:

```bash
git add crates/flpdf/tests/json_tests.rs
git commit -m "test(json): complete live writer branch coverage"
scripts/patch-coverage.sh --base main
```

Do not use a reasonless `cov:ignore`.

- [ ] **Step 3: Record evidence and rebase the upstack branches**

Run:

```bash
bd update flpdf-qxba.6.1 --append-notes \
  "Definitive parity follow-up: live dictionary/array writer timing and re-entrant, success-finalized blob encoding pass focused/full tests and 100% patch coverage vs main."
gh stack rebase --upstack --no-trunk
gh stack checkout feature/flpdf-qxba-6-2-json-parser
bd update flpdf-qxba.6.2 --claim
```

Expected: parser, validation, and integration branches are replayed over the new core head; current branch is the parser layer.

---

### Task 5: Introduce `JsonMessage` and preserve parser error bytes

**Files:**
- Create: `crates/flpdf/src/json/message.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Modify: `crates/flpdf/src/json/value.rs`
- Modify: `crates/flpdf/src/json/parser.rs`
- Modify: `crates/flpdf/tests/json_parse_tests.rs`

**Interfaces:**
- Consumes: `JsonError` and qpdf's byte-oriented parser messages.
- Produces: public `JsonMessage`, `JsonError::{Type, Parse}(JsonMessage)`, and exact raw-byte parser errors for #561.

- [ ] **Step 1: Add RED high-bit parser assertions that compile with `String` and `JsonMessage`**

Import `JsonError` and add:

```rust
fn parse_error_bytes(input: &[u8]) -> Vec<u8> {
    match Json::parse(input).unwrap_err() {
        JsonError::Parse(message) => message.as_bytes().to_vec(),
        other => panic!("expected parse error, got {other:?}"),
    }
}

#[test]
fn parser_preserves_raw_bytes_in_lexical_diagnostics() {
    for (input, expected) in [
        (
            b"\x80".as_slice(),
            b"JSON: offset 0: unexpected character \x80".as_slice(),
        ),
        (
            b"a\xff".as_slice(),
            b"JSON: offset 1: keyword: unexpected character \xff".as_slice(),
        ),
        (
            b"1\x80".as_slice(),
            b"JSON: offset 1: numeric literal: unexpected character \x80".as_slice(),
        ),
        (
            b"\"\\\xff\"".as_slice(),
            b"JSON: offset 2: invalid character after backslash: \xff".as_slice(),
        ),
        (
            b"true \"\xff\"".as_slice(),
            b"JSON: offset 8: material follows end of object: \xff".as_slice(),
        ),
    ] {
        assert_eq!(parse_error_bytes(input), expected, "{input:?}");
    }
}
```

- [ ] **Step 2: Run the parser RED test**

Run:

```bash
cargo test -p flpdf --test json_parse_tests \
  parser_preserves_raw_bytes_in_lexical_diagnostics
```

Expected: failures show UTF-8 expansion (`c2 80`) or replacement bytes (`ef bf bd`) instead of the original byte.

- [ ] **Step 3: Create the exact-byte message type**

Create `message.rs`:

```rust
//! qpdf correspondence: JSON.cc and JSONHandler.cc use byte-oriented std::string diagnostics.

use std::fmt;

#[derive(Clone, Eq, PartialEq)]
pub struct JsonMessage(Vec<u8>);

impl JsonMessage {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl From<&str> for JsonMessage {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}

impl From<String> for JsonMessage {
    fn from(value: String) -> Self {
        Self(value.into_bytes())
    }
}

impl From<Vec<u8>> for JsonMessage {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl fmt::Debug for JsonMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("JsonMessage").field(&self.0).finish()
    }
}

impl fmt::Display for JsonMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&String::from_utf8_lossy(&self.0))
    }
}
```

Wire it in `mod.rs`:

```rust
mod message;
pub use message::JsonMessage;
```

Change `JsonError`:

```rust
Type(JsonMessage),
Parse(JsonMessage),
```

Import `JsonMessage` in `value.rs`; existing ASCII `.into()` constructors continue to work.

- [ ] **Step 4: Build raw parser messages without `char` or lossy conversion**

Import `JsonMessage` in `parser.rs` and change:

```rust
fn error(&self, message: impl Into<JsonMessage>) -> JsonError {
    JsonError::Parse(message.into())
}

fn error_with_byte(&self, prefix: String, byte: u8) -> JsonError {
    let mut message = prefix.into_bytes();
    message.push(byte);
    self.error(message)
}

fn error_with_bytes(&self, prefix: String, bytes: &[u8]) -> JsonError {
    let mut message = prefix.into_bytes();
    message.extend_from_slice(bytes);
    self.error(message)
}
```

Use `error_with_byte` at the top-level unexpected-character, keyword,
number, and post-backslash sites. Use `error_with_bytes` for:

```rust
self.error_with_byte(
    format!("JSON: offset {offset}: unexpected character "),
    byte,
)

self.error_with_byte(
    format!("JSON: offset {offset}: keyword: unexpected character "),
    byte,
)

self.error_with_byte(
    format!("JSON: offset {offset}: numeric literal: unexpected character "),
    byte,
)

self.error_with_byte(
    format!("JSON: offset {offset}: invalid character after backslash: "),
    byte,
)

self.error_with_bytes(
    format!(
        "JSON: offset {}: material follows end of object: ",
        self.offset()
    ),
    &self.token,
)

self.error_with_bytes(
    format!("JSON: offset {}: invalid keyword ", self.offset()),
    &self.token,
)
```

Delete `printable_byte` and every `String::from_utf8_lossy` in `parser.rs`.
Keep ASCII-only `format!` calls; their `String` converts losslessly into
`JsonMessage`.

- [ ] **Step 5: Test `JsonMessage` ownership APIs and all parser diagnostics**

Add:

```rust
#[test]
fn json_message_exposes_exact_owned_bytes_and_lossy_display() {
    let message = flpdf::json::JsonMessage::from_bytes(vec![b'x', 0xff]);
    assert_eq!(message.as_bytes(), b"x\xff");
    assert_eq!(message.to_string(), "x\u{fffd}");
    assert_eq!(message.into_bytes(), b"x\xff");
}
```

Run:

```bash
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf --test json_tests
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
```

Expected: all pass; existing ASCII `Display` assertions are unchanged and the
generated index contains `json/message.rs`.

- [ ] **Step 6: Commit the parser diagnostic layer**

Run:

```bash
git add crates/flpdf/src/json/message.rs crates/flpdf/src/json/mod.rs \
  crates/flpdf/src/json/value.rs crates/flpdf/src/json/parser.rs \
  crates/flpdf/tests/json_parse_tests.rs docs/qpdf-module-doc-index.md
git commit -m "fix(json): preserve parser diagnostic bytes"
```

Expected: clean parser branch.

---

### Task 6: Gate #560 and restack onto validation

**Files:**
- Verify: all #560 changes
- Update through Beads only: `flpdf-qxba.6.2`

**Interfaces:**
- Consumes: Task 5 committed on the parser branch.
- Produces: a 100%-covered parser layer and rebased validation/integration branches.

- [ ] **Step 1: Run parser-layer gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf
python3 scripts/qpdf-module-docs.py --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: all pass.

- [ ] **Step 2: Measure #560 coverage against the core branch**

Run:

```bash
git status --short
scripts/patch-coverage.sh --base feature/flpdf-qxba-6-1-json-core
```

Expected: 100% changed-line coverage. Add explicit parser inputs for any reachable uncovered state, commit, and rerun; do not rely on tests added in #561.

- [ ] **Step 3: Record and restack**

Run:

```bash
bd update flpdf-qxba.6.2 --append-notes \
  "Definitive parity follow-up: JsonMessage and raw high-bit parser diagnostics pass exact-byte tests and 100% patch coverage vs the core branch."
gh stack rebase --upstack --no-trunk
gh stack checkout feature/flpdf-qxba-6-3-json-validation
bd update flpdf-qxba.6.3 --claim
```

Expected: current branch is validation and contains `JsonMessage`.

---

### Task 7: Replace handler snapshots with a live cloneable handle

**Files:**
- Modify: `crates/flpdf/src/json/handler.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Modify: `crates/flpdf/tests/json_handler_tests.rs`

**Interfaces:**
- Consumes: `Json`, `JsonMessage`, `Rc`, `Weak`, and `RefCell`.
- Produces: cloneable `JsonHandler`, `WeakJsonHandler`,
  `handle(&self, path: &[u8], value: Json)`, `&self` registration methods, and
  live dispatch with no borrow held during callbacks.

- [ ] **Step 1: Add RED tests against the current shared entry point**

Add a same-callback test using the old API:

```rust
// Update the existing import to `use std::cell::{Cell, RefCell};`.
#[test]
fn same_active_callback_can_reenter_itself_synchronously() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let depth = Rc::new(Cell::new(0));
    let handler = JsonHandler::shared();
    handler.borrow_mut().add_string_handler({
        let weak = Rc::downgrade(&handler);
        let seen = seen.clone();
        let depth = depth.clone();
        move |path, _| {
            seen.borrow_mut().push(path.to_vec());
            if depth.replace(1) == 0 {
                JsonHandler::handle_shared(
                    &weak.upgrade().expect("handler is alive"),
                    b".nested",
                    Json::make_string(b"inner"),
                )
                .unwrap();
            }
        }
    });

    JsonHandler::handle_shared(&handler, b".", Json::make_string(b"outer")).unwrap();

    assert_eq!(&*seen.borrow(), &[b".".to_vec(), b".nested".to_vec()]);
}
```

Add a lifetime probe:

```rust
struct DropProbe(Rc<Cell<usize>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn replacing_an_unselected_target_drops_it_during_dispatch() {
    let drops = Rc::new(Cell::new(0));
    let root = JsonHandler::shared();
    let stale = JsonHandler::shared();
    stale.borrow_mut().add_any_handler({
        let probe = DropProbe(drops.clone());
        move |_, _| {
            let _ = &probe;
        }
    });
    root.borrow_mut()
        .add_dictionary_key_handler(b"b", stale.clone());
    drop(stale);

    let replacement = JsonHandler::shared();
    replacement.borrow_mut().add_number_handler(|_, _| {});
    root.borrow_mut().add_dictionary_handlers(
        {
            let weak = Rc::downgrade(&root);
            let drops = drops.clone();
            move |_, _| {
                weak.upgrade()
                    .expect("root is alive")
                    .borrow_mut()
                    .add_dictionary_key_handler(b"b", replacement.clone());
                assert_eq!(drops.get(), 1);
            }
        },
        |_| {},
    );

    JsonHandler::handle_shared(&root, b".", Json::parse(br#"{"b":1}"#).unwrap()).unwrap();
}
```

- [ ] **Step 2: Run the RED handler tests**

Run:

```bash
cargo test -p flpdf --test json_handler_tests \
  same_active_callback_can_reenter_itself_synchronously
cargo test -p flpdf --test json_handler_tests \
  replacing_an_unselected_target_drops_it_during_dispatch
```

Expected:

- same-callback re-entry panics with `already borrowed: BorrowMutError`;
- the stale target's `DropProbe` remains at zero inside dictionary-start because `HandlerSnapshot` retains it.

- [ ] **Step 3: Replace the handler storage model**

Rewrite the top-level types in `handler.rs` as:

```rust
type JsonCallback = Rc<dyn Fn(&[u8], Json)>;
type BytesCallback = Rc<dyn Fn(&[u8], &[u8])>;
type PathCallback = Rc<dyn Fn(&[u8])>;
type BoolCallback = Rc<dyn Fn(&[u8], bool)>;

#[derive(Default)]
struct Handlers {
    any: Option<JsonCallback>,
    null: Option<PathCallback>,
    string: Option<BytesCallback>,
    number: Option<BytesCallback>,
    boolean: Option<BoolCallback>,
    dictionary_start: Option<JsonCallback>,
    dictionary_end: Option<PathCallback>,
    array_start: Option<JsonCallback>,
    array_end: Option<PathCallback>,
    dictionary_keys: BTreeMap<Vec<u8>, JsonHandler>,
    fallback_dictionary: Option<JsonHandler>,
    array_item: Option<JsonHandler>,
    fallback: Option<JsonHandler>,
}

#[derive(Clone, Default)]
pub struct JsonHandler {
    inner: Rc<RefCell<Handlers>>,
}

#[derive(Clone)]
pub struct WeakJsonHandler {
    inner: std::rc::Weak<RefCell<Handlers>>,
}
```

Implement:

```rust
impl JsonHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn downgrade(&self) -> WeakJsonHandler {
        WeakJsonHandler {
            inner: Rc::downgrade(&self.inner),
        }
    }
}

impl WeakJsonHandler {
    pub fn upgrade(&self) -> Option<JsonHandler> {
        self.inner.upgrade().map(|inner| JsonHandler { inner })
    }
}
```

`add_any_handler`, `add_null_handler`, `add_string_handler`,
`add_number_handler`, `add_bool_handler`, `add_dictionary_handlers`,
`add_dictionary_key_handler`, `add_fallback_dictionary_handler`,
`add_array_handlers`, and `add_fallback_handler` all take `&self`.
Implement their exact signatures and storage as:

```rust
pub fn add_any_handler(&self, callback: impl Fn(&[u8], Json) + 'static) {
    self.inner.borrow_mut().any = Some(Rc::new(callback));
}

pub fn add_null_handler(&self, callback: impl Fn(&[u8]) + 'static) {
    self.inner.borrow_mut().null = Some(Rc::new(callback));
}

pub fn add_string_handler(
    &self,
    callback: impl Fn(&[u8], &[u8]) + 'static,
) {
    self.inner.borrow_mut().string = Some(Rc::new(callback));
}

pub fn add_number_handler(
    &self,
    callback: impl Fn(&[u8], &[u8]) + 'static,
) {
    self.inner.borrow_mut().number = Some(Rc::new(callback));
}

pub fn add_bool_handler(
    &self,
    callback: impl Fn(&[u8], bool) + 'static,
) {
    self.inner.borrow_mut().boolean = Some(Rc::new(callback));
}

pub fn add_dictionary_handlers(
    &self,
    start: impl Fn(&[u8], Json) + 'static,
    end: impl Fn(&[u8]) + 'static,
) {
    let mut handlers = self.inner.borrow_mut();
    handlers.dictionary_start = Some(Rc::new(start));
    handlers.dictionary_end = Some(Rc::new(end));
}

pub fn add_dictionary_key_handler(
    &self,
    key: impl AsRef<[u8]>,
    handler: JsonHandler,
) {
    self.inner
        .borrow_mut()
        .dictionary_keys
        .insert(key.as_ref().to_vec(), handler);
}

pub fn add_fallback_dictionary_handler(&self, handler: JsonHandler) {
    self.inner.borrow_mut().fallback_dictionary = Some(handler);
}

pub fn add_array_handlers(
    &self,
    start: impl Fn(&[u8], Json) + 'static,
    end: impl Fn(&[u8]) + 'static,
    item: JsonHandler,
) {
    let mut handlers = self.inner.borrow_mut();
    handlers.array_start = Some(Rc::new(start));
    handlers.array_end = Some(Rc::new(end));
    handlers.array_item = Some(item);
}

pub fn add_fallback_handler(&self, handler: JsonHandler) {
    self.inner.borrow_mut().fallback = Some(handler);
}
```

Delete `SharedJsonHandler`, `HandlerTarget`, `HandlerSnapshot`,
`ActiveHandler`, `DispatchContext`, `shared`, and `handle_shared`.

- [ ] **Step 4: Implement live short-borrow dispatch**

For every callback or child handler:

1. borrow `self.inner`;
2. clone only the exact current callback/target;
3. drop the borrow at the end of the expression; and
4. invoke or recurse afterward.

The dictionary item selection must have this shape:

```rust
let target = {
    let handlers = self.inner.borrow();
    handlers
        .dictionary_keys
        .get(key)
        .cloned()
        .or_else(|| handlers.fallback_dictionary.clone())
};
```

The array item target and dictionary/array end callbacks are re-read for every
later dispatch boundary. The general fallback is read only after scalar and
container handling do not return.

Use this complete control flow:

```rust
pub fn handle(
    &self,
    path: &[u8],
    value: Json,
) -> Result<(), JsonHandlerError> {
    let callback = { self.inner.borrow().any.clone() };
    if let Some(callback) = callback {
        callback(path, value);
        return Ok(());
    }

    if value.is_null() {
        let callback = { self.inner.borrow().null.clone() };
        if let Some(callback) = callback {
            callback(path);
            return Ok(());
        }
    }
    if let Some(string) = value.get_string() {
        let callback = { self.inner.borrow().string.clone() };
        if let Some(callback) = callback {
            callback(path, &string);
            return Ok(());
        }
    }
    if let Some(number) = value.get_number() {
        let callback = { self.inner.borrow().number.clone() };
        if let Some(callback) = callback {
            callback(path, &number);
            return Ok(());
        }
    }
    if let Some(boolean) = value.get_bool() {
        let callback = { self.inner.borrow().boolean.clone() };
        if let Some(callback) = callback {
            callback(path, boolean);
            return Ok(());
        }
    }

    if value.is_dictionary() {
        let start = { self.inner.borrow().dictionary_start.clone() };
        if let Some(start) = start {
            start(path, value.clone());
            let mut path_base = path.to_vec();
            if path_base != b"." {
                path_base.push(b'.');
            }
            let mut item_error = None;
            value.for_each_dict_item(|key, item| {
                if item_error.is_some() {
                    return;
                }
                let target = {
                    let handlers = self.inner.borrow();
                    handlers
                        .dictionary_keys
                        .get(key)
                        .cloned()
                        .or_else(|| handlers.fallback_dictionary.clone())
                };
                let mut item_path = path_base.clone();
                item_path.extend_from_slice(key);
                item_error = match target {
                    Some(target) => target.handle(&item_path, item).err(),
                    None => Some(unexpected_key(key, path)),
                };
            });
            if let Some(error) = item_error {
                return Err(error);
            }
            let end = self
                .inner
                .borrow()
                .dictionary_end
                .clone()
                .expect("dictionary start and end handlers are registered together");
            end(path);
            return Ok(());
        }
    }

    if value.is_array() {
        let start = { self.inner.borrow().array_start.clone() };
        if let Some(start) = start {
            start(path, value.clone());
            let mut items = Vec::new();
            value.for_each_array_item(|item| items.push(item));
            for (index, item) in items.into_iter().enumerate() {
                let target = self
                    .inner
                    .borrow()
                    .array_item
                    .clone()
                    .expect("array handlers are registered together");
                let mut item_path = path.to_vec();
                item_path.extend_from_slice(format!("[{index}]").as_bytes());
                target.handle(&item_path, item)?;
            }
            let end = self
                .inner
                .borrow()
                .array_end
                .clone()
                .expect("array start and end handlers are registered together");
            end(path);
            return Ok(());
        }
    }

    let fallback = { self.inner.borrow().fallback.clone() };
    if let Some(fallback) = fallback {
        return fallback.handle(path, value);
    }

    Err(unexpected_type(path))
}
```

Keep path construction in bytes:

```rust
let mut item_path = path_base.clone();
item_path.extend_from_slice(key);
```

and:

```rust
let mut item_path = path.to_vec();
item_path.extend_from_slice(format!("[{index}]").as_bytes());
```

- [ ] **Step 5: Migrate the handler test suite to the new public API**

Apply these exact transformations throughout `json_handler_tests.rs`; the
`add_any_handler` example applies individually to every registration method
named in Step 3:

```text
JsonHandler::shared()
→ JsonHandler::new()

handler.borrow_mut().add_any_handler(callback)
→ handler.add_any_handler(callback)

JsonHandler::handle_shared(&handler, path, value)
→ handler.handle(path, value)

Rc::downgrade(&handler)
→ handler.downgrade()

weak.upgrade().unwrap().borrow_mut().add_any_handler(callback)
→ weak.upgrade().unwrap().add_any_handler(callback)
```

Remove `mut` from handler bindings that no longer require it. Keep `Rc` imports
used for callback state and `DropProbe`.

Translate the same-callback RED test to `handler.downgrade()`,
`handler.add_string_handler`, and `handler.handle(path, value)`. Translate the
lifetime RED test to `root.downgrade()`, `root.add_dictionary_handlers`,
`root.add_dictionary_key_handler`, and `root.handle(path, value)`.

Export:

```rust
pub use handler::{JsonHandler, JsonHandlerError, WeakJsonHandler};
```

- [ ] **Step 6: Run the full handler suite**

Run:

```bash
cargo test -p flpdf --test json_handler_tests
```

Expected: all existing live-reconfiguration, recursive fallback, cycle,
ordering, new same-callback, and Drop timing tests pass without `RefCell`
panics.

- [ ] **Step 7: Commit the handler architecture**

Run:

```bash
git add crates/flpdf/src/json/handler.rs crates/flpdf/src/json/mod.rs \
  crates/flpdf/tests/json_handler_tests.rs
git commit -m "refactor(json): make handlers live shared handles"
```

Expected: a reviewable architecture commit before diagnostic migration.

---

### Task 8: Preserve schema and handler error bytes

**Files:**
- Modify: `crates/flpdf/src/json/handler.rs`
- Modify: `crates/flpdf/src/json/schema.rs`
- Modify: `crates/flpdf/tests/json_handler_tests.rs`
- Modify: `crates/flpdf/tests/json_schema_tests.rs`

**Interfaces:**
- Consumes: `JsonMessage` from #560 and the cloneable handler from Task 7.
- Produces: `JsonHandlerError(JsonMessage)` and
  `check_schema(&self, schema: &Json, errors: &mut Vec<JsonMessage>)`.

- [ ] **Step 1: Add RED byte assertions without depending on `Display`**

Add to `json_handler_tests.rs`:

```rust
#[test]
fn handler_errors_preserve_non_utf8_key_and_path_bytes() {
    let handler = JsonHandler::new();
    handler.add_dictionary_handlers(|_, _| {}, |_| {});

    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"\xff", Json::make_null())
        .unwrap();
    let error = handler.handle(b".\x80", dictionary).unwrap_err();

    assert_eq!(
        error.0.as_bytes(),
        b"JSON handler found unexpected key \xff in object at .\x80"
    );

    let scalar = JsonHandler::new();
    let error = scalar
        .handle(b".\xff", Json::make_null())
        .unwrap_err();
    assert_eq!(
        error.0.as_bytes(),
        b"JSON handler: value at .\xff is not of expected type"
    );
}
```

Add to `json_schema_tests.rs`:

```rust
#[test]
fn schema_errors_preserve_non_utf8_keys_in_paths_and_messages() {
    let schema = Json::make_dictionary();
    schema
        .add_dictionary_member(b"\xff", Json::make_string(b"value"))
        .unwrap();
    let value = Json::make_dictionary();
    let mut errors = Vec::new();

    assert!(!value.check_schema(&schema, &mut errors));
    assert_eq!(
        errors[0].as_bytes(),
        b"top-level object: key \"\xff\" is present in schema but missing in object"
    );

    let pattern = Json::make_dictionary();
    pattern
        .add_dictionary_member(b"<item>", schema)
        .unwrap();
    let nested = Json::make_dictionary();
    nested
        .add_dictionary_member(b"\x80", Json::make_dictionary())
        .unwrap();
    let mut errors = Vec::new();
    assert!(!nested.check_schema(&pattern, &mut errors));
    assert_eq!(
        errors[0].as_bytes(),
        b"json key \".\x80\": key \"\xff\" is present in schema but missing in object"
    );
}
```

- [ ] **Step 2: Run RED byte tests**

Run:

```bash
cargo test -p flpdf --test json_handler_tests \
  handler_errors_preserve_non_utf8_key_and_path_bytes
cargo test -p flpdf --test json_schema_tests \
  schema_errors_preserve_non_utf8_keys_in_paths_and_messages
```

Expected: replacement bytes appear in both suites.

- [ ] **Step 3: Change handler errors to `JsonMessage`**

Use:

```rust
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("{0}")]
pub struct JsonHandlerError(pub JsonMessage);
```

Build errors by appending raw slices:

```rust
fn unexpected_key(key: &[u8], path: &[u8]) -> JsonHandlerError {
    let mut message = b"JSON handler found unexpected key ".to_vec();
    message.extend_from_slice(key);
    message.extend_from_slice(b" in object at ");
    message.extend_from_slice(path);
    JsonHandlerError(JsonMessage::from_bytes(message))
}

fn unexpected_type(path: &[u8]) -> JsonHandlerError {
    let mut message = b"JSON handler: value at ".to_vec();
    message.extend_from_slice(path);
    message.extend_from_slice(b" is not of expected type");
    JsonHandlerError(JsonMessage::from_bytes(message))
}
```

Remove both lossy conversions from `handler.rs`.

- [ ] **Step 4: Change schema prefixes and errors to bytes**

Change public signatures to:

```rust
pub fn check_schema(&self, schema: &Json, errors: &mut Vec<JsonMessage>) -> bool
pub fn check_schema_with_flags(
    &self,
    schema: &Json,
    flags: SchemaFlags,
    errors: &mut Vec<JsonMessage>,
) -> bool
```

Change recursive `prefix` from `&str` to `&[u8]`. Use:

```rust
fn append_path(prefix: &[u8], key: &[u8]) -> Vec<u8> {
    let mut path = prefix.to_vec();
    path.push(b'.');
    path.extend_from_slice(key);
    path
}

fn described_prefix(prefix: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        b"top-level object".to_vec()
    } else {
        let mut description = b"json key \"".to_vec();
        description.extend_from_slice(prefix);
        description.push(b'"');
        description
    }
}

fn key_error(prefix: &[u8], key: &[u8], suffix: &[u8]) -> JsonMessage {
    let mut message = described_prefix(prefix);
    message.extend_from_slice(b": key \"");
    message.extend_from_slice(key);
    message.extend_from_slice(b"\" ");
    message.extend_from_slice(suffix);
    JsonMessage::from_bytes(message)
}

fn described_error(prefix: &[u8], suffix: &[u8]) -> JsonMessage {
    let mut message = described_prefix(prefix);
    message.extend_from_slice(suffix);
    JsonMessage::from_bytes(message)
}
```

Use `key_error` with exact suffixes:

```rust
b"is present in schema but missing in object"
b"is not present in schema but appears in object"
```

For array indexes, append `b'.'` plus `index.to_string().as_bytes()` to a
`Vec<u8>`. Build the fixed errors with:

```rust
described_error(prefix, b" is supposed to be a dictionary")

let mut suffix = b" is supposed to be an array of length ".to_vec();
suffix.extend_from_slice(schema_array.len().to_string().as_bytes());
described_error(prefix, &suffix)

described_error(
    prefix,
    b" schema value is not dictionary, array, or string",
)
```

Delete `key_name` and every lossy conversion in `schema.rs`.

- [ ] **Step 5: Migrate all schema assertions to byte APIs**

Add:

```rust
fn error_bytes(errors: &[flpdf::json::JsonMessage]) -> Vec<&[u8]> {
    errors.iter().map(|error| error.as_bytes()).collect()
}
```

Replace every direct `Vec<String>` equality with `error_bytes`. For example:

```rust
assert_eq!(
    error_bytes(&errors),
    [b"top-level object is supposed to be a dictionary".as_slice()]
);
```

Preserve each existing ASCII expected literal byte-for-byte in its converted
assertion. Replace:

```rust
let mut errors = vec!["previous error".to_owned()];
```

with:

```rust
let mut errors = vec![flpdf::json::JsonMessage::from("previous error")];
```

Change `errors.last().unwrap()` string comparisons to:

```rust
assert_eq!(
    errors.last().unwrap().as_bytes(),
    b"top-level object: key \"x\" is not present in schema but appears in object"
);
```

Update direct handler error construction to:

```rust
flpdf::json::JsonHandlerError(
    flpdf::json::JsonMessage::from(
        "JSON handler: value at . is not of expected type"
    )
)
```

- [ ] **Step 6: Run handler and schema suites**

Run:

```bash
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf --test json_schema_tests
cargo test -p flpdf
```

Expected: exact raw-byte regressions and all prior ASCII behavior pass.

- [ ] **Step 7: Commit byte-native validation errors**

Run:

```bash
git add crates/flpdf/src/json/handler.rs crates/flpdf/src/json/schema.rs \
  crates/flpdf/tests/json_handler_tests.rs crates/flpdf/tests/json_schema_tests.rs
git commit -m "fix(json): preserve validation diagnostic bytes"
```

Expected: clean validation branch with architecture and diagnostic commits.

---

### Task 9: Gate #561 and restack onto integration

**Files:**
- Verify: all #561 changes
- Update through Beads only: `flpdf-qxba.6.3`

**Interfaces:**
- Consumes: Tasks 7–8.
- Produces: a 100%-covered validation layer and rebased integration branch.

- [ ] **Step 1: Prove the removed API has no remaining consumer**

Run:

```bash
rg -n 'SharedJsonHandler|handle_shared|HandlerSnapshot|ActiveHandler|DispatchContext' \
  crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/src crates/flpdf-cli/tests
```

Expected: no matches. `JsonError` and `JsonHandlerError` have no current CLI
consumer; therefore no unused CLI diagnostic adapter is added. Any future CLI
consumer must write `JsonMessage::as_bytes()` directly.

- [ ] **Step 2: Run validation-layer gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf --test json_schema_tests
cargo test -p flpdf
python3 scripts/qpdf-module-docs.py --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: all pass.

- [ ] **Step 3: Measure #561 coverage against parser**

Run:

```bash
git status --short
scripts/patch-coverage.sh --base feature/flpdf-qxba-6-2-json-parser
```

Expected: 100%. Add direct handler/schema behavior tests for uncovered
reachable branches on this branch, commit, and rerun.

- [ ] **Step 4: Record and restack**

Run:

```bash
bd update flpdf-qxba.6.3 --append-notes \
  "Definitive parity follow-up: cloneable live JsonHandler, same-callback reentry, exact target lifetime, and byte-native schema/handler errors pass all focused tests and 100% patch coverage vs parser."
gh stack rebase --upstack --no-trunk
gh stack checkout feature/flpdf-qxba-6-4-json-integration
bd update flpdf-qxba.6.4 --claim
```

Expected: the approved design and plan commits remain on the top branch after
rebase.

---

### Task 10: Add the measured qpdf stdio side-file adapter

**Files:**
- Create: `crates/flpdf/src/json/stdio.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Generated: `docs/qpdf-module-doc-index.md`

**Interfaces:**
- Consumes: `std::io::Write` and Linux errno values `ENOSPC = 28`, `EBADF = 9`.
- Produces: `pub(crate) QpdfStdioWriter<W: Write>` with `new`, `Write`, and `finish`.

- [ ] **Step 1: Create the module with tests and an intentionally strict skeleton**

Create `stdio.rs` with the correspondence line:

```rust
//! qpdf correspondence: Pl_StdioFile.cc write and finish semantics for JSON side files.
```

Declare:

```rust
use std::io::{self, Write};

const BUFFER_CAPACITY: usize = 4096;
const EBADF_ERRNO: i32 = 9;

pub(crate) struct QpdfStdioWriter<W> {
    inner: W,
    buffer: Vec<u8>,
}

impl<W: Write> QpdfStdioWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(BUFFER_CAPACITY),
        }
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        self.inner.write_all(&self.buffer)?;
        self.buffer.clear();
        self.inner.flush()
    }
}
```

Add a test sink with separately controlled write and flush errno:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ProbeSink {
        bytes: Vec<u8>,
        write_errno: Option<i32>,
        flush_errno: Option<i32>,
    }

    impl Write for ProbeSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(errno) = self.write_errno {
                return Err(io::Error::from_raw_os_error(errno));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if let Some(errno) = self.flush_errno {
                return Err(io::Error::from_raw_os_error(errno));
            }
            Ok(())
        }
    }
}
```

Wire `mod stdio;` and:

```rust
pub(crate) use stdio::QpdfStdioWriter;
```

- [ ] **Step 2: Add boundary and finish-policy tests**

Inside the module tests add:

```rust
#[test]
fn final_enospc_below_stdio_boundary_is_ignored() {
    let sink = ProbeSink {
        write_errno: Some(28),
        ..ProbeSink::default()
    };
    let mut writer = QpdfStdioWriter::new(sink);
    writer.write_all(&vec![b'x'; 4095]).unwrap();
    assert!(writer.finish().is_ok());
}

#[test]
fn enospc_at_stdio_boundary_is_an_ordinary_write_error() {
    let sink = ProbeSink {
        write_errno: Some(28),
        ..ProbeSink::default()
    };
    let mut writer = QpdfStdioWriter::new(sink);
    let error = writer.write_all(&vec![b'x'; 4096]).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(28));
}

#[test]
fn final_ebadf_remains_fatal() {
    let sink = ProbeSink {
        write_errno: Some(EBADF_ERRNO),
        ..ProbeSink::default()
    };
    let mut writer = QpdfStdioWriter::new(sink);
    writer.write_all(b"x").unwrap();
    let error = writer.finish().unwrap_err();
    assert_eq!(error.raw_os_error(), Some(EBADF_ERRNO));
}

#[test]
fn non_ebadf_underlying_flush_error_is_ignored() {
    let sink = ProbeSink {
        flush_errno: Some(28),
        ..ProbeSink::default()
    };
    let mut writer = QpdfStdioWriter::new(sink);
    writer.write_all(b"payload").unwrap();
    assert!(writer.finish().is_ok());
}

#[test]
fn writes_above_boundary_preserve_all_normal_file_bytes() {
    let mut writer = QpdfStdioWriter::new(ProbeSink::default());
    let payload = vec![b'x'; 4097];
    writer.write_all(&payload).unwrap();
    writer.finish().unwrap();
    assert_eq!(writer.inner.bytes, payload);
}
```

- [ ] **Step 3: Run the RED unit tests**

Run:

```bash
cargo test -p flpdf --lib json::stdio::tests
```

Expected: the skeleton does not implement `Write`; after adding a simple
delegating `Write`, the 4,095-byte final `ENOSPC` test still fails. This proves
both the buffering and finish policy are missing.

- [ ] **Step 4: Implement exact measured buffering**

Add:

```rust
impl<W: Write> QpdfStdioWriter<W> {
    fn drain_for_write(&mut self) -> io::Result<()> {
        self.inner.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    fn ignore_unless_ebadf(error: io::Error) -> io::Result<()> {
        if error.raw_os_error() == Some(EBADF_ERRNO) {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        if let Err(error) = self.inner.write_all(&self.buffer) {
            self.buffer.clear();
            return Self::ignore_unless_ebadf(error);
        }
        self.buffer.clear();
        match self.inner.flush() {
            Ok(()) => Ok(()),
            Err(error) => Self::ignore_unless_ebadf(error),
        }
    }
}
```

Implement `Write`:

```rust
impl<W: Write> Write for QpdfStdioWriter<W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let input_len = input.len();

        if !self.buffer.is_empty() {
            let available = BUFFER_CAPACITY - self.buffer.len();
            let copied = available.min(input.len());
            self.buffer.extend_from_slice(&input[..copied]);
            input = &input[copied..];
            if self.buffer.len() == BUFFER_CAPACITY {
                self.drain_for_write()?;
            }
        }

        while input.len() >= BUFFER_CAPACITY {
            self.inner.write_all(&input[..BUFFER_CAPACITY])?;
            input = &input[BUFFER_CAPACITY..];
        }

        self.buffer.extend_from_slice(input);
        Ok(input_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain_for_write()?;
        self.inner.flush()
    }
}
```

Do not implement `Drop`. `Write::flush` remains strict; only `finish` has
qpdf's relaxed non-`EBADF` rule.

- [ ] **Step 5: Run unit tests and regenerate module documentation**

Run:

```bash
cargo test -p flpdf --lib json::stdio::tests
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
```

Expected: all five adapter tests pass and the generated index contains
`json/stdio.rs`.

- [ ] **Step 6: Commit the isolated adapter**

Run:

```bash
git add crates/flpdf/src/json/stdio.rs crates/flpdf/src/json/mod.rs \
  docs/qpdf-module-doc-index.md
git commit -m "feat(json): add qpdf stdio side-file adapter"
```

Expected: an isolated #562 adapter commit with no integration behavior changed yet.

---

### Task 11: Integrate qpdf finish timing and prove `/dev/full` parity

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`

**Interfaces:**
- Consumes: `QpdfStdioWriter<File>` from Task 10.
- Produces: file-mode JSON that finishes the stream JSON first, ignores final `ENOSPC`, and still propagates ordinary drain errors and final `EBADF`.

- [ ] **Step 1: Change the existing flush-failure expectation to qpdf timing**

Replace `file_mode_payload_flush_failure_leaves_complete_datafile_before_dict`
with a fake that counts flush calls and this contract:

```rust
#[test]
fn file_mode_stream_value_does_not_finish_the_side_sink_before_dict() {
    let mut pdf = load_one_page_pdf();
    let stream = Stream::new(Dictionary::new(), b"payload".to_vec());
    let mut side_file = FlushFails { bytes: Vec::new() };
    let mut out = Vec::new();

    write_file_mode_stream_value(
        &mut pdf,
        &stream,
        DecodeLevel::None,
        "side-file",
        &mut side_file,
        &mut out,
    )
    .unwrap();

    assert_eq!(side_file.bytes, b"payload");
    assert!(out.windows(br#""dict""#.len()).any(|w| w == br#""dict""#));
    assert!(out.ends_with(b"\n        }"));
}
```

The fake's `flush` still returns `io::Error::other("flush full")`. The test
passes only when `write_file_mode_stream_value` no longer finishes the side
sink before writing `dict`.

- [ ] **Step 2: Add a Linux live-oracle `/dev/full` CLI regression**

Add:

```rust
#[cfg(target_os = "linux")]
#[test]
fn file_stream_to_dev_full_matches_qpdf_success_and_complete_json() {
    use std::os::unix::fs::symlink;

    if !is_qpdf_available() {
        return;
    }

    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let prefix = temp.path().join("stream");
    symlink("/dev/full", temp.path().join("stream-4")).unwrap();
    let prefix_arg = format!("--json-stream-prefix={}", prefix.display());
    let args = [
        "--json=2",
        "--json-key=qpdf",
        "--json-object=4",
        "--json-stream-data=file",
        prefix_arg.as_str(),
    ];

    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "{qpdf:?}");

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();

    assert!(flpdf.status.success(), "{flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    serde_json::from_slice::<serde_json::Value>(&flpdf.stdout).unwrap();
}
```

- [ ] **Step 3: Run both RED tests**

Run:

```bash
cargo test -p flpdf --lib \
  json_inspect::tests::file_mode_stream_value_does_not_finish_the_side_sink_before_dict
cargo test -p flpdf-cli --test cli_json \
  file_stream_to_dev_full_matches_qpdf_success_and_complete_json
```

Expected:

- the library test receives the old fatal flush error;
- flpdf exits 2 with partial JSON while qpdf exits 0 with complete JSON.

- [ ] **Step 4: Move side-file finish to the qpdf boundary**

Import `QpdfStdioWriter` in `json_inspect.rs`. In
`json_inspect.rs`, add:

```rust
fn finish_file_mode_side_file<W: Write>(
    side_file: &mut QpdfStdioWriter<W>,
    side_path: &str,
) -> Result<(), JsonOutputError> {
    side_file
        .finish()
        .map_err(|source| side_file_io_error("flush", side_path, source))
}
```

In `write_file_mode_object_entry`, wrap the opened file:

```rust
let side_file = File::create(&side_path)
    .map_err(|source| side_file_io_error("open", &side_path, source))?;
let mut side_file = QpdfStdioWriter::new(side_file);
write_file_mode_stream_value(
    pdf,
    &stream,
    decode_level,
    &side_path,
    &mut side_file,
    out,
)?;
finish_file_mode_side_file(&mut side_file, &side_path)?;
Json::write_dictionary_close(out, object_first, 3)?;
```

Delete the direct `side_file.flush()` call from
`write_file_mode_stream_value`. Keep `payload.write_all` strict. The stream
inner dictionary, including `dict`, completes before `finish`; the outer raw
object dictionary closes only after `finish`, matching `QPDF_json.cc:911-916`.

- [ ] **Step 5: Update deterministic side-file tests**

Test the helper added in Step 4 with this sink:

```rust
struct ErrnoSink(i32);

impl Write for ErrnoSink {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(self.0))
    }
}
```

Add exact finish tests:

```rust
#[test]
fn file_mode_final_enospc_is_ignored_at_the_integration_boundary() {
    let mut side_file = QpdfStdioWriter::new(ErrnoSink(28));
    side_file.write_all(b"small payload").unwrap();
    finish_file_mode_side_file(&mut side_file, "side-file").unwrap();
}

#[test]
fn file_mode_final_ebadf_keeps_qpdf_flush_error_context() {
    let mut side_file = QpdfStdioWriter::new(ErrnoSink(9));
    side_file.write_all(b"small payload").unwrap();
    let error = finish_file_mode_side_file(&mut side_file, "side-file").unwrap_err();
    assert!(matches!(
        error,
        JsonOutputError::SideFileIo {
            operation: "flush",
            ref path,
            ref source,
            ..
        } if path == "side-file" && source.raw_os_error() == Some(9)
    ));
}
```

Keep the existing `FailAfter` ordinary-write assertion unchanged: it must
remain `JsonOutputError::SideFileIo` with operation `"write"`. Keep the
existing normal-file test that reads the completed payload back byte for byte.

- [ ] **Step 6: Run focused and full JSON integration suites**

Run:

```bash
cargo test -p flpdf --lib json_inspect::tests::file_mode
cargo test -p flpdf --test json_tests
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf --test json_schema_tests
cargo test -p flpdf-cli --test cli_json
cargo test -p flpdf-cli --test json_schema_diff
```

Expected: all pass; live `/dev/full` output is complete and byte-identical to qpdf when qpdf is installed.

- [ ] **Step 7: Commit integration behavior**

Run:

```bash
git add crates/flpdf/src/json_inspect.rs crates/flpdf-cli/tests/cli_json.rs
git commit -m "fix(json): match qpdf side-file finish semantics"
```

Expected: clean top branch.

---

### Task 12: Run every final gate, publish atomically, and close Beads

**Files:**
- Verify: complete stack
- Update through Beads: `flpdf-qxba.6`, `.6.1`, `.6.2`, `.6.3`, `.6.4`

**Interfaces:**
- Consumes: all four locally verified layers.
- Produces: atomically pushed PRs #559–#562, green per-PR gates, and pushed Beads state.

- [ ] **Step 1: Run top-layer full workspace gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 scripts/qpdf-module-docs.py --check
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
git diff --check
```

Expected: every command exits zero.

- [ ] **Step 2: Measure #562 patch coverage against validation**

Run:

```bash
git status --short
scripts/patch-coverage.sh --base feature/flpdf-qxba-6-3-json-validation
```

Expected: 100%. Any coverage repair belongs on #562 only if it covers #562
production lines. Commit a focused repair before rerunning.

- [ ] **Step 3: Reconfirm every layer's direct-parent diff**

Run:

```bash
gh stack view --json
git log --oneline --decorate --graph \
  main..feature/flpdf-qxba-6-4-json-integration
```

Then explicitly check out each branch and reuse a fresh coverage report:

```bash
gh stack checkout feature/flpdf-qxba-6-1-json-core
scripts/patch-coverage.sh --base main
gh stack checkout feature/flpdf-qxba-6-2-json-parser
scripts/patch-coverage.sh --base feature/flpdf-qxba-6-1-json-core
gh stack checkout feature/flpdf-qxba-6-3-json-validation
scripts/patch-coverage.sh --base feature/flpdf-qxba-6-2-json-parser
gh stack checkout feature/flpdf-qxba-6-4-json-integration
scripts/patch-coverage.sh --base feature/flpdf-qxba-6-3-json-validation
```

Expected: all four report 100%. Do not reuse a lower branch's lcov report on
an upper branch.

- [ ] **Step 4: Publish the rewritten stack atomically**

State before running: this force-with-lease updates all four open PR branches
and synchronizes their adjacent bases.

Run:

```bash
git status --short --branch
gh stack submit --auto --remote origin
gh stack view --json
```

Expected: all four active branches push atomically; PRs remain #559–#562 with
adjacent bases and new remote heads.

- [ ] **Step 5: Verify remote heads and GitHub checks**

Run:

```bash
git fetch origin
for branch in \
  feature/flpdf-qxba-6-1-json-core \
  feature/flpdf-qxba-6-2-json-parser \
  feature/flpdf-qxba-6-3-json-validation \
  feature/flpdf-qxba-6-4-json-integration
do
  test "$(git rev-parse "$branch")" = "$(git rev-parse "origin/$branch")"
done
gh pr checks 559
gh pr checks 560
gh pr checks 561
gh pr checks 562
```

Expected: local and remote heads match; every required check, including each
PR's patch coverage gate, is successful. If checks are still running, wait and
re-run the four `gh pr checks` commands; do not close the Beads early.

- [ ] **Step 6: Record final evidence, close, and push Beads**

Use the actual heads and coverage numerators in the notes:

```bash
bd update flpdf-qxba.6 --append-notes \
  "Definitive qpdf 11.9.0 JSON parity follow-up shipped across PRs #559-#562: live writer/handler boundaries, success-only blob finalization, raw-byte diagnostics, and measured 4 KiB stdio side-file behavior. All focused/workspace/clippy/rustdoc/module-doc gates and all four direct-parent 100% patch coverage jobs pass."
bd close flpdf-qxba.6.1 flpdf-qxba.6.2 flpdf-qxba.6.3 flpdf-qxba.6.4 \
  --reason="Definitive parity fixes verified and pushed with per-PR 100% patch coverage."
bd close flpdf-qxba.6 \
  --reason="Open JSON stack now matches all defined qpdf 11.9.0 differential probes."
bd dolt push
git status --short --branch
```

Expected: Beads push succeeds; git worktree is clean; top branch matches
origin. Report the four heads, focused/full test results, exact patch coverage
summaries, and the separate missing-`/Pages` Bead boundary.
