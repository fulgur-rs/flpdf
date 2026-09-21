//! qpdf 11.9.0 byte parity for `--json` combined with `--split-pages`.
//!
//! `QPDFJob::writeQPDF` picks one of three arms -- `doInspection`,
//! `doSplitPages`, `writeOutfile` (`libqpdf/QPDFJob.cc:486-491`) -- and the
//! JSON document is written from inside `writeOutfile`. A truthy
//! `--split-pages` therefore claims the run before JSON output is reachable,
//! and the split chunks carry no trace of the JSON selectors.
//!
//! The JSON selectors really are inert for the writer, not merely unused:
//! `Config::jsonOutput` lowers `decode_level` to `qpdf_dl_none` without
//! setting `decode_level_set` (`libqpdf/QPDFJob_config.cc:312-326`), and
//! `setWriterOptions` replays the decode level only when that flag is set
//! (`libqpdf/QPDFJob.cc:2873-2875`). Every remaining JSON option -- keys,
//! objects, stream data, stream prefix -- is consumed by `writeJSON` alone.
//! So a split run with `--json` must be byte-identical to the same run
//! without it, and this file pins both against real qpdf.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
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
        panic!("qpdf 11.9.0 is required for json/split-pages precedence parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed ({:?}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{label} wrote to standard output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Read `out_1.pdf`, `out_2.pdf`, ... until the next one is missing.
fn split_chunks(directory: &Path) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    for index in 1..=9 {
        match std::fs::read(directory.join(format!("out_{index}.pdf"))) {
            Ok(bytes) => chunks.push(bytes),
            Err(_) => break,
        }
    }
    assert!(
        !chunks.is_empty(),
        "no split chunk was written in {}",
        directory.display()
    );
    chunks
}

/// Run one split invocation in its own directory and return the chunks.
///
/// The template is `out_%d.pdf`, so a literal file of that name is exactly
/// the shape bug this parity test guards against; it is asserted absent.
fn run_split(label: &str, program: &str, selectors: &[&str], input: &Path) -> Vec<Vec<u8>> {
    let directory = tempfile::tempdir().expect("temporary split directory");
    let template = directory.path().join("out_%d.pdf");
    let mut arguments: Vec<&str> = vec!["--deterministic-id", "--split-pages=1"];
    arguments.extend_from_slice(selectors);

    let output = if program == "qpdf" {
        ShellCommand::new("qpdf")
            .args(&arguments)
            .arg(input)
            .arg(&template)
            .output()
            .expect("qpdf should spawn")
    } else {
        Command::cargo_bin("flpdf")
            .expect("flpdf binary should build")
            .args(&arguments)
            .arg(input)
            .arg(&template)
            .output()
            .expect("flpdf should spawn")
    };
    assert_success(label, &output);
    assert!(
        !template.exists(),
        "{label} wrote the split template as a literal file name"
    );
    split_chunks(directory.path())
}

#[test]
fn json_selectors_leave_split_output_byte_identical_to_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let input = fixture("three-page.pdf");

    // The baseline the other rows must reproduce: the same split with no JSON
    // selector at all.
    let qpdf_plain = run_split("qpdf (no json)", "qpdf", &[], &input);
    assert_eq!(
        qpdf_plain.len(),
        3,
        "a three-page input yields three chunks"
    );

    for selectors in [
        vec!["--json=2"],
        vec!["--json=latest"],
        vec!["--json-output=2"],
        vec!["--json=2", "--json-key=pages"],
        vec!["--json=2", "--json-stream-data=inline"],
    ] {
        let label = selectors.join(" ");
        let qpdf_chunks = run_split(&format!("qpdf {label}"), "qpdf", &selectors, &input);
        let flpdf_chunks = run_split(&format!("flpdf {label}"), "flpdf", &selectors, &input);

        assert_eq!(
            qpdf_chunks, qpdf_plain,
            "{label}: qpdf's split output must ignore the JSON selectors"
        );
        assert_eq!(
            flpdf_chunks, qpdf_chunks,
            "{label}: flpdf's split chunks must match qpdf byte for byte"
        );
    }
}

#[test]
fn a_split_json_run_matches_qpdf_for_a_fresh_page_selection_target() {
    if skip_if_qpdf_missing() {
        return;
    }
    // `--empty --pages` builds the output document from scratch, so the split
    // runs against a target qpdf assembled rather than the primary input.
    // `--preserve-unreferenced` is a writer option, which is precisely the
    // configuration a JSON-only write path would drop.
    let input = fixture("three-page.pdf");
    let arguments = [
        "--empty",
        "--pages",
        input.to_str().expect("fixture paths are UTF-8"),
        "1-2",
        "--",
        "--deterministic-id",
        "--preserve-unreferenced",
        "--split-pages=1",
        "--json=2",
    ];

    let qpdf_directory = tempfile::tempdir().expect("temporary qpdf directory");
    let qpdf = ShellCommand::new("qpdf")
        .args(arguments)
        .arg(qpdf_directory.path().join("out_%d.pdf"))
        .output()
        .expect("qpdf should spawn");
    let flpdf_directory = tempfile::tempdir().expect("temporary flpdf directory");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(arguments)
        .arg(flpdf_directory.path().join("out_%d.pdf"))
        .output()
        .expect("flpdf should spawn");

    assert_success("qpdf --empty --pages split", &qpdf);
    assert_success("flpdf --empty --pages split", &flpdf);

    let qpdf_chunks = split_chunks(qpdf_directory.path());
    assert_eq!(
        qpdf_chunks.len(),
        2,
        "a two-page selection yields two chunks"
    );
    assert_eq!(
        split_chunks(flpdf_directory.path()),
        qpdf_chunks,
        "a page-selection split with --json must match qpdf byte for byte"
    );
}

/// Which route claims the run is decided by qpdf's own integer conversion,
/// not by Rust's `str::parse`.
///
/// `Config::splitPages` (`libqpdf/QPDFJob_config.cc:604-609`) sends a
/// non-empty parameter through `QUtil::string_to_int`, whose `strtoll` stage
/// (`libqpdf/QUtil.cc:373-393`) reads a leading digit run and converts
/// nothing -- returning zero -- when there is none. So `--split-pages=garbage`
/// is a falsy split and the JSON document still reaches stdout, while
/// `--split-pages=2x` is a real split of two. Both are measured against real
/// qpdf here because a `parse::<usize>()` gate gets each one backwards.
#[test]
fn split_pages_activation_follows_qpdf_integer_conversion() {
    if skip_if_qpdf_missing() {
        return;
    }
    let input = fixture("three-page.pdf");

    for value in ["garbage", "0abc", "0"] {
        let flag = format!("--split-pages={value}");
        let qpdf = ShellCommand::new("qpdf")
            .args(["--static-id", "--json=2", &flag])
            .arg(&input)
            .output()
            .expect("qpdf should spawn");
        let flpdf = Command::cargo_bin("flpdf")
            .expect("flpdf binary should build")
            .args(["--static-id", "--json=2", &flag])
            .arg(&input)
            .output()
            .expect("flpdf should spawn");
        assert!(
            qpdf.status.success(),
            "qpdf {flag} failed ({:?}): {}",
            qpdf.status,
            String::from_utf8_lossy(&qpdf.stderr)
        );
        assert!(
            !qpdf.stdout.is_empty(),
            "oracle guard: qpdf must treat {flag} as a falsy split and still write JSON"
        );
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "{flag}: exit status must match qpdf; stderr:\n{}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert_eq!(
            flpdf.stdout, qpdf.stdout,
            "{flag}: the JSON document must match qpdf byte for byte"
        );
    }

    // A digit run followed by garbage really does select a split, so the
    // written chunks have to follow qpdf rather than the value being
    // rejected. `--split-pages=2` names its outputs by page range, so read
    // the directory rather than guessing the file names.
    let qpdf_chunks = split_named_chunks("qpdf", "qpdf", &input);
    assert_eq!(
        qpdf_chunks.len(),
        2,
        "oracle guard: qpdf must read 2 out of \"2x\" and split a three-page file into two chunks"
    );
    let flpdf_chunks = split_named_chunks("flpdf", "flpdf", &input);
    assert_eq!(
        flpdf_chunks, qpdf_chunks,
        "\"2x\" must produce the same chunk names and bytes qpdf does"
    );
}

/// Run `--json=2 --split-pages=2x` into a fresh directory and return every
/// written chunk as `(file name, bytes)`, sorted by name.
fn split_named_chunks(label: &str, program: &str, input: &Path) -> Vec<(String, Vec<u8>)> {
    let directory = tempfile::tempdir().expect("temporary split directory");
    let template = directory.path().join("out_%d.pdf");
    let arguments = ["--static-id", "--json=2", "--split-pages=2x"];
    let output = if program == "qpdf" {
        ShellCommand::new("qpdf")
            .args(arguments)
            .arg(input)
            .arg(&template)
            .output()
            .expect("qpdf should spawn")
    } else {
        Command::cargo_bin("flpdf")
            .expect("flpdf binary should build")
            .args(arguments)
            .arg(input)
            .arg(&template)
            .output()
            .expect("flpdf should spawn")
    };
    assert_success(&format!("{label} --split-pages=2x"), &output);
    let mut chunks: Vec<(String, Vec<u8>)> = std::fs::read_dir(directory.path())
        .expect("read split directory")
        .map(|entry| {
            let entry = entry.expect("split directory entry");
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).expect("read split chunk"),
            )
        })
        .collect();
    chunks.sort_by(|left, right| left.0.cmp(&right.0));
    chunks
}
