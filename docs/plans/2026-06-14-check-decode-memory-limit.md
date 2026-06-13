# `--check` decode-output limit (opt-in, qpdf-faithful) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Give `flpdf --check` an opt-in `--decode-memory-limit=BYTES` flag that bounds page content-stream decompression (decode-bomb guard); a stream that trips the cap is a warning (exit 3), not a decode error (exit 2), and the default stays unlimited to match qpdf.

**Architecture:** qpdf defaults `flate_max_memory` to 0 = unlimited and makes the cap opt-in (`--global`, qpdf 12.3/12.4); exceeding it during `--check` surfaces as a warning. flpdf mirrors that *posture only*: default unlimited, opt-in flag, exceed → warning. The cap reuses the existing `flpdf::filters::DecodeLimits` (its `max_output` bounds each `FlateDecode`/`LZWDecode` stage). Limit-exceeded is distinguished from genuine corruption — both currently collapse to `Error::Unsupported` — via an in-crate **shared sentinel** (a `pub(crate)` message-prefix const + a `pub(crate)` predicate) rather than a new public `Error` variant (consistent with flpdf-hn1g.4's deliberate decision). Scope is the check pass only; document-wide decode threading was deferred by flpdf-hn1g.4 and stays out of scope.

**Tech Stack:** Rust, `cargo test`, `clap` (CLI flags), `assert_cmd` + `predicates` (CLI integration tests). Crates: `crates/flpdf` (library), `crates/flpdf-cli` (CLI).

**Constraint (project gate):** Every changed line in `crates/flpdf` must be covered by tests (`scripts/patch-coverage.sh`, 100%). `crates/flpdf-cli` coverage is report-only. Commit before running the coverage gate.

---

## Task 1: Shared limit-exceeded sentinel + predicate (filters.rs)

Foundation: a single `pub(crate)` constant both decode sites build their limit message from, and a `pub(crate)` predicate the check pass uses to recognise it. This is what lets the check pass tell "decode-bomb guard trip" apart from "corrupt stream" without a new `Error` variant.

**Files:**
- Modify: `crates/flpdf/src/filters.rs` (limit message producers at ~`:583-587` Flate and ~`:740-744` LZW; add const + predicate near `decode_stream_data_with_limits` at `:89`)
- Test: `crates/flpdf/src/filters.rs` (`#[cfg(test)] mod tests`)

**Step 1: Write the failing test**

Add to `filters.rs`'s `mod tests`:

```rust
#[test]
fn is_decode_output_limit_error_matches_only_the_limit_sentinel() {
    // The bounded-decode limit message is recognised as a limit trip...
    let limit_err = Error::Unsupported(format!("{DECODE_OUTPUT_LIMIT_PREFIX} 1024 bytes"));
    assert!(is_decode_output_limit_error(&limit_err));

    // ...but an unrelated Unsupported message (genuine corruption / unknown
    // codec) is not.
    let corrupt = Error::Unsupported("corrupt deflate stream: invalid distance code".to_string());
    assert!(!is_decode_output_limit_error(&corrupt));

    // ...and a non-Unsupported error never matches.
    let parse = Error::parse(0, "boom");
    assert!(!is_decode_output_limit_error(&parse));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib is_decode_output_limit_error_matches_only_the_limit_sentinel`
Expected: FAIL — `DECODE_OUTPUT_LIMIT_PREFIX` and `is_decode_output_limit_error` are not defined (compile error).

**Step 3: Write minimal implementation**

Add near `decode_stream_data_with_limits` (e.g. just above it, ~`:88`):

```rust
/// Message prefix shared by every bounded-decode site that aborts because the
/// decoded output would exceed [`DecodeLimits::max_output`]. Producers build
/// their message as `"{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"`, and
/// [`is_decode_output_limit_error`] matches on this prefix. Keeping both on one
/// constant stops the message and the matcher from drifting apart.
pub(crate) const DECODE_OUTPUT_LIMIT_PREFIX: &str = "decoded output exceeds configured limit of";

/// Returns `true` when `error` is the limit-exceeded signal raised when a
/// `FlateDecode`/`LZWDecode` stage aborts because its output would exceed
/// [`DecodeLimits::max_output`].
///
/// Both limit-exceeded and genuine decode failures surface as
/// [`Error::Unsupported`]; this predicate lets the `--check` pass classify a
/// decompression-bomb guard trip (the stream is intact, merely larger than the
/// configured cap) as a warning rather than a stream-encoding error. The
/// sentinel is flpdf-internal — the trailing byte count is flpdf's own value —
/// so PDF content cannot forge a corrupt-stream message into this shape.
pub(crate) fn is_decode_output_limit_error(error: &Error) -> bool {
    matches!(error, Error::Unsupported(message) if message.starts_with(DECODE_OUTPUT_LIMIT_PREFIX))
}
```

Then rewrite the two producers to build from the const.

Flate (`apply_single_filter_decode`, currently ~`:583-587`):

```rust
                if decoded.len() > limit {
                    return Err(format!("{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"));
                }
```

LZW (`lzw_decode`, currently ~`:740-744`):

```rust
            if output.len() > limit {
                return Err(format!("{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"));
            }
```

> Note: the existing limit tests (`filters.rs` ~`:1675` Flate, ~`:1710` LZW) assert the message `.contains("exceeds configured limit")`. The const text preserves that substring, so they keep passing **and** keep covering the two rewritten `format!` lines.

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --lib filters`
Expected: PASS — new predicate test passes; the two existing limit tests still pass.

**Step 5: Commit**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "feat(flpdf): shared decode-output-limit sentinel + predicate (flpdf-svbm)"
```

---

## Task 2: Thread DecodeLimits through the check pass + classify limit trips (check.rs)

Bound each content stream's decode and branch on the result: limit trip → warning, other decode error → existing error path.

**Files:**
- Modify: `crates/flpdf/src/check.rs` (imports `:7`; `check_content_streams` `:266`/`:300`; `check_reader_inner` `:115`; `check_reader_with_options` `:90`; add new public entry near `:95`)
- Modify: `crates/flpdf/src/lib.rs` (re-export list `:120-122`)
- Test: `crates/flpdf/src/check.rs` (`#[cfg(test)] mod tests`)

**Step 1: Write the failing tests**

Add a bomb builder near the other content builders (after `clean_flate_content_pdf`, ~`:653`):

```rust
    /// A single-page PDF whose `/Contents 4 0 R` is a *valid* FlateDecode stream
    /// that inflates to `decoded_len` bytes — small compressed, large inflated:
    /// a decompression bomb relative to a tight output cap.
    fn bomb_flate_content_pdf(decoded_len: usize) -> Vec<u8> {
        content_pdf("4 0 R", &[(4, clean_flate_object(4, &vec![0u8; decoded_len]))])
    }
```

Add tests in `mod tests` (these reference `check_reader_with_options_and_limits`, `PdfOpenOptions`, and `crate::filters::DecodeLimits`):

```rust
    #[test]
    fn decode_output_one_over_limit_warns_not_errors() {
        // 1025 inflated bytes under a 1024-byte cap: the stream is intact, so the
        // guard trips as a WARNING (still valid), never a decode error.
        let report = check_reader_with_options_and_limits(
            Cursor::new(bomb_flate_content_pdf(1025)),
            PdfOpenOptions { repair: false, ..PdfOpenOptions::default() },
            crate::filters::DecodeLimits { max_output: Some(1024) },
        )
        .unwrap();
        assert!(report.valid); // warning only -> still valid
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Warning && d.message.contains("decode-bomb guard")
        }));
        assert!(!report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Error
                && d.message.contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn decode_output_exactly_at_limit_is_clean() {
        // Boundary: inflated output == cap succeeds (no warning, no error).
        let report = check_reader_with_options_and_limits(
            Cursor::new(bomb_flate_content_pdf(1024)),
            PdfOpenOptions { repair: false, ..PdfOpenOptions::default() },
            crate::filters::DecodeLimits { max_output: Some(1024) },
        )
        .unwrap();
        assert!(report.valid);
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("content stream")));
    }

    #[test]
    fn decode_limit_does_not_mask_corruption() {
        // With a cap set, a genuinely corrupt FlateDecode stream is still an
        // ERROR, not a guard warning — the limit path must not swallow real
        // decode failures.
        let report = check_reader_with_options_and_limits(
            Cursor::new(corrupt_flate_content_pdf()),
            PdfOpenOptions { repair: false, ..PdfOpenOptions::default() },
            crate::filters::DecodeLimits { max_output: Some(1024) },
        )
        .unwrap();
        assert!(!report.valid);
        assert!(report.diagnostics.entries().iter().any(|d| {
            d.severity == Severity::Error
                && d.message.contains("errors while decoding content stream")
        }));
    }

    #[test]
    fn unlimited_default_decodes_large_stream_without_warning() {
        // Regression guard: with no cap (default), the same large stream decodes
        // fine — behaviour is unchanged from before the limit existed.
        let report = check_reader_with_options_and_limits(
            Cursor::new(bomb_flate_content_pdf(64 * 1024)),
            PdfOpenOptions { repair: false, ..PdfOpenOptions::default() },
            crate::filters::DecodeLimits::default(),
        )
        .unwrap();
        assert!(report.valid);
        assert!(!report
            .diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("content stream")));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --lib check::tests::decode_`
Expected: FAIL — `check_reader_with_options_and_limits` is not defined (compile error).

**Step 3: Write minimal implementation**

(a) Imports at `check.rs:7` — add the limit type/predicate:

```rust
use crate::filters::{decode_stream_data_with_limits, is_decode_output_limit_error, DecodeLimits};
```

(b) `check_content_streams` (`:266`) — add the `limits` param and replace the decode call + classify:

```rust
fn check_content_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    diagnostics: &mut Diagnostics,
    limits: DecodeLimits,
) {
```

Replace the decode block (currently `:296-312`) with:

```rust
            // A decode `Err` is one of two things:
            //   * the opt-in output cap tripped — the stream is intact, just
            //     larger than the configured limit, so this is a deliberate
            //     decode-bomb guard, reported as a WARNING (qpdf's posture:
            //     exceeding flate_max_memory is a warning, not an error);
            //   * any other failure means the stream cannot be decoded as
            //     declared (corrupt payload, `/Filter` chain past the cap, bad
            //     `/DecodeParms`) — a genuine stream-encoding ERROR.
            if let Err(error) =
                decode_stream_data_with_limits(&stream.dict, &stream.data, limits)
            {
                // qpdf renders the location as "content stream object N G" (no
                // trailing " R"); format the number/generation pair directly.
                let location = match stream_ref {
                    Some(r) => format!("content stream object {} {}", r.number, r.generation),
                    None => "inline content stream".to_string(),
                };
                if is_decode_output_limit_error(&error) {
                    diagnostics.push(Diagnostic::warning(
                        format!(
                            "page {page_number}: {location}: decoded output exceeds the configured limit; skipped (decode-bomb guard)"
                        ),
                        None,
                    ));
                } else {
                    diagnostics.push(Diagnostic::error(
                        format!("page {page_number}: {location}: errors while decoding content stream"),
                        None,
                    ));
                }
            }
```

(c) `check_reader_inner_with_options` (`:125`) — add a `limits` param and pass it to `check_content_streams`:

```rust
fn check_reader_inner_with_options<R: Read + Seek>(
    reader: R,
    options: PdfOpenOptions,
    limits: DecodeLimits,
) -> crate::Result<CheckReport> {
```

At the `check_content_streams` call (`:183`):

```rust
    check_content_streams(&mut pdf, &mut diagnostics, limits);
```

(d) `check_reader_inner` (`:115`) — delegate with unlimited:

```rust
fn check_reader_inner<R: Read + Seek>(reader: R, allow_repair: bool) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(
        reader,
        PdfOpenOptions {
            repair: allow_repair,
            ..PdfOpenOptions::default()
        },
        DecodeLimits::default(),
    )
}
```

(e) `check_reader_with_options` (`:90`) — delegate with unlimited:

```rust
pub fn check_reader_with_options<R: Read + Seek>(
    reader: R,
    options: PdfOpenOptions,
) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(reader, options, DecodeLimits::default())
}
```

(f) New public entry (add after `check_reader_with_options`, ~`:95`):

```rust
/// Validate the document with explicit open options and an opt-in decode-output
/// limit.
///
/// Behaves like [`check_reader_with_options`], but bounds each page content
/// stream's `FlateDecode`/`LZWDecode` output to [`DecodeLimits::max_output`]. A
/// stream whose decoded output would exceed that cap is reported as a warning
/// (a decompression-bomb guard trip), not a stream-encoding error: the stream
/// is intact, merely larger than the caller allowed. With
/// [`DecodeLimits::default`] (no cap) this is identical to
/// [`check_reader_with_options`].
///
/// # Errors
///
/// Same as [`check_reader_with_options`].
pub fn check_reader_with_options_and_limits<R: Read + Seek>(
    reader: R,
    options: PdfOpenOptions,
    limits: DecodeLimits,
) -> crate::Result<CheckReport> {
    check_reader_inner_with_options(reader, options, limits)
}
```

(g) `lib.rs` re-export (`:120-122`) — add the new entry:

```rust
pub use check::{
    check_reader, check_reader_strict, check_reader_with_options,
    check_reader_with_options_and_limits, CheckReport, CheckSummary,
};
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --lib check`
Expected: PASS — the four new tests pass; all existing check tests still pass (they call the unchanged `check_reader*` entries, which now delegate with `DecodeLimits::default()`).

**Step 5: Commit**

```bash
git add crates/flpdf/src/check.rs crates/flpdf/src/lib.rs
git commit -m "feat(flpdf): --check bounds content-stream decode via opt-in DecodeLimits (flpdf-svbm)"
```

---

## Task 3: CLI `--decode-memory-limit` flag + run_check wiring (flpdf-cli)

Expose the cap on both `--check` surfaces (legacy top-level flag and the `check` subcommand), default unlimited.

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (import `:17`; `Cli` struct `~:145`; `CheckCommand` struct `:737`; `run_check` `:2046`/`:2050`; call sites `:1351` and `:1790`)
- Test: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`

**Step 1: Write the failing integration tests**

Add a bomb fixture builder near the top of `cli_check_exitcodes.rs` (uses the public encoder so the stream round-trips):

```rust
/// A structurally valid single-page PDF whose page `/Contents 4 0 R` is a
/// *valid* FlateDecode stream that inflates to `decoded_len` bytes (small
/// compressed, large inflated). Exercises the opt-in `--decode-memory-limit`
/// guard: the stream is intact, so without a cap it is clean, and with a tight
/// cap it trips the guard as a warning.
fn bomb_content_stream_pdf_bytes(decoded_len: usize) -> Vec<u8> {
    let mut flate_dict = flpdf::Dictionary::new();
    flate_dict.insert("Filter", flpdf::Object::Name(b"FlateDecode".to_vec()));
    let encoded =
        flpdf::filters::encode_stream_data(&flate_dict, &vec![0u8; decoded_len]).unwrap();

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&encoded);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}
```

Add tests:

```rust
/// With `--decode-memory-limit` set below the inflated size, the intact-but-large
/// content stream trips the decompression-bomb guard: a WARNING (exit 3), and
/// the trailing "clean" reassurance note is suppressed — but it is NOT an error
/// (exit 2).
#[test]
fn check_decode_memory_limit_bomb_warns_exit_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&bomb_content_stream_pdf_bytes(64 * 1024)).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", "--decode-memory-limit", "1024", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("No syntax or stream encoding errors found").not())
        .stderr(predicate::str::contains("decode-bomb guard"));
}

/// Without the flag, the same large stream decodes fine: clean exit 0 (default
/// unlimited, matching qpdf).
#[test]
fn check_no_limit_large_stream_exits_0() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&bomb_content_stream_pdf_bytes(64 * 1024)).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(0);
}

/// The `check` subcommand carries the same flag.
#[test]
fn check_subcommand_decode_memory_limit_bomb_warns_exit_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&bomb_content_stream_pdf_bytes(64 * 1024)).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", "--decode-memory-limit", "1024", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("decode-bomb guard"));
}

/// A genuinely corrupt content stream is still an error (exit 2) even with the
/// cap set — the limit path must not mask real decode failures.
#[test]
fn check_decode_memory_limit_does_not_mask_corruption() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_content_stream_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--decode-memory-limit", "1024", f.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("errors while decoding content stream"));
}
```

> `corrupt_content_stream_pdf_bytes()` already exists in this file (`:100`). Confirm `flpdf` is reachable from the test crate (it is — `flpdf-cli` depends on `flpdf`; `Dictionary`, `Object`, and `filters::encode_stream_data` are all public).

**Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes check_decode_memory_limit`
Expected: FAIL — `--decode-memory-limit` is an unknown argument (clap error / non-matching exit code).

**Step 3: Write minimal implementation**

(a) Import (`main.rs:17`) — add the new check entry:

```rust
    check_reader_with_options, check_reader_with_options_and_limits, enumerate_document_annotations,
    filters, flatten_annotations,
```

(b) `Cli` struct (after the `repair`/`password` legacy fields, ~`:150`):

```rust
    /// Bound each page content stream's decoded output to BYTES during `--check`
    /// (opt-in decompression-bomb guard). Absent = unlimited (qpdf's default).
    /// A stream exceeding the cap is a warning (exit 3), not an error.
    #[arg(long = "decode-memory-limit", value_name = "BYTES")]
    decode_memory_limit: Option<usize>,
```

(c) `CheckCommand` struct (`:737-744`) — same field:

```rust
#[derive(Debug, ClapArgs)]
struct CheckCommand {
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    /// Bound each page content stream's decoded output to BYTES (opt-in
    /// decompression-bomb guard). Absent = unlimited. Exceeding the cap is a
    /// warning (exit 3), not an error.
    #[arg(long = "decode-memory-limit", value_name = "BYTES")]
    decode_memory_limit: Option<usize>,
}
```

(d) `run_check` signature (`:2046`) and the check call (`:2050`):

```rust
fn run_check(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    decode_limits: filters::DecodeLimits,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(&input).map_err(|error| error_with_file(&input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let report = check_reader_with_options_and_limits(BufReader::new(file), options, decode_limits)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
```

(e) Top-level call site (`:1351`):

```rust
    } else if args.check {
        run_check(
            args.input,
            args.repair,
            &args.password,
            filters::DecodeLimits { max_output: args.decode_memory_limit },
        )
```

(f) Subcommand call site (`:1790`):

```rust
        Commands::Check(cmd) => run_check(
            Some(cmd.input),
            cmd.repair,
            &cmd.password,
            filters::DecodeLimits { max_output: cmd.decode_memory_limit },
        ),
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes`
Expected: PASS — the four new tests pass; all existing exit-code tests still pass.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_check_exitcodes.rs
git commit -m "feat(flpdf-cli): --decode-memory-limit opt-in decode-bomb guard for --check (flpdf-svbm)"
```

---

## Task 4: Quality gates + docs

**Step 1: Full workspace test**

Run: `cargo test --workspace`
Expected: all green (no failures).

**Step 2: Format + lint**

Run: `cargo fmt --all` then `cargo fmt --all --check` (must be clean — CI gates on this), and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no diffs, no warnings.

**Step 3: Doc check** (public doc rules)

Run: `cargo doc -p flpdf --no-deps`
Verify the new `check_reader_with_options_and_limits` doc: English only, intra-doc links resolve (`[`check_reader_with_options`]`, `[`DecodeLimits::max_output`]`, `[`DecodeLimits::default`]`), no beads IDs / internal jargon in `///`. (The `flpdf-svbm` / `flpdf-hn1g.4` references live only in commit messages and this plan, never in `///`.)

**Step 4: Patch-coverage gate (flpdf = 100% of changed lines)**

Commit first (the gate diffs HEAD; a dirty tree errors). Then:

Run: `scripts/patch-coverage.sh --base main`
Expected: `crates/flpdf` changed lines 100% covered. If a const-declaration line is flagged as uncovered (string consts are usually not instrumented, but if so), add `// cov:ignore: compile-time constant, exercised via producers/predicate tests` and note it in the PR description. `crates/flpdf-cli` is report-only.

**Step 5: Update the beads design note for accuracy (optional, pre-PR)**

The `DecodeLimits::max_output` cap is **per `FlateDecode`/`LZWDecode` stage**, not per-stream-total — keep the PR description and any prose aligned with that (do not claim per-stream-total or qpdf byte parity).

**Step 6: Commit any formatting/doc fixups**

```bash
git add -A
git commit -m "chore(flpdf): fmt/clippy/doc fixups for --check decode limit (flpdf-svbm)"
```

---

## Out of scope (do NOT implement here)

- Document-wide decode-limit threading (~15 decode sites) — deferred by flpdf-hn1g.4.
- Per-filter-type limits or human-readable size suffixes (e.g. `256M`).
- Any new public `Error` variant — classification stays on the in-crate shared sentinel.
- qpdf byte-for-byte parity claims — flpdf mirrors qpdf's *posture* only.
