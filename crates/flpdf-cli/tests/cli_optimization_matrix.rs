//! E2E acceptance tests for the five optimization flags wired in flpdf-9hc.12.7.
//!
//! These exercise the CLI end-to-end (the `flpdf rewrite` binary), so they
//! complement the per-crate unit tests by validating the wired-up flag
//! behavior as a user would invoke it.
//!
//! Tests five matrix cells via `flpdf rewrite`:
//!
//! 1. **normalize-content=y/n** — asserts exact decoded-content byte parity with
//!    qpdf 11.9.0 for normalization and unchanged decoded bytes when disabled.
//! 2. **coalesce-contents** — a page with `/Contents [2 0 R 3 0 R]` becomes a
//!    single `/Contents` reference after rewriting.
//! 3. **remove-unreferenced-resources=auto|yes|no** — on a plain rewrite qpdf
//!    never prunes a page's `/Resources` entries (resource-entry pruning is
//!    page-operation-only, e.g. `--pages`/`--split-pages`), so an unused `/F2`
//!    in `/Resources/Font` is retained under EVERY mode — verified against
//!    `qpdf --static-id --remove-unreferenced-resources=yes`, which still keeps
//!    it. (flpdf-79ef corrected the earlier divergence where the CLI pruned on a
//!    plain rewrite.)
//! 4. **compress-streams=y/n** — decoded stream bytes are preserved; the `/Filter`
//!    key is `/FlateDecode` when `y` and absent when `n`.
//! 5. **newline-before-endstream=y/n** — raw output bytes are inspected for the
//!    presence/absence of `\n` before every `endstream` keyword.
//!
//! # Comparison strategy
//!
//! - Content normalization is compared by exact decoded bytes from real qpdf and
//!   flpdf CLI outputs, including malformed content and warning exits.
//! - Stream encoding is compared by decoded payload bytes.
//! - Resource dictionaries are compared by key sets.
//! - Raw byte patterns (newlines) are searched via `memchr`-style scans.
//!
//! # qpdf-byte divergence documentation
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
//! - **CI** (`CI` env var set): panics if qpdf is absent.
//! - **Local runs**: print a diagnostic and skip the qpdf guard.

#[path = "support/filter_handles.rs"]
mod filter_handles;

use assert_cmd::Command as CargoCommand;
use flpdf::{
    filters::decode_stream_data,
    normalize_content_stream,
    pages::{page_content_bytes, page_refs},
    parse_content_operations, Object, ParseControl, Pdf,
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

fn classic_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(pdf.len());
        pdf.extend_from_slice(object);
    }
    let size = offsets.len() + 1;
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    pdf
}

fn one_page_content_pdf(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        stream.as_slice(),
    ])
}

fn one_page_indirect_contents_array_pdf(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        b"4 0 obj\n[5 0 R]\nendobj\n",
        stream.as_slice(),
    ])
}

fn two_page_indirect_contents_alias_pdf(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("7 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 6 0 R >>\nendobj\n",
        b"5 0 obj\n[7 0 R]\nendobj\n",
        b"6 0 obj\n[7 0 R]\nendobj\n",
        stream.as_slice(),
    ])
}

fn single_page_content(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let page = page_refs(&mut pdf).unwrap()[0];
    page_content_bytes(&mut pdf, page).unwrap()
}

fn all_page_content(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    page_refs(&mut pdf)
        .unwrap()
        .into_iter()
        .map(|page| page_content_bytes(&mut pdf, page).unwrap())
        .collect()
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
    if on_ci {
        panic!(
            "qpdf is required for cli_optimization_matrix tests on CI; \
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

/// Returns `true` when the pinned qpdf 11.9.0 behavioral oracle is unavailable.
///
/// A missing qpdf installation remains a CI configuration error, while another
/// installed release is skipped because it is not the oracle for these
/// byte-for-byte compatibility assertions.
#[must_use]
fn skip_unless_qpdf_11_9() -> bool {
    if skip_if_qpdf_missing() {
        return true;
    }
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .expect("run qpdf --version");
    let version = String::from_utf8(version.stdout).expect("qpdf version must be UTF-8");
    let first_line = version.lines().next();
    if first_line == Some("qpdf version 11.9.0") {
        return false;
    }
    eprintln!(
        "skipping content-normalization parity: expected qpdf version 11.9.0, found {}",
        first_line.unwrap_or("<no version output>")
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
    run_rewrite(&input, &output, &["--normalize-content=y"]);

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
        let expected = normalize_content_stream(&in_content).into_bytes();

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
        let normalized_out = normalize_content_stream(&out_content).into_bytes();
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

#[test]
fn normalize_content_y_matches_qpdf_11_9_decoded_bytes() {
    if skip_unless_qpdf_11_9() {
        return;
    }

    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input.pdf");
    let qpdf_output = tmp.path().join("qpdf.pdf");
    let flpdf_output = tmp.path().join("flpdf.pdf");
    std::fs::write(
        &input,
        one_page_content_pdf(b"% keep\r\nBT  /N#61me (a\rb) Tj\rBI /W 1 ID raw EI Q"),
    )
    .unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--normalize-content=y",
            "--stream-data=uncompress",
            "--object-streams=disable",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf content normalization");
    assert!(
        qpdf.status.success(),
        "qpdf failed:\n{}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    run_rewrite(
        &input,
        &flpdf_output,
        &["--normalize-content=y", "--compress-streams=n"],
    );

    assert_eq!(
        single_page_content(&flpdf_output),
        single_page_content(&qpdf_output)
    );
}

#[test]
fn normalize_content_bad_tokens_match_qpdf_bytes_and_warning_exit() {
    if skip_unless_qpdf_11_9() {
        return;
    }

    let tmp = tempdir().unwrap();
    let input = tmp.path().join("bad-input.pdf");
    let qpdf_output = tmp.path().join("qpdf-bad.pdf");
    let flpdf_output = tmp.path().join("flpdf-bad.pdf");
    std::fs::write(&input, one_page_content_pdf(b"\r<0g")).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--normalize-content=y",
            "--stream-data=uncompress",
            "--object-streams=disable",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf malformed content normalization");
    assert_eq!(qpdf.status.code(), Some(3));

    let mut flpdf = CargoCommand::cargo_bin("flpdf").unwrap();
    let flpdf = flpdf
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf malformed content normalization");
    assert_eq!(flpdf.status.code(), Some(3));

    assert_eq!(
        single_page_content(&flpdf_output),
        single_page_content(&qpdf_output)
    );
}

#[test]
fn normalize_content_indirect_forms_match_qpdf_11_9() {
    if skip_unless_qpdf_11_9() {
        return;
    }

    let tmp = tempdir().unwrap();
    let cases = [
        (
            "indirect-array",
            one_page_indirect_contents_array_pdf(b"\r<0g"),
            vec![b"\n<0g".to_vec()],
        ),
        (
            "terminal-alias",
            two_page_indirect_contents_alias_pdf(b"\r<0g"),
            vec![b"\n<0g".to_vec(), b"\n<0g".to_vec()],
        ),
    ];

    for (name, bytes, expected) in cases {
        let input = tmp.path().join(format!("{name}-input.pdf"));
        let qpdf_output = tmp.path().join(format!("{name}-qpdf.pdf"));
        let flpdf_output = tmp.path().join(format!("{name}-flpdf.pdf"));
        std::fs::write(&input, bytes).unwrap();

        let qpdf = ShellCommand::new("qpdf")
            .args([
                "--normalize-content=y",
                "--stream-data=uncompress",
                "--object-streams=disable",
            ])
            .arg(&input)
            .arg(&qpdf_output)
            .output()
            .expect("run qpdf indirect content normalization");
        assert_eq!(
            qpdf.status.code(),
            Some(3),
            "{name}: qpdf stderr:\n{}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let flpdf = CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
            .arg(&input)
            .arg(&flpdf_output)
            .output()
            .expect("run flpdf indirect content normalization");
        assert_eq!(
            flpdf.status.code(),
            Some(3),
            "{name}: flpdf stderr:\n{}",
            String::from_utf8_lossy(&flpdf.stderr)
        );

        let qpdf_stderr = String::from_utf8(qpdf.stderr).unwrap();
        let mut last_position = 0;
        for warning in [
            "content normalization encountered bad tokens",
            "normalized content ended with a bad token",
            "Resulting stream data may be corrupted but is may still useful",
        ] {
            assert_eq!(
                qpdf_stderr.matches(warning).count(),
                1,
                "{name}: {qpdf_stderr}"
            );
            let position = qpdf_stderr.find(warning).unwrap();
            assert!(
                position >= last_position,
                "{name}: qpdf warning order: {qpdf_stderr}"
            );
            last_position = position;
        }

        assert_eq!(all_page_content(&qpdf_output), expected, "{name}: qpdf");
        assert_eq!(
            all_page_content(&flpdf_output),
            all_page_content(&qpdf_output),
            "{name}: flpdf/qpdf"
        );
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

    run_rewrite(&input, &output, &["--normalize-content=n"]);

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

    run_rewrite(&input, &output, &["--coalesce-contents"]);

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&output).unwrap();

    let mut in_pdf = Pdf::open(Cursor::new(input_bytes)).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes)).unwrap();

    let out_pages = page_refs(&mut out_pdf).unwrap();
    assert_eq!(out_pages.len(), 1, "output must have exactly one page");

    let out_page_ref = out_pages[0];

    // Resolve the output page dict and verify /Contents is NOT an array.
    let out_page_obj = out_pdf.resolve_object(out_page_ref).unwrap();
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

/// Collect operation events (operator + operand values) from a content stream
/// for semantic comparison (order-sensitive, but ignores whitespace
/// differences). The coalesce fixture contains only text operators.
fn collect_content_tokens(bytes: &[u8]) -> Vec<(Vec<Object>, Vec<u8>)> {
    let mut tokens = Vec::new();
    parse_content_operations(bytes, |operands, operator| {
        tokens.push((operands.to_vec(), operator.to_vec()));
        Ok(ParseControl::Continue)
    })
    .expect("content stream must parse successfully");
    tokens
}

// ---------------------------------------------------------------------------
// Cell 3a: remove-unreferenced-resources=auto  (keeps /F1 AND /F2 — qpdf parity)
// Cell 3b: remove-unreferenced-resources=yes   (keeps /F1 AND /F2 — qpdf parity)
// Cell 3c: remove-unreferenced-resources=no    (leaves /F1 and /F2)
//
// Input: unref-resources-one-page.pdf — single page with /Resources/Font{F1,F2};
// content stream uses /F1 (via `Tf`) but NOT /F2.
//
// qpdf does NOT prune /Resources entries on a plain rewrite — neither `auto` nor
// `yes` removes the unreferenced /F2 (verified with `qpdf --static-id
// --remove-unreferenced-resources=yes`). Resource-entry pruning fires only on
// page-copy operations (`--pages`), which flpdf performs in run_page_extraction,
// not on the plain `rewrite` path. So all three modes must retain /F2 here.
// (flpdf-79ef: the CLI previously pruned on a plain rewrite, diverging from qpdf.)
// ---------------------------------------------------------------------------

#[test]
fn remove_unref_resources_auto_keeps_unused_font_like_qpdf() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("unref-resources-one-page.pdf");
    let output = tmp.path().join("unref-auto.pdf");

    run_rewrite(&input, &output, &["--remove-unreferenced-resources=auto"]);

    let font_keys = extract_page_resource_keys(&output, "Font");
    assert!(
        font_keys.contains(&b"F1".to_vec()),
        "remove-unreferenced-resources=auto: /F1 (used) must be retained; font keys: {:?}",
        font_keys
    );
    assert!(
        font_keys.contains(&b"F2".to_vec()),
        "remove-unreferenced-resources=auto: /F2 (unused) must be retained on a plain \
         rewrite (qpdf does not prune resource entries outside page operations); \
         font keys: {:?}",
        font_keys
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

#[test]
fn remove_unref_resources_yes_keeps_unused_font_like_qpdf() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("unref-resources-one-page.pdf");
    let output = tmp.path().join("unref-yes.pdf");

    run_rewrite(&input, &output, &["--remove-unreferenced-resources=yes"]);

    let font_keys = extract_page_resource_keys(&output, "Font");
    assert!(
        font_keys.contains(&b"F1".to_vec()),
        "remove-unreferenced-resources=yes: /F1 (used) must be retained; font keys: {:?}",
        font_keys
    );
    assert!(
        font_keys.contains(&b"F2".to_vec()),
        "remove-unreferenced-resources=yes: /F2 (unused) must be retained on a plain \
         rewrite — qpdf --remove-unreferenced-resources=yes also keeps it; font keys: {:?}",
        font_keys
    );

    if !skip_if_qpdf_missing() {
        assert_qpdf_check(&output);
    }
}

#[test]
fn kept_indirect_length_plain_rewrite_keeps_image_xobject() {
    // flpdf-79ef regression: a page whose content (`BT ET`) references no XObject
    // name still must NOT lose its image. The image XObject (/Im0) carries an
    // indirect /Length whose holder is also referenced by the catalog. A plain
    // CLI rewrite previously dropped /Im0 (empty /Resources) because it ran the
    // page-op-only resource pruning. qpdf --static-id keeps /Im0 (6 objects,
    // /DCTDecode); flpdf must match.
    let tmp = tempdir().unwrap();
    let input = fixture_path("kept-indirect-length.pdf");
    let output = tmp.path().join("kept-indirect-length-out.pdf");

    run_rewrite(&input, &output, &["--static-id"]);

    let xobject_keys = extract_page_resource_keys(&output, "XObject");
    assert!(
        xobject_keys.contains(&b"Im0".to_vec()),
        "plain rewrite must keep the unreferenced image XObject /Im0 (qpdf keeps it); \
         /Resources/XObject keys: {:?}",
        xobject_keys
    );
    let out_bytes = std::fs::read(&output).unwrap();
    assert!(
        out_bytes.windows(9).any(|w| w == b"DCTDecode"),
        "the DCTDecode image stream must survive the rewrite"
    );

    // No assert_qpdf_check here: this fixture's image carries placeholder (non-real)
    // JPEG bytes, so `qpdf --check` warns ("invalid jpeg data") and exits 3 on the
    // ORIGINAL fixture AND on qpdf's own golden output — the warning is inherent to
    // the fixture, not a defect in flpdf's rewrite. The /Im0 + /DCTDecode presence
    // checks above are the meaningful assertions for flpdf-79ef.

    // The issue also cited the canonical rewrite with `--static-id` as a repro; it hits the same
    // run_rewrite path, so pin it explicitly too.
    let output_fr = tmp.path().join("kept-indirect-length-fr.pdf");
    run_rewrite(&input, &output_fr, &["--static-id"]);
    assert!(
        extract_page_resource_keys(&output_fr, "XObject").contains(&b"Im0".to_vec()),
        "canonical rewrite with --static-id must also keep the image XObject /Im0",
    );
}

#[test]
fn remove_unref_resources_no_retains_all_fonts() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("unref-resources-one-page.pdf");
    let output = tmp.path().join("unref-no.pdf");

    run_rewrite(&input, &output, &["--remove-unreferenced-resources=no"]);

    let font_keys = extract_page_resource_keys(&output, "Font");
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

/// Open `path` as a PDF, get page 1's `/Resources/<category>` sub-dict (e.g.
/// `Font` or `XObject`), and return its name keys (as byte vecs). Returns an
/// empty vec if the page has no `/Resources` or no such category sub-dict.
fn extract_page_resource_keys(path: &Path, category: &str) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let pages = page_refs(&mut pdf).unwrap();
    assert!(!pages.is_empty(), "output PDF must have at least one page");

    let page_ref = pages[0];
    let page_obj = pdf.resolve_object(page_ref).unwrap();
    let page_dict = match page_obj {
        Object::Dictionary(d) => d,
        other => panic!("page must be a Dictionary, got {other:?}"),
    };

    // Resolve /Resources (may be inline or a reference).
    let resources_obj = match page_dict.get("Resources").cloned() {
        Some(Object::Reference(r)) => pdf.resolve_object(r).unwrap(),
        Some(obj) => obj,
        None => return vec![],
    };
    let resources_dict = match resources_obj {
        Object::Dictionary(d) => d,
        _ => return vec![],
    };

    // Resolve the requested category sub-dict (e.g. /Font, /XObject).
    let cat_obj = match resources_dict.get(category).cloned() {
        Some(Object::Reference(r)) => pdf.resolve_object(r).unwrap(),
        Some(obj) => obj,
        None => return vec![],
    };
    let cat_dict = match cat_obj {
        Object::Dictionary(d) => d,
        _ => return vec![],
    };

    cat_dict.iter().map(|(k, _)| k.to_vec()).collect()
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

    run_rewrite(&input, &output, &["--compress-streams=y"]);

    let output_bytes = std::fs::read(&output).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes.clone())).unwrap();

    // Verify: every page's content stream is compressed with FlateDecode.
    let pages = page_refs(&mut out_pdf).unwrap();
    assert!(!pages.is_empty());

    for page_ref in &pages {
        let page_obj = out_pdf.resolve_object(*page_ref).unwrap();
        let page_dict = match &page_obj {
            Object::Dictionary(d) => d.clone(),
            other => panic!("page must be a Dictionary, got {other:?}"),
        };

        let contents_ref = match page_dict.get("Contents").cloned() {
            Some(Object::Reference(r)) => r,
            Some(other) => panic!("expected /Contents to be a reference, got {other:?}"),
            None => continue, // empty page
        };

        let content_stream = match out_pdf.resolve_object(contents_ref).unwrap() {
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
        let decoded = decode_stream_data(
            &filter_handles::dictionary(&content_stream.dict),
            &content_stream.data,
        )
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
// qpdf's `--compress-streams=n` only disables newly generated compression; it
// does not raise the decode level. Existing filter chains therefore remain
// intact. Use `--stream-data=uncompress` when all decodable filters must be
// removed. This cell asserts qpdf's actual `--compress-streams=n` behaviour.
// ---------------------------------------------------------------------------

#[test]
fn compress_streams_n_preserves_existing_filters_and_roundtrips() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("compress-n.pdf");

    run_rewrite(&input, &output, &["--compress-streams=n"]);

    let output_bytes = std::fs::read(&output).unwrap();
    let mut out_pdf = Pdf::open(Cursor::new(output_bytes.clone())).unwrap();

    let pages = page_refs(&mut out_pdf).unwrap();
    assert!(!pages.is_empty());

    for page_ref in &pages {
        let page_obj = out_pdf.resolve_object(*page_ref).unwrap();
        let page_dict = match &page_obj {
            Object::Dictionary(d) => d.clone(),
            other => panic!("page must be a Dictionary, got {other:?}"),
        };

        let contents_ref = match page_dict.get("Contents").cloned() {
            Some(Object::Reference(r)) => r,
            Some(other) => panic!("expected /Contents reference, got {other:?}"),
            None => continue,
        };

        let content_stream = match out_pdf.resolve_object(contents_ref).unwrap() {
            Object::Stream(s) => s,
            other => panic!("expected a Stream, got {other:?}"),
        };

        let filter = content_stream.dict.get("Filter");
        assert!(
            matches!(filter, Some(Object::Array(filters)) if filters.as_slice() == [
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
            "compress-streams=n: qpdf preserves the existing filter chain; got {filter:?}"
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

    run_rewrite(&input, &output, &["--newline-before-endstream=y"]);

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
// Cell 5b: newline-before-endstream=n (qpdf boolean alias)
//
// qpdf 11.9.0 treats the value form `--newline-before-endstream=n` as the
// presence of its boolean option, so it inserts an unconditional framing LF,
// including when the payload already ends with LF.
//
// To make this test discriminating — i.e., able to detect a regression where
// `n` is incorrectly treated as qpdf's default — we use
// `--stream-data=uncompress` so that the
// content-stream payload is written as raw decoded bytes.  The decoded payload
// for `one-page.pdf` ends with `\n` (the last line of the content stream),
// which means:
//
//   flag=n/y: exactly one extra `\n` is inserted unconditionally → byte before
//             `endstream` is the inserted `\n` and the byte before THAT is
//             the payload's own `\n` (two consecutive `\n` bytes).
//
// Assertions:
//   (n-test) at least one `endstream` is found.
//   (n-test) at least one EOL-terminated payload has two consecutive `\n`
//            bytes before `endstream`, proving the option is unconditional.
//   (y-contrast, in the y-test above) every `endstream` is preceded by `\n`.
// ---------------------------------------------------------------------------

#[test]
fn newline_before_endstream_n_matches_qpdf_boolean_flag() {
    let tmp = tempdir().unwrap();
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("newline-n.pdf");

    // Use --stream-data=uncompress so the payload is written as raw decoded bytes.
    // The decoded content stream for one-page.pdf ends with b'\n', so qpdf's
    // boolean flag must insert an additional newline before endstream.
    run_rewrite(
        &input,
        &output,
        &["--newline-before-endstream=n", "--stream-data=uncompress"],
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

    // `n` now maps to the identical `NewlineBeforeEndstream::Yes` code path
    // as `y`, so every real `endstream` must be preceded by `\n` — the same
    // universal check the `y`-test above uses.
    let mut violations = 0usize;
    for &start in &endstream_offsets {
        if start == 0 || output_bytes[start - 1] != b'\n' {
            violations += 1;
            eprintln!(
                "newline-before-endstream=n violation at offset {start}: \
                 preceding byte is 0x{:02x}",
                output_bytes[start - 1]
            );
        }
    }

    assert_eq!(
        violations, 0,
        "newline-before-endstream=n: {violations} `endstream` keyword(s) not preceded by \\n"
    );

    // At least one real `endstream` must show the double-newline shape from an
    // EOL-terminated payload plus qpdf's unconditional framing LF, proving the
    // flag is unconditional rather than "insert only if missing".
    let mut found_double_newline = false;
    for &start in &endstream_offsets {
        if start >= 2 && output_bytes[start - 1] == b'\n' && output_bytes[start - 2] == b'\n' {
            found_double_newline = true;
        }
    }

    // Sanity-check: the fixture must have produced at least one EOL-terminated
    // stream (so the unconditional qpdf flag is actually exercised).
    assert!(
        found_double_newline,
        "newline-before-endstream=n: no `endstream` was preceded by '\\n' in the output; \
         no double LF was observed with --stream-data=uncompress"
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
    let page_obj = out_pdf.resolve_object(pages[0]).unwrap();
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
        let obj = out_pdf.resolve_object(oref).expect("resolve must succeed");
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
