//! Per-flag golden matrix baseline test.
//!
//! For each (fixture × flag) tuple in the curated corpus:
//! - Runs `flpdf rewrite [<flag>] <fixture> <tmp>` (plain = no flag)
//! - Reads the golden reference from `tests/golden/references/<stem>/<flag>.pdf`
//! - Evaluates the output against three comparators:
//!   `ByteComparator`, `QpdfJsonComparator`, `StructuralComparator`
//! - Records each verdict as `match`, `diverge`, or `skip`
//!
//! Results are rendered as a Markdown table and either:
//! - compared byte-for-byte against `tests/golden/compat-matrix.md`, or
//! - written there when the environment variable `BLESS=1` is set.
//!
//! Run initial generation with:
//!   `BLESS=1 cargo test --test compat_matrix_baseline`
//!
//! The entire test is skipped when `qpdf` is not available on PATH (the
//! `qpdf-json` comparator requires it, and without it the matrix would be
//! meaningless).

#[allow(dead_code, unused_imports)]
#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};

use assert_cmd::Command as CargoCommand;
use support::{
    is_qpdf_available, ByteComparator, Comparator, ComparatorResult, QpdfJsonComparator,
    RunOutputs, StructuralComparator, ToolOutput,
};

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn golden_references_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .to_path_buf()
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/compat-matrix.md")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Flag descriptor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Flag {
    Plain,
    StaticId,
    Linearize,
    /// Exercises `--stream-data=uncompress`, which strips all stream filters
    /// and writes raw decoded bytes.  This column guards sub-6 regressions.
    StreamDataUncompress,
}

impl Flag {
    fn name(self) -> &'static str {
        match self {
            Flag::Plain => "plain",
            Flag::StaticId => "static-id",
            Flag::Linearize => "linearize",
            Flag::StreamDataUncompress => "stream-data-uncompress",
        }
    }

    /// Extra args to pass to `flpdf rewrite`.
    fn flpdf_args(self) -> &'static [&'static str] {
        match self {
            Flag::Plain => &[],
            Flag::StaticId => &["--static-id"],
            Flag::Linearize => &["--linearize"],
            Flag::StreamDataUncompress => &["--stream-data=uncompress"],
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix descriptor — 12 (fixture, flag) tuples
// ---------------------------------------------------------------------------

struct Entry {
    fixture: &'static str,
    flag: Flag,
}

const MATRIX: &[Entry] = &[
    Entry {
        fixture: "one-page.pdf",
        flag: Flag::Plain,
    },
    Entry {
        fixture: "one-page.pdf",
        flag: Flag::StaticId,
    },
    Entry {
        fixture: "one-page.pdf",
        flag: Flag::Linearize,
    },
    Entry {
        fixture: "two-page.pdf",
        flag: Flag::Plain,
    },
    Entry {
        fixture: "two-page.pdf",
        flag: Flag::StaticId,
    },
    Entry {
        fixture: "two-page.pdf",
        flag: Flag::Linearize,
    },
    Entry {
        fixture: "three-page.pdf",
        flag: Flag::Plain,
    },
    Entry {
        fixture: "three-page.pdf",
        flag: Flag::StaticId,
    },
    Entry {
        fixture: "three-page.pdf",
        flag: Flag::Linearize,
    },
    Entry {
        fixture: "linearized-one-page.pdf",
        flag: Flag::Plain,
    },
    Entry {
        fixture: "attachment-two-page.pdf",
        flag: Flag::Plain,
    },
    Entry {
        fixture: "attachment-two-page.pdf",
        flag: Flag::StaticId,
    },
    // StreamDataUncompress column — guards the sub-6 --stream-data=uncompress path
    Entry {
        fixture: "one-page.pdf",
        flag: Flag::StreamDataUncompress,
    },
    Entry {
        fixture: "two-page.pdf",
        flag: Flag::StreamDataUncompress,
    },
    Entry {
        fixture: "three-page.pdf",
        flag: Flag::StreamDataUncompress,
    },
];

// ---------------------------------------------------------------------------
// Per-row result
// ---------------------------------------------------------------------------

struct MatrixRow {
    fixture: &'static str,
    flag: &'static str,
    flpdf_sha: String,
    byte_equal: &'static str,
    qpdf_json: &'static str,
    structural: &'static str,
}

fn verdict_str(result: &ComparatorResult) -> &'static str {
    match result {
        ComparatorResult::Match => "match",
        ComparatorResult::Diverge { .. } => "diverge",
        ComparatorResult::Skipped { .. } => "skip",
    }
}

/// 64-bit FNV-1a hex digest of `bytes`. Used as a stable fingerprint of
/// flpdf's output so the baseline detects drift even when comparator
/// verdicts stay the same (e.g. flpdf changes bytes but the byte/json/
/// structural verdicts remain `diverge`). FNV-1a is not cryptographic,
/// but for our small corpus its collision probability is negligible and
/// the algorithm is stable across Rust and stdlib versions, unlike the
/// std DefaultHasher.
fn fnv1a_64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Elide every `/ID [<..><..>]` array that lives inside a trailer (classic
/// xref) or cross-reference-stream dictionary, replacing it with a stable
/// placeholder so byte-level comparisons and fingerprints ignore the file
/// identifier.
///
/// The default (no-flag) `/ID` strategy emits a fresh random identifier on
/// every save (ISO 32000-1 §14.4), which would otherwise make the `byte-equal`
/// verdict and the `flpdf-sha` fingerprint non-deterministic for the `Plain`
/// and `Linearize` rows.  Eliding the trailer `/ID` keeps the matrix tracking
/// every other byte of flpdf's output.  The `StaticId` rows are unaffected
/// because their `/ID` is the fixed π constant on both sides; eliding it on
/// both sides preserves the byte-equal verdict.
///
/// Scope is intentionally restricted to trailer-equivalent dictionaries.  A
/// whole-file `/ID [...]` scan would silently mask drift if a future corpus
/// fixture exposed a `/ID` array in some other place (e.g. a custom dict,
/// structure tree, or a literal-string body that happens to contain the
/// byte sequence).  Linearized PDFs carry two trailer dicts that share the
/// same file identifier; both are picked up by the `trailer` keyword scan
/// and elided in lock-step.
fn elide_trailer_id_arrays(bytes: &[u8]) -> Vec<u8> {
    let ranges = trailer_dict_ranges(bytes);
    if ranges.is_empty() {
        return bytes.to_vec();
    }

    // Build a bit-mask of "this byte is inside a trailer-equivalent dict".
    // O(n) memory is fine for test fixtures (a few KiB to a few MiB) and lets
    // the elide loop stay branch-light.
    let mut in_trailer = vec![false; bytes.len()];
    for (open, close) in &ranges {
        for slot in &mut in_trailer[*open..*close] {
            *slot = true;
        }
    }

    const KEY: &[u8] = b"/ID";
    const PLACEHOLDER: &[u8] = b"/ID <ELIDED>";
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if in_trailer[i] && bytes[i..].starts_with(KEY) {
            // Skip optional whitespace, then require an opening '['.
            let mut j = i + KEY.len();
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'[' {
                // Consume up to and including the matching ']'.
                if let Some(close) = bytes[j..].iter().position(|&b| b == b']') {
                    out.extend_from_slice(PLACEHOLDER);
                    i = j + close + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Locate every trailer-equivalent dictionary in `bytes` and return
/// `(open, close)` pairs covering each `<<...>>` (inclusive of both
/// delimiters).
///
/// PDF (ISO 32000-1) places the file `/ID` array inside the trailer
/// dictionary or the cross-reference stream's dictionary.  This helper
/// finds:
///
/// - every line-anchored `trailer` keyword → the next `<<...>>` after it
///   (classic xref tables; linearized PDFs carry two such trailers);
/// - every object dictionary that declares `/Type /XRef` → its `<<...>>`
///   (modern xref-stream PDFs).
///
/// Returned ranges may overlap in pathological hybrid documents; the
/// caller's bit-mask deduplicates by construction.  The line-anchored
/// keyword scan rejects literal `trailer` byte sequences that may appear
/// inside stream payloads.
fn trailer_dict_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    // Classic xref: line-anchored `trailer` followed by `<<...>>`.
    let mut from = 0usize;
    while from < bytes.len() {
        let Some(kw) = find_line_keyword_from(bytes, b"trailer", from) else {
            break;
        };
        let after = kw + b"trailer".len();
        if let Some(rel) = find_subslice(&bytes[after..], b"<<") {
            let dict_open = after + rel;
            if let Some(dict_close_lt) = find_matching_dict_close(bytes, dict_open) {
                let dict_close = dict_close_lt + 2;
                ranges.push((dict_open, dict_close));
                from = dict_close;
                continue;
            }
        }
        from = after;
    }

    // Xref streams: object dicts declaring `/Type /XRef`.
    let mut search = 0usize;
    while let Some(rel) = find_subslice(&bytes[search..], b"<<") {
        let dict_open = search + rel;
        let Some(dict_close_lt) = find_matching_dict_close(bytes, dict_open) else {
            break;
        };
        let dict_close = dict_close_lt + 2;
        let dict = &bytes[dict_open..dict_close];
        if dict_declares_type_xref(dict) && !ranges.contains(&(dict_open, dict_close)) {
            ranges.push((dict_open, dict_close));
        }
        search = dict_close;
    }

    ranges
}

/// Whether `dict` (the bytes from `<<` to `>>` inclusive) declares
/// `/Type /XRef` at any level.  We accept any whitespace between the
/// `/Type` key and the `/XRef` value, and reject `/TypeX` / `/XRefY`
/// false matches via simple token-boundary checks.
fn dict_declares_type_xref(dict: &[u8]) -> bool {
    const TYPE_KEY: &[u8] = b"/Type";
    const XREF_VAL: &[u8] = b"/XRef";
    let mut i = 0usize;
    while i + TYPE_KEY.len() <= dict.len() {
        if &dict[i..i + TYPE_KEY.len()] != TYPE_KEY {
            i += 1;
            continue;
        }
        let mut j = i + TYPE_KEY.len();
        // Reject `/TypeFoo`: next byte must be whitespace, `/`, or end.
        if j < dict.len() && !matches!(dict[j], b' ' | b'\t' | b'\r' | b'\n' | b'/') {
            i += 1;
            continue;
        }
        while j < dict.len() && matches!(dict[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if dict[j..].starts_with(XREF_VAL) {
            let after = j + XREF_VAL.len();
            let ok_end = after >= dict.len()
                || matches!(
                    dict[after],
                    b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'[' | b'('
                );
            if ok_end {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Forward-search for the leftmost occurrence of `kw` at-or-after `from`
/// that is anchored to a line start (preceded by `\n`, `\r`, or BOF) and
/// terminated by a token boundary (whitespace, `<`, or EOF).  Line-anchoring
/// keeps the scan from matching `trailer` / `endobj` / etc. that may appear
/// inside stream payloads.
fn find_line_keyword_from(bytes: &[u8], kw: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + kw.len() <= bytes.len() {
        if &bytes[i..i + kw.len()] == kw {
            let line_anchored = i == 0 || matches!(bytes[i - 1], b'\n' | b'\r');
            let trailing_ok = i + kw.len() >= bytes.len()
                || matches!(bytes[i + kw.len()], b' ' | b'\t' | b'\r' | b'\n' | b'<');
            if line_anchored && trailing_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Forward substring search.  Naive O(n*m) is fine for the test corpus.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Locate the `>>` that closes the dict opening at `open` (`bytes[open..]`
/// must start with `<<`).  Returns the offset of the first `>` of the
/// closing delimiter.  Handles nested dicts, hex strings `<...>`, literal
/// strings `(...)` with backslash escapes, and `%` end-of-line comments —
/// mirroring `flpdf::qdf_fix::find_matching_dict_close`, which is private
/// to its crate.
fn find_matching_dict_close(input: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < input.len() {
        match input[i] {
            b'%' => {
                while i < input.len() && input[i] != b'\n' && input[i] != b'\r' {
                    i += 1;
                }
            }
            b'<' if input.get(i + 1) == Some(&b'<') => {
                depth += 1;
                i += 2;
            }
            b'>' if input.get(i + 1) == Some(&b'>') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
                i += 2;
            }
            b'<' => {
                i += 1;
                while i < input.len() && input[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            b'(' => {
                i += 1;
                let mut sdepth = 1usize;
                while i < input.len() && sdepth > 0 {
                    match input[i] {
                        b'\\' => i += 1, // skip escaped byte
                        b'(' => sdepth += 1,
                        b')' => sdepth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Markdown renderer
// ---------------------------------------------------------------------------

fn render_markdown(rows: &[MatrixRow]) -> String {
    let mut out = String::new();
    out.push_str("# Golden Compatibility Matrix\n\n");
    out.push_str("Matrix of where flpdf and qpdf agree under at least one comparator strategy\n");
    out.push_str("on the curated corpus. Generated by the compat_matrix_baseline integration\n");
    out.push_str("test.\n\n");
    out.push_str("## Comparator strategies\n\n");
    out.push_str("- **byte-equal**: byte-for-byte identical output\n");
    out.push_str(
        "- **qpdf-json**: qpdf --json=2 representations match (requires qpdf at runtime)\n",
    );
    out.push_str("- **structural**: flpdf-parsed object graphs match (encoding-only stream\n");
    out.push_str("  dict keys excluded when decoded content matches)\n\n");
    out.push_str("The **flpdf-sha** column is a stable 64-bit FNV-1a fingerprint of flpdf's\n");
    out.push_str("output bytes with the trailer `/ID` array elided (the default `/ID` is\n");
    out.push_str("randomized per ISO 32000-1 §14.4, so fingerprinting it verbatim would be\n");
    out.push_str("non-deterministic). It changes whenever flpdf's non-`/ID` output changes,\n");
    out.push_str("so the baseline detects byte-level drift even when every comparator\n");
    out.push_str("verdict stays at `diverge`. The `byte-equal` column likewise compares\n");
    out.push_str("with `/ID` elided.\n\n");
    out.push_str("## Review cadence\n\n");
    out.push_str("Re-bless this file when:\n");
    out.push_str("- qpdf binary is upgraded (qpdf-json comparator may shift)\n");
    out.push_str("- flpdf changes byte / structural output for any covered fixture × flag\n");
    out.push_str("- New fixtures or flags are added to the corpus\n\n");
    out.push_str("Update via: `BLESS=1 cargo test --test compat_matrix_baseline`\n\n");
    out.push_str("## Matrix\n\n");
    out.push_str("| fixture | flag | flpdf-sha | byte-equal | qpdf-json | structural |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.fixture, row.flag, row.flpdf_sha, row.byte_equal, row.qpdf_json, row.structural
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// BLESS helper
// ---------------------------------------------------------------------------

fn check_or_bless(actual: &str) {
    let path = baseline_path();
    if std::env::var("BLESS").is_ok() {
        std::fs::write(&path, actual).expect("write baseline");
        return;
    }
    // Normalize CRLF → LF on read. Git on Windows defaults to
    // autocrlf=true and converts LF to CRLF at checkout, so the file
    // on disk ends up with \r\n even though the committed bytes are
    // LF. render_markdown uses bare \n, so the comparison would
    // otherwise fail on Windows runners.
    let expected = std::fs::read_to_string(&path)
        .expect("baseline missing; run BLESS=1 to create it")
        .replace("\r\n", "\n");
    if actual != expected {
        panic!(
            "baseline drift\n--- expected ---\n{expected}\n--- actual ---\n{actual}\nRun BLESS=1 to update."
        );
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn compat_matrix_baseline() {
    // Without qpdf the qpdf-json comparator skips and the matrix becomes
    // meaningless. Locally we accept that and skip the whole test; on
    // the Linux CI runner (where the workflow installs qpdf) a missing
    // qpdf would silently bypass this gate, so fail loudly. The Windows
    // CI runner intentionally does not install qpdf — `.github/workflows/
    // ci.yml` documents this — so the loud-fail guard must not fire
    // there. CI=true is set by GitHub Actions and most other CI runners;
    // cfg!(target_os = "linux") is what gates the strict assertion.
    if !is_qpdf_available() {
        let in_ci = std::env::var("CI").is_ok();
        let is_linux = cfg!(target_os = "linux");
        if in_ci && is_linux {
            panic!(
                "compat_matrix_baseline is running in Linux CI but qpdf is not on PATH. \
                 Install qpdf in the workflow before this test so the matrix \
                 actually exercises the qpdf-json comparator."
            );
        }
        eprintln!("qpdf not available — skipping compat_matrix_baseline");
        return;
    }

    let byte_cmp = ByteComparator;
    let qpdf_cmp = QpdfJsonComparator;
    let struct_cmp = StructuralComparator;

    let mut rows: Vec<MatrixRow> = Vec::new();

    for entry in MATRIX {
        let fixture_path = fixtures_dir().join(entry.fixture);
        let stem = entry.fixture.strip_suffix(".pdf").unwrap_or(entry.fixture);
        let golden_path = golden_references_dir()
            .join(stem)
            .join(format!("{}.pdf", entry.flag.name()));

        // Read golden bytes — panics if missing (all goldens must exist).
        let golden_bytes = std::fs::read(&golden_path)
            .unwrap_or_else(|e| panic!("failed to read golden {}: {e}", golden_path.display()));

        // Run flpdf rewrite [<flag>] <fixture> <tmp>.
        let tmp_dir = tempfile::tempdir().expect("failed to create tempdir");
        let out_path = tmp_dir.path().join("flpdf-out.pdf");

        let mut cmd = CargoCommand::cargo_bin("flpdf").expect("flpdf binary must exist");
        cmd.arg("rewrite");
        for arg in entry.flag.flpdf_args() {
            cmd.arg(arg);
        }
        cmd.arg(fixture_path.to_str().unwrap());
        cmd.arg(out_path.to_str().unwrap());

        let result = cmd.output().expect("failed to spawn flpdf");
        if !result.status.success() {
            panic!(
                "flpdf rewrite failed for fixture={} flag={}: exit={:?}\nstderr: {}",
                entry.fixture,
                entry.flag.name(),
                result.status.code(),
                String::from_utf8_lossy(&result.stderr)
            );
        }

        let flpdf_bytes =
            std::fs::read(&out_path).expect("flpdf output file missing after success");

        // Build RunOutputs: golden bytes on the qpdf side, flpdf output on
        // the flpdf side.
        let run_outputs = RunOutputs {
            qpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(golden_bytes),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(flpdf_bytes),
            },
        };

        // The default /ID is randomized per ISO 32000-1 §14.4, so byte-level
        // comparison and the fingerprint must elide it on both sides.  The
        // qpdf-json and structural comparators parse the raw PDF, so they keep
        // the original (un-elided) bytes.
        let id_normalized = RunOutputs {
            qpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(elide_trailer_id_arrays(
                    run_outputs.qpdf.output_bytes.as_ref().unwrap(),
                )),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(elide_trailer_id_arrays(
                    run_outputs.flpdf.output_bytes.as_ref().unwrap(),
                )),
            },
        };

        let byte_result = byte_cmp.compare(&id_normalized);
        let qpdf_result = qpdf_cmp.compare(&run_outputs);
        let struct_result = struct_cmp.compare(&run_outputs);

        let flpdf_sha = fnv1a_64_hex(
            id_normalized
                .flpdf
                .output_bytes
                .as_ref()
                .expect("flpdf output bytes must be set"),
        );

        rows.push(MatrixRow {
            fixture: entry.fixture,
            flag: entry.flag.name(),
            flpdf_sha,
            byte_equal: verdict_str(&byte_result),
            qpdf_json: verdict_str(&qpdf_result),
            structural: verdict_str(&struct_result),
        });
    }

    let markdown = render_markdown(&rows);
    println!("{markdown}");
    check_or_bless(&markdown);
}

// ---------------------------------------------------------------------------
// Unit tests for trailer/xref-stream scoping
// ---------------------------------------------------------------------------
//
// These guard the regression flpdf-d6j was filed for: prior versions of the
// /ID-eliding helper scanned the entire file byte stream and would silently
// strip any `/ID [...]` array — including one that future fixtures might
// legitimately carry in a non-trailer dict.  The scoping must keep eliding
// the trailer file identifier (so byte-equal and the FNV fingerprint stay
// deterministic across runs) while leaving any other `/ID [...]` alone.

#[test]
fn elide_trailer_id_arrays_classic_trailer_replaced() {
    let pdf: &[u8] = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n\
        xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
        trailer\n<< /Size 2 /Root 1 0 R /ID [<aa><bb>] >>\nstartxref\n55\n%%EOF\n";
    let out = elide_trailer_id_arrays(pdf);
    assert!(
        contains_subslice(&out, b"/ID <ELIDED>"),
        "trailer /ID must be replaced with the elision placeholder"
    );
    assert!(
        !contains_subslice(&out, b"<aa><bb>"),
        "original /ID hex strings must not survive"
    );
}

#[test]
fn elide_trailer_id_arrays_linearized_two_trailers_both_replaced() {
    // Two `trailer\n<<...>>` blocks share the same /ID, as qpdf emits for
    // linearized PDFs.  Both must be elided in lock-step so the
    // fingerprint stays deterministic between runs.
    let pdf: &[u8] = b"%PDF-1.4\ntrailer\n<< /Size 5 /ID [<aa><bb>] /Prev 999 >>\n\
        startxref\n0\n%%EOF\n\
        1 0 obj << /Foo /Bar >> endobj\nxref\n0 1\n0000000000 65535 f \n\
        trailer\n<< /Size 1 /ID [<aa><bb>] >>\nstartxref\n42\n%%EOF\n";
    let out = elide_trailer_id_arrays(pdf);
    assert_eq!(
        count_subslice(&out, b"/ID <ELIDED>"),
        2,
        "both trailer /ID arrays must be elided"
    );
    assert!(
        !contains_subslice(&out, b"<aa><bb>"),
        "no original /ID hex string may remain"
    );
}

#[test]
fn elide_trailer_id_arrays_non_trailer_id_preserved() {
    // `/ID [<aa>]` appears inside an object dict that is neither a trailer
    // nor an xref stream.  The scoped elision must leave it alone — that
    // is exactly the latent risk flpdf-d6j was filed for.
    let pdf: &[u8] = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /CustomThing /ID [<bodyid01>] /Other 1 >>\nendobj\n\
        xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
        trailer\n<< /Size 2 /Root 1 0 R /ID [<trailerid>] >>\nstartxref\n55\n%%EOF\n";
    let out = elide_trailer_id_arrays(pdf);
    assert!(
        contains_subslice(&out, b"/ID [<bodyid01>]"),
        "non-trailer /ID array must be preserved, but output was: {}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        !contains_subslice(&out, b"<trailerid>"),
        "trailer /ID array must still be elided"
    );
    assert_eq!(count_subslice(&out, b"/ID <ELIDED>"), 1);
}

#[test]
fn elide_trailer_id_arrays_xref_stream_dict_replaced() {
    // Synthetic xref-stream PDF: the object dict declaring `/Type /XRef`
    // carries the file identifier.  No `trailer` keyword is present.
    let pdf: &[u8] = b"%PDF-1.5\n\
        1 0 obj\n<< /Type /Catalog >>\nendobj\n\
        2 0 obj\n<< /Type /XRef /Size 3 /W [1 2 1] /Root 1 0 R \
        /ID [<aabb><ccdd>] /Length 12 >>\nstream\n\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\nendstream\nendobj\n\
        startxref\n40\n%%EOF\n";
    let out = elide_trailer_id_arrays(pdf);
    assert!(
        contains_subslice(&out, b"/ID <ELIDED>"),
        "xref-stream /ID must be elided"
    );
    assert!(
        !contains_subslice(&out, b"<aabb>"),
        "original xref-stream /ID hex strings must not survive"
    );
}

#[test]
fn elide_trailer_id_arrays_trailer_with_nested_encrypt_dict() {
    // Encrypted PDFs put a nested `<< ... >>` /Encrypt dict inside the
    // trailer; the `>>` of that inner dict must not be mistaken for the
    // trailer's close (the dict-close matcher counts depth).
    let pdf: &[u8] = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n\
        xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
        trailer\n<< /Size 2 /Root 1 0 R \
        /Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 \
        /O <00010203> /U <04050607> >> \
        /ID [<trailerid01><trailerid02>] >>\nstartxref\n55\n%%EOF\n";
    let out = elide_trailer_id_arrays(pdf);
    assert!(
        contains_subslice(&out, b"/ID <ELIDED>"),
        "trailer /ID inside an Encrypt-bearing dict must still be elided, output: {}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        contains_subslice(&out, b"/Encrypt <<"),
        "the nested /Encrypt dict must survive intact"
    );
    assert!(
        !contains_subslice(&out, b"<trailerid01>"),
        "original /ID hex strings must not survive"
    );
}

#[test]
fn elide_trailer_id_arrays_id_prefix_name_collision_preserved() {
    // `/IDLine` is a hypothetical custom name in some body dict.  The
    // current code already required whitespace or `[` after `/ID` so this
    // would not falsely match, but the test pins that contract.
    let pdf: &[u8] = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /CustomThing /IDLine 5 >>\nendobj\n\
        xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
        trailer\n<< /Size 2 /Root 1 0 R /ID [<trailerid>] >>\nstartxref\n55\n%%EOF\n";
    let out = elide_trailer_id_arrays(pdf);
    assert!(
        contains_subslice(&out, b"/IDLine 5"),
        "name-prefix collision must not be elided, output: {}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        !contains_subslice(&out, b"<trailerid>"),
        "trailer /ID array must still be elided"
    );
}

#[test]
fn trailer_dict_ranges_rejects_keyword_inside_stream() {
    // A literal `trailer` byte sequence inside a stream payload (not at a
    // line start) must not be picked up as a trailer keyword.  The real
    // trailer at EOF is the only valid range.
    let pdf: &[u8] = b"%PDF-1.4\n1 0 obj << /Length 20 >> stream\n\
        garbage trailer << bait >>\nendstream\nendobj\n\
        xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
        trailer\n<< /Size 2 /Root 1 0 R /ID [<trailerid>] >>\nstartxref\n55\n%%EOF\n";
    let ranges = trailer_dict_ranges(pdf);
    // Exactly one classic-trailer range, and no xref-stream range (the
    // stream-bait dict has no `/Type /XRef`).
    assert_eq!(
        ranges.len(),
        1,
        "expected one trailer-equivalent dict range, got {ranges:?}"
    );
    let (open, close) = ranges[0];
    let dict = &pdf[open..close];
    assert!(
        dict.starts_with(b"<<") && dict.ends_with(b">>"),
        "range must enclose `<<...>>`, got: {}",
        String::from_utf8_lossy(dict)
    );
    assert!(
        contains_subslice(dict, b"/ID [<trailerid>]"),
        "range must cover the real trailer /ID"
    );
}

fn contains_subslice(hay: &[u8], needle: &[u8]) -> bool {
    find_subslice(hay, needle).is_some()
}

fn count_subslice(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}
