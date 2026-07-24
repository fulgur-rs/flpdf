# qpdf-test-compare Rust Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `crates/qpdf-test-compare` — a Rust reimplementation of qpdf's `compare-for-test/qpdf-test-compare.cc` (v11.9.0), used by the flpdf-qtest harness's "check output" step to compare two PDFs semantically (tolerating FlateDecode compression differences).

**Architecture:** New workspace crate `crates/qpdf-test-compare` (`publish = false`) with a single `qpdf-test-compare` binary. Faithful port of qpdf's algorithm: clean trailer → compare trailer → clean encryption → compare all objects in obj-num order; stream data compared raw unless `/FlateDecode` is present (then compared decoded). Because both inputs go through the same flpdf parser, unparse output need not match qpdf byte-for-byte — only be internally deterministic.

**Tech Stack:** Rust workspace (existing), `flpdf` (path dep) for parsing/decoding, `std::env`/`std::fs`/`std::io` for CLI I/O. No new external deps.

**Beads:** flpdf-iurg. See design in `bd show flpdf-iurg`.

**Oracle:** qpdf v11.9.0 `compare-for-test/qpdf-test-compare.cc` (`main` differs only in `std::endl → '\n'`; semantic-equivalent).

---

## Ground Rules (read before every task)

- **qpdf oracle mandate**: the compare algorithm's semantics and ordering must mirror qpdf exactly. Deviations require a 1-line justification in the PR body.
- **Review patterns**: consult `.claude/rules/pdf-rust-review-patterns.md` before writing code (no unnecessary `.clone()`, resolve indirect refs, verify signed→unsigned casts, bound graph traversals).
- **Doc patterns**: for anything under `crates/qpdf-test-compare/src/` that ends up in `cargo doc`, follow `.claude/rules/pdf-rust-doc-review-patterns.md` (English, no beads IDs in `///`, `# Errors`/`# Panics` where relevant). Since this crate is `publish = false`, doc surface is small — internal doc comments (`///` on private items) are fine in either language.
- **Test coverage**: this crate is test tooling; it will be added to `patch-coverage.sh` as **report-only** (like flpdf-cli), not gated. We still aim for meaningful coverage of every branch.
- **No qpdf-qtest fixtures**: never vendor content from `flpdf-qtest/vendor/qpdf-qtest/` (Artistic 2.0). Fixtures live in `crates/qpdf-test-compare/tests/fixtures/` and are either hand-authored PDFs or generated via a local `regenerate.sh` (qpdf-as-tool pattern).
- **Commit cadence**: commit after each Task's tests are green.

---

## Task 0: Confirm remaining flpdf API gaps

**Purpose:** Two unverified assumptions from the design that, if wrong, change the plan.

**Files:** none (investigation only)

**Step 0.1: Confirm `live_object_refs()` returns objs in `(number, gen)` ascending order.**

Run: `grep -nA 15 "pub fn live_object_refs" crates/flpdf/src/reader.rs`
Verify that `self.cache.entries()` yields entries in `BTreeMap` key order (i.e., ObjectRef ordering). If not, add a `.sorted_by_key(|r| (r.number, r.generation))` in the caller.

**Step 0.2: Confirm `Object::String(vec![])` unparses to `()`.**

Already verified in this plan's prep: `write_literal_string` at `crates/flpdf/src/object.rs:627-639` outputs `(` + escaped body + `)`. For empty input → `()`. No action.

**Step 0.3: Confirm `ObjectRef::to_string()` renders as `N G R`.**

Run: `grep -nB 2 -A 15 "impl.*Display.*ObjectRef\|fn to_string" crates/flpdf/src/object.rs | head`
Verify the produced format is `<number> <generation> R`. If not, adjust the label formatting in `compare_objects`.

**Step 0.4: Confirm `Pdf::open_with_options` accepts empty password gracefully (no password case).**

Run: `grep -nA 5 "PdfOpenOptions::default" crates/flpdf/src/reader.rs | head`
Check the default `PdfOpenOptions` has an empty `password: Vec<u8>` and does not error when the file is unencrypted.

**Step 0.5: Confirm `filters::decode_stream_data(dict, raw)` handles /FlateDecode chain, /Predictor, and multi-filter arrays.**

Run: `grep -nA 30 "pub fn decode_stream_data" crates/flpdf/src/filters.rs | head -40`
Verify it returns `Result<Vec<u8>>` and applies all filters in `/Filter` order. This matches qpdf's `getStreamData()` semantics (fully decoded).

**Step 0.6: Report findings.**

If any check fails or reveals an API gap, stop and add a Task 0.5 to add/adjust flpdf API surface before continuing.

**Step 0.7: Commit any notes.**

No code changes expected. Skip commit.

---

## Task 1: Bootstrap the crate

**Files:**
- Create: `crates/qpdf-test-compare/Cargo.toml`
- Create: `crates/qpdf-test-compare/src/main.rs`
- Modify: `Cargo.toml` (workspace `members`)

**Step 1.1: Add crate directory + `Cargo.toml`.**

```toml
# crates/qpdf-test-compare/Cargo.toml
[package]
name = "qpdf-test-compare"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false
description = "Rust port of qpdf's compare-for-test/qpdf-test-compare, for the flpdf-qtest harness."

[[bin]]
name = "qpdf-test-compare"
path = "src/main.rs"

[features]
default = []
# Match flpdf-cli: forward zlib backend for tests that assert byte-identical
# compressed data with qpdf goldens.
qpdf-zlib-compat = ["flpdf/qpdf-zlib-compat"]

[dependencies]
flpdf = { path = "../flpdf" }

[dev-dependencies]
assert_cmd.workspace = true
predicates.workspace = true
tempfile.workspace = true
```

**Step 1.2: Add stub `src/main.rs`.**

```rust
fn main() {
    // Implemented in later tasks. Exit 2 so any accidental invocation is a
    // loud failure, matching qpdf-test-compare's default error exit code.
    std::process::exit(2);
}
```

**Step 1.3: Add to workspace `members`.**

Edit `Cargo.toml` (workspace root) — add `"crates/qpdf-test-compare"` to `members`.

**Step 1.4: Verify build.**

Run: `cargo build -p qpdf-test-compare`
Expected: succeeds. Also: `cargo check --workspace` succeeds.

**Step 1.5: Commit.**

```bash
git add crates/qpdf-test-compare Cargo.toml
git commit -m "chore(qpdf-test-compare): bootstrap crate skeleton"
```

---

## Task 2: `--version` command

**Files:**
- Create: `crates/qpdf-test-compare/tests/cli_version.rs`
- Modify: `crates/qpdf-test-compare/src/main.rs`

**Step 2.1: Write failing test.**

```rust
// tests/cli_version.rs
use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_prints_and_exits_zero() {
    Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("qpdf-test-compare from flpdf version "));
}
```

**Step 2.2: Run test → FAIL.**

Run: `cargo test -p qpdf-test-compare --test cli_version`
Expected: fail (exit 2, no stdout match).

**Step 2.3: Implement.**

Refactor `src/main.rs` to a small `run() -> ExitCode` shape; handle `--version` first.

```rust
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    run(&args)
}

fn run(args: &[String]) -> ExitCode {
    let whoami = program_name(args.first().map(String::as_str).unwrap_or("qpdf-test-compare"));
    if args.len() == 2 && args[1] == "--version" {
        println!("{whoami} from flpdf version {}", flpdf::VERSION);
        return ExitCode::from(0);
    }
    ExitCode::from(2)  // real logic in later tasks
}

fn program_name(argv0: &str) -> &str {
    argv0.rsplit('/').next().unwrap_or(argv0)
}
```

Verify `flpdf::VERSION` exists (search `crates/flpdf/src/lib.rs`); if the crate exposes it as `version()` fn instead, use that.

**Step 2.4: Run test → PASS.**

Run: `cargo test -p qpdf-test-compare --test cli_version`

**Step 2.5: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): --version prints program name and flpdf version"
```

---

## Task 3: Usage / arg validation

**Files:**
- Create: `crates/qpdf-test-compare/tests/cli_usage.rs`
- Modify: `crates/qpdf-test-compare/src/main.rs`

**Step 3.1: Write failing tests.**

```rust
// tests/cli_usage.rs
use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn zero_args_prints_usage_and_exits_two() {
    Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .assert()
        .failure()
        .code(2)
        .stderr(contains("Usage:"))
        .stderr(contains("actual expected"));
}

#[test]
fn too_many_args_prints_usage_and_exits_two() {
    Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .args(["a", "b", "c", "d"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("Usage:"));
}
```

**Step 3.2: Run tests → FAIL.**

Run: `cargo test -p qpdf-test-compare --test cli_usage`
Expected: fails (no stderr).

**Step 3.3: Implement usage handling.**

```rust
fn usage(whoami: &str) {
    eprintln!("Usage: {whoami} actual expected");
    eprintln!(r#"Where "actual" is the actual output and "expected" is the expected"#);
    eprintln!("output of a test, compare the two PDF files. The files are considered");
    eprintln!("to match if all their objects are identical except that, if a stream is");
    eprintln!("compressed with FlateDecode, the uncompressed data must match.");
    eprintln!();
    eprintln!("If the files match, the output is the expected file. Otherwise, it is");
    eprintln!("the actual file. Read comments in the code for rationale.");
}

// in run(): when args.len() < 3 || args.len() > 4 → usage() + return ExitCode::from(2)
```

Match qpdf's exact wording for the usage block (copy from oracle file, minus `std::endl` differences).

**Step 3.4: Run tests → PASS.**

**Step 3.5: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): usage handling for missing / excess args (exit 2)"
```

---

## Task 4: File I/O + panic-free error handling scaffold

**Files:**
- Create: `crates/qpdf-test-compare/src/output.rs` (helpers)
- Modify: `crates/qpdf-test-compare/src/main.rs`
- Create: `crates/qpdf-test-compare/tests/cli_bad_input.rs`

**Step 4.1: Write failing test — non-existent input file emits `whoami: <err>` and exits 2.**

```rust
// tests/cli_bad_input.rs
use assert_cmd::Command;
use predicates::str::{contains, is_empty};

#[test]
fn missing_actual_file_reports_error_no_panic() {
    Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .args(["/no/such/actual.pdf", "/no/such/expected.pdf"])
        .assert()
        .failure()
        .code(2)
        .stdout(is_empty())
        .stderr(contains("qpdf-test-compare:"));
}
```

**Step 4.2: Run → FAIL.**

**Step 4.3: Implement error scaffold + `dump_file_to_stdout(path) -> io::Result<()>`.**

```rust
// src/output.rs
use std::fs::File;
use std::io::{self, Read, Write};

pub fn dump_file_to_stdout(path: &str) -> io::Result<()> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; 2048];
    let stdout = io::stdout();
    let mut out = stdout.lock();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { return Ok(()); }
        out.write_all(&buf[..n])?;
    }
}
```

In `main.rs`, wrap the future compare/emit call in a `Result` and print `eprintln!("{whoami}: {err}")` + exit 2 on any error. Add a stub `compare(actual, expected, password) -> Result<Option<Vec<u8>>, io::Error>` that currently just tries to open the actual file so the test flows through the error path.

**Step 4.4: Run → PASS.**

**Step 4.5: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): panic-free error reporting scaffold + stdout dumper"
```

---

## Task 5: `clean_trailer`

**Files:**
- Create: `crates/qpdf-test-compare/src/clean.rs`
- Modify: `crates/qpdf-test-compare/src/main.rs` (mod)

**Step 5.1: Write failing unit tests (inline in `clean.rs`).**

Cases (small, in-memory `Dictionary` construction; no PDF parsing needed):

1. Trailer with `/Length 42` → key removed.
2. Trailer with `/ID [<hex1><hex2>]` and hex1 != hex2 → `/ID[1]` replaced with empty String.
3. Trailer with `/ID [<same><same>]` → both slots replaced with empty String.
4. Trailer with no `/ID` → no-op.
5. Trailer with `/ID [ x ]` (length != 2) → no-op.
6. Trailer with `/ID` that is not an Array → no-op.

```rust
// src/clean.rs
use flpdf::{Dictionary, Object};

pub fn clean_trailer(trailer: &mut Dictionary) {
    trailer.remove(b"/Length");
    let Some(id_obj) = trailer.get(b"/ID") else { return; };
    let Some(items) = id_obj.as_array() else { return; };
    if items.len() != 2 { return; }
    let mut id0_bytes = Vec::new();
    items[0].write_pdf(&mut id0_bytes);
    let mut id1_bytes = Vec::new();
    items[1].write_pdf(&mut id1_bytes);
    let equal = id0_bytes == id1_bytes;
    // Now mutate — need &mut. Use `insert` to replace.
    let mut new_items = items.to_vec();
    new_items[1] = Object::String(Vec::new());
    if equal { new_items[0] = Object::String(Vec::new()); }
    trailer.insert(b"/ID", Object::Array(new_items));
}
```

**Step 5.2: Run → FAIL (the module doesn't exist yet).**

Run: `cargo test -p qpdf-test-compare clean::`

**Step 5.3: Wire the module + verify tests pass.**

Add `mod clean;` in `main.rs`.

Run: `cargo test -p qpdf-test-compare clean::`
Expected: all 6 cases pass.

**Step 5.4: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): clean_trailer (strip /Length, blank /ID halves)"
```

---

## Task 6: `clean_encryption`

**Files:**
- Modify: `crates/qpdf-test-compare/src/clean.rs`

**Step 6.1: Write failing unit tests.**

Cases (using an in-memory Pdf via `Pdf::open_mem_owned` on a fixture — small encrypted PDF or a hand-crafted trailer with an indirect /Encrypt ref):

1. Trailer has no `/Encrypt` → no-op.
2. Trailer `/Encrypt` is an indirect ref → resolve, strip `/O /OE /U /UE /Perms`, set_object back; a subsequent resolve shows the stripped keys are gone.
3. `/Encrypt` is inline dict → no-op (documented as mirrored qpdf blind spot).

**Step 6.2: Implement `clean_encryption`.**

```rust
pub fn clean_encryption<R: std::io::Read + std::io::Seek>(pdf: &mut flpdf::Pdf<R>) -> flpdf::Result<()> {
    let Some(encrypt_obj_ref) = pdf.trailer().get_ref(b"/Encrypt") else { return Ok(()); };
    let mut enc = pdf.resolve(encrypt_obj_ref)?;
    let Some(dict) = enc.as_dict_mut() else { return Ok(()); };
    for k in [b"/O".as_ref(), b"/OE", b"/U", b"/UE", b"/Perms"] { dict.remove(k); }
    pdf.set_object(encrypt_obj_ref, enc);
    Ok(())
}
```

**Step 6.3: Run tests → PASS.**

Run: `cargo test -p qpdf-test-compare clean::`

**Step 6.4: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): clean_encryption (strip /O /OE /U /UE /Perms)"
```

---

## Task 7: `compare_objects` — non-stream branch

**Files:**
- Create: `crates/qpdf-test-compare/src/compare.rs`

**Step 7.1: Write failing unit tests.**

Cases:

1. Two identical scalars → empty string (match).
2. Two ints of different value → `"label: object contents differ"`.
3. Int vs Name (different type codes) → `"label: different types"`.
4. Two equal dicts (nested refs preserved as `N G R`) → match.
5. Two dicts with different key order — since flpdf's `Dictionary` is BTreeMap-backed, `write_pdf` output is deterministic and equal → match.
6. Reference on one side, Integer on the other → `"label: different types"`.

**Step 7.2: Implement.**

```rust
// src/compare.rs
use flpdf::Object;

pub fn compare_objects(label: &str, act: &Object, exp: &Object) -> String {
    if type_code(act) != type_code(exp) {
        return format!("{label}: different types");
    }
    if matches!(act, Object::Stream(_)) {
        // Stream branch is Task 8. For now, delegate to a stub or hit the
        // non-stream path.
        return String::new();
    }
    let mut a = Vec::new(); act.write_pdf(&mut a);
    let mut e = Vec::new(); exp.write_pdf(&mut e);
    if a != e {
        return format!("{label}: object contents differ");
    }
    String::new()
}

fn type_code(o: &Object) -> u8 {
    match o {
        Object::Null => 0,
        Object::Boolean(_) => 1,
        Object::Integer(_) => 2,
        Object::Real(_) | Object::RealLiteral { .. } => 3,
        Object::Name(_) => 4,
        Object::String(_) => 5,
        Object::Array(_) => 6,
        Object::Dictionary(_) => 7,
        Object::Stream(_) => 8,
        Object::Reference(_) => 9,
    }
}
```

**Note:** qpdf's typecode differentiates literal-string from hex-string only through `unparse` output, not through the top-level typecode enum. flpdf collapses both into `Object::String`, matching qpdf's `ot_string`. This is fine.

**Step 7.3: Run tests → PASS.**

**Step 7.4: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): compare_objects non-stream branch"
```

---

## Task 8: `compare_objects` — stream branch

**Files:**
- Modify: `crates/qpdf-test-compare/src/compare.rs`

**Step 8.1: Write failing unit tests.**

Cases:

1. Two streams with identical dict (both have `/Length 10`) and identical raw data → match.
2. Two streams with different `/Length` but identical raw data & other dict entries → match (Length stripped before dict compare).
3. Stream dicts differ in `/Type` → `"label: stream dictionaries differ"`.
4. Stream `/Type == /XRef` with differing data but same dict → match (skip data compare).
5. Stream `/Filter /FlateDecode` with same decoded payload but different compressed bytes → match (decompress path).
6. Stream `/Filter [/FlateDecode /Crypt]` — FlateDecode present in array → decompress path.
7. Stream `/Filter /LZWDecode` (no FlateDecode) — raw compare (differing raw data → `"label: stream data differs"`).
8. Stream data size differs (uncompressed path) → `"label: stream data size differs"`.
9. Stream data size differs (compressed path, i.e., decoded sizes differ) → `"label: stream data size differs"`.

**Step 8.2: Implement.**

```rust
pub fn compare_objects(label: &str, act: &Object, exp: &Object) -> String {
    if type_code(act) != type_code(exp) {
        return format!("{label}: different types");
    }
    if let (Object::Stream(a_s), Object::Stream(e_s)) = (act, exp) {
        // Compare dicts with /Length stripped.
        let mut a_dict = a_s.dict.clone();
        let mut e_dict = e_s.dict.clone();
        a_dict.remove(b"/Length");
        e_dict.remove(b"/Length");
        let mut a_dict_bytes = Vec::new();
        Object::Dictionary(a_dict.clone()).write_pdf(&mut a_dict_bytes);
        let mut e_dict_bytes = Vec::new();
        Object::Dictionary(e_dict.clone()).write_pdf(&mut e_dict_bytes);
        if a_dict_bytes != e_dict_bytes {
            return format!("{label}: stream dictionaries differ");
        }
        // /Type == /XRef skips data compare
        if is_xref_stream(&a_dict) {
            return String::new();
        }
        let uncompress = filter_uses_flatedecode(&a_dict);
        let (a_data, e_data) = if uncompress {
            match (
                flpdf::filters::decode_stream_data(&a_s.dict, &a_s.data),
                flpdf::filters::decode_stream_data(&e_s.dict, &e_s.data),
            ) {
                (Ok(a), Ok(e)) => (a, e),
                // Decode failure treated as data-differs? Mirror qpdf: qpdf
                // would throw and main() catches → exit 2 with the error msg.
                // We propagate up by returning the error string wrapped.
                (Err(err), _) | (_, Err(err)) => return format!("{label}: decode error: {err}"),
            }
        } else {
            (a_s.data.clone(), e_s.data.clone())
        };
        if a_data.len() != e_data.len() {
            return format!("{label}: stream data size differs");
        }
        if a_data != e_data {
            return format!("{label}: stream data differs");
        }
        return String::new();
    }
    // non-stream: reuse Task 7 logic
    let mut a = Vec::new(); act.write_pdf(&mut a);
    let mut e = Vec::new(); exp.write_pdf(&mut e);
    if a != e {
        return format!("{label}: object contents differ");
    }
    String::new()
}

fn is_xref_stream(d: &flpdf::Dictionary) -> bool {
    matches!(d.get(b"/Type"), Some(Object::Name(n)) if n.as_slice() == b"/XRef")
    // Note: /Type in trailer/xref-stream is unlikely to be an indirect ref,
    // but if pdf-rust-review-patterns.md rule 2 bites us here we should
    // resolve. For now, mirror qpdf's direct isNameAndEquals check.
}

fn filter_uses_flatedecode(d: &flpdf::Dictionary) -> bool {
    match d.get(b"/Filter") {
        Some(Object::Name(n)) => n.as_slice() == b"/FlateDecode",
        Some(Object::Array(items)) => items.iter().any(|it| matches!(it, Object::Name(n) if n.as_slice() == b"/FlateDecode")),
        _ => false,
    }
}
```

**Watchpoints (from review patterns):**
- `.clone()` on stream data (`a_s.data.clone()`) is O(N) — but oracle does this too (`getRawStreamData()` returns a `shared_ptr<Buffer>` which shares memory). For our comparison we don't actually need to clone if we have `&`; refactor to `&[u8]` comparison to avoid the clone. Do so before commit.
- Dict clone for stripping `/Length` is unavoidable (`d.remove(b"/Length")` needs `&mut Dictionary`, and we can't own it out of `&Stream`). Accept the shallow dict clone (Dictionary is a BTreeMap of Objects — refs are cheap; a stream data blob is not cloned since it's in the outer Stream).

**Step 8.3: Refactor to avoid data clone.**

```rust
let (a_data, e_data): (Vec<u8>, Vec<u8>);
let (a_slice, e_slice): (&[u8], &[u8]) = if uncompress {
    a_data = flpdf::filters::decode_stream_data(&a_s.dict, &a_s.data)
        .map_err(...)?;
    e_data = flpdf::filters::decode_stream_data(&e_s.dict, &e_s.data)
        .map_err(...)?;
    (&a_data, &e_data)
} else {
    (&a_s.data, &e_s.data)
};
if a_slice.len() != e_slice.len() { return "...size differs".into(); }
if a_slice != e_slice { return "...data differs".into(); }
```

**Step 8.4: Run tests → PASS.**

**Step 8.5: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): compare_objects stream branch (/XRef skip, FlateDecode uncompress, size+data)"
```

---

## Task 9: `compare` orchestrator

**Files:**
- Create: `crates/qpdf-test-compare/src/lib.rs` (mod exports)
- Modify: `crates/qpdf-test-compare/src/main.rs`
- Create: `crates/qpdf-test-compare/src/orchestrator.rs`

**Step 9.1: Write failing integration tests using in-memory PDFs.**

Cases (via `Pdf::open_mem_owned` on hand-crafted byte streams; small fixtures written inline):

1. Two identical PDFs → `Ok(None)` (no difference).
2. Trailer with /ID differing only in [1] → `Ok(None)`.
3. Two PDFs with different object counts → `Ok(Some("different number of objects"))`.
4. Two PDFs where obj (1, 0) has different content → `Ok(Some("1 0: object contents differ"))`.
5. Two PDFs where obj (1, 0) has different obj-gen (i.e., mismatch position) → `Ok(Some("different object IDs"))` — but see note.

**Note on Case 5:** qpdf's check compares `getObjGen()` between the zipped actuals & expecteds. Since flpdf's `live_object_refs()` order is deterministic and both should include the same ObjectRef set for equivalent PDFs, position mismatch means the sorted sets differ — usually caught by count mismatch. Reproducing this in a test is tricky; consider skipping and documenting as "not exercised in tests, matches oracle comment 'not reproduced in the test suite'".

**Step 9.2: Implement the orchestrator.**

```rust
// src/orchestrator.rs
use std::io::{Read, Seek};
use flpdf::{Pdf, PdfOpenOptions};
use crate::clean::{clean_encryption, clean_trailer};
use crate::compare::compare_objects;

pub fn compare_files(actual: &[u8], expected: &[u8], password: &[u8]) -> flpdf::Result<Option<String>> {
    let opts = |pw: &[u8]| PdfOpenOptions {
        password: pw.to_vec(),
        allow_weak_crypto: true,  // permit RC4 fixtures for parity with qpdf
        ..Default::default()
    };
    let mut actual_pdf = Pdf::open_mem_owned_with_options(actual.to_vec(), opts(password))?;
    let mut expected_pdf = Pdf::open_mem_owned_with_options(expected.to_vec(), opts(password))?;

    let mut act_trailer = actual_pdf.trailer().clone();
    let mut exp_trailer = expected_pdf.trailer().clone();
    clean_trailer(&mut act_trailer);
    clean_trailer(&mut exp_trailer);
    let trailer_diff = compare_objects(
        "trailer",
        &flpdf::Object::Dictionary(act_trailer),
        &flpdf::Object::Dictionary(exp_trailer),
    );
    if !trailer_diff.is_empty() {
        return Ok(Some(trailer_diff));
    }

    clean_encryption(&mut actual_pdf)?;
    clean_encryption(&mut expected_pdf)?;

    let a_refs = actual_pdf.live_object_refs();
    let e_refs = expected_pdf.live_object_refs();
    if a_refs.len() != e_refs.len() {
        return Ok(Some("different number of objects".into()));
    }
    for (a_ref, e_ref) in a_refs.iter().zip(e_refs.iter()) {
        if a_ref != e_ref {
            return Ok(Some("different object IDs".into()));
        }
        let a_obj = actual_pdf.resolve(*a_ref)?;
        let e_obj = expected_pdf.resolve(*e_ref)?;
        let label = a_ref.to_string();  // "N G R" — but qpdf uses "N G"; adjust in step 9.3.
        let diff = compare_objects(&label, &a_obj, &e_obj);
        if !diff.is_empty() {
            return Ok(Some(diff));
        }
    }
    Ok(None)
}
```

**Step 9.3: Fix label format.**

qpdf's `QPDFObjGen::unparse()` produces `N G` (no `R`). Verify what flpdf's `ObjectRef::to_string()` emits (Task 0.3) and, if it emits `N G R`, strip the `R` here or add a dedicated `format!("{} {}", n, g)` helper.

**Step 9.4: Wire lib.rs so tests can `use qpdf_test_compare::compare_files`.**

```rust
// src/lib.rs
pub mod clean;
pub mod compare;
pub mod orchestrator;
pub use orchestrator::compare_files;
```

Cargo.toml: add `[lib] name = "qpdf_test_compare" path = "src/lib.rs"` alongside the existing `[[bin]]`.

**Step 9.5: Run tests → PASS.**

Run: `cargo test -p qpdf-test-compare orchestrator::`

**Step 9.6: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): compare orchestrator (trailer→encryption→per-object)"
```

---

## Task 10: main() wiring — full CLI integration

**Files:**
- Modify: `crates/qpdf-test-compare/src/main.rs`
- Create: `crates/qpdf-test-compare/tests/fixtures/README.md` (explains "no vendored qpdf fixtures")

**Step 10.1: Write failing integration test.**

```rust
// tests/cli_match_path.rs
use assert_cmd::Command;
use tempfile::TempDir;
use std::fs;

// Reuse a tiny hand-authored PDF from flpdf's fixtures (Apache/MIT-clean).
const TINY_PDF: &[u8] = include_bytes!("fixtures/tiny.pdf");

#[test]
fn identical_files_emit_expected_and_exit_zero() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.pdf"); fs::write(&a, TINY_PDF).unwrap();
    let b = dir.path().join("b.pdf"); fs::write(&b, TINY_PDF).unwrap();
    let out = Command::cargo_bin("qpdf-test-compare").unwrap()
        .args([a.to_str().unwrap(), b.to_str().unwrap()])
        .assert().success().get_output().clone();
    assert_eq!(out.stdout, TINY_PDF);  // cat expected
}
```

Author `tests/fixtures/tiny.pdf` as the smallest valid PDF flpdf can parse (or reuse an existing tiny fixture from `crates/flpdf/tests/fixtures/`).

**Step 10.2: Run → FAIL.**

**Step 10.3: Wire main().**

```rust
// After --version / usage handling:
let actual = &args[1];
let expected = &args[2];
let password = if args.len() == 4 { args[3].as_bytes() } else { b"" };
let show_why = env::var_os("QPDF_COMPARE_WHY").is_some();

let (to_output, diff) = match load_and_compare(actual, expected, password) {
    Ok((maybe_diff, act_bytes, exp_bytes)) => match maybe_diff {
        None => (expected.as_str(), None),
        Some(d) => {
            if show_why {
                eprintln!("{d}");
                return ExitCode::from(2);
            }
            (actual.as_str(), Some(d))
        }
    },
    Err(err) => {
        eprintln!("{whoami}: {err}");
        return ExitCode::from(2);
    }
};

if let Err(err) = output::dump_file_to_stdout(to_output) {
    eprintln!("{whoami}: {err}");
    return ExitCode::from(2);
}
if diff.is_some() { ExitCode::from(2) } else { ExitCode::from(0) }
```

Provide a `load_and_compare(a, e, pw)` helper in `orchestrator.rs` that reads both files, calls `compare_files`, and returns bytes for potential dumping — or (simpler) just read once via `dump_file_to_stdout` from the path.

Note: we read the file for parsing (via `fs::read`) and then re-read for stdout dumping (via `dump_file_to_stdout`). qpdf's oracle also re-reads the file (`safe_fopen` after `processFile`). This matches the "no re-serialize" contract.

**Step 10.4: Run → PASS.**

**Step 10.5: Commit.**

```bash
git commit -am "feat(qpdf-test-compare): main() wired — cats expected on match, actual on diff"
```

---

## Task 11: `QPDF_COMPARE_WHY` env

**Files:**
- Create: `crates/qpdf-test-compare/tests/cli_compare_why.rs`

**Step 11.1: Write test.**

```rust
#[test]
fn compare_why_prints_reason_and_skips_output() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.pdf");
    let b = dir.path().join("b.pdf");
    fs::write(&a, TINY_PDF).unwrap();
    fs::write(&b, TINY_PDF_ALT).unwrap();  // differs in one object
    let out = Command::cargo_bin("qpdf-test-compare").unwrap()
        .env("QPDF_COMPARE_WHY", "1")
        .args([a.to_str().unwrap(), b.to_str().unwrap()])
        .assert().failure().code(2).get_output().clone();
    assert!(out.stdout.is_empty(), "no bytes on stdout when WHY set");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("object contents differ") || stderr.contains("stream data differs"));
}
```

Requires a second fixture `TINY_PDF_ALT` — same as `tiny.pdf` but with one object mutated (change a `/MediaBox` value or similar).

**Step 11.2: Verify implementation from Task 10 covers this path.**

If Task 10 already implemented the `show_why` branch, this test should already pass. Otherwise, add the branch.

**Step 11.3: Run → PASS.**

**Step 11.4: Commit.**

```bash
git commit -am "test(qpdf-test-compare): QPDF_COMPARE_WHY end-to-end coverage"
```

---

## Task 12: Fixture regenerate script

**Files:**
- Create: `crates/qpdf-test-compare/tests/fixtures/regenerate.sh`
- Create: `crates/qpdf-test-compare/tests/fixtures/README.md`
- Create: `crates/qpdf-test-compare/tests/fixtures/*.pdf` (hand-authored or regenerated)

**Step 12.1: Determine fixture set.**

Fixtures needed for full Section 6 coverage:

- `tiny.pdf` — smallest valid single-page PDF (flpdf-authored)
- `tiny_alt.pdf` — same tree, one object mutated
- `id1_differs.pdf` — trailer /ID[1] differs from a peer
- `id0_equals_id1.pdf` — /ID with both halves identical
- `length_only_differs.pdf` — same stream data, different /Length
- `flate_miniz.pdf` — content stream compressed by flpdf (miniz backend)
- `flate_zlib.pdf` — same content compressed by qpdf (zlib backend) — generated with `qpdf --recompress-flate` or similar as-tool
- `xref_stream_data_differs.pdf` — same xref-stream dict, different xref data
- `encrypted_permissions.pdf` — RC4 encrypted; /O /OE /U /UE /Perms mutated peer

**Step 12.2: Author `regenerate.sh` following `tests/golden/regenerate.sh` pattern.**

- Uses qpdf-as-tool to generate all deterministic goldens
- Fails loudly if qpdf is missing
- Emits fixture files with `--static-id` where possible for stable output

**Step 12.3: Add `README.md` explaining licensing.**

Content: "These fixtures are generated locally via `regenerate.sh` from flpdf-authored source PDFs, or hand-authored. No file in this directory is copied from qpdf's `qtest/` corpus (Artistic 2.0). See `bd recall flpdf-qtest-is-a-separate-repo-specifically-to`."

**Step 12.4: Run the script; commit outputs.**

```bash
bash crates/qpdf-test-compare/tests/fixtures/regenerate.sh
git add crates/qpdf-test-compare/tests/fixtures/
git commit -m "test(qpdf-test-compare): flpdf-authored fixtures + regenerate.sh (no qpdf-qtest vendoring)"
```

---

## Task 13: End-to-end integration tests (all Section 6 scenarios)

**Files:**
- Create: `crates/qpdf-test-compare/tests/e2e.rs`

**Step 13.1: Write one test per scenario in the design's Section 6.**

Reuse the fixtures from Task 12. Each test invokes the CLI and asserts:
- Exit code
- Stdout contents (empty vs expected/actual bytes)
- Stderr contents (empty vs specific reason text — when `QPDF_COMPARE_WHY=1`)

Structure:

```rust
mod match_paths {
    #[test] fn identical_files_match() { ... }
    #[test] fn id1_ignored() { ... }
    #[test] fn id_both_ignored_when_id0_eq_id1() { ... }
    #[test] fn length_ignored_in_stream_dict() { ... }
    #[test] fn xref_stream_data_ignored() { ... }
    #[test] fn encryption_dict_diffs_ignored() { ... }
    #[cfg(feature = "qpdf-zlib-compat")]
    #[test] fn flate_compressed_bytes_differ_but_decoded_match() { ... }
}

mod differ_paths {
    #[test] fn object_count_mismatch() { ... }
    #[test] fn object_contents_differ() { ... }
    #[test] fn stream_data_size_differs() { ... }
    #[test] fn stream_data_differs_uncompressed() { ... }
}
```

**Step 13.2: Run → PASS on default features.**

Run: `cargo test -p qpdf-test-compare --test e2e`

**Step 13.3: Run with qpdf-zlib-compat.**

Run: `cargo test -p qpdf-test-compare --features qpdf-zlib-compat --test e2e`

The `flate_compressed_bytes_differ_but_decoded_match` test should now pass (miniz vs zlib compressed goldens).

**Step 13.4: Commit.**

```bash
git commit -am "test(qpdf-test-compare): end-to-end scenarios for match and differ paths"
```

---

## Task 14: patch-coverage report-only extension

**Files:**
- Modify: `scripts/patch-coverage.sh`

**Step 14.1: Extend REPORT_PREFIX to a tuple.**

Change:

```python
REPORT_PREFIX = "crates/flpdf-cli/src/"
```

to:

```python
REPORT_PREFIXES = ("crates/flpdf-cli/src/", "crates/qpdf-test-compare/src/")
```

Update the `render(...)` calls and any string-prefix checks to iterate over `REPORT_PREFIXES`. The gate (`GATE_PREFIX`) stays a single string.

**Step 14.2: Verify the script still errors on flpdf gap and reports on the new crate.**

```bash
# Introduce a temporary uncovered line in qpdf-test-compare/src/main.rs, then:
bash scripts/patch-coverage.sh --base origin/main
# Expected: FAIL only if flpdf has uncovered lines; qpdf-test-compare uncovered lines are reported (not gated).
# Revert the tweak.
```

**Step 14.3: Commit.**

```bash
git commit -am "chore(patch-coverage): report (not gate) crates/qpdf-test-compare/src"
```

---

## Task 15: CI wiring

**Files:**
- Modify: `.github/workflows/ci.yml`

**Step 15.1: Verify current CI matrix.**

```bash
grep -nE "qpdf-zlib-compat|--features|cargo test" .github/workflows/ci.yml | head -30
```

`cargo test --workspace` in the standard job should already build & test `qpdf-test-compare` for default features. The `qpdf-zlib-compat` gated tests (Task 13.3) need to be explicitly listed if CI uses a curated test-list for feature-gated tests (per bd memory `flpdf-ci-bytes-identical-explicit-test-list`).

**Step 15.2: Add feature-gated test entry.**

If CI has an explicit list like `--test cli_byte_identical`, append `--test e2e` (with `--features qpdf-zlib-compat`) so the compression test runs there too.

**Step 15.3: Run local sanity.**

```bash
cargo test --workspace
cargo test --workspace --features qpdf-zlib-compat
```

**Step 15.4: Commit.**

```bash
git commit -am "ci: run qpdf-test-compare e2e under qpdf-zlib-compat feature"
```

---

## Task 16: fmt + clippy + doc + patch-coverage + PR

**Step 16.1: Format check.**

```bash
cargo fmt --all --check
# If diffs: cargo fmt --all && git commit -am "style: cargo fmt"
```

**Step 16.2: Clippy on workspace.**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Fix any lints. Commit fixes.

**Step 16.3: Doc build (public API shape).**

```bash
cargo doc -p qpdf-test-compare --no-deps
```

Confirm no broken intra-doc links. Given `publish = false`, this is a light check.

**Step 16.4: Patch-coverage.**

```bash
scripts/patch-coverage.sh --base origin/main
```

Ensure `flpdf` gate is green (no changed lines under `crates/flpdf/src/` — we don't touch flpdf). Note `qpdf-test-compare` uncovered lines if any, and add tests to close any gaps that reasonably can be closed. Document ignored lines with `// cov:ignore: <reason>` in a PR-description line.

**Step 16.5: Push branch.**

```bash
git push -u origin feat/flpdf-iurg-qpdf-test-compare
```

**Step 16.6: Open PR.**

```bash
gh pr create --title "feat(qpdf-test-compare): Rust port of qpdf-test-compare (flpdf-iurg)" \
  --body "$(cat <<'EOF'
## Summary
- New `crates/qpdf-test-compare` crate (`publish = false`) reimplementing qpdf v11.9.0's `compare-for-test/qpdf-test-compare.cc` in Rust
- Semantic compare of two PDFs, tolerating FlateDecode compression differences
- Consumers: the flpdf-qtest harness's "check output" step (shim wiring is a separate follow-up issue)

## Design
Full design in `bd show flpdf-iurg` (`design` field). Key points:
- Faithful port — order and asymmetry preserved (unparse for streams, unparseResolved for non-streams)
- Both inputs parsed by flpdf; unparse is required to be internally deterministic, not qpdf-byte-identical
- No qpdf-qtest fixtures vendored (Artistic 2.0 isolation preserved)
- patch-coverage treats the crate as report-only (matches flpdf-cli)

## Test plan
- [ ] `cargo test -p qpdf-test-compare` (default features)
- [ ] `cargo test -p qpdf-test-compare --features qpdf-zlib-compat`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --all --check`
- [ ] `scripts/patch-coverage.sh --base origin/main` — flpdf gate green

## Follow-ups (separate issues)
- flpdf-qtest: PATH shim wiring for the new binary
- Cover `--password` argument end-to-end against an RC4 fixture

## Byte-identical mandate
This crate is *test tooling* comparing two PDFs; it does not itself write PDFs, so the pre-v1.0 qpdf-byte-identical writer mandate does not directly apply. The compare *algorithm* mirrors qpdf's C++ exactly.
EOF
)"
```

**Step 16.7: bd close after merge.**

Do NOT close during PR review. After merge:

```bash
bd close flpdf-iurg --reason="merged in PR #<n>"
```

---

## Rollback / abort

- All commits are new (never amending). To abandon: `git worktree remove .worktrees/flpdf-iurg-qpdf-test-compare --force` from the main repo, then `git branch -D feat/flpdf-iurg-qpdf-test-compare`.
- The design remains in bd (recoverable via `bd show flpdf-iurg`).

## Post-completion

After merge, the follow-up work:

1. File a bd issue: "flpdf-qtest: PATH shim wiring for qpdf-test-compare" as a child of `flpdf-n9t0`.
2. If `--password` path uncovered, file a bd issue to cover it against a small encrypted fixture.
