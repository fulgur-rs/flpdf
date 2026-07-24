# qpdf-style File Object Stream Reader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's provisional stream placeholder path with qpdf 11.9.0-shaped direct-object parsing and file-object completion so the good14-shaped adjacent-`endstream` case completes silently and every file-object stream consumer shares one implementation.

**Architecture:** Build the new reader bottom-up behind the existing production path. `parser.rs` parses only direct-object syntax and returns the exact stopping offset; `reader/file_object.rs` parses indirect framing and purely completes streams after a caller supplies an indirect length; `reader.rs` retains I/O, cache recursion, decryption, and diagnostic registration; `xref.rs` uses the same completion engine with bootstrap recovery.

**Tech Stack:** Rust 2021 workspace, existing `Object`/`Dictionary`/`Stream`/`Pdf` types, qpdf 11.9.0 as the source and behavioral oracle, synthetic PDF fixtures, `assert_cmd`, Beads, dependent Git branches and draft PRs, Cargo tests/Clippy/rustdoc, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the parity oracle.
- Do not change public `parse_object`, strict indirect-object parsing, content-stream tokenization, QDF formatting, object numbering, filtering, compression, or ObjStm member framing.
- An unresolved stream must remain `PendingBody::Stream`; it must never be represented by an empty provisional `Object::Stream`.
- Complete raw stream boundaries before decryption; preserve existing recovered-EOL input to stream decryption.
- Preserve bounded object windows, the fallback-to-EOF budget, parser nesting limits, cache `Reserved` recursion guards, and restoration of `Unresolved` cache entries on all completion failures.
- Recovery scans only the current bounded object slice and accepts token-bounded `endstream` or `endobj`; it must not scan the whole file unconditionally or accept keyword substrings inside payload bytes.
- Register diagnostics only when the completed object is first committed to the cache.
- Keep `endstream` and `endobj` validation separate; a valid stream with a missing `endobj` is returned with `ExpectedEndobj`.
- The in-repository good14-shaped PDF is authored from scratch; do not copy or vendor qpdf's qtest fixture.
- Compare the good14-shaped QDF object-body region separately from known generic QDF header/trailer formatting gaps.
- Every layer must be green and reviewable on its own. Production routing changes are forbidden in Layers 1 and 2.
- Run changed-line coverage from a clean, committed `HEAD`; gated `crates/flpdf/src` patch coverage must be 100%.
- Before closing the parent issue, `bd dolt push` and every Git branch push must succeed.

---

## File Structure

- Create `crates/flpdf/src/reader/file_object.rs`
  - Own indirect-object header framing, pending direct/stream states, stream-start EOL classification, exact-length completion, bounded recovery, separate terminator checks, and structured diagnostics.
- Modify `crates/flpdf/src/parser.rs`
  - Add the internal direct-object-only mode and `ParsedDirectObject`; retain the old stream parser until Layer 4.
- Modify `crates/flpdf/src/reader.rs`
  - Declare the child module; retain I/O/cache/decryption ownership; resolve indirect lengths between syntax and completion; register structured warnings once; switch normal objects first and ObjStm containers later.
- Modify `crates/flpdf/src/xref.rs`
  - Replace the bootstrap xref-stream parser with `reader::file_object` syntax/completion and carry its warnings into `LoadedXref.repair_diagnostics`.
- Modify `crates/flpdf-cli/tests/cli_tests.rs`
  - Add the authored good14-shaped fixture builder and a CLI regression asserting silent exit 0.
- Modify `crates/flpdf-cli/tests/compat_matrix_tests.rs`
  - Add qpdf 11.9.0 differential coverage that compares the QDF object-body region without conflating header/trailer formatting.
- Create `tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf`
  - Minimal authored Catalog/Pages/Page/content-stream graph with indirect `/Length` and payload-adjacent `endstream`.
- Modify `docs/superpowers/specs/2026-07-24-qpdf-file-object-stream-reader-design.md`
  - Only update delivery status and exact Beads/PR links after all layers are implemented; do not change the approved behavior.

---

### Task 1: Layer 1 — Direct-object syntax and pending file-object model

**Files:**
- Modify: `crates/flpdf/src/parser.rs:65-170, 266-283, 840-1030`
- Create: `crates/flpdf/src/reader/file_object.rs`
- Modify: `crates/flpdf/src/reader.rs:1-8`
- Test: `crates/flpdf/src/parser.rs` unit module
- Test: `crates/flpdf/src/reader/file_object.rs` unit module

**Interfaces:**
- Consumes: `Parser::new(&[u8]) -> Parser`, `Parser::integer_for_indirect() -> Result<i64>`, `Parser::expect_keyword_for_indirect(&[u8]) -> Result<()>`, `Parser::position() -> usize`, `ObjectRef::new(u32, u16) -> ObjectRef`.
- Produces: `parse_qpdf_direct_object(input: &[u8]) -> Result<ParsedDirectObject>`, `parse_file_object_syntax(input: &[u8]) -> Result<PendingFileObject>`, `PendingFileObject::indirect_length_ref(&self) -> Option<ObjectRef>`, and the exact types below.

```rust
#[derive(Debug, PartialEq)]
pub(crate) struct ParsedDirectObject {
    pub(crate) object: Object,
    pub(crate) next_offset: usize,
    pub(crate) empty_offset: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamStartEol {
    Lf,
    CrLf,
    Cr,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileObjectDiagnosticKind {
    EmptyObject,
    StreamLineEnding,
    InvalidStreamLength,
    ExpectedEndstream,
    AttemptingStreamLengthRecovery,
    RecoveredStreamLength { length: usize },
    EmptyRecoveredStream,
    ExpectedEndobj,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileObjectDiagnostic {
    pub(crate) kind: FileObjectDiagnosticKind,
    pub(crate) relative_offset: usize,
}

#[derive(Debug, PartialEq)]
pub(crate) enum PendingBody {
    Direct {
        object: Object,
        next_offset: usize,
    },
    Stream {
        dict: Dictionary,
        data_start: usize,
        start_eol: StreamStartEol,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) struct PendingFileObject {
    pub(crate) object_ref: ObjectRef,
    pub(crate) body: PendingBody,
    pub(crate) diagnostics: Vec<FileObjectDiagnostic>,
}
```

- [ ] **Step 1: Create and claim the four Beads children, then record their dependency order on the parent**

Run this as one shell block from the existing design worktree:

```bash
syntax_id="$(bd create "qpdf file-object reader: direct syntax model" --type task --parent flpdf-15jp --priority 1 --silent)"
completion_id="$(bd create "qpdf file-object reader: pure stream completion" --type task --parent flpdf-15jp --priority 1 --deps "$syntax_id" --silent)"
normal_id="$(bd create "qpdf file-object reader: normal object routing" --type task --parent flpdf-15jp --priority 1 --deps "$completion_id" --silent)"
bootstrap_id="$(bd create "qpdf file-object reader: ObjStm and xref routing" --type task --parent flpdf-15jp --priority 1 --deps "$normal_id" --silent)"
bd update "$syntax_id" --claim
bd update flpdf-15jp --append-notes "Implementation stack: $syntax_id -> $completion_id -> $normal_id -> $bootstrap_id"
bd dolt push
```

Expected: four child IDs are created, the syntax child is `in_progress`, the parent notes contain the exact chain, and Beads push succeeds.

- [ ] **Step 2: Start the bottom stack branch**

Run:

```bash
git switch -c stack/flpdf-15jp-file-object-syntax
```

Expected: the new branch points at `b1f91088` and `git status --short --branch` is clean.

- [ ] **Step 3: Write failing direct-object parser tests**

Add these tests to `stream_length_tests` in `crates/flpdf/src/parser.rs` and import `parse_qpdf_direct_object`:

```rust
#[test]
fn qpdf_direct_object_stops_before_stream_framing() {
    let input = b"<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n";
    let parsed = parse_qpdf_direct_object(input).unwrap();
    let dict = parsed.object.into_dict().expect("dictionary");
    assert_eq!(dict.get("Length"), Some(&Object::Integer(3)));
    assert_eq!(&input[parsed.next_offset..parsed.next_offset + 6], b"stream");
    assert_eq!(parsed.empty_offset, None);
}

#[test]
fn qpdf_direct_object_preserves_top_level_and_nested_reference_rules() {
    let bare = parse_qpdf_direct_object(b"6 0 R\nendobj").unwrap();
    assert_eq!(bare.object, Object::Integer(6));
    assert_eq!(&b"6 0 R\nendobj"[bare.next_offset..], b"0 R\nendobj");

    let nested = parse_qpdf_direct_object(b"[6 0 R << /V 7 0 R >>]\nendobj").unwrap();
    let Object::Array(values) = nested.object else {
        panic!("expected array");
    };
    assert_eq!(values[0], Object::Reference(ObjectRef::new(6, 0)));
    assert_eq!(
        values[1].as_dict().unwrap().get_ref("V"),
        Some(ObjectRef::new(7, 0))
    );
}

#[test]
fn qpdf_direct_object_reports_empty_body_without_consuming_endobj() {
    let input = b" \nendobj\n";
    let parsed = parse_qpdf_direct_object(input).unwrap();
    assert_eq!(parsed.object, Object::Null);
    assert_eq!(parsed.empty_offset, Some(2));
    assert_eq!(parsed.next_offset, 2);
    assert_eq!(&input[parsed.next_offset..parsed.next_offset + 6], b"endobj");
}
```

- [ ] **Step 4: Run the direct-object tests and verify the missing API failure**

Run:

```bash
cargo test -p flpdf parser::stream_length_tests::qpdf_direct_object -- --nocapture
```

Expected: compilation fails because `parse_qpdf_direct_object` and `ParsedDirectObject` do not exist.

- [ ] **Step 5: Implement the direct-object-only parser mode**

In `crates/flpdf/src/parser.rs`, add:

```rust
#[derive(Debug, PartialEq)]
pub(crate) struct ParsedDirectObject {
    pub(crate) object: Object,
    pub(crate) next_offset: usize,
    pub(crate) empty_offset: Option<usize>,
}

pub(crate) fn parse_qpdf_direct_object(input: &[u8]) -> Result<ParsedDirectObject> {
    let mut parser = Parser::new(input);
    parser.top_level_no_reference = true;
    parser.parse_streams = false;
    parser.skip_ws();

    let empty_offset = keyword_token_end(input, parser.pos, b"endobj").map(|_| parser.pos);
    if let Some(empty_offset) = empty_offset {
        return Ok(ParsedDirectObject {
            object: Object::Null,
            next_offset: empty_offset,
            empty_offset: Some(empty_offset),
        });
    }

    let object = parser.object()?;
    Ok(ParsedDirectObject {
        object,
        next_offset: parser.pos,
        empty_offset: None,
    })
}

pub(crate) fn keyword_token_end(input: &[u8], pos: usize, keyword: &[u8]) -> Option<usize> {
    let end = pos.checked_add(keyword.len())?;
    if input.get(pos..end)? != keyword {
        return None;
    }
    match input.get(end) {
        None => Some(end),
        Some(&byte) if is_ws(byte) || is_delimiter(byte) => Some(end),
        Some(_) => None,
    }
}
```

Add `parse_streams: bool` to `Parser`, initialize it to `true` in both constructors, and change the dictionary tail to:

```rust
if self.starts_with(b">>") {
    self.pos += 2;
    self.skip_ws();
    if self.parse_streams && self.starts_with(b"stream") {
        return self.stream_from_dict(dict);
    }
    return Ok(Object::Dictionary(dict));
}
```

- [ ] **Step 6: Run direct-object tests and strict-parser regressions**

Run:

```bash
cargo test -p flpdf parser::stream_length_tests::qpdf_direct_object -- --nocapture
cargo test -p flpdf parser::stream_length_tests -- --nocapture
```

Expected: all new tests pass; all existing strict stream, empty-object, bare-reference, and recursion tests remain green.

- [ ] **Step 7: Write failing pending-file-object syntax tests**

Create `crates/flpdf/src/reader/file_object.rs` with the interface types above and this test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_returns_pending_direct_and_empty_objects() {
        let direct = parse_file_object_syntax(b"4 0 obj\n[6 0 R]\nendobj\n").unwrap();
        assert_eq!(direct.object_ref, ObjectRef::new(4, 0));
        assert!(matches!(
            direct.body,
            PendingBody::Direct {
                object: Object::Array(_),
                ..
            }
        ));
        assert!(direct.diagnostics.is_empty());

        let empty = parse_file_object_syntax(b"5 0 obj\nendobj\n").unwrap();
        assert_eq!(
            empty.diagnostics,
            vec![FileObjectDiagnostic {
                kind: FileObjectDiagnosticKind::EmptyObject,
                relative_offset: 8,
            }]
        );
    }

    #[test]
    fn syntax_returns_pending_stream_without_reading_payload() {
        let input = b"7 0 obj\n<< /Length 9 0 R >>\nstream\nabcendstream\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        assert_eq!(pending.object_ref, ObjectRef::new(7, 0));
        assert_eq!(pending.indirect_length_ref(), Some(ObjectRef::new(9, 0)));
        let PendingBody::Stream {
            dict,
            data_start,
            start_eol,
        } = pending.body
        else {
            panic!("expected pending stream");
        };
        assert_eq!(dict.get_ref("Length"), Some(ObjectRef::new(9, 0)));
        assert_eq!(&input[data_start..data_start + 3], b"abc");
        assert_eq!(start_eol, StreamStartEol::Lf);
    }

    #[test]
    fn syntax_classifies_every_stream_start_line_ending() {
        for (suffix, expected, warns) in [
            (&b"\nabc"[..], StreamStartEol::Lf, false),
            (&b"\r\nabc"[..], StreamStartEol::CrLf, false),
            (&b"\rabc"[..], StreamStartEol::Cr, true),
            (&b" abc"[..], StreamStartEol::Missing, true),
        ] {
            let mut input = b"1 0 obj\n<< /Length 3 >>\nstream".to_vec();
            input.extend_from_slice(suffix);
            input.extend_from_slice(b"endstream\nendobj\n");
            let pending = parse_file_object_syntax(&input).unwrap();
            assert!(matches!(
                pending.body,
                PendingBody::Stream { start_eol, .. } if start_eol == expected
            ));
            assert_eq!(
                pending
                    .diagnostics
                    .iter()
                    .any(|d| d.kind == FileObjectDiagnosticKind::StreamLineEnding),
                warns
            );
        }
    }
}
```

Declare the child module at the top of `reader.rs`:

```rust
pub(crate) mod file_object;
```

- [ ] **Step 8: Run syntax tests and verify they fail**

Run:

```bash
cargo test -p flpdf reader::file_object::tests::syntax_ -- --nocapture
```

Expected: compilation fails because `parse_file_object_syntax` and `PendingFileObject::indirect_length_ref` are not implemented.

- [ ] **Step 9: Implement pending file-object syntax**

Add this implementation to `reader/file_object.rs`:

```rust
use crate::parser::{is_ws, keyword_token_end, parse_qpdf_direct_object, Parser};
use crate::{Dictionary, Error, Object, ObjectRef, Result};

pub(crate) fn parse_file_object_syntax(input: &[u8]) -> Result<PendingFileObject> {
    let mut header = Parser::new(input);
    let number = header.integer_for_indirect()?;
    let generation = header.integer_for_indirect()?;
    header.expect_keyword_for_indirect(b"obj")?;
    header.skip_ws();
    let body_start = header.position();
    let parsed = parse_qpdf_direct_object(&input[body_start..])?;
    let object_ref = ObjectRef::new(
        u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
        u16::try_from(generation)
            .map_err(|_| Error::parse(0, "invalid indirect generation"))?,
    );
    let next_offset = body_start + parsed.next_offset;
    let mut diagnostics = Vec::new();
    if let Some(empty_offset) = parsed.empty_offset {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::EmptyObject,
            relative_offset: body_start + empty_offset,
        });
    }

    if let Object::Dictionary(dict) = parsed.object {
        let stream_pos = skip_pdf_ws(input, next_offset);
        if let Some(after_stream) = keyword_token_end(input, stream_pos, b"stream") {
            let (data_start, start_eol) = consume_stream_start_eol(input, after_stream);
            if matches!(start_eol, StreamStartEol::Cr | StreamStartEol::Missing) {
                diagnostics.push(FileObjectDiagnostic {
                    kind: FileObjectDiagnosticKind::StreamLineEnding,
                    relative_offset: after_stream,
                });
            }
            return Ok(PendingFileObject {
                object_ref,
                body: PendingBody::Stream {
                    dict,
                    data_start,
                    start_eol,
                },
                diagnostics,
            });
        }
        return Ok(PendingFileObject {
            object_ref,
            body: PendingBody::Direct {
                object: Object::Dictionary(dict),
                next_offset,
            },
            diagnostics,
        });
    }

    Ok(PendingFileObject {
        object_ref,
        body: PendingBody::Direct {
            object: parsed.object,
            next_offset,
        },
        diagnostics,
    })
}

impl PendingFileObject {
    pub(crate) fn indirect_length_ref(&self) -> Option<ObjectRef> {
        match &self.body {
            PendingBody::Stream { dict, .. } => dict.get_ref("Length"),
            PendingBody::Direct { .. } => None,
        }
    }
}

fn skip_pdf_ws(input: &[u8], mut pos: usize) -> usize {
    while input.get(pos).is_some_and(|&byte| is_ws(byte)) {
        pos += 1;
    }
    pos
}

fn consume_stream_start_eol(input: &[u8], pos: usize) -> (usize, StreamStartEol) {
    match input.get(pos..) {
        Some([b'\r', b'\n', ..]) => (pos + 2, StreamStartEol::CrLf),
        Some([b'\n', ..]) => (pos + 1, StreamStartEol::Lf),
        Some([b'\r', ..]) => (pos + 1, StreamStartEol::Cr),
        _ => (pos, StreamStartEol::Missing),
    }
}
```

- [ ] **Step 10: Run Layer 1 gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf reader::file_object::tests::syntax_ -- --nocapture
cargo test -p flpdf parser::stream_length_tests -- --nocapture
cargo test -p flpdf
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: every command exits 0. Production behavior remains unchanged because no existing caller uses `parse_file_object_syntax`.

- [ ] **Step 11: Commit, run committed-HEAD coverage, push, and open the bottom draft PR**

Run:

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader.rs crates/flpdf/src/reader/file_object.rs
git commit -m "refactor: add qpdf file-object syntax model"
scripts/patch-coverage.sh --base origin/main
git push -u origin stack/flpdf-15jp-file-object-syntax
gh pr create --draft --base main --head stack/flpdf-15jp-file-object-syntax --title "refactor: add qpdf file-object syntax model" --body "Layer 1 of flpdf-15jp. Adds direct-object stopping offsets and pending file-object syntax only; production routing is unchanged."
```

Expected: patch coverage reports 100% for changed `crates/flpdf/src` lines, push succeeds, and the draft PR targets `main`.

---

### Task 2: Layer 2 — Pure stream completion, recovery, and diagnostics

**Files:**
- Modify: `crates/flpdf/src/reader/file_object.rs`
- Test: `crates/flpdf/src/reader/file_object.rs` unit module

**Interfaces:**
- Consumes: all Task 1 types and `RecoveredStreamEol::as_bytes(self) -> &'static [u8]`.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryPolicy {
    Strict,
    Bounded,
}

#[derive(Debug, PartialEq)]
pub(crate) struct FileObjectRead {
    pub(crate) object_ref: ObjectRef,
    pub(crate) object: Object,
    pub(crate) diagnostics: Vec<FileObjectDiagnostic>,
    pub(crate) recovered_stream_eol: Option<RecoveredStreamEol>,
}

pub(crate) fn finish_file_object(
    input: &[u8],
    pending: PendingFileObject,
    resolved_indirect_length: Option<usize>,
    policy: RecoveryPolicy,
) -> Result<FileObjectRead>;

impl FileObjectDiagnosticKind {
    pub(crate) fn message(&self) -> String;
}
```

- [ ] **Step 1: Create and claim the Layer 2 branch/Beads child**

Resolve the child by its exact title and claim it:

```bash
git switch -c stack/flpdf-15jp-stream-completion
completion_id="$(bd list --parent flpdf-15jp --title "qpdf file-object reader: pure stream completion" --json | jq -r '.[0].id')"
test -n "$completion_id"
test "$completion_id" != "null"
bd update "$completion_id" --claim
```

Expected: the branch is based on Layer 1 and the completion child is `in_progress`; both guards exit 0.

- [ ] **Step 2: Write failing table-driven exact-length and terminator tests**

Add to `reader/file_object.rs`:

```rust
#[test]
fn exact_lengths_accept_eol_and_adjacent_endstream_payloads() {
    for (payload, tail) in [
        (&b"abc"[..], &b"endstream\nendobj\n"[..]),
        (&b"abc\n"[..], &b"endstream\nendobj\n"[..]),
        (&b"abc\r\n"[..], &b"endstream\nendobj\n"[..]),
    ] {
        let mut input = b"7 0 obj\n<< /Length 9 0 R >>\nstream\n".to_vec();
        input.extend_from_slice(payload);
        input.extend_from_slice(tail);
        let pending = parse_file_object_syntax(&input).unwrap();
        let completed =
            finish_file_object(&input, pending, Some(payload.len()), RecoveryPolicy::Bounded)
                .unwrap();
        assert_eq!(completed.object.as_stream().unwrap().data, payload);
        assert!(!completed
            .diagnostics
            .iter()
            .any(|d| d.kind == FileObjectDiagnosticKind::ExpectedEndobj));
    }
}

#[test]
fn endstream_and_endobj_are_separate_results() {
    let input = b"1 0 obj\n<< /Length 3 >>\nstream\nabcendstream\nnot-endobj\n";
    let pending = parse_file_object_syntax(input).unwrap();
    let completed =
        finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
    assert_eq!(completed.object.as_stream().unwrap().data, b"abc");
    assert_eq!(
        completed.diagnostics.last().unwrap().kind,
        FileObjectDiagnosticKind::ExpectedEndobj
    );
}

#[test]
fn exact_boundary_rejects_endstream_substring_without_token_end() {
    let input = b"1 0 obj\n<< /Length 3 >>\nstream\nabcendstreamX\nendobj\n";
    let pending = parse_file_object_syntax(input).unwrap();
    assert!(finish_file_object(input, pending, None, RecoveryPolicy::Strict).is_err());
}
```

- [ ] **Step 3: Run exact completion tests and verify they fail**

Run:

```bash
cargo test -p flpdf reader::file_object::tests::exact_ -- --nocapture
cargo test -p flpdf reader::file_object::tests::endstream_and_endobj -- --nocapture
```

Expected: compilation fails because `finish_file_object`, `RecoveryPolicy`, and `FileObjectRead` do not exist.

- [ ] **Step 4: Implement direct-object completion and exact stream completion**

Add the Task 2 interface types and:

```rust
use crate::parser::RecoveredStreamEol;
use crate::Stream;

pub(crate) fn finish_file_object(
    input: &[u8],
    pending: PendingFileObject,
    resolved_indirect_length: Option<usize>,
    policy: RecoveryPolicy,
) -> Result<FileObjectRead> {
    let PendingFileObject {
        object_ref,
        body,
        mut diagnostics,
    } = pending;

    match body {
        PendingBody::Direct {
            object,
            next_offset,
        } => {
            check_endobj(input, next_offset, &mut diagnostics);
            Ok(FileObjectRead {
                object_ref,
                object,
                diagnostics,
                recovered_stream_eol: None,
            })
        }
        PendingBody::Stream {
            dict,
            data_start,
            ..
        } => finish_stream(
            input,
            object_ref,
            dict,
            data_start,
            resolved_indirect_length,
            policy,
            diagnostics,
        ),
    }
}

fn finish_stream(
    input: &[u8],
    object_ref: ObjectRef,
    dict: Dictionary,
    data_start: usize,
    resolved_indirect_length: Option<usize>,
    policy: RecoveryPolicy,
    mut diagnostics: Vec<FileObjectDiagnostic>,
) -> Result<FileObjectRead> {
    let length = match dict.get("Length") {
        Some(Object::Integer(value)) => usize::try_from(*value).ok(),
        Some(Object::Reference(_)) => resolved_indirect_length,
        _ => None,
    };
    let exact_end = length.and_then(|length| data_start.checked_add(length));
    let exact_terminator = exact_end
        .filter(|&end| end <= input.len())
        .and_then(|end| keyword_token_end(input, end, b"endstream").map(|after| (end, after)));

    let (data_end, after_endstream, recovered_stream_eol) = match exact_terminator {
        Some((end, after)) => (end, after, None),
        None if policy == RecoveryPolicy::Bounded => {
            recover_stream_boundary(input, data_start, exact_end, &mut diagnostics)?
        }
        None => {
            return Err(Error::parse(
                exact_end.unwrap_or(data_start),
                "expected endstream",
            ));
        }
    };

    check_endobj(input, after_endstream, &mut diagnostics);
    Ok(FileObjectRead {
        object_ref,
        object: Object::Stream(Stream::new(dict, input[data_start..data_end].to_vec())),
        diagnostics,
        recovered_stream_eol,
    })
}

fn check_endobj(
    input: &[u8],
    after_body: usize,
    diagnostics: &mut Vec<FileObjectDiagnostic>,
) {
    let expected = skip_pdf_ws(input, after_body);
    if keyword_token_end(input, expected, b"endobj").is_none() {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::ExpectedEndobj,
            relative_offset: expected,
        });
    }
}
```

- [ ] **Step 5: Run exact completion tests**

Run:

```bash
cargo test -p flpdf reader::file_object::tests::exact_ -- --nocapture
cargo test -p flpdf reader::file_object::tests::endstream_and_endobj -- --nocapture
```

Expected: the three exact-boundary tests pass.

- [ ] **Step 6: Write failing bounded-recovery and diagnostic-order tests**

Add:

```rust
#[test]
fn recovery_matrix_is_bounded_token_aware_and_ordered() {
    struct Case {
        length: &'static [u8],
        payload: &'static [u8],
        terminator: &'static [u8],
        recovered: &'static [u8],
        kinds: Vec<FileObjectDiagnosticKind>,
    }
    let cases = [
        Case {
            length: b"/Length /Bad",
            payload: b"abc\n",
            terminator: b"endstream\nendobj\n",
            recovered: b"abc",
            kinds: vec![
                FileObjectDiagnosticKind::InvalidStreamLength,
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                FileObjectDiagnosticKind::RecoveredStreamLength { length: 3 },
            ],
        },
        Case {
            length: b"/Length -1",
            payload: b"\n",
            terminator: b"endstream\nendobj\n",
            recovered: b"",
            kinds: vec![
                FileObjectDiagnosticKind::InvalidStreamLength,
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                FileObjectDiagnosticKind::RecoveredStreamLength { length: 0 },
                FileObjectDiagnosticKind::EmptyRecoveredStream,
            ],
        },
        Case {
            length: b"/Length 2",
            payload: b"abc\r\n",
            terminator: b"endstream\nendobj\n",
            recovered: b"abc",
            kinds: vec![
                FileObjectDiagnosticKind::ExpectedEndstream,
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                FileObjectDiagnosticKind::RecoveredStreamLength { length: 3 },
            ],
        },
        Case {
            length: b"/Length 99",
            payload: b"abc\n",
            terminator: b"endstream\nendobj\n",
            recovered: b"abc",
            kinds: vec![
                FileObjectDiagnosticKind::ExpectedEndstream,
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                FileObjectDiagnosticKind::RecoveredStreamLength { length: 3 },
            ],
        },
    ];

    for case in cases {
        let mut input = b"1 0 obj\n<< ".to_vec();
        input.extend_from_slice(case.length);
        input.extend_from_slice(b" >>\nstream\n");
        input.extend_from_slice(case.payload);
        input.extend_from_slice(case.terminator);
        let pending = parse_file_object_syntax(&input).unwrap();
        let completed =
            finish_file_object(&input, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(completed.object.as_stream().unwrap().data, case.recovered);
        assert_eq!(
            completed
                .diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.kind)
                .collect::<Vec<_>>(),
            case.kinds
        );
    }
}

#[test]
fn recovery_ignores_keyword_substrings_inside_payload() {
    let input = b"1 0 obj\n<< /Length /Bad >>\nstream\nAendstreamXB endobjY C\nendstream\nendobj\n";
    let pending = parse_file_object_syntax(input).unwrap();
    let completed =
        finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
    assert_eq!(
        completed.object.as_stream().unwrap().data,
        b"AendstreamXB endobjY C"
    );
}

#[test]
fn missing_endstream_can_recover_at_token_bounded_endobj() {
    let input = b"1 0 obj\n<< /Length /Bad >>\nstream\nabc\nendobj\n";
    let pending = parse_file_object_syntax(input).unwrap();
    let completed =
        finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
    assert_eq!(completed.object.as_stream().unwrap().data, b"abc");
    assert!(completed
        .diagnostics
        .iter()
        .any(|d| d.kind == FileObjectDiagnosticKind::ExpectedEndstream));
}
```

- [ ] **Step 7: Run recovery tests and verify they fail**

Run:

```bash
cargo test -p flpdf reader::file_object::tests::recovery_ -- --nocapture
cargo test -p flpdf reader::file_object::tests::missing_endstream -- --nocapture
```

Expected: tests fail because `recover_stream_boundary` is not implemented.

- [ ] **Step 8: Implement bounded recovery and diagnostic messages**

Add:

```rust
fn recover_stream_boundary(
    input: &[u8],
    data_start: usize,
    exact_end: Option<usize>,
    diagnostics: &mut Vec<FileObjectDiagnostic>,
) -> Result<(usize, usize, Option<RecoveredStreamEol>)> {
    diagnostics.push(FileObjectDiagnostic {
        kind: if exact_end.is_some() {
            FileObjectDiagnosticKind::ExpectedEndstream
        } else {
            FileObjectDiagnosticKind::InvalidStreamLength
        },
        relative_offset: exact_end.unwrap_or(data_start).min(input.len()),
    });
    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
        relative_offset: data_start,
    });

    if let Some(endstream_pos) = find_bounded_keyword(input, data_start, b"endstream") {
        let (data_end, recovered_eol) = trim_one_framing_eol(input, data_start, endstream_pos);
        let length = data_end - data_start;
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::RecoveredStreamLength { length },
            relative_offset: endstream_pos,
        });
        if length == 0 {
            diagnostics.push(FileObjectDiagnostic {
                kind: FileObjectDiagnosticKind::EmptyRecoveredStream,
                relative_offset: data_start,
            });
        }
        let after = keyword_token_end(input, endstream_pos, b"endstream")
            .expect("bounded keyword finder returns a token");
        return Ok((data_end, after, recovered_eol));
    }

    if let Some(endobj_pos) = find_bounded_keyword(input, data_start, b"endobj") {
        let (data_end, recovered_eol) = trim_one_framing_eol(input, data_start, endobj_pos);
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::ExpectedEndstream,
            relative_offset: endobj_pos,
        });
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::RecoveredStreamLength {
                length: data_end - data_start,
            },
            relative_offset: endobj_pos,
        });
        return Ok((data_end, endobj_pos, recovered_eol));
    }

    Err(Error::parse(data_start, "unable to recover stream length"))
}

fn find_bounded_keyword(input: &[u8], start: usize, keyword: &[u8]) -> Option<usize> {
    let last = input.len().checked_sub(keyword.len())?;
    (start..=last).find(|&pos| {
        let left_bounded = pos == start
            || input
                .get(pos.wrapping_sub(1))
                .is_some_and(|&byte| is_ws(byte) || crate::parser::is_delimiter(byte));
        left_bounded && keyword_token_end(input, pos, keyword).is_some()
    })
}

fn trim_one_framing_eol(
    input: &[u8],
    data_start: usize,
    terminator: usize,
) -> (usize, Option<RecoveredStreamEol>) {
    if terminator >= data_start + 2 && &input[terminator - 2..terminator] == b"\r\n" {
        (terminator - 2, Some(RecoveredStreamEol::CrLf))
    } else if terminator > data_start && input[terminator - 1] == b'\n' {
        (terminator - 1, Some(RecoveredStreamEol::Lf))
    } else if terminator > data_start && input[terminator - 1] == b'\r' {
        (terminator - 1, Some(RecoveredStreamEol::Cr))
    } else {
        (terminator, None)
    }
}

impl FileObjectDiagnosticKind {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::EmptyObject => "empty object treated as null".into(),
            Self::StreamLineEnding => "stream keyword not followed by proper line ending".into(),
            Self::InvalidStreamLength => "stream length is not a non-negative integer".into(),
            Self::ExpectedEndstream => "expected endstream".into(),
            Self::AttemptingStreamLengthRecovery => {
                "attempting to recover stream length".into()
            }
            Self::RecoveredStreamLength { length } => {
                format!("recovered stream length: {length}")
            }
            Self::EmptyRecoveredStream => "recovered empty stream".into(),
            Self::ExpectedEndobj => "expected endobj".into(),
        }
    }
}
```

Before committing, compare these strings and their order against qpdf 11.9.0 for each malformed case. If qpdf wording differs, update the literal strings and the matching assertions together; do not collapse diagnostic kinds.

- [ ] **Step 9: Run the full matrix and Layer 2 gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf reader::file_object::tests -- --nocapture
cargo test -p flpdf parser::stream_length_tests -- --nocapture
cargo test -p flpdf
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all tests pass and no production caller uses `finish_file_object`.

- [ ] **Step 10: Commit, cover, push, and open the middle draft PR**

Run:

```bash
git add crates/flpdf/src/reader/file_object.rs
git commit -m "refactor: add qpdf stream completion engine"
scripts/patch-coverage.sh --base stack/flpdf-15jp-file-object-syntax
git push -u origin stack/flpdf-15jp-stream-completion
gh pr create --draft --base stack/flpdf-15jp-file-object-syntax --head stack/flpdf-15jp-stream-completion --title "refactor: add qpdf stream completion engine" --body "Layer 2 of flpdf-15jp. Adds pure exact-length completion, bounded recovery, and structured diagnostics; production routing is unchanged."
```

Expected: 100% patch coverage relative to Layer 1 and a draft PR based on the Layer 1 branch.

---

### Task 3: Layer 3 — Switch normal uncompressed object resolution

**Files:**
- Modify: `crates/flpdf/src/reader.rs:103-111, 1144-1425, 2630-3380`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs:540-620` and test helpers
- Modify: `crates/flpdf-cli/tests/compat_matrix_tests.rs`
- Create: `tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf`

**Interfaces:**
- Consumes: `parse_file_object_syntax`, `PendingFileObject::indirect_length_ref`, `finish_file_object`, `RecoveryPolicy::Bounded`, `FileObjectRead`, `FileObjectDiagnosticKind::message`.
- Produces:

```rust
fn read_object_at_qpdf(
    &mut self,
    expected_ref: ObjectRef,
    offset: u64,
) -> Result<file_object::FileObjectRead>;
fn parse_and_finish_file_object(
    &mut self,
    expected_ref: ObjectRef,
    bytes: &[u8],
    offset: u64,
) -> Result<file_object::FileObjectRead>;
fn resolve_pending_stream_length(
    &mut self,
    expected_ref: ObjectRef,
    pending: &file_object::PendingFileObject,
    offset: u64,
) -> Result<Option<usize>>;
fn record_file_object_diagnostics(
    &mut self,
    object_ref: ObjectRef,
    offset: u64,
    diagnostics: Vec<file_object::FileObjectDiagnostic>,
);
```

The old `read_object_at`, `FileObjectRead`, `apply_indirect_stream_length`, and `reslice_indirect_stream_length` stay temporarily for `resolve_compressed_entry`; only `resolve_to_cache` switches in this layer.

- [ ] **Step 1: Start and claim Layer 3**

Run:

```bash
git switch -c stack/flpdf-15jp-normal-object-routing
normal_id="$(bd list --parent flpdf-15jp --title "qpdf file-object reader: normal object routing" --json | jq -r '.[0].id')"
test -n "$normal_id"
test "$normal_id" != "null"
bd update "$normal_id" --claim
```

Expected: the branch is based on Layer 2, both guards exit 0, and the normal-routing child is claimed.

- [ ] **Step 2: Add the authored good14-shaped compatibility fixture**

Create `tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf` with these exact authored bytes:

```text
%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Contents 4 0 R >>
endobj
4 0 obj
<< /Length 5 0 R >>
stream
q 1 0 0 1 0 0 cm Qendstream
endobj
5 0 obj
18
endobj
xref
0 6
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
0000000200 00000 n 
0000000270 00000 n 
trailer
<< /Size 6 /Root 1 0 R >>
startxref
288
%%EOF
```

The offsets above are verified for those exact LF bytes: objects start at 9, 58, 115, 200, and 270; xref starts at 288. Run:

```bash
qpdf --check tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf
```

Expected: qpdf 11.9.0 exits 0 with no warnings.

- [ ] **Step 3: Write failing reader and CLI regressions**

Add to `reader.rs` tests:

```rust
#[test]
fn qpdf_reader_completes_adjacent_endstream_before_endobj_check() {
    let bytes = recovered_stream_fixture(
        b"/Length 2 0 R",
        b"",
        Some(b"2 0 obj\n3\nendobj\n"),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    let object_ref = ObjectRef::new(1, 0);
    assert_eq!(
        pdf.resolve(object_ref).unwrap().as_stream().unwrap().data,
        b"abc"
    );
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .all(|diagnostic| !diagnostic.message.contains("expected endobj")));
}

#[test]
fn qpdf_reader_registers_file_object_diagnostics_once_after_cache_commit() {
    let mut pdf = Pdf::open_mem_owned(top_level_bare_reference_pdf()).unwrap();
    let object_ref = ObjectRef::new(4, 0);
    assert_eq!(pdf.resolve(object_ref).unwrap(), Object::Integer(3));
    assert_eq!(
        pdf.resolve_qpdf_json_object(object_ref).unwrap(),
        Object::Integer(3)
    );
    assert_eq!(
        pdf.repair_diagnostics()
            .entries()
            .iter()
            .filter(|entry| entry.message.contains("expected endobj"))
            .count(),
        1
    );
}

#[test]
fn qpdf_reader_bounds_unusable_indirect_length_recovery() {
    let cases = [
        classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 1 0 R >>\nstream\nabc\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        ),
        classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 99 0 R >>\nstream\nabc\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        ),
        classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n",
                b"2 0 obj\n<< /Length 1 0 R >>\nstream\nxyz\nendstream\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        ),
    ];

    for bytes in cases {
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        assert_eq!(
            pdf.resolve(ObjectRef::new(1, 0))
                .unwrap()
                .as_stream()
                .unwrap()
                .data,
            b"abc"
        );
    }
}
```

Add to `cli_tests.rs`:

```rust
#[test]
fn qdf_adjacent_endstream_with_indirect_length_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("good14-shaped-qdf.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--qdf",
            "../../tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .code(0)
        .stderr(predicate::str::is_empty());
    assert!(output.exists());
}
```

- [ ] **Step 4: Run regressions and confirm the current false warning**

Run:

```bash
cargo test -p flpdf qpdf_reader_completes_adjacent_endstream_before_endobj_check -- --nocapture
cargo test -p flpdf-cli --test cli_tests qdf_adjacent_endstream_with_indirect_length_is_silent -- --nocapture
```

Expected: the reader test finds an `expected endobj` diagnostic and the CLI test exits 3 with the same warning.

- [ ] **Step 5: Add the new normal-object read path and preserve fallback bounds**

Import:

```rust
use self::file_object::{
    finish_file_object, parse_file_object_syntax, FileObjectDiagnostic, PendingFileObject,
    RecoveryPolicy,
};
```

Implement:

```rust
fn parse_and_finish_file_object(
    &mut self,
    expected_ref: ObjectRef,
    bytes: &[u8],
    offset: u64,
) -> Result<file_object::FileObjectRead> {
    let pending = parse_file_object_syntax(bytes)?;
    let resolved_length =
        self.resolve_pending_stream_length(expected_ref, &pending, offset)?;
    let result = finish_file_object(bytes, pending, resolved_length, RecoveryPolicy::Bounded);
    self.cache.set_unresolved(expected_ref, offset);
    result
}

fn resolve_pending_stream_length(
    &mut self,
    expected_ref: ObjectRef,
    pending: &PendingFileObject,
    offset: u64,
) -> Result<Option<usize>> {
    let Some(holder) = pending.indirect_length_ref() else {
        return Ok(None);
    };
    if holder == pending.object_ref {
        return Ok(None);
    }
    self.cache.set_reserved(expected_ref);
    let resolved = match self.resolve_borrowed(holder) {
        Ok(Object::Integer(value)) => usize::try_from(*value).ok(),
        Ok(_) => None,
        Err(err) => {
            self.cache.set_unresolved(expected_ref, offset);
            return Err(err);
        }
    };
    self.cache.set_unresolved(expected_ref, offset);
    Ok(resolved)
}

fn read_object_at_qpdf(
    &mut self,
    expected_ref: ObjectRef,
    offset: u64,
) -> Result<file_object::FileObjectRead> {
    let next = self.next_object_offset(offset);
    self.reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    match next {
        Some(next) => {
            self.reader
                .by_ref()
                .take(next.saturating_sub(offset))
                .read_to_end(&mut bytes)?;
        }
        None => self.reader.read_to_end(&mut bytes)?,
    };

    match self.parse_and_finish_file_object(expected_ref, &bytes, offset) {
        Ok(parsed) => Ok(parsed),
        Err(window_err) if next.is_some() && self.resolution_fallbacks_remaining > 0 => {
            self.resolution_fallbacks_remaining -= 1;
            self.reader.seek(SeekFrom::Start(offset))?;
            let mut full = Vec::new();
            self.reader.read_to_end(&mut full)?;
            self.parse_and_finish_file_object(expected_ref, &full, offset)
                .or(Err(window_err))
        }
        Err(err) => Err(err),
    }
}
```

When implementing the final error preference, retain the existing contract: return the successful full parse; if full parsing also fails, return `window_err`.

- [ ] **Step 6: Switch only `resolve_to_cache` and register structured diagnostics**

Replace its read call with:

```rust
let parsed = self.read_object_at_qpdf(object_ref, offset)?;
if parsed.object_ref != object_ref {
    return Ok(false);
}
let recovered_eol = parsed.recovered_stream_eol.map(RecoveredStreamEol::as_bytes);
let (object, stream_payload_transformed) =
    self.decrypt_resolved_object(object_ref, parsed.object, recovered_eol)?;
self.cache.set_resolved(object_ref, object);
if stream_payload_transformed {
    self.transformed_stream_refs.insert(object_ref);
} else {
    self.transformed_stream_refs.remove(&object_ref);
}
if let Some(eol) = parsed.recovered_stream_eol {
    self.recovered_stream_eols.insert(object_ref, eol);
} else {
    self.recovered_stream_eols.remove(&object_ref);
}
self.record_file_object_diagnostics(object_ref, offset, parsed.diagnostics);
```

Replace the two-field warning helper for the new path with:

```rust
fn record_file_object_diagnostics(
    &mut self,
    object_ref: ObjectRef,
    offset: u64,
    diagnostics: Vec<FileObjectDiagnostic>,
) {
    for diagnostic in diagnostics {
        self.push_warning(format!(
            "(object {} {}, offset {}): {}",
            object_ref.number,
            object_ref.generation,
            offset.saturating_add(diagnostic.relative_offset as u64),
            diagnostic.kind.message()
        ));
    }
}
```

Do not alter `resolve_compressed_entry` yet; it must continue to use the old `read_object_at` and old warning helper until Layer 4.

- [ ] **Step 7: Run normal-path, encryption, cache, bounded-window, and CLI tests**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf qpdf_reader_ -- --nocapture
cargo test -p flpdf normal_and_json_resolution_share_qpdf_file_object_value_and_warning -- --nocapture
cargo test -p flpdf qpdf_object_read_uses_bounded_fallback_and_preserves_strict_errors -- --nocapture
cargo test -p flpdf encryption -- --nocapture
cargo test -p flpdf-cli --test cli_tests qdf_adjacent_endstream_with_indirect_length_is_silent -- --nocapture
cargo test -p flpdf
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0; the new CLI regression exits 0 with empty stderr; cached resolution emits one warning; encryption tests prove completion precedes decryption.

- [ ] **Step 8: Add qpdf differential object-body comparison**

In `compat_matrix_tests.rs`, add helpers and test:

```rust
fn qdf_object_body(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .windows(b"%QDF-1.0".len())
        .position(|window| window == b"%QDF-1.0")
        .expect("QDF marker");
    let end = bytes
        .windows(b"\nxref\n".len())
        .position(|window| window == b"\nxref\n")
        .expect("classic xref");
    &bytes[start..end]
}

#[test]
fn qpdf_good14_shaped_qdf_object_body_matches_qpdf_11_9() {
    if !is_qpdf_available() {
        return;
    }
    let input = fixture_path("good14-shaped-indirect-length-adjacent-endstream.pdf");
    let tmp = tempdir().unwrap();
    let qpdf_out = tmp.path().join("qpdf.pdf");
    let flpdf_out = tmp.path().join("flpdf.pdf");
    run_qpdf_with_args(&["--qdf", input.to_str().unwrap(), qpdf_out.to_str().unwrap()]);
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--qdf", input.to_str().unwrap(), flpdf_out.to_str().unwrap()])
        .assert()
        .success()
        .code(0)
        .stderr(predicate::str::is_empty());
    assert_eq!(
        qdf_object_body(&fs::read(qpdf_out).unwrap()),
        qdf_object_body(&fs::read(flpdf_out).unwrap())
    );
}
```

If the existing CLI argument convention requires input before `--qdf`, use the convention already exercised by nearby tests while preserving the same assertions.

- [ ] **Step 9: Run differential and Layer 3 gates**

Run:

```bash
cargo test -p flpdf-cli --test compat_matrix_tests qpdf_good14_shaped_qdf_object_body_matches_qpdf_11_9 -- --nocapture
cargo fmt --all -- --check
cargo test -p flpdf
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: qpdf/flpdf object-body regions are byte-identical, and all crate gates pass.

- [ ] **Step 10: Commit, cover, push, and open the Layer 3 draft PR**

Run:

```bash
git add crates/flpdf/src/reader.rs crates/flpdf-cli/tests/cli_tests.rs crates/flpdf-cli/tests/compat_matrix_tests.rs tests/fixtures/compat/good14-shaped-indirect-length-adjacent-endstream.pdf
git commit -m "fix: complete file-object streams before endobj"
scripts/patch-coverage.sh --base stack/flpdf-15jp-stream-completion
git push -u origin stack/flpdf-15jp-normal-object-routing
gh pr create --draft --base stack/flpdf-15jp-stream-completion --head stack/flpdf-15jp-normal-object-routing --title "fix: complete file-object streams before endobj" --body "Layer 3 of flpdf-15jp. Switches normal uncompressed objects to the qpdf-shaped completion path and fixes the authored good14-shaped regression."
```

Expected: 100% Layer 3 patch coverage and a draft PR based on Layer 2.

---

### Task 4: Layer 4 — Switch ObjStm containers and xref bootstrap, then remove the old path

**Files:**
- Modify: `crates/flpdf/src/reader.rs:1-10, 103-111, 1144-1540, 1800-1860, 2090-2160, 2630-3380`
- Modify: `crates/flpdf/src/parser.rs:16-148, 285-400, 840-1030`
- Modify: `crates/flpdf/src/xref.rs:1-12, 741-787` and xref unit tests
- Modify: `docs/superpowers/specs/2026-07-24-qpdf-file-object-stream-reader-design.md`

**Interfaces:**
- Consumes: the final Task 1–3 file-object interfaces.
- Produces: one canonical `read_object_at(&mut self, expected_ref: ObjectRef, offset: u64) -> Result<file_object::FileObjectRead>` used by normal objects and ObjStm containers; one bootstrap `parse_xref_stream` using `parse_file_object_syntax` plus `finish_file_object`; no parser-owned file-object stream placeholder/reslice path.

- [ ] **Step 1: Start and claim Layer 4**

Run:

```bash
git switch -c stack/flpdf-15jp-container-xref-routing
bootstrap_id="$(bd list --parent flpdf-15jp --title "qpdf file-object reader: ObjStm and xref routing" --json | jq -r '.[0].id')"
test -n "$bootstrap_id"
test "$bootstrap_id" != "null"
bd update "$bootstrap_id" --claim
```

Expected: the branch is based on Layer 3, both guards exit 0, and the bootstrap child is claimed.

- [ ] **Step 2: Write failing ObjStm-container and xref-bootstrap tests**

Add to `reader.rs`:

```rust
#[test]
fn objstm_container_uses_qpdf_completion_but_members_remain_direct_objects() {
    let bytes = classic_pdf_with_bodies(
        &[
            b"1 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 2 0 R >>\nstream\n7 0 6 0 Rendstream\nendobj\n",
            b"2 0 obj\n9\nendobj\n",
        ],
        ObjectRef::new(1, 0),
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    pdf.cache.set_compressed(ObjectRef::new(7, 0), 1, 0);
    assert_eq!(
        pdf.resolve(ObjectRef::new(7, 0)).unwrap(),
        Object::Integer(6)
    );
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .all(|entry| !entry.message.contains("expected endobj")));
}
```

Add an xref unit fixture whose xref stream has direct `/Length` and payload-adjacent `endstream`, then assert:

```rust
let loaded = load_xref_and_trailer(Cursor::new(bytes)).unwrap();
assert_eq!(loaded.loaded.last_xref_form, XrefForm::Stream);
assert!(loaded.loaded.repair_diagnostics.entries().is_empty());
```

Add another bootstrap fixture with unusable indirect `/Length` and a line-anchored, token-bounded `endstream`; assert it loads through bounded recovery and records the expected structured warning order.

- [ ] **Step 3: Run the new tests and verify current routing failures**

Run:

```bash
cargo test -p flpdf objstm_container_uses_qpdf_completion_but_members_remain_direct_objects -- --nocapture
cargo test -p flpdf xref_stream -- --nocapture
```

Expected: the ObjStm test exposes the old placeholder/endobj ordering or fails to resolve its member; the authored bootstrap recovery test fails because `xref.rs` still calls `parse_indirect_object`.

- [ ] **Step 4: Switch ObjStm containers to the canonical reader**

Rename `read_object_at_qpdf` to `read_object_at`, delete the old function with that name, and replace the `resolve_compressed_entry` unresolved-container arm with the same completion/decrypt/cache/diagnostic sequence used by `resolve_to_cache`:

```rust
let parsed = self.read_object_at(stream_ref, offset)?;
if parsed.object_ref != stream_ref {
    return Ok(false);
}
let recovered_eol = parsed.recovered_stream_eol.map(RecoveredStreamEol::as_bytes);
let (object, stream_payload_transformed) =
    self.decrypt_resolved_object(stream_ref, parsed.object, recovered_eol)?;
self.cache.set_resolved(stream_ref, object.clone());
if stream_payload_transformed {
    self.transformed_stream_refs.insert(stream_ref);
} else {
    self.transformed_stream_refs.remove(&stream_ref);
}
if let Some(eol) = parsed.recovered_stream_eol {
    self.recovered_stream_eols.insert(stream_ref, eol);
} else {
    self.recovered_stream_eols.remove(&stream_ref);
}
self.record_file_object_diagnostics(stream_ref, offset, parsed.diagnostics);
object
```

Keep this member parser unchanged:

```rust
parse_qpdf_file_object(&stream_data[start..])
```

It parses ObjStm members as direct objects without `obj`, `stream`, `endstream`, or `endobj` checks.

- [ ] **Step 5: Switch xref-stream bootstrap to the shared completion engine**

In `xref.rs`, import the child module and diagnostics:

```rust
use crate::diagnostics::Diagnostic;
use crate::reader::file_object::{
    finish_file_object, parse_file_object_syntax, FileObjectDiagnostic, RecoveryPolicy,
};
```

Replace the old parse call in `parse_xref_stream`:

```rust
let pending =
    parse_file_object_syntax(tail).map_err(|err| err.rebase_offset(xref_pos))?;
let object_ref = pending.object_ref;
let completed = finish_file_object(tail, pending, None, RecoveryPolicy::Bounded)
    .map_err(|err| err.rebase_offset(xref_pos))?;
let object = completed.object;
let mut repair_diagnostics = Diagnostics::default();
for diagnostic in completed.diagnostics {
    repair_diagnostics.push(xref_file_object_diagnostic(
        object_ref,
        xref_pos as u64,
        diagnostic,
    ));
}
```

Add:

```rust
fn xref_file_object_diagnostic(
    object_ref: ObjectRef,
    offset: u64,
    diagnostic: FileObjectDiagnostic,
) -> Diagnostic {
    Diagnostic::warning(
        format!(
            "(object {} {}, offset {}): {}",
            object_ref.number,
            object_ref.generation,
            offset.saturating_add(diagnostic.relative_offset as u64),
            diagnostic.kind.message()
        ),
        Some(offset.saturating_add(diagnostic.relative_offset as u64)),
    )
}
```

Put `repair_diagnostics` into the returned `LoadedXref` instead of `Diagnostics::default()`. A direct integer `/Length` is authoritative; an unresolved indirect length passes `None` and therefore uses bounded recovery. Do not introduce an xref-only stream scanner.

- [ ] **Step 6: Delete superseded parser and reader machinery**

Delete from `parser.rs`:

```rust
IndirectStreamLength
ParsedIndirectObject
parse_indirect_object_detailed
parse_indirect_object_detailed_qpdf
parse_indirect_object_detailed_impl
Parser::last_indirect_stream_len
Parser::last_recovered_stream_eol
Parser::stream_from_dict
```

Add `parse_strict_direct_object(input: &[u8]) -> Result<ParsedDirectObject>` in `parser.rs`; it sets `parse_streams = false`, keeps `top_level_no_reference = false`, and returns a parse error for an empty body. Add `parse_strict_file_object_syntax(input: &[u8]) -> Result<PendingFileObject>` in `reader/file_object.rs` by sharing the header and stream-detection implementation with `parse_file_object_syntax`, but selecting `parse_strict_direct_object`.

Retain strict `parse_indirect_object` by composing those strict syntax primitives with strict completion:

```rust
pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let pending = crate::reader::file_object::parse_strict_file_object_syntax(input)?;
    let completed = crate::reader::file_object::finish_file_object(
        input,
        pending,
        None,
        crate::reader::file_object::RecoveryPolicy::Strict,
    )?;
    Ok((completed.object_ref, completed.object))
}
```

Retain `parse_qpdf_file_object` for ObjStm members. Retain `RecoveredStreamEol` because the shared completion result and decryption metadata use it.

Delete from `reader.rs`:

```rust
the old FileObjectRead struct
record_file_object_warnings
apply_indirect_stream_length
reslice_indirect_stream_length
stream_end_boundary_at
the duplicate reader-local keyword_token_at if no remaining caller needs it
```

Move any still-used token helper into `parser.rs` or `reader/file_object.rs` so there is one token-boundary implementation.

- [ ] **Step 7: Run focused routing and deletion regressions**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf objstm_container_uses_qpdf_completion_but_members_remain_direct_objects -- --nocapture
cargo test -p flpdf object_stream_file_object_mode_only_integerizes_bare_reference_member -- --nocapture
cargo test -p flpdf xref_stream -- --nocapture
cargo test -p flpdf parser::stream_length_tests -- --nocapture
cargo test -p flpdf qpdf_object_read_uses_bounded_fallback_and_preserves_strict_errors -- --nocapture
cargo test -p flpdf normal_and_json_resolution_share_qpdf_file_object_value_and_warning -- --nocapture
```

Expected: all pass; ObjStm containers and xref streams use the shared engine; ObjStm member behavior is unchanged; strict parser tests remain strict.

- [ ] **Step 8: Run full repository gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test compat_matrix_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --document-private-items
```

Expected: every command exits 0; the compatibility suite may skip qpdf-dependent tests only when qpdf is absent.

- [ ] **Step 9: Run the external qpdf 11.9.0 and qtest gates**

Build the final CLI and run the authored differential:

```bash
cargo build --release -p flpdf-cli
qpdf --version
cargo test -p flpdf-cli --test compat_matrix_tests qpdf_good14_shaped_qdf_object_body_matches_qpdf_11_9 -- --nocapture
```

Expected: version output identifies qpdf 11.9.0, the differential test passes, and flpdf exits 0 silently.

Then from `/home/ubuntu/flpdf-qtest` run:

```bash
QTEST_TESTS=basic-parsing FLPDF_CLI_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-15jp-qpdf-file-reader/target/release/flpdf ./scripts/run.sh
rg -n "^basic-parsing 41 \\(create qdf\\).*PASSED$" harness.log
```

Expected: the harness line for `basic-parsing 41 (create qdf)` ends in `PASSED`. Do not accept the suite's aggregate exit code alone because its allowlist may mask unrelated known failures.

- [ ] **Step 10: Commit first, then run authoritative full-stack patch coverage**

Run:

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader.rs crates/flpdf/src/reader/file_object.rs crates/flpdf/src/xref.rs docs/superpowers/specs/2026-07-24-qpdf-file-object-stream-reader-design.md
git commit -m "refactor: unify file-object stream consumers"
scripts/patch-coverage.sh --base origin/main
```

Expected: the worktree is clean before coverage and the summary reports 100% changed-line coverage for every changed executable line under `crates/flpdf/src`.

If coverage identifies a line, add a focused behavioral test that reaches that branch, commit it, and rerun the fresh coverage command. Use `cov:ignore` only for a demonstrated compiler/llvm-cov artifact or an actually unreachable defensive arm, with the concrete reason in the inline comment.

- [ ] **Step 11: Close/push tracker state, push the top branch, and open the top draft PR**

Resolve all four children by exact title, guard every lookup, then close them:

```bash
syntax_id="$(bd list --all --parent flpdf-15jp --title "qpdf file-object reader: direct syntax model" --json | jq -r '.[0].id')"
completion_id="$(bd list --all --parent flpdf-15jp --title "qpdf file-object reader: pure stream completion" --json | jq -r '.[0].id')"
normal_id="$(bd list --all --parent flpdf-15jp --title "qpdf file-object reader: normal object routing" --json | jq -r '.[0].id')"
bootstrap_id="$(bd list --all --parent flpdf-15jp --title "qpdf file-object reader: ObjStm and xref routing" --json | jq -r '.[0].id')"
test -n "$syntax_id"
test "$syntax_id" != "null"
test -n "$completion_id"
test "$completion_id" != "null"
test -n "$normal_id"
test "$normal_id" != "null"
test -n "$bootstrap_id"
test "$bootstrap_id" != "null"
bd close "$syntax_id" --reason "Direct-object syntax and pending file-object model implemented with green tests and 100% layer coverage"
bd close "$completion_id" --reason "Pure qpdf-style completion and bounded recovery implemented with green tests and 100% layer coverage"
bd close "$normal_id" --reason "Normal object path uses completed stream framing; good14-shaped QDF is silent and byte-matched"
bd close "$bootstrap_id" --reason "ObjStm containers and xref streams use the shared engine; old placeholder path removed; final gates pass"
bd close flpdf-15jp --reason "qpdf 11.9.0 file-object read ordering implemented across normal, ObjStm, and xref paths; qtest 41 and 100% patch coverage pass"
bd dolt push
git push -u origin stack/flpdf-15jp-container-xref-routing
gh pr create --draft --base stack/flpdf-15jp-normal-object-routing --head stack/flpdf-15jp-container-xref-routing --title "refactor: unify file-object stream consumers" --body "Layer 4 of flpdf-15jp. Switches ObjStm containers and xref bootstrap to the shared qpdf-style completion engine, removes the old placeholder/reslice path, and passes qtest basic-parsing 41."
```

Expected: all Beads items are closed, Beads and Git pushes succeed, and the top draft PR targets Layer 3.

- [ ] **Step 12: Verify the published stack and hand off for review**

Run:

```bash
git status --short --branch
gh pr view --json number,url,state,isDraft,baseRefName,headRefName
bd show flpdf-15jp
```

Expected: clean pushed branch, open draft top PR with the correct base/head, and the parent plus four children are closed. Add the four PR URLs and final test/coverage evidence to the parent Beads notes if they are not already present, then run `bd dolt push` once more.

---

## Self-Review Checklist

- [ ] The parser entry stops before `stream`, `endstream`, and `endobj`; empty and top-level bare-reference behavior is explicit.
- [ ] Pending and completed stream states cannot be confused by type.
- [ ] Direct, indirect, missing, non-integer, negative, dangling, self/cyclic, stale-short, and stale-long lengths have exact/recovery coverage at unit or reader level.
- [ ] LF, CRLF, CR, missing stream-start EOL, EOL/non-EOL payload, and adjacent `endstream` are covered.
- [ ] Missing/malformed `endstream`, missing/malformed `endobj`, and token-like payload substrings are covered separately.
- [ ] Normal, encrypted, ObjStm-container, ObjStm-member, xref-bootstrap, first-resolution, and cached-resolution paths are covered.
- [ ] Cache `Reserved` and `Unresolved` restoration is tested on success and error.
- [ ] Production does not route through the new path until Layer 3; ObjStm/xref do not switch until Layer 4.
- [ ] The old placeholder/reslice/combined-boundary implementation is absent at the end.
- [ ] qpdf 11.9.0 differential compares only the intended QDF object-body region.
- [ ] flpdf-qtest `basic-parsing 41 (create qdf)` is explicitly `PASSED`.
- [ ] Full workspace tests, Clippy, private rustdoc, and clean committed-HEAD patch coverage all pass.
- [ ] All Beads children, branches, PR bases, and final remote pushes are verified.
