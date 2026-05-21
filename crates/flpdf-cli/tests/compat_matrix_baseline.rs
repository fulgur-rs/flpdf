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

/// Elide every `/ID [<..><..>]` array from `bytes`, replacing it with a
/// stable placeholder so byte-level comparisons and fingerprints ignore the
/// trailer file identifier.
///
/// The default (no-flag) `/ID` strategy emits a fresh random identifier on
/// every save (ISO 32000-1 §14.4), which would otherwise make the `byte-equal`
/// verdict and the `flpdf-sha` fingerprint non-deterministic for the `Plain`
/// and `Linearize` rows.  Eliding `/ID` keeps the matrix tracking every other
/// byte of flpdf's output.  The `StaticId` rows are unaffected because their
/// `/ID` is the fixed π constant on both sides; eliding it on both sides
/// preserves the byte-equal verdict.
fn elide_id_arrays(bytes: &[u8]) -> Vec<u8> {
    const KEY: &[u8] = b"/ID";
    const PLACEHOLDER: &[u8] = b"/ID <ELIDED>";
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(KEY) {
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
                output_bytes: Some(elide_id_arrays(
                    run_outputs.qpdf.output_bytes.as_ref().unwrap(),
                )),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(elide_id_arrays(
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
