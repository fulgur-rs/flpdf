//! E2E acceptance tests for the five optimization flags wired in flpdf-9hc.12.7.
//!
//! Tests five matrix cells via `flpdf rewrite`:
//!
//! 1. **normalize-content=y/n** — re-parses the decoded page content stream and
//!    asserts semantic equivalence using `flpdf::normalize_content_stream`.
//! 2. **coalesce-contents** — a page with `/Contents [2 0 R 3 0 R]` becomes a
//!    single `/Contents` reference after rewriting.
//! 3. **remove-unreferenced-resources=auto|yes|no** — a page with `/F2` in
//!    `/Resources/Font` that is not referenced in its content stream has `/F2`
//!    pruned under `auto`/`yes` and retained under `no`.
//! 4. **compress-streams=y/n** — decoded stream bytes are preserved; the `/Filter`
//!    key is `/FlateDecode` when `y` and absent when `n`.
//! 5. **newline-before-endstream=y/n** — raw output bytes are inspected for the
//!    presence/absence of `\n` before every `endstream` keyword.
//!
//! # Observability strategy
//!
//! These tests use **observable equivalence**, not byte equality:
//!
//! - Content streams are compared after `normalize_content_stream` application.
//! - Stream encoding is compared by decoded payload bytes.
//! - Resource dictionaries are compared by key sets.
//! - Raw byte patterns (newlines) are searched via `memchr`-style scans.
//!
//! # qpdf-byte divergence documentation
//!
//! ## .12.2 (normalize-content)
//!
//! `flpdf::normalize_content_stream` diverges from qpdf's `--normalize-content`
//! at the byte level in three known ways (documented in `crates/flpdf/src/content_stream.rs`):
//! - **Integer-valued reals**: `Real(1.0)` is emitted as `"1"` (no trailing `.0`).
//!   qpdf preserves the decimal point.
//! - **Dictionary key ordering**: `BTreeMap` gives lexicographic order; qpdf may
//!   use insertion order.
//! - **Token separation**: a single space is always emitted between operands;
//!   qpdf may omit spaces between adjacent delimiters (`>>`, `<<`).
//!
//! These tests therefore validate observable equivalence (re-parsing yields the same
//! operator sequence and operand values) rather than byte identity with qpdf.
//!
//! ## .12.5 (compress-streams)
//!
//! `flpdf` uses `flate2` with `Compression::default()`, which selects a different
//! block layout than qpdf's internal zlib build. As a result, FlateDecode output
//! is observably equivalent (same decoded bytes) but NOT byte-identical.
//! (Documented in `crates/flpdf/src/writer.rs` §"Byte-vs-observable policy".)
//!
//! # qpdf gating
//!
//! Tests that call `qpdf --check` require qpdf on PATH.
//! - **Linux CI** (`CI` env var set, non-Windows): panics if qpdf is absent.
//! - **Local / Windows CI**: prints a diagnostic and skips the qpdf guard.

use assert_cmd::Command as CargoCommand;
use flpdf::{
    filters::decode_stream_data,
    normalize_content_stream,
    pages::{page_content_bytes, page_refs},
    ContentStreamParser, ContentToken, Object, Pdf,
};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

// ---------------------------------------------------------------------------
// qpdf guards
// ---------------------------------------------------------------------------

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns `true` when the caller should skip the qpdf guard.
#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    let on_windows = cfg!(target_os = "windows");
    if on_ci && !on_windows {
        panic!(
            "qpdf is required for cli_optimization_matrix tests on CI (Linux); \
             install qpdf in the workflow before running this test suite"
        );
    }
    eprintln!(
        "skipping qpdf --check guard: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    true
}

// ---------------------------------------------------------------------------
// flpdf runner helper
// ---------------------------------------------------------------------------

/// Run `flpdf rewrite [extra_args...] <input> <output>` and assert success.
fn run_rewrite(input: &Path, output: &Path, extra_args: &[&str]) {
    let mut cmd = CargoCommand::cargo_bin("flpdf").expect("flpdf binary must exist");
    cmd.arg("rewrite");
    for &arg in extra_args {
        cmd.arg(arg);
    }
    cmd.arg(input.to_str().unwrap());
    cmd.arg(output.to_str().unwrap());
    cmd.assert().success();
}

/// Assert `qpdf --check <path>` succeeds (no syntax/stream-encoding errors).
fn assert_qpdf_check(path: &Path) {
    let out = ShellCommand::new("qpdf")
        .arg("--check")
        .arg(path.to_str().unwrap())
        .output()
        .expect("failed to spawn qpdf --check");
    assert!(
        out.status.success(),
        "qpdf --check reported errors on {}:\nstdout: {}\nstderr: {}",
        path.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ---------------------------------------------------------------------------
// Cell 1a: normalize-content=y
//
// Asserts that the output page's decoded content stream equals the result of
// applying `normalize_content_stream` to the input's decoded content stream.
// Uses one-page.pdf which has a single /Contents stream.
// ---------------------------------------------------------------------------

#[test]
fn normalize_content_y_produces_canonical_form() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("normalized.pdf");

    // Run with full-rewrite (required by normalize-content) + normalize-content=y
    run_rewrite(
        &input,
        &output,
        &["--full-rewrite", "--normalize-content=y"],
    );

    // Open input and output with flpdf::Pdf
    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&output).unwrap();

    let mut in_pdf = Pdf::open(Cursor::new(input_bytes)).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes)).unwrap();

    // Get page content bytes from both
    let in_pages = page_refs(&mut in_pdf).unwrap();
    let out_pages = page_refs(&mut out_pdf).unwrap();

    assert_eq!(in_pages.len(), out_pages.len(), "page count must match");

    for (in_pr, out_pr) in in_pages.iter().zip(out_pages.iter()) {
        let in_content = page_content_bytes(&mut in_pdf, *in_pr).unwrap();
        let out_content = page_content_bytes(&mut out_pdf, *out_pr).unwrap();

        // The expected bytes are the result of normalize(input content).
        let expected = normalize_content_stream(&in_content)
            .expect("normalize_content_stream must succeed on input");

        // Primary assertion: the decoded output bytes must equal normalize(input)
        // directly.  This catches any regression where the CLI emits semantically
        // equivalent but non-normalized bytes (e.g. the flag was silently ignored).
        assert_eq!(
            out_content,
            expected,
            "normalize-content=y: decoded output content stream bytes do not equal \
             normalize(input);\n\
             output content (first 200 bytes): {:?}",
            &out_content[..out_content.len().min(200)]
        );

        // Diagnostic: verify idempotency — normalize(output) == normalize(input).
        // This should always hold after the primary assertion, but it catches any
        // re-normalization divergence independently.
        let normalized_out = normalize_content_stream(&out_content)
            .expect("normalize_content_stream must succeed on output");
        assert_eq!(
            normalized_out, expected,
            "normalize-content=y: output content stream is not idempotent under \
             normalize_content_stream"
        );
    }

    // qpdf --check guard
    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Cell 1b: normalize-content=n
//
// Asserts that the decoded content stream bytes are unchanged.
// ---------------------------------------------------------------------------

#[test]
fn normalize_content_n_leaves_content_unchanged() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("norm-n.pdf");

    run_rewrite(
        &input,
        &output,
        &["--full-rewrite", "--normalize-content=n"],
    );

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&output).unwrap();

    let mut in_pdf = Pdf::open(Cursor::new(input_bytes)).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes)).unwrap();

    let in_pages = page_refs(&mut in_pdf).unwrap();
    let out_pages = page_refs(&mut out_pdf).unwrap();

    for (in_pr, out_pr) in in_pages.iter().zip(out_pages.iter()) {
        let in_content = page_content_bytes(&mut in_pdf, *in_pr).unwrap();
        let out_content = page_content_bytes(&mut out_pdf, *out_pr).unwrap();

        assert_eq!(
            in_content, out_content,
            "normalize-content=n: decoded content must be identical to input"
        );
    }

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Cell 2: coalesce-contents
//
// Input: multi-contents-one-page.pdf — has /Contents [2 0 R 3 0 R].
// Expected output: /Contents is a single indirect object reference.
// The decoded content of the merged stream must equal the concatenation of
// the two source streams (with whitespace separation as per ISO 32000-1 §7.8.2).
// ---------------------------------------------------------------------------

#[test]
fn coalesce_contents_merges_array_to_single_stream() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("multi-contents-one-page.pdf");
    let output = tmp.path().join("coalesced.pdf");

    run_rewrite(&input, &output, &["--full-rewrite", "--coalesce-contents"]);

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&output).unwrap();

    let mut in_pdf = Pdf::open(Cursor::new(input_bytes)).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes)).unwrap();

    let out_pages = page_refs(&mut out_pdf).unwrap();
    assert_eq!(out_pages.len(), 1, "output must have exactly one page");

    let out_page_ref = out_pages[0];

    // Resolve the output page dict and verify /Contents is NOT an array.
    let out_page_obj = out_pdf.resolve(out_page_ref).unwrap();
    let out_page_dict = match &out_page_obj {
        Object::Dictionary(d) => d.clone(),
        other => panic!("page object must be a Dictionary, got {other:?}"),
    };

    let contents_entry = out_page_dict
        .get("Contents")
        .cloned()
        .expect("/Contents must be present in output page");

    // After coalescing, /Contents must be a single indirect reference (not an array).
    assert!(
        matches!(contents_entry, Object::Reference(_)),
        "coalesce-contents: /Contents must be a single indirect reference after merge; \
         got {contents_entry:?}"
    );

    // The merged stream's decoded bytes must equal the concatenation of the
    // two source streams. Collect the input streams' decoded bytes.
    let in_pages = page_refs(&mut in_pdf).unwrap();
    assert_eq!(in_pages.len(), 1);
    let in_content = page_content_bytes(&mut in_pdf, in_pages[0]).unwrap();
    let out_content = page_content_bytes(&mut out_pdf, out_page_ref).unwrap();

    // Both must be parseable as content streams with the same operator sequence.
    let in_tokens = collect_content_tokens(&in_content);
    let out_tokens = collect_content_tokens(&out_content);

    assert_eq!(
        in_tokens,
        out_tokens,
        "coalesce-contents: merged stream operator sequence must match concatenated inputs;\n\
         input operators: {:?}\n\
         output operators: {:?}",
        &in_tokens[..in_tokens.len().min(10)],
        &out_tokens[..out_tokens.len().min(10)],
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

/// Collect `ContentToken::Op` entries (operator + operand values) from a content
/// stream for semantic comparison (order-sensitive, but ignores whitespace
/// differences).  InlineImage and Comment tokens are excluded because the
/// coalesce fixture contains only text operators; excluding them keeps the
/// filter consistent with the original while comparing full operand values.
fn collect_content_tokens(bytes: &[u8]) -> Vec<ContentToken> {
    ContentStreamParser::new(bytes)
        .filter_map(|t| t.ok())
        .filter(|t| matches!(t, ContentToken::Op { .. }))
        .collect()
}

// ---------------------------------------------------------------------------
// Cell 3a: remove-unreferenced-resources=auto  (prunes /F2, keeps /F1)
// Cell 3b: remove-unreferenced-resources=yes   (prunes /F2, keeps /F1)
// Cell 3c: remove-unreferenced-resources=no    (leaves /F1 and /F2)
//
// Input: unref-resources-one-page.pdf — single page with /Resources/Font{F1,F2};
// content stream uses /F1 (via `Tf`) but NOT /F2.
// ---------------------------------------------------------------------------

#[test]
fn remove_unref_resources_auto_prunes_unused_font() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("unref-resources-one-page.pdf");
    let output = tmp.path().join("unref-auto.pdf");

    run_rewrite(
        &input,
        &output,
        &["--full-rewrite", "--remove-unreferenced-resources=auto"],
    );

    let font_keys = extract_page_font_keys(&output);
    assert!(
        font_keys.contains(&b"F1".to_vec()),
        "remove-unreferenced-resources=auto: /F1 (used) must be retained; font keys: {:?}",
        font_keys
    );
    assert!(
        !font_keys.contains(&b"F2".to_vec()),
        "remove-unreferenced-resources=auto: /F2 (unused) must be pruned; font keys: {:?}",
        font_keys
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

#[test]
fn remove_unref_resources_yes_prunes_unused_font() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("unref-resources-one-page.pdf");
    let output = tmp.path().join("unref-yes.pdf");

    run_rewrite(
        &input,
        &output,
        &["--full-rewrite", "--remove-unreferenced-resources=yes"],
    );

    let font_keys = extract_page_font_keys(&output);
    assert!(
        font_keys.contains(&b"F1".to_vec()),
        "remove-unreferenced-resources=yes: /F1 (used) must be retained; font keys: {:?}",
        font_keys
    );
    assert!(
        !font_keys.contains(&b"F2".to_vec()),
        "remove-unreferenced-resources=yes: /F2 (unused) must be pruned; font keys: {:?}",
        font_keys
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

#[test]
fn remove_unref_resources_no_retains_all_fonts() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("unref-resources-one-page.pdf");
    let output = tmp.path().join("unref-no.pdf");

    run_rewrite(
        &input,
        &output,
        &["--full-rewrite", "--remove-unreferenced-resources=no"],
    );

    let font_keys = extract_page_font_keys(&output);
    assert!(
        font_keys.contains(&b"F1".to_vec()),
        "remove-unreferenced-resources=no: /F1 must be retained; font keys: {:?}",
        font_keys
    );
    assert!(
        font_keys.contains(&b"F2".to_vec()),
        "remove-unreferenced-resources=no: /F2 must be retained (no pruning); font keys: {:?}",
        font_keys
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

/// Open `path` as a PDF, get page 1's /Resources/Font dict, and return the
/// font name keys (as byte vecs).
fn extract_page_font_keys(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let pages = page_refs(&mut pdf).unwrap();
    assert!(!pages.is_empty(), "output PDF must have at least one page");

    let page_ref = pages[0];
    let page_obj = pdf.resolve(page_ref).unwrap();
    let page_dict = match page_obj {
        Object::Dictionary(d) => d,
        other => panic!("page must be a Dictionary, got {other:?}"),
    };

    // Resolve /Resources (may be inline or a reference).
    let resources_obj = match page_dict.get("Resources").cloned() {
        Some(Object::Reference(r)) => pdf.resolve(r).unwrap(),
        Some(obj) => obj,
        None => return vec![],
    };
    let resources_dict = match resources_obj {
        Object::Dictionary(d) => d,
        _ => return vec![],
    };

    // Resolve /Font sub-dict.
    let font_obj = match resources_dict.get("Font").cloned() {
        Some(Object::Reference(r)) => pdf.resolve(r).unwrap(),
        Some(obj) => obj,
        None => return vec![],
    };
    let font_dict = match font_obj {
        Object::Dictionary(d) => d,
        _ => return vec![],
    };

    font_dict.iter().map(|(k, _)| k.to_vec()).collect()
}

// ---------------------------------------------------------------------------
// Cell 4a: compress-streams=y
//
// Asserts: the output page's stream dict has /Filter = /FlateDecode, and
// the decoded bytes match the decoded input stream bytes.
//
// Note: byte-level zlib output differs from qpdf (.12.5 divergence — see
// module-level doc comment); only decoded-bytes equality is asserted.
// ---------------------------------------------------------------------------

#[test]
fn compress_streams_y_applies_flatedecode_and_roundtrips() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("compress-y.pdf");

    run_rewrite(&input, &output, &["--full-rewrite", "--compress-streams=y"]);

    let output_bytes = std::fs::read(&output).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(&output_bytes)).unwrap();

    // Verify: every page's content stream is compressed with FlateDecode.
    let pages = page_refs(&mut out_pdf).unwrap();
    assert!(!pages.is_empty());

    for page_ref in &pages {
        let page_obj = out_pdf.resolve(*page_ref).unwrap();
        let page_dict = match &page_obj {
            Object::Dictionary(d) => d.clone(),
            other => panic!("page must be a Dictionary, got {other:?}"),
        };

        let contents_ref = match page_dict.get("Contents").cloned() {
            Some(Object::Reference(r)) => r,
            Some(other) => panic!("expected /Contents to be a reference, got {other:?}"),
            None => continue, // empty page
        };

        let content_stream = match out_pdf.resolve(contents_ref).unwrap() {
            Object::Stream(s) => s,
            other => panic!("expected /Contents to resolve to a Stream, got {other:?}"),
        };

        // /Filter must be /FlateDecode (either as a Name or single-element array).
        let filter = content_stream.dict.get("Filter").cloned();
        let is_flatedecode = match &filter {
            Some(Object::Name(n)) => n.as_slice() == b"FlateDecode",
            Some(Object::Array(arr)) => {
                matches!(arr.as_slice(), [Object::Name(n)] if n.as_slice() == b"FlateDecode")
            }
            _ => false,
        };
        assert!(
            is_flatedecode,
            "compress-streams=y: /Contents stream must have /Filter /FlateDecode; got {filter:?}"
        );

        // Decoded bytes must be non-empty (round-trip sanity).
        let decoded = decode_stream_data(&content_stream.dict, &content_stream.data)
            .expect("decoding FlateDecode stream must succeed");
        assert!(
            !decoded.is_empty(),
            "compress-streams=y: decoded content stream must be non-empty"
        );
    }

    // Round-trip: decoded output content must equal decoded input content.
    let input_bytes = std::fs::read(&input).unwrap();
    let mut in_pdf = Pdf::open(Cursor::new(input_bytes)).unwrap();
    let in_pages = page_refs(&mut in_pdf).unwrap();

    for (in_pr, out_pr) in in_pages.iter().zip(pages.iter()) {
        let in_content = page_content_bytes(&mut in_pdf, *in_pr).unwrap();
        let out_content = page_content_bytes(&mut out_pdf, *out_pr).unwrap();
        assert_eq!(
            in_content, out_content,
            "compress-streams=y: decoded content round-trip must be byte-identical to input"
        );
    }

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Cell 4b: compress-streams=n
//
// Asserts: the output page's stream dict has NO /Filter key, and decoded
// bytes match the input.
// ---------------------------------------------------------------------------

#[test]
fn compress_streams_n_omits_filter_and_roundtrips() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("compress-n.pdf");

    run_rewrite(&input, &output, &["--full-rewrite", "--compress-streams=n"]);

    let output_bytes = std::fs::read(&output).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(&output_bytes)).unwrap();

    let pages = page_refs(&mut out_pdf).unwrap();
    assert!(!pages.is_empty());

    for page_ref in &pages {
        let page_obj = out_pdf.resolve(*page_ref).unwrap();
        let page_dict = match &page_obj {
            Object::Dictionary(d) => d.clone(),
            other => panic!("page must be a Dictionary, got {other:?}"),
        };

        let contents_ref = match page_dict.get("Contents").cloned() {
            Some(Object::Reference(r)) => r,
            Some(other) => panic!("expected /Contents reference, got {other:?}"),
            None => continue,
        };

        let content_stream = match out_pdf.resolve(contents_ref).unwrap() {
            Object::Stream(s) => s,
            other => panic!("expected a Stream, got {other:?}"),
        };

        let filter = content_stream.dict.get("Filter");
        assert!(
            filter.is_none(),
            "compress-streams=n: /Contents stream must have no /Filter key; got {filter:?}"
        );
    }

    // Round-trip: decoded output content must equal decoded input content.
    let input_bytes = std::fs::read(&input).unwrap();
    let mut in_pdf = Pdf::open(Cursor::new(input_bytes)).unwrap();
    let in_pages = page_refs(&mut in_pdf).unwrap();

    for (in_pr, out_pr) in in_pages.iter().zip(pages.iter()) {
        let in_content = page_content_bytes(&mut in_pdf, *in_pr).unwrap();
        let out_content = page_content_bytes(&mut out_pdf, *out_pr).unwrap();
        assert_eq!(
            in_content, out_content,
            "compress-streams=n: raw content must be byte-identical to decoded input"
        );
    }

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Cell 5a: newline-before-endstream=y
//
// Every `endstream` keyword in the raw output bytes must be preceded by `\n`.
// ---------------------------------------------------------------------------

#[test]
fn newline_before_endstream_y_always_inserts_newline() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("newline-y.pdf");

    run_rewrite(
        &input,
        &output,
        &["--full-rewrite", "--newline-before-endstream=y"],
    );

    let output_bytes = std::fs::read(&output).unwrap();
    // Use the structural helper to find only genuine endstream keyword positions,
    // skipping any accidental matches inside compressed payload bytes.
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes.clone())).unwrap();
    let endstream_offsets = real_endstream_offsets(&output_bytes, &mut out_pdf);

    assert!(
        !endstream_offsets.is_empty(),
        "newline-before-endstream=y: no stream objects found in output"
    );

    let mut violations = 0usize;
    for &start in &endstream_offsets {
        if start == 0 || output_bytes[start - 1] != b'\n' {
            violations += 1;
            eprintln!(
                "newline-before-endstream=y violation at offset {start}: \
                 preceding byte is 0x{:02x}",
                output_bytes[start - 1]
            );
        }
    }

    assert_eq!(
        violations, 0,
        "newline-before-endstream=y: {violations} `endstream` keyword(s) not preceded by \\n"
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Cell 5b: newline-before-endstream=n
//
// With `n`, flpdf omits the extra newline when the payload already ends with
// `\n` or `\r`; it still inserts one `\n` for ISO 32000-1 parseability when
// the payload is NOT EOL-terminated.
//
// To make this test discriminating — i.e., able to detect a regression where
// `n` behaves identically to `y` — we use `--compress-streams=n` so that the
// content-stream payload is written as raw decoded bytes.  The decoded payload
// for `one-page.pdf` ends with `\n` (the last line of the content stream),
// which means:
//
//   flag=n: no extra `\n` is inserted → byte before `endstream` is the
//           payload's own trailing `\n` and the byte before THAT is the
//           last non-newline content byte (NOT another `\n`).
//   flag=y: exactly one extra `\n` is inserted unconditionally → byte before
//           `endstream` is the inserted `\n` and the byte before THAT is the
//           payload's own `\n` (two consecutive `\n` bytes).
//
// Assertions:
//   (n-test) at least one `endstream` is found.
//   (n-test) for every `endstream` preceded by `\n`, the byte two positions
//            before `endstream` is NOT `\n` (no double-newline → extra `\n`
//            was not inserted for a payload that already ends with `\n`).
//   (y-contrast, in the y-test above) every `endstream` is preceded by `\n`.
// ---------------------------------------------------------------------------

#[test]
fn newline_before_endstream_n_omits_extra_newline_for_eol_terminated_payload() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("newline-n.pdf");

    // Use --compress-streams=n so the payload is written as raw decoded bytes.
    // The decoded content stream for one-page.pdf ends with b'\n', so flag=n
    // must NOT insert an additional newline before endstream.
    run_rewrite(
        &input,
        &output,
        &[
            "--full-rewrite",
            "--newline-before-endstream=n",
            "--compress-streams=n",
        ],
    );

    let output_bytes = std::fs::read(&output).unwrap();

    // Use the structural helper to find only genuine endstream keyword positions,
    // skipping any accidental matches inside compressed or raw payload bytes.
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes.clone())).unwrap();
    let endstream_offsets = real_endstream_offsets(&output_bytes, &mut out_pdf);

    assert!(
        !endstream_offsets.is_empty(),
        "newline-before-endstream=n: no stream objects found in output"
    );

    // For every real `endstream` position, verify that an extra newline was NOT
    // inserted when the payload already ends with `\n`.
    //
    // Concretely: if byte[idx-1] == '\n' (the payload's own trailing newline),
    // then byte[idx-2] must NOT be '\n' (no double-newline injected by flag=n).
    // If byte[idx-1] != '\n' (payload did not end with EOL), the check is not
    // applicable for the "omit" direction (an extra '\n' is correctly inserted
    // for parseability in that case).
    let mut found_eol_terminated = false;
    for &start in &endstream_offsets {
        if start >= 2 && output_bytes[start - 1] == b'\n' {
            // The payload ends with '\n'.  With flag=n, no extra '\n' is added,
            // so byte[start-2] must NOT be '\n'.
            assert_ne!(
                output_bytes[start - 2],
                b'\n',
                "newline-before-endstream=n: double \\n before `endstream` at offset {start}; \
                 flag=n must not insert an extra newline when the payload already ends with \\n.\n\
                 bytes[-4..0]: {:?}",
                &output_bytes[start.saturating_sub(4)..start]
            );
            found_eol_terminated = true;
        }
    }

    // Sanity-check: the fixture must have produced at least one stream whose
    // payload ends with '\n' (so the above assertion was actually exercised).
    assert!(
        found_eol_terminated,
        "newline-before-endstream=n: no `endstream` was preceded by '\\n' in the output; \
         the fixture may no longer produce an EOL-terminated payload with --compress-streams=n"
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Combination: normalize-content=y + coalesce-contents + compress-streams=y
//
// Sanity check that multiple flags compose correctly without crashing.
// ---------------------------------------------------------------------------

#[test]
fn combination_normalize_coalesce_compress_succeeds() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("multi-contents-one-page.pdf");
    let output = tmp.path().join("combo.pdf");

    run_rewrite(
        &input,
        &output,
        &[
            "--full-rewrite",
            "--normalize-content=y",
            "--coalesce-contents",
            "--compress-streams=y",
        ],
    );

    // Verify the output is a readable PDF with one page.
    let output_bytes = std::fs::read(&output).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes)).unwrap();
    let pages = page_refs(&mut out_pdf).unwrap();
    assert_eq!(pages.len(), 1, "combination: output must have 1 page");

    // /Contents must be a single reference (coalesce applied).
    let page_obj = out_pdf.resolve(pages[0]).unwrap();
    let page_dict = match page_obj {
        Object::Dictionary(d) => d,
        other => panic!("page must be a Dictionary, got {other:?}"),
    };
    assert!(
        matches!(page_dict.get("Contents"), Some(Object::Reference(_))),
        "combination: /Contents must be a single reference after coalesce"
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

// ---------------------------------------------------------------------------
// Structural endstream-offset helper
// ---------------------------------------------------------------------------

/// Return the byte offsets (in `output_bytes`) of the `endstream` keyword that
/// immediately follows each real stream payload in the PDF.
///
/// The function uses `Pdf::live_object_refs()` + `Pdf::resolve()` to enumerate
/// every indirect stream object, then locates its payload in `output_bytes` via
/// the unique anchor `stream\n<data>` (or `stream\r\n<data>`).  This avoids the
/// false-positive problem caused by accidentally matching `endstream` bytes
/// embedded inside a compressed payload.
///
/// # Panics
/// Panics if an anchor cannot be found (indicates a real fixture mismatch).
fn real_endstream_offsets(
    output_bytes: &[u8],
    out_pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>,
) -> Vec<usize> {
    let refs = out_pdf.live_object_refs();
    let mut offsets = Vec::new();

    for oref in refs {
        let obj = out_pdf.resolve(oref).expect("resolve must succeed");
        let stream = match obj {
            Object::Stream(s) => s,
            _ => continue,
        };

        // Build anchors: the `stream` keyword + EOL + encoded payload bytes
        // appear exactly once in the raw output per stream object.
        let data = &stream.data;
        let mut anchor_lf = b"stream\n".to_vec();
        anchor_lf.extend_from_slice(data);
        let mut anchor_crlf = b"stream\r\n".to_vec();
        anchor_crlf.extend_from_slice(data);

        // Try \n anchor first, fall back to \r\n.
        let anchor = if find_all_occurrences(output_bytes, &anchor_lf).len() == 1 {
            anchor_lf
        } else if find_all_occurrences(output_bytes, &anchor_crlf).len() == 1 {
            anchor_crlf
        } else {
            // Try a shorter anchor using only the stream keyword + first 32 bytes
            // for very short or empty payloads where the data alone may be ambiguous.
            let prefix: &[u8] = &data[..data.len().min(32)];
            let mut short_lf = b"stream\n".to_vec();
            short_lf.extend_from_slice(prefix);
            let short_hits = find_all_occurrences(output_bytes, &short_lf);
            if short_hits.len() == 1 {
                anchor_lf = short_lf;
                anchor_lf
            } else {
                panic!(
                    "real_endstream_offsets: could not uniquely anchor stream payload for {:?} \
                     (data len={}, lf hits={}, crlf hits={}). \
                     Fixture may need updating.",
                    oref,
                    data.len(),
                    find_all_occurrences(output_bytes, &anchor_lf).len(),
                    find_all_occurrences(output_bytes, &anchor_crlf).len(),
                )
            }
        };

        let anchor_pos = find_all_occurrences(output_bytes, &anchor)[0];
        // `after_payload` points to the first byte after the encoded stream data.
        // Between this position and the `endstream` keyword, ISO 32000-1 §7.3.8.1
        // allows an optional EOL (the y/n flag controls whether one is inserted).
        // Scan forward up to 4 bytes to locate the actual `endstream` start.
        let after_payload = anchor_pos + anchor.len();
        let endstream_kw = b"endstream";
        let endstream_off = (after_payload..after_payload + 4)
            .find(|&pos| {
                pos + endstream_kw.len() <= output_bytes.len()
                    && &output_bytes[pos..pos + endstream_kw.len()] == endstream_kw
            })
            .unwrap_or_else(|| {
                panic!(
                    "real_endstream_offsets: `endstream` not found within 4 bytes after payload \
                     for {:?} at offset {}. bytes: {:?}",
                    oref,
                    after_payload,
                    &output_bytes[after_payload..after_payload.min(output_bytes.len() - 1) + 16]
                )
            });
        offsets.push(endstream_off);
    }

    offsets
}

// ---------------------------------------------------------------------------
// Byte-scan helper
// ---------------------------------------------------------------------------

/// Return the start offsets of all occurrences of `needle` in `haystack`.
fn find_all_occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut results = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return results;
    }
    let mut start = 0;
    while start + needle.len() <= haystack.len() {
        if &haystack[start..start + needle.len()] == needle {
            results.push(start);
            start += needle.len();
        } else {
            start += 1;
        }
    }
    results
}
