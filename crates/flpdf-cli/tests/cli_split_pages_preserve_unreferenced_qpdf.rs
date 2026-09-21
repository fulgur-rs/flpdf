//! qpdf 11.9.0 parity for `--pages` + `--preserve-unreferenced` + `--split-pages`.
//!
//! `--preserve-unreferenced` is a writer option
//! (`libqpdf/QPDFWriter.cc:2907-2913`), and `QPDFJob::handlePageSpecs` keeps
//! the primary `QPDF` as the output document, so the writer finds the
//! primary's page-tree-unreferenced objects still in place.
//! `QPDFJob::doSplitPages` (`libqpdf/QPDFJob.cc:2940-3027`) instead builds
//! every chunk from its own `QPDF`/`emptyPDF()` populated only by
//! `addPage(page, false)`, so the writer option it re-applies per chunk
//! (`QPDFJob.cc:3021` reaching `QPDFJob.cc:2856`) has no unreferenced object
//! to enqueue: real qpdf's split output carries none.
//!
//! flpdf's multi-source route merges into a fresh target rather than mutating
//! the primary, so it has to copy that object graph across to reproduce the
//! unsplit case -- and must skip the copy when a split is going to discard it,
//! without moving a single output byte. Both CLI surfaces are pinned here: the
//! top-level qpdf-shaped route, where `QPDFJob` owns the split, and the
//! `rewrite` subcommand, where the CLI owns it.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const COMPAT: &str = "../../tests/fixtures/compat";

/// The primary's only unreferenced objects carry this label, so its presence
/// in a normalized output is exactly "the primary orphan graph survived".
const ORPHAN_MARKER: &[u8] = b"unreachable root";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT)
        .join(name)
}

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        });
    if version
        .as_deref()
        .is_some_and(|stdout| stdout.lines().next() == Some("qpdf version 11.9.0"))
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for split-pages preserve-unreferenced parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        // `--static-id` warns on every run; the warning is not part of what
        // this parity test pins.
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed ({:?}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Read `<stem>-1.pdf`, `<stem>-2.pdf`, ... until the next one is missing.
///
/// qpdf names split output from the template's `.pdf` suffix
/// (`libqpdf/QPDFJob.cc:2950-2959`), zero-padded to the page-count width, so a
/// run of up to nine pages yields plain `-1`, `-2`, ... names.
fn split_chunks(directory: &Path, stem: &str) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    for index in 1..=9 {
        let path = directory.join(format!("{stem}-{index}.pdf"));
        match std::fs::read(&path) {
            Ok(bytes) => chunks.push(bytes),
            Err(_) => break,
        }
    }
    assert!(
        !chunks.is_empty(),
        "no split chunk was written for {stem} in {}",
        directory.display()
    );
    chunks
}

/// Decompress `input` through qpdf's QDF normalizer so an object that landed
/// inside an object stream is still visible to a byte search.
fn normalized(input: &Path, output: &Path) -> Vec<u8> {
    let result = run_qpdf(&[
        "--qdf",
        "--object-streams=disable",
        "--no-original-object-ids",
        "--preserve-unreferenced",
        input.to_str().expect("temp paths are UTF-8 under cargo"),
        output.to_str().expect("temp paths are UTF-8 under cargo"),
    ]);
    assert_success(
        &format!("qpdf QDF normalization of {}", input.display()),
        &result,
    );
    std::fs::read(output).expect("normalized QDF should be readable")
}

fn contains_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(ORPHAN_MARKER.len())
        .any(|window| window == ORPHAN_MARKER)
}

fn assert_chunks_match(qpdf_chunks: &[Vec<u8>], flpdf_chunks: &[Vec<u8>], label: &str) {
    assert_eq!(
        qpdf_chunks.len(),
        flpdf_chunks.len(),
        "{label}: chunk count must match qpdf"
    );
    for (index, (expected, actual)) in qpdf_chunks.iter().zip(flpdf_chunks).enumerate() {
        assert_eq!(
            expected,
            actual,
            "{label}: chunk {} must be byte-identical to qpdf 11.9.0",
            index + 1
        );
    }
}

/// Splitting a multi-source `--preserve-unreferenced` selection matches qpdf
/// byte for byte, and no chunk carries the primary's unreferenced objects.
///
/// The primary contributes a page of its own here (`. 1`), so the merge is a
/// genuine multi-source copy rather than a foreign-only rebuild.
#[test]
fn split_pages_chunks_match_qpdf_and_drop_primary_orphans() {
    if skip_if_qpdf_missing() {
        return;
    }
    let primary = fixture("null-visible-preserve-unreachable.pdf");
    let secondary = fixture("two-page.pdf");
    let temp = tempfile::tempdir().unwrap();
    let qpdf_template = temp.path().join("qpdf.pdf");
    let top_level_template = temp.path().join("top-level.pdf");
    let rewrite_template = temp.path().join("rewrite.pdf");

    let primary = primary.to_str().unwrap();
    let secondary = secondary.to_str().unwrap();

    assert_success(
        "qpdf --pages --preserve-unreferenced --split-pages",
        &run_qpdf(&[
            "--static-id",
            "--preserve-unreferenced",
            primary,
            "--pages",
            secondary,
            "1-z",
            ".",
            "1",
            "--",
            "--split-pages=1",
            qpdf_template.to_str().unwrap(),
        ]),
    );
    let qpdf_chunks = split_chunks(temp.path(), "qpdf");
    assert!(
        qpdf_chunks.len() > 1,
        "the fixture pair must produce several chunks"
    );

    // The top-level route configures --split-pages on QPDFJob itself, exactly
    // as qpdf's own CLI does.
    assert_success(
        "flpdf top-level --pages --preserve-unreferenced --split-pages",
        &run_flpdf(&[
            "--static-id",
            "--preserve-unreferenced",
            primary,
            top_level_template.to_str().unwrap(),
            "--pages",
            secondary,
            "1-z",
            ".",
            "1",
            "--",
            "--split-pages=1",
        ]),
    );
    let top_level_chunks = split_chunks(temp.path(), "top-level");
    assert_chunks_match(&qpdf_chunks, &top_level_chunks, "top-level route");

    // The rewrite subcommand splits the merged document itself, so its
    // create-stage job needs the same decision.
    assert_success(
        "flpdf rewrite --pages --preserve-unreferenced --split-pages",
        &run_flpdf(&[
            "rewrite",
            "--static-id",
            "--preserve-unreferenced",
            "--split-pages=1",
            "--pages",
            secondary,
            "1-z",
            ".",
            "1",
            "--",
            primary,
            rewrite_template.to_str().unwrap(),
        ]),
    );
    let rewrite_chunks = split_chunks(temp.path(), "rewrite");
    assert_chunks_match(&qpdf_chunks, &rewrite_chunks, "rewrite route");

    for (index, chunk) in qpdf_chunks.iter().enumerate() {
        let raw = temp.path().join(format!("oracle-chunk-{index}.pdf"));
        let normal = temp.path().join(format!("oracle-chunk-{index}-qdf.pdf"));
        std::fs::write(&raw, chunk).unwrap();
        assert!(
            !contains_marker(&normalized(&raw, &normal)),
            "qpdf's own split chunk {} must not carry the primary's unreferenced objects",
            index + 1
        );
    }
}

/// Without `--split-pages` the primary's unreferenced objects must still
/// survive the fresh-target merge, byte for byte with qpdf.
///
/// This is the control for the split gate: it must not reach any other
/// multi-source `--preserve-unreferenced` run.
#[test]
fn the_unsplit_multi_source_route_still_preserves_primary_orphans() {
    if skip_if_qpdf_missing() {
        return;
    }
    let primary = fixture("null-visible-preserve-unreachable.pdf");
    let secondary = fixture("two-page.pdf");
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let top_level_output = temp.path().join("top-level.pdf");
    let rewrite_output = temp.path().join("rewrite.pdf");

    let primary = primary.to_str().unwrap();
    let secondary = secondary.to_str().unwrap();

    assert_success(
        "qpdf --pages --preserve-unreferenced",
        &run_qpdf(&[
            "--static-id",
            "--preserve-unreferenced",
            primary,
            "--pages",
            secondary,
            "1-z",
            ".",
            "1",
            "--",
            qpdf_output.to_str().unwrap(),
        ]),
    );
    assert_success(
        "flpdf top-level --pages --preserve-unreferenced",
        &run_flpdf(&[
            "--static-id",
            "--preserve-unreferenced",
            primary,
            top_level_output.to_str().unwrap(),
            "--pages",
            secondary,
            "1-z",
            ".",
            "1",
            "--",
        ]),
    );
    assert_success(
        "flpdf rewrite --pages --preserve-unreferenced",
        &run_flpdf(&[
            "rewrite",
            "--static-id",
            "--preserve-unreferenced",
            "--pages",
            secondary,
            "1-z",
            ".",
            "1",
            "--",
            primary,
            rewrite_output.to_str().unwrap(),
        ]),
    );

    let expected = std::fs::read(&qpdf_output).unwrap();
    assert_eq!(
        std::fs::read(&top_level_output).unwrap(),
        expected,
        "the unsplit top-level route must stay byte-identical to qpdf 11.9.0"
    );
    assert_eq!(
        std::fs::read(&rewrite_output).unwrap(),
        expected,
        "the unsplit rewrite route must stay byte-identical to qpdf 11.9.0"
    );

    let qpdf_qdf = temp.path().join("qpdf-qdf.pdf");
    let top_level_qdf = temp.path().join("top-level-qdf.pdf");
    let rewrite_qdf = temp.path().join("rewrite-qdf.pdf");
    assert!(
        contains_marker(&normalized(&qpdf_output, &qpdf_qdf)),
        "qpdf keeps the primary's unreferenced objects when it does not split"
    );
    assert!(
        contains_marker(&normalized(&top_level_output, &top_level_qdf)),
        "the unsplit top-level route must keep the primary's unreferenced objects"
    );
    assert!(
        contains_marker(&normalized(&rewrite_output, &rewrite_qdf)),
        "the unsplit rewrite route must keep the primary's unreferenced objects"
    );
}
