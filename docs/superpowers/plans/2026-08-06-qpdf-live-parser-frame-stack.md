# qpdf Live Parser Frame Stack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make live file-object parsing accept qpdf's 500 nested containers without consuming one native call frame per container.

**Architecture:** Replace the recursive `LiveFileParser::{array,dictionary,parse_from_token}` container path with an explicit `Vec<LiveFrame>` and one token-processing loop, mirroring qpdf 11.9.0 `QPDFParser::parseRemainder`. Scalar parsing, reference lookahead, diagnostic emission, parsed offsets, and no-context errors remain owned by `LiveFileParser`.

**Tech Stack:** Rust, `ObjectHandle`, live tokenizer, qpdf 11.9.0 source oracle, cargo test.

## Global Constraints

- qpdf 11.9.0 source is the semantic oracle; preserve its 500 accepted / 501st recovered boundary.
- `LiveFileParser` must not use native recursion to enter arrays or dictionaries.
- Preserve existing diagnostics, parsed offsets, `endobj` unread behavior, and no-context parsing errors.
- Keep the 256 KiB cross-platform stack regression; do not weaken it.
- Do not change public APIs or unrelated parser paths.

---

### Task 1: Establish the stack-independent regression

**Files:**
- Modify: `crates/flpdf/src/parser.rs:live_input_tests`

**Interfaces:**
- Consumes: `parse_live_file_object`, `CountingInput`, `NullResolver`, `MAX_PARSE_DEPTH`.
- Produces: a 500-level nested-array regression that fails on recursive implementations and asserts successful non-null output.

- [ ] **Step 1: Confirm the existing cross-platform RED evidence**

Run: `gh run view 31025108242 --log-failed`

Expected: the macOS, Windows, and Ubuntu ARM jobs show the unnamed 256 KiB
thread overflowing its stack in
`live_file_parser_accepts_qpdfs_500_container_limit_on_a_small_stack`.

- [ ] **Step 2: Keep one real-behavior test**

Retain the test input and assertion shape below; it must never test a mock or
merely inspect parser internals:

```rust
let mut bytes = vec![b'['; MAX_PARSE_DEPTH];
bytes.extend(std::iter::repeat_n(b']', MAX_PARSE_DEPTH));
let outcome = std::thread::Builder::new()
    .stack_size(256 * 1024)
    .spawn(move || parse_live_file_object(&mut input, &mut resolver))
    .expect("spawn small-stack parser thread")
    .join()
    .expect("live parser must not overflow the caller stack");
assert!(!outcome.expect("500 nested containers must parse").value.is_null());
```

- [ ] **Step 3: Run the focused regression**

Run: `cargo test -p flpdf --lib parser::live_input_tests::live_file_parser_accepts_qpdfs_500_container_limit_on_a_small_stack -- --exact`

Expected before Task 2: CI remains the authoritative RED result on the three
failing targets; after Task 2 this command passes locally and the same test
must pass in all CI matrix jobs.

### Task 2: Port qpdf's iterative parser-frame machine

**Files:**
- Modify: `crates/flpdf/src/parser.rs:LiveFileParser`

**Interfaces:**
- Consumes: `LiveTokenSource`, `HandleResolver`, `ObjectHandle`,
  `ParserDiagnostic`, `Token`.
- Produces: `LiveFileParser::parse() -> Result<LiveParsedObject>` with no
  recursive container descent.

- [ ] **Step 1: Introduce a frame representation**

Add a private enum that owns the incomplete container state rather than a
call frame:

```rust
enum LiveFrame {
    Array { values: Vec<ObjectHandle>, start: usize },
    Dictionary {
        values: BTreeMap<Vec<u8>, ObjectHandle>,
        orphan_values: Vec<ObjectHandle>,
        pending_key: Option<Vec<u8>>,
        start: usize,
        frame_offset: usize,
    },
}
```

- [ ] **Step 2: Write the single-loop control flow**

Replace `array` and `dictionary` recursion with a loop that reads one token
at a time. On `[` or `<<`, check `frames.len() >= MAX_PARSE_DEPTH`; if true,
emit `ignoring excessively deeply nested data structure` and return null.
Otherwise push an `Array` or `Dictionary` frame. On a matching close, finish
the top frame, pop it, and add the resulting handle to its parent; if no
parent remains, return it.

```rust
match token.token_type {
    TokenType::ArrayOpen => frames.push(LiveFrame::Array { values: vec![], start: token.start }),
    TokenType::ArrayClose => {
        let child = finish_array(frames.pop(), token.start)?;
        if let Some(parent) = frames.last_mut() { add_to_frame(parent, child)?; }
        else { return Ok(child); }
    }
    _ => add_to_frame(frames.last_mut().expect("open frame"), parse_scalar_or_ref(token)?)?,
}
```

- [ ] **Step 3: Preserve qpdf recovery semantics**

Move the existing logic, without changing message text or offsets, into
helpers used by the loop:

- `add_to_frame` handles array append, dictionary name keys, missing-key
  orphan collection, and duplicate-key warnings;
- `finish_dictionary` assigns a pending final key to null and synthesizes
  `/QPDFFakeN` keys in the existing deterministic order;
- EOF emits `parse error while reading object` then `unexpected EOF`;
- unmatched closing tokens retain their existing null/recovery behavior;
- scalar/reference parsing remains the existing `integer_or_ref` path.

- [ ] **Step 4: Remove recursive container entry points**

Delete `array`, `dictionary`, and `enter_container`, and remove the
`stacker::maybe_grow` calls that guarded only their recursive path. Retain
unrelated legacy/content-parser stack growth.

- [ ] **Step 5: Run focused tests**

Run:
`cargo test -p flpdf --lib live_input_tests:: -- --test-threads=1`

Expected: all live-input tests pass, including both the 500 accepted and 501
recovered boundaries.

### Task 3: Prove unchanged observable behavior and publish the repair

**Files:**
- Modify: `crates/flpdf/src/parser.rs` only if a focused regression exposes a
  frame-loop behavior mismatch.
- Verify: `docs/qpdf-correspondence.md`,
  `docs/qpdf-module-doc-index.md`.

**Interfaces:**
- Consumes: finished iterative live parser.
- Produces: a qpdf-faithful parser that passes the current test and
  documentation quality gates.

- [ ] **Step 1: Run behavioral parser suites**

Run:

```bash
cargo test -p flpdf --lib parser::live_input_tests:: -- --test-threads=1
cargo test -p flpdf --lib parser::handle_path_parity_tests:: -- --test-threads=1
cargo test -p flpdf --test reader_tests
```

Expected: success; any diagnostic or parsed-offset mismatch is corrected in
the frame loop before broader verification.

- [ ] **Step 2: Run project quality gates**

Run:

```bash
cargo fmt -- --check
cargo test -p flpdf --lib
python3 scripts/qpdf-module-docs.py --check
git diff --check
```

Expected: all commands exit zero.

- [ ] **Step 3: Commit and update the existing PR**

Run:

```bash
git add crates/flpdf/src/parser.rs docs/superpowers/plans/2026-08-06-qpdf-live-parser-frame-stack.md
git commit -m "fix(parser): use qpdf-style live frame stack"
git push
```

Expected: the PR branch updates without including unrelated files. Recheck
`gh pr checks 651` after Actions start; the three former stack-overflow jobs
must no longer fail for this test.
