# flpdf Initial Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first working qpdf-style Pure Rust core that can lazily read simple PDF objects, check fixture PDFs, and rewrite simple PDFs through a qpdf-like CLI subset.

**Architecture:** Create a Rust workspace with separate `flpdf` library and `flpdf-cli` binary crates. The library uses `Read + Seek`, xref-first loading, lazy indirect object resolution, structured diagnostics, and complete rewrite output. The first milestone supports simple PDF 1.7 fixtures with classic xref tables; xref streams and object streams are represented in APIs and diagnosed clearly when encountered.

**Tech Stack:** Rust 2021, `thiserror`, `clap`, `assert_cmd`, `predicates`, standard `std::io::{Read, Seek, Write}`.

---

## File Structure

- Create `Cargo.toml`: workspace root.
- Create `crates/flpdf/Cargo.toml`: core library manifest.
- Create `crates/flpdf/src/lib.rs`: public module exports and crate-level API.
- Create `crates/flpdf/src/error.rs`: `Error`, `Result`, and parse/context helpers.
- Create `crates/flpdf/src/diagnostics.rs`: `Severity`, `Diagnostic`, `Diagnostics`.
- Create `crates/flpdf/src/object.rs`: PDF object model, dictionaries, streams, object refs.
- Create `crates/flpdf/src/parser.rs`: byte tokenizer and parser for primitive objects, indirect objects, and trailers.
- Create `crates/flpdf/src/xref.rs`: classic xref table and trailer loading.
- Create `crates/flpdf/src/cache.rs`: object cache state and lazy resolution storage.
- Create `crates/flpdf/src/reader.rs`: `Pdf` opening, trailer access, object resolution, check entrypoint.
- Create `crates/flpdf/src/writer.rs`: complete rewrite writer.
- Create `crates/flpdf/src/check.rs`: `CheckReport` construction.
- Create `crates/flpdf-cli/Cargo.toml`: CLI manifest.
- Create `crates/flpdf-cli/src/main.rs`: qpdf-like CLI subset.
- Create `tests/fixtures/minimal.pdf`: smallest valid fixture with one page tree.
- Create `crates/flpdf/tests/reader_tests.rs`: library integration tests.
- Create `crates/flpdf/tests/writer_tests.rs`: writer integration tests.
- Create `crates/flpdf-cli/tests/cli_tests.rs`: CLI integration tests.

---

### Task 1: Workspace Skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `crates/flpdf/Cargo.toml`
- Create: `crates/flpdf/src/lib.rs`
- Create: `crates/flpdf/src/error.rs`
- Create: `crates/flpdf/src/diagnostics.rs`
- Create: `crates/flpdf-cli/Cargo.toml`
- Create: `crates/flpdf-cli/src/main.rs`

- [ ] **Step 1: Write the workspace manifests**

Create `Cargo.toml`:

```toml
[workspace]
members = [
    "crates/flpdf",
    "crates/flpdf-cli",
]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT OR Apache-2.0"
version = "0.1.0"

[workspace.dependencies]
thiserror = "2"
clap = { version = "4", features = ["derive"] }
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

Create `crates/flpdf/Cargo.toml`:

```toml
[package]
name = "flpdf"
edition.workspace = true
license.workspace = true
version.workspace = true

[dependencies]
thiserror.workspace = true
```

Create `crates/flpdf-cli/Cargo.toml`:

```toml
[package]
name = "flpdf-cli"
edition.workspace = true
license.workspace = true
version.workspace = true

[[bin]]
name = "flpdf"
path = "src/main.rs"

[dependencies]
clap.workspace = true
flpdf = { path = "../flpdf" }

[dev-dependencies]
assert_cmd.workspace = true
predicates.workspace = true
tempfile.workspace = true
```

- [ ] **Step 2: Add minimal library modules**

Create `crates/flpdf/src/lib.rs`:

```rust
pub mod diagnostics;
pub mod error;

pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

Create `crates/flpdf/src/error.rs`:

```rust
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error at byte {offset}: {message}")]
    Parse { offset: usize, message: String },
    #[error("unsupported PDF feature: {0}")]
    Unsupported(String),
    #[error("missing required PDF entry: {0}")]
    Missing(&'static str),
}

impl Error {
    pub fn parse(offset: usize, message: impl Into<String>) -> Self {
        Self::Parse { offset, message: message.into() }
    }
}
```

Create `crates/flpdf/src/diagnostics.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub offset: Option<u64>,
}

impl Diagnostic {
    pub fn warning(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self { severity: Severity::Warning, message: message.into(), offset }
    }

    pub fn error(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self { severity: Severity::Error, message: message.into(), offset }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.entries.push(diagnostic);
    }

    pub fn entries(&self) -> &[Diagnostic] {
        &self.entries
    }

    pub fn has_errors(&self) -> bool {
        self.entries.iter().any(|entry| entry.severity == Severity::Error)
    }
}
```

- [ ] **Step 3: Add a minimal CLI**

Create `crates/flpdf-cli/src/main.rs`:

```rust
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
struct Args {
    #[arg(long)]
    check: bool,
    input: Option<std::path::PathBuf>,
    output: Option<std::path::PathBuf>,
}

fn main() {
    let args = Args::parse();
    if args.check {
        println!("flpdf {}", flpdf::version());
        return;
    }
    eprintln!("flpdf: reading and writing PDFs is not wired yet");
    std::process::exit(2);
}
```

- [ ] **Step 4: Run formatting and build checks**

Run: `cargo fmt --check`

Expected: PASS with no output.

Run: `cargo check --workspace`

Expected: PASS and both crates compile.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/flpdf crates/flpdf-cli
git commit -m "chore: create flpdf workspace"
```

---

### Task 2: Object Model

**Files:**
- Create: `crates/flpdf/src/object.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create: `crates/flpdf/tests/object_tests.rs`

- [ ] **Step 1: Write failing object model tests**

Create `crates/flpdf/tests/object_tests.rs`:

```rust
use flpdf::{Dictionary, Object, ObjectRef};

#[test]
fn object_ref_formats_as_pdf_reference() {
    let object_ref = ObjectRef::new(12, 3);
    assert_eq!(object_ref.to_string(), "12 3 R");
}

#[test]
fn dictionary_returns_required_references() {
    let mut dict = Dictionary::new();
    dict.insert("Root", Object::reference(ObjectRef::new(1, 0)));
    assert_eq!(dict.get_ref("Root"), Some(ObjectRef::new(1, 0)));
    assert_eq!(dict.get_ref("Info"), None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --test object_tests`

Expected: FAIL with unresolved imports for `Dictionary`, `Object`, and `ObjectRef`.

- [ ] **Step 3: Implement the object model**

Create `crates/flpdf/src/object.rs`:

```rust
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    pub number: u32,
    pub generation: u16,
}

impl ObjectRef {
    pub fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjectRef),
}

impl Object {
    pub fn reference(object_ref: ObjectRef) -> Self {
        Self::Reference(object_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dictionary {
    entries: BTreeMap<Vec<u8>, Object>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl AsRef<[u8]>, value: Object) {
        self.entries.insert(key.as_ref().to_vec(), value);
    }

    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&Object> {
        self.entries.get(key.as_ref())
    }

    pub fn get_ref(&self, key: impl AsRef<[u8]>) -> Option<ObjectRef> {
        match self.get(key) {
            Some(Object::Reference(object_ref)) => Some(*object_ref),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &Object)> {
        self.entries.iter().map(|(key, value)| (key.as_slice(), value))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dictionary,
    pub data: Vec<u8>,
}

impl Stream {
    pub fn new(dict: Dictionary, data: Vec<u8>) -> Self {
        Self { dict, data }
    }
}
```

Modify `crates/flpdf/src/lib.rs`:

```rust
pub mod diagnostics;
pub mod error;
pub mod object;

pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --test object_tests`

Expected: PASS, 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/object.rs crates/flpdf/tests/object_tests.rs
git commit -m "feat: add PDF object model"
```

---

### Task 3: Primitive Parser

**Files:**
- Create: `crates/flpdf/src/parser.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create: `crates/flpdf/tests/parser_tests.rs`

- [ ] **Step 1: Write failing parser tests**

Create `crates/flpdf/tests/parser_tests.rs`:

```rust
use flpdf::{parse_object, Dictionary, Object, ObjectRef};

#[test]
fn parses_dictionary_with_reference() {
    let object = parse_object(b"<< /Type /Catalog /Pages 2 0 R >>").unwrap();
    let Object::Dictionary(dict) = object else { panic!("expected dictionary") };
    assert_eq!(dict.get("Type"), Some(&Object::Name(b"Catalog".to_vec())));
    assert_eq!(dict.get_ref("Pages"), Some(ObjectRef::new(2, 0)));
}

#[test]
fn parses_array_and_strings() {
    let object = parse_object(b"[1 true false null (hello)]").unwrap();
    assert_eq!(
        object,
        Object::Array(vec![
            Object::Integer(1),
            Object::Boolean(true),
            Object::Boolean(false),
            Object::Null,
            Object::String(b"hello".to_vec()),
        ])
    );
}

#[test]
fn dictionary_type_is_exported_for_downstream_code() {
    let dict = Dictionary::new();
    assert_eq!(dict.iter().count(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --test parser_tests`

Expected: FAIL with unresolved import `parse_object`.

- [ ] **Step 3: Implement a minimal recursive parser**

Create `crates/flpdf/src/parser.rs`:

```rust
use crate::{Dictionary, Error, Object, ObjectRef, Result};

pub fn parse_object(input: &[u8]) -> Result<Object> {
    let mut parser = Parser { input, pos: 0 };
    let object = parser.object()?;
    parser.skip_ws();
    if parser.pos != parser.input.len() {
        return Err(Error::parse(parser.pos, "trailing bytes after object"));
    }
    Ok(object)
}

pub(crate) struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn object(&mut self) -> Result<Object> {
        self.skip_ws();
        if self.starts_with(b"<<") {
            return self.dictionary();
        }
        match self.peek() {
            Some(b'[') => self.array(),
            Some(b'/') => self.name().map(Object::Name),
            Some(b'(') => self.literal_string().map(Object::String),
            Some(b't') if self.take_keyword(b"true") => Ok(Object::Boolean(true)),
            Some(b'f') if self.take_keyword(b"false") => Ok(Object::Boolean(false)),
            Some(b'n') if self.take_keyword(b"null") => Ok(Object::Null),
            Some(b'-' | b'+' | b'0'..=b'9') => self.number_or_ref(),
            _ => Err(Error::parse(self.pos, "expected PDF object")),
        }
    }

    fn dictionary(&mut self) -> Result<Object> {
        self.expect_bytes(b"<<")?;
        let mut dict = Dictionary::new();
        loop {
            self.skip_ws();
            if self.starts_with(b">>") {
                self.pos += 2;
                return Ok(Object::Dictionary(dict));
            }
            let key = self.name()?;
            let value = self.object()?;
            dict.insert(key, value);
        }
    }

    fn array(&mut self) -> Result<Object> {
        self.expect_byte(b'[')?;
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Object::Array(values));
            }
            values.push(self.object()?);
        }
    }

    fn number_or_ref(&mut self) -> Result<Object> {
        let first = self.integer()?;
        let saved = self.pos;
        self.skip_ws();
        if let Ok(second) = self.integer() {
            self.skip_ws();
            if self.peek() == Some(b'R') {
                self.pos += 1;
                let number = u32::try_from(first).map_err(|_| Error::parse(saved, "invalid object number"))?;
                let generation = u16::try_from(second).map_err(|_| Error::parse(saved, "invalid generation number"))?;
                return Ok(Object::Reference(ObjectRef::new(number, generation)));
            }
        }
        self.pos = saved;
        Ok(Object::Integer(first))
    }

    fn integer(&mut self) -> Result<i64> {
        self.skip_ws();
        let start = self.pos;
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if start == self.pos || matches!(self.input.get(start), Some(b'-' | b'+')) && start + 1 == self.pos {
            return Err(Error::parse(start, "expected integer"));
        }
        let text = std::str::from_utf8(&self.input[start..self.pos]).map_err(|_| Error::parse(start, "integer is not utf-8"))?;
        text.parse::<i64>().map_err(|_| Error::parse(start, "invalid integer"))
    }

    fn name(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'/')?;
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if is_delimiter(byte) || is_ws(byte) {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(Error::parse(start, "empty name"));
        }
        Ok(self.input[start..self.pos].to_vec())
    }

    fn literal_string(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'(')?;
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b')' {
                let value = self.input[start..self.pos].to_vec();
                self.pos += 1;
                return Ok(value);
            }
            self.pos += 1;
        }
        Err(Error::parse(start, "unterminated literal string"))
    }

    fn skip_ws(&mut self) {
        loop {
            while matches!(self.peek(), Some(byte) if is_ws(byte)) {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                while !matches!(self.peek(), None | Some(b'\n' | b'\r')) {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn take_keyword(&mut self, keyword: &[u8]) -> bool {
        if self.starts_with(keyword) {
            self.pos += keyword.len();
            true
        } else {
            false
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<()> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Error::parse(self.pos, format!("expected byte {expected}")))
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<()> {
        if self.starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(Error::parse(self.pos, "expected token"))
        }
    }

    fn starts_with(&self, bytes: &[u8]) -> bool {
        self.input[self.pos..].starts_with(bytes)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }
}

fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(byte, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}
```

Modify `crates/flpdf/src/lib.rs`:

```rust
pub mod diagnostics;
pub mod error;
pub mod object;
pub mod parser;

pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};
pub use parser::parse_object;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 4: Run parser tests**

Run: `cargo test -p flpdf --test parser_tests`

Expected: PASS, 3 tests pass.

- [ ] **Step 5: Run object tests to catch regressions**

Run: `cargo test -p flpdf --test object_tests`

Expected: PASS, 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/parser.rs crates/flpdf/tests/parser_tests.rs
git commit -m "feat: parse basic PDF objects"
```

---

### Task 4: Xref And Trailer Loading

**Files:**
- Create: `crates/flpdf/src/xref.rs`
- Modify: `crates/flpdf/src/parser.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create: `tests/fixtures/minimal.pdf`
- Create: `crates/flpdf/tests/xref_tests.rs`

- [ ] **Step 1: Create the minimal PDF fixture**

Create `tests/fixtures/minimal.pdf` with this exact content:

```text
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 0 /Kids [] >>
endobj
xref
0 3
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
trailer
<< /Size 3 /Root 1 0 R >>
startxref
110
%%EOF
```

- [ ] **Step 2: Write failing xref tests**

Create `crates/flpdf/tests/xref_tests.rs`:

```rust
use flpdf::{load_xref_and_trailer, ObjectRef};
use std::fs::File;
use std::io::BufReader;

#[test]
fn loads_xref_table_and_trailer() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut reader = BufReader::new(file);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, 110);
    assert_eq!(loaded.entries.get(&ObjectRef::new(1, 0)).copied(), Some(9));
    assert_eq!(loaded.entries.get(&ObjectRef::new(2, 0)).copied(), Some(58));
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p flpdf --test xref_tests`

Expected: FAIL with unresolved import `load_xref_and_trailer`.

- [ ] **Step 4: Expose parser position for trailer parsing**

Modify `crates/flpdf/src/parser.rs` by changing `Parser::object` from `pub(crate)` to callable by `xref.rs`. Keep this signature:

```rust
impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn object(&mut self) -> Result<Object> {
        self.skip_ws();
        if self.starts_with(b"<<") {
            return self.dictionary();
        }
        match self.peek() {
            Some(b'[') => self.array(),
            Some(b'/') => self.name().map(Object::Name),
            Some(b'(') => self.literal_string().map(Object::String),
            Some(b't') if self.take_keyword(b"true") => Ok(Object::Boolean(true)),
            Some(b'f') if self.take_keyword(b"false") => Ok(Object::Boolean(false)),
            Some(b'n') if self.take_keyword(b"null") => Ok(Object::Null),
            Some(b'-' | b'+' | b'0'..=b'9') => self.number_or_ref(),
            _ => Err(Error::parse(self.pos, "expected PDF object")),
        }
    }
}
```

- [ ] **Step 5: Implement xref loader**

Create `crates/flpdf/src/xref.rs`:

```rust
use crate::parser::Parser;
use crate::{Dictionary, Error, Object, ObjectRef, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone)]
pub struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, u64>,
    pub trailer: Dictionary,
}

pub fn load_xref_and_trailer<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    let mut bytes = Vec::new();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_to_end(&mut bytes)?;
    let version = parse_header(&bytes)?;
    let startxref = parse_startxref(&bytes)?;
    let xref_pos = usize::try_from(startxref).map_err(|_| Error::parse(0, "startxref does not fit usize"))?;
    if !bytes.get(xref_pos..).is_some_and(|tail| tail.starts_with(b"xref")) {
        return Err(Error::Unsupported("xref streams are not supported in the first milestone".to_string()));
    }
    let mut cursor = ByteCursor::new(&bytes, xref_pos + 4);
    let mut entries = BTreeMap::new();
    loop {
        cursor.skip_ws();
        if cursor.starts_with(b"trailer") {
            cursor.pos += b"trailer".len();
            break;
        }
        let first = cursor.read_u32()?;
        let count = cursor.read_u32()?;
        for index in 0..count {
            cursor.skip_ws();
            let offset = cursor.read_fixed_u64(10)?;
            cursor.skip_ws();
            let generation = cursor.read_fixed_u16(5)?;
            cursor.skip_ws();
            let in_use = cursor.read_byte()?;
            cursor.skip_line();
            if in_use == b'n' {
                entries.insert(ObjectRef::new(first + index, generation), offset);
            }
        }
    }
    cursor.skip_ws();
    let mut parser = Parser::new(&bytes[cursor.pos..]);
    let trailer = match parser.object()? {
        Object::Dictionary(dict) => dict,
        _ => return Err(Error::parse(cursor.pos + parser.position(), "trailer is not a dictionary")),
    };
    Ok(LoadedXref { version, startxref, entries, trailer })
}

fn parse_header(bytes: &[u8]) -> Result<String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err(Error::parse(0, "missing PDF header"));
    }
    let end = bytes.iter().position(|byte| *byte == b'\n' || *byte == b'\r').unwrap_or(bytes.len());
    let header = std::str::from_utf8(&bytes[5..end]).map_err(|_| Error::parse(5, "PDF version is not utf-8"))?;
    Ok(header.to_string())
}

fn parse_startxref(bytes: &[u8]) -> Result<u64> {
    let marker = b"startxref";
    let Some(pos) = bytes.windows(marker.len()).rposition(|window| window == marker) else {
        return Err(Error::parse(bytes.len(), "missing startxref"));
    };
    let mut cursor = ByteCursor::new(bytes, pos + marker.len());
    cursor.skip_ws();
    cursor.read_u64()
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    fn starts_with(&self, token: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(token)
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')) {
            self.pos += 1;
        }
    }

    fn skip_line(&mut self) {
        while !matches!(self.bytes.get(self.pos), None | Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
        while matches!(self.bytes.get(self.pos), Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn read_byte(&mut self) -> Result<u8> {
        let Some(byte) = self.bytes.get(self.pos).copied() else {
            return Err(Error::parse(self.pos, "unexpected end of input"));
        };
        self.pos += 1;
        Ok(byte)
    }

    fn read_u32(&mut self) -> Result<u32> {
        let value = self.read_unsigned()?;
        u32::try_from(value).map_err(|_| Error::parse(self.pos, "number does not fit u32"))
    }

    fn read_u64(&mut self) -> Result<u64> {
        self.read_unsigned()
    }

    fn read_fixed_u64(&mut self, width: usize) -> Result<u64> {
        self.read_fixed(width)?.parse::<u64>().map_err(|_| Error::parse(self.pos, "invalid fixed-width u64"))
    }

    fn read_fixed_u16(&mut self, width: usize) -> Result<u16> {
        self.read_fixed(width)?.parse::<u16>().map_err(|_| Error::parse(self.pos, "invalid fixed-width u16"))
    }

    fn read_fixed(&mut self, width: usize) -> Result<&str> {
        if self.pos + width > self.bytes.len() {
            return Err(Error::parse(self.pos, "unexpected end of fixed-width field"));
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + width]).map_err(|_| Error::parse(self.pos, "field is not utf-8"))?;
        self.pos += width;
        Ok(text)
    }

    fn read_unsigned(&mut self) -> Result<u64> {
        self.skip_ws();
        let start = self.pos;
        while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(Error::parse(start, "expected unsigned integer"));
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| Error::parse(start, "number is not utf-8"))?;
        text.parse::<u64>().map_err(|_| Error::parse(start, "invalid unsigned integer"))
    }
}
```

Modify `crates/flpdf/src/lib.rs`:

```rust
pub mod diagnostics;
pub mod error;
pub mod object;
pub mod parser;
pub mod xref;

pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};
pub use parser::parse_object;
pub use xref::{load_xref_and_trailer, LoadedXref};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 6: Run xref tests**

Run: `cargo test -p flpdf --test xref_tests`

Expected: PASS, 1 test passes.

- [ ] **Step 7: Run all library tests**

Run: `cargo test -p flpdf`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/parser.rs crates/flpdf/src/xref.rs crates/flpdf/tests/xref_tests.rs tests/fixtures/minimal.pdf
git commit -m "feat: load classic xref tables"
```

---

### Task 5: Lazy Reader And Object Cache

**Files:**
- Create: `crates/flpdf/src/cache.rs`
- Create: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/parser.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create: `crates/flpdf/tests/reader_tests.rs`

- [ ] **Step 1: Write failing reader tests**

Create `crates/flpdf/tests/reader_tests.rs`:

```rust
use flpdf::{Object, ObjectRef, Pdf};
use std::fs::File;
use std::io::BufReader;

#[test]
fn opens_pdf_without_resolving_all_objects() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.resolved_count(), 0);
    assert_eq!(pdf.trailer().get_ref("Root"), Some(ObjectRef::new(1, 0)));
}

#[test]
fn resolves_indirect_object_on_access() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let root = pdf.resolve(ObjectRef::new(1, 0)).unwrap();
    let Object::Dictionary(dict) = root else { panic!("expected catalog dictionary") };
    assert_eq!(dict.get_ref("Pages"), Some(ObjectRef::new(2, 0)));
    assert_eq!(pdf.resolved_count(), 1);
}

#[test]
fn missing_reference_resolves_to_null() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    assert_eq!(pdf.resolve(ObjectRef::new(99, 0)).unwrap(), Object::Null);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --test reader_tests`

Expected: FAIL with unresolved import `Pdf`.

- [ ] **Step 3: Add indirect object parsing helper**

Modify `crates/flpdf/src/parser.rs` by adding this function after `parse_object`:

```rust
pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let mut parser = Parser::new(input);
    let number = parser.integer_for_indirect()?;
    let generation = parser.integer_for_indirect()?;
    parser.expect_keyword_for_indirect(b"obj")?;
    let object = parser.object()?;
    Ok((
        ObjectRef::new(
            u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
            u16::try_from(generation).map_err(|_| Error::parse(0, "invalid indirect generation"))?,
        ),
        object,
    ))
}
```

Add these methods inside `impl<'a> Parser<'a>`:

```rust
    pub(crate) fn integer_for_indirect(&mut self) -> Result<i64> {
        self.integer()
    }

    pub(crate) fn expect_keyword_for_indirect(&mut self, keyword: &[u8]) -> Result<()> {
        self.skip_ws();
        if self.starts_with(keyword) {
            self.pos += keyword.len();
            Ok(())
        } else {
            Err(Error::parse(self.pos, "expected indirect object keyword"))
        }
    }
```

- [ ] **Step 4: Implement cache**

Create `crates/flpdf/src/cache.rs`:

```rust
use crate::{Object, ObjectRef};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum CacheEntry {
    Unresolved { offset: u64 },
    Resolved(Object),
    Missing,
    Reserved,
    Deleted,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectCache {
    entries: BTreeMap<ObjectRef, CacheEntry>,
}

impl ObjectCache {
    pub fn from_offsets(offsets: &BTreeMap<ObjectRef, u64>) -> Self {
        let entries = offsets.iter().map(|(object_ref, offset)| (*object_ref, CacheEntry::Unresolved { offset: *offset })).collect();
        Self { entries }
    }

    pub fn entry(&self, object_ref: ObjectRef) -> Option<&CacheEntry> {
        self.entries.get(&object_ref)
    }

    pub fn set_resolved(&mut self, object_ref: ObjectRef, object: Object) {
        self.entries.insert(object_ref, CacheEntry::Resolved(object));
    }

    pub fn resolved_count(&self) -> usize {
        self.entries.values().filter(|entry| matches!(entry, CacheEntry::Resolved(_))).count()
    }
}
```

- [ ] **Step 5: Implement Pdf reader**

Create `crates/flpdf/src/reader.rs`:

```rust
use crate::cache::{CacheEntry, ObjectCache};
use crate::parser::parse_indirect_object;
use crate::{load_xref_and_trailer, Dictionary, Object, ObjectRef, Result};
use std::io::{Read, Seek, SeekFrom};

pub struct Pdf<R: Read + Seek> {
    reader: R,
    version: String,
    trailer: Dictionary,
    cache: ObjectCache,
}

impl<R: Read + Seek> Pdf<R> {
    pub fn open(mut reader: R) -> Result<Self> {
        let loaded = load_xref_and_trailer(&mut reader)?;
        let cache = ObjectCache::from_offsets(&loaded.entries);
        Ok(Self { reader, version: loaded.version, trailer: loaded.trailer, cache })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    pub fn resolved_count(&self) -> usize {
        self.cache.resolved_count()
    }

    pub fn resolve(&mut self, object_ref: ObjectRef) -> Result<Object> {
        match self.cache.entry(object_ref).cloned() {
            Some(CacheEntry::Resolved(object)) => Ok(object),
            Some(CacheEntry::Unresolved { offset }) => {
                self.reader.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                self.reader.read_to_end(&mut bytes)?;
                let (parsed_ref, object) = parse_indirect_object(&bytes)?;
                if parsed_ref != object_ref {
                    return Ok(Object::Null);
                }
                self.cache.set_resolved(object_ref, object.clone());
                Ok(object)
            }
            Some(CacheEntry::Missing | CacheEntry::Deleted) | None => Ok(Object::Null),
            Some(CacheEntry::Reserved) => Ok(Object::Null),
        }
    }
}
```

Modify `crates/flpdf/src/lib.rs`:

```rust
pub mod cache;
pub mod diagnostics;
pub mod error;
pub mod object;
pub mod parser;
pub mod reader;
pub mod xref;

pub use cache::{CacheEntry, ObjectCache};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};
pub use parser::parse_object;
pub use reader::Pdf;
pub use xref::{load_xref_and_trailer, LoadedXref};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 6: Run reader tests**

Run: `cargo test -p flpdf --test reader_tests`

Expected: PASS, 3 tests pass.

- [ ] **Step 7: Run all library tests**

Run: `cargo test -p flpdf`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/cache.rs crates/flpdf/src/reader.rs crates/flpdf/src/parser.rs crates/flpdf/tests/reader_tests.rs
git commit -m "feat: lazily resolve PDF objects"
```

---

### Task 6: Check Report

**Files:**
- Create: `crates/flpdf/src/check.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create: `crates/flpdf/tests/check_tests.rs`

- [ ] **Step 1: Write failing check tests**

Create `crates/flpdf/tests/check_tests.rs`:

```rust
use flpdf::{check_reader, Severity};
use std::fs::File;
use std::io::BufReader;

#[test]
fn check_reports_valid_minimal_pdf() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let report = check_reader(BufReader::new(file)).unwrap();
    assert!(report.valid);
    assert_eq!(report.diagnostics.entries().len(), 0);
}

#[test]
fn check_reports_missing_header() {
    let input = std::io::Cursor::new(b"not a pdf".to_vec());
    let report = check_reader(input).unwrap();
    assert!(!report.valid);
    assert!(report.diagnostics.entries().iter().any(|entry| entry.severity == Severity::Error));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --test check_tests`

Expected: FAIL with unresolved import `check_reader`.

- [ ] **Step 3: Implement check report**

Create `crates/flpdf/src/check.rs`:

```rust
use crate::{Diagnostic, Diagnostics, Pdf};
use std::io::{Read, Seek};

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub valid: bool,
    pub diagnostics: Diagnostics,
}

pub fn check_reader<R: Read + Seek>(reader: R) -> crate::Result<CheckReport> {
    match Pdf::open(reader) {
        Ok(pdf) => {
            let mut diagnostics = Diagnostics::default();
            if pdf.trailer().get_ref("Root").is_none() {
                diagnostics.push(Diagnostic::error("trailer is missing /Root", None));
            }
            Ok(CheckReport { valid: !diagnostics.has_errors(), diagnostics })
        }
        Err(error) => {
            let mut diagnostics = Diagnostics::default();
            diagnostics.push(Diagnostic::error(error.to_string(), None));
            Ok(CheckReport { valid: false, diagnostics })
        }
    }
}
```

Modify `crates/flpdf/src/lib.rs`:

```rust
pub mod cache;
pub mod check;
pub mod diagnostics;
pub mod error;
pub mod object;
pub mod parser;
pub mod reader;
pub mod xref;

pub use cache::{CacheEntry, ObjectCache};
pub use check::{check_reader, CheckReport};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};
pub use parser::parse_object;
pub use reader::Pdf;
pub use xref::{load_xref_and_trailer, LoadedXref};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 4: Run check tests**

Run: `cargo test -p flpdf --test check_tests`

Expected: PASS, 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/check.rs crates/flpdf/tests/check_tests.rs
git commit -m "feat: add PDF check report"
```

---

### Task 7: Complete Rewrite Writer

**Files:**
- Create: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/object.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Create: `crates/flpdf/tests/writer_tests.rs`

- [ ] **Step 1: Write failing writer test**

Create `crates/flpdf/tests/writer_tests.rs`:

```rust
use flpdf::{check_reader, write_pdf, Pdf};
use std::fs::File;
use std::io::{BufReader, Cursor};

#[test]
fn rewrites_minimal_pdf_to_valid_pdf() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(report.valid, "diagnostics: {:?}", report.diagnostics.entries());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --test writer_tests`

Expected: FAIL with unresolved import `write_pdf`.

- [ ] **Step 3: Add object serialization**

Modify `crates/flpdf/src/object.rs` by adding this implementation at the end:

```rust
impl Object {
    pub(crate) fn write_pdf(&self, out: &mut Vec<u8>) {
        match self {
            Object::Null => out.extend_from_slice(b"null"),
            Object::Boolean(value) => out.extend_from_slice(if *value { b"true" } else { b"false" }),
            Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Name(name) => {
                out.push(b'/');
                out.extend_from_slice(name);
            }
            Object::String(value) => {
                out.push(b'(');
                out.extend_from_slice(value);
                out.push(b')');
            }
            Object::Array(values) => {
                out.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(b' ');
                    }
                    value.write_pdf(out);
                }
                out.push(b']');
            }
            Object::Dictionary(dict) => dict.write_pdf(out),
            Object::Stream(stream) => {
                stream.dict.write_pdf(out);
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&stream.data);
                out.extend_from_slice(b"\nendstream");
            }
            Object::Reference(object_ref) => out.extend_from_slice(object_ref.to_string().as_bytes()),
        }
    }
}

impl Dictionary {
    pub(crate) fn write_pdf(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"<<");
        for (key, value) in self.iter() {
            out.extend_from_slice(b" /");
            out.extend_from_slice(key);
            out.push(b' ');
            value.write_pdf(out);
        }
        out.extend_from_slice(b" >>");
    }
}
```

- [ ] **Step 4: Add Pdf accessors for writer traversal**

Modify `crates/flpdf/src/reader.rs` by adding these methods inside `impl<R: Read + Seek> Pdf<R>`:

```rust
    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer.get_ref("Root")
    }
```

- [ ] **Step 5: Implement rewrite writer**

Create `crates/flpdf/src/writer.rs`:

```rust
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, Write};

pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut queue = VecDeque::from([root_ref]);
    let mut old_to_new = BTreeMap::from([(root_ref, ObjectRef::new(1, 0))]);
    let mut objects = Vec::new();

    while let Some(old_ref) = queue.pop_front() {
        let object = pdf.resolve(old_ref)?;
        collect_refs(&object, &mut old_to_new, &mut queue);
        objects.push((old_ref, object));
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (old_ref, object) in &objects {
        let new_ref = old_to_new[old_ref];
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", new_ref.number).as_bytes());
        let rewritten = rewrite_refs(object, &old_to_new);
        rewritten.write_pdf(&mut bytes);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer((objects.len() + 1) as i64));
    trailer.insert("Root", Object::Reference(old_to_new[&root_ref]));
    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(&mut bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    out.write_all(&bytes)?;
    Ok(())
}

fn collect_refs(object: &Object, old_to_new: &mut BTreeMap<ObjectRef, ObjectRef>, queue: &mut VecDeque<ObjectRef>) {
    match object {
        Object::Reference(object_ref) => {
            if !old_to_new.contains_key(object_ref) {
                let next = ObjectRef::new((old_to_new.len() + 1) as u32, 0);
                old_to_new.insert(*object_ref, next);
                queue.push_back(*object_ref);
            }
        }
        Object::Array(values) => values.iter().for_each(|value| collect_refs(value, old_to_new, queue)),
        Object::Dictionary(dict) => dict.iter().for_each(|(_, value)| collect_refs(value, old_to_new, queue)),
        Object::Stream(stream) => stream.dict.iter().for_each(|(_, value)| collect_refs(value, old_to_new, queue)),
        Object::Null | Object::Boolean(_) | Object::Integer(_) | Object::Real(_) | Object::Name(_) | Object::String(_) => {}
    }
}

fn rewrite_refs(object: &Object, old_to_new: &BTreeMap<ObjectRef, ObjectRef>) -> Object {
    match object {
        Object::Reference(object_ref) => old_to_new.get(object_ref).copied().map(Object::Reference).unwrap_or(Object::Null),
        Object::Array(values) => Object::Array(values.iter().map(|value| rewrite_refs(value, old_to_new)).collect()),
        Object::Dictionary(dict) => {
            let mut rewritten = Dictionary::new();
            for (key, value) in dict.iter() {
                rewritten.insert(key, rewrite_refs(value, old_to_new));
            }
            Object::Dictionary(rewritten)
        }
        Object::Stream(stream) => Object::Stream(crate::Stream::new(match rewrite_refs(&Object::Dictionary(stream.dict.clone()), old_to_new) {
            Object::Dictionary(dict) => dict,
            _ => Dictionary::new(),
        }, stream.data.clone())),
        other => other.clone(),
    }
}
```

Modify `crates/flpdf/src/lib.rs`:

```rust
pub mod cache;
pub mod check;
pub mod diagnostics;
pub mod error;
pub mod object;
pub mod parser;
pub mod reader;
pub mod writer;
pub mod xref;

pub use cache::{CacheEntry, ObjectCache};
pub use check::{check_reader, CheckReport};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};
pub use parser::parse_object;
pub use reader::Pdf;
pub use writer::write_pdf;
pub use xref::{load_xref_and_trailer, LoadedXref};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 6: Run writer tests**

Run: `cargo test -p flpdf --test writer_tests`

Expected: PASS, 1 test passes.

- [ ] **Step 7: Run all library tests**

Run: `cargo test -p flpdf`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/object.rs crates/flpdf/src/reader.rs crates/flpdf/src/writer.rs crates/flpdf/tests/writer_tests.rs
git commit -m "feat: rewrite reachable PDF objects"
```

---

### Task 8: Wire The CLI

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs`
- Create: `crates/flpdf-cli/tests/cli_tests.rs`

- [ ] **Step 1: Write failing CLI tests**

Create `crates/flpdf-cli/tests/cli_tests.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn check_valid_fixture_exits_successfully() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PDF check succeeded"));
}

#[test]
fn rewrite_fixture_creates_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("../../tests/fixtures/minimal.pdf")
        .arg(&output)
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(output).unwrap().len() > 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf-cli --test cli_tests`

Expected: FAIL because CLI still prints version for `--check` and exits `2` for rewrite.

- [ ] **Step 3: Implement CLI behavior**

Modify `crates/flpdf-cli/src/main.rs`:

```rust
use clap::Parser;
use flpdf::{check_reader, write_pdf, Pdf, Severity};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
struct Args {
    #[arg(long)]
    check: bool,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    let result = if args.check { run_check(args.input) } else { run_rewrite(args.input, args.output) };
    if let Err(error) = result {
        eprintln!("flpdf: {error}");
        std::process::exit(2);
    }
}

fn run_check(input: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(input)?;
    let report = check_reader(BufReader::new(file))?;
    for diagnostic in report.diagnostics.entries() {
        let label = match diagnostic.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        eprintln!("{label}: {}", diagnostic.message);
    }
    if report.valid {
        println!("PDF check succeeded");
        Ok(())
    } else {
        Err("PDF check failed".into())
    }
}

fn run_rewrite(input: Option<PathBuf>, output: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let file = File::open(input)?;
    let mut pdf = Pdf::open(BufReader::new(file))?;
    let mut out = File::create(output)?;
    write_pdf(&mut pdf, &mut out)?;
    Ok(())
}
```

- [ ] **Step 4: Run CLI tests**

Run: `cargo test -p flpdf-cli --test cli_tests`

Expected: PASS, 2 tests pass.

- [ ] **Step 5: Run workspace tests**

Run: `cargo test --workspace --all-targets`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "feat: wire qpdf-style CLI subset"
```

---

### Task 9: Final Verification And Documentation Alignment

**Files:**
- Modify: `docs/superpowers/specs/2026-05-09-flpdf-qpdf-core-design.md` only if implementation changes require documented scope adjustment.

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --check`

Expected: PASS.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets --all-features`

Expected: PASS with no warnings.

- [ ] **Step 3: Run full tests**

Run: `cargo test --workspace --all-targets --all-features`

Expected: PASS.

- [ ] **Step 4: Manual CLI smoke test for check**

Run: `cargo run -p flpdf-cli -- --check tests/fixtures/minimal.pdf`

Expected stdout contains `PDF check succeeded` and exit code is `0`.

- [ ] **Step 5: Manual CLI smoke test for rewrite**

Run: `cargo run -p flpdf-cli -- tests/fixtures/minimal.pdf /tmp/flpdf-minimal-out.pdf`

Expected: exit code `0` and `/tmp/flpdf-minimal-out.pdf` exists.

- [ ] **Step 6: Verify rewritten output**

Run: `cargo run -p flpdf-cli -- --check /tmp/flpdf-minimal-out.pdf`

Expected stdout contains `PDF check succeeded` and exit code is `0`.

- [ ] **Step 7: Commit final verification docs if changed**

```bash
git status --short
git add docs/superpowers/specs/2026-05-09-flpdf-qpdf-core-design.md
git commit -m "docs: align flpdf initial core scope"
```

If `git status --short` shows no documentation changes, do not create an empty commit.

---

## Self-Review

Spec coverage:

- Workspace split into library and CLI: covered by Task 1.
- qpdf-style `Read + Seek` loading: covered by Tasks 4 and 5.
- Lazy indirect object resolution and cache: covered by Task 5.
- Structured diagnostics and check report: covered by Task 6.
- Complete rewrite writer: covered by Task 7.
- qpdf-like CLI subset: covered by Task 8.
- Verification commands: covered by Task 9.

Known scope cuts for this first implementation plan:

- Xref stream and object stream support are represented as explicit unsupported paths in the first milestone plan, not implemented here.
- Stream filters are not implemented because the initial fixture has no streams; Flate support should be the next plan after this milestone.
- Encryption is not implemented and must remain an explicit unsupported error when encountered.

Red-flag scan result:

- No incomplete markers or unspecified implementation steps remain.

Type consistency result:

- Public exports used by tests are introduced before use: `ObjectRef`, `Dictionary`, `Object`, `parse_object`, `load_xref_and_trailer`, `Pdf`, `check_reader`, and `write_pdf`.
