//! Top-level qpdf-shaped compression-level and recompress-flate coverage.
//!
//! The native `rewrite --recompress-flate` surface already exercises the
//! writer toggle. These tests cover the qpdf argv surface used by qtest and
//! prove that the requested Flate level reaches the real writer: a level-1
//! and level-9 rewrite of the same lone-Flate stream must produce different
//! encoded bytes while preserving the decoded content.

use assert_cmd::Command;
use flpdf::{filters, ObjectHandle};
use predicates::prelude::*;
use std::path::{Path, PathBuf};
#[cfg(feature = "qpdf-zlib-compat")]
use std::process::Command as ProcessCommand;

const FIXTURE: &str = "lone-flate-l9.pdf";

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(FIXTURE)
}

fn largest_stream_payload(data: &[u8]) -> Vec<u8> {
    let needle = b"stream\n";
    let mut best = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = data[cursor..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let start = cursor + relative + needle.len();
        let end = start
            + data[start..]
                .windows(b"endstream".len())
                .position(|window| window == b"endstream")
                .expect("stream must have an endstream marker");
        if end - start > best.len() {
            best = data[start..end].to_vec();
        }
        cursor = end + b"endstream".len();
    }
    best
}

fn source_payload() -> Vec<u8> {
    largest_stream_payload(&std::fs::read(fixture_path()).unwrap())
}

fn run_top_level(level: &str) -> Vec<u8> {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--recompress-flate",
            "--object-streams=disable",
        ])
        .arg(format!("--compression-level={level}"))
        .arg(fixture_path())
        .arg(&output)
        .assert()
        .success();
    std::fs::read(output).unwrap()
}

fn run_native_rewrite(level: &str) -> Vec<u8> {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--static-id", "--recompress-flate"])
        .arg(format!("--compression-level={level}"))
        .arg(fixture_path())
        .arg(&output)
        .assert()
        .success();
    std::fs::read(output).unwrap()
}

fn decode(payload: &[u8]) -> Vec<u8> {
    let dictionary = ObjectHandle::dictionary(vec![(
        b"/Filter".to_vec(),
        ObjectHandle::name(b"FlateDecode".to_vec()),
    )]);
    filters::decode_stream_data(&dictionary, payload).expect("valid Flate stream")
}

#[test]
fn top_level_compression_level_reaches_recompress_flate_writer() {
    let level_one = run_top_level("1");
    let level_nine = run_top_level("9");
    let payload_one = largest_stream_payload(&level_one);
    let payload_nine = largest_stream_payload(&level_nine);

    assert_ne!(
        payload_one, payload_nine,
        "top-level --compression-level must change recompressed Flate bytes"
    );
    assert_eq!(
        decode(&payload_one),
        decode(&source_payload()),
        "changing compression level must preserve decoded stream bytes"
    );
    assert!(
        level_one
            .windows(b"/Filter /FlateDecode".len())
            .any(|window| window == b"/Filter /FlateDecode"),
        "recompressed top-level stream must retain its FlateDecode filter"
    );
}

#[test]
fn native_rewrite_compression_level_reaches_the_same_writer() {
    let level_one = run_native_rewrite("1");
    let level_nine = run_native_rewrite("9");

    assert_ne!(
        largest_stream_payload(&level_one),
        largest_stream_payload(&level_nine),
        "native rewrite --compression-level must reach the Flate writer"
    );
}

#[test]
fn top_level_compression_level_zero_matches_qpdfs_accepted_boundary() {
    let output = run_top_level("0");
    let payload = largest_stream_payload(&output);
    assert_eq!(
        decode(&payload),
        decode(&source_payload()),
        "level 0 must still emit a valid Flate stream"
    );
}

#[test]
fn top_level_compression_level_above_zlib_domain_is_a_recoverable_stream_warning() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--recompress-flate",
            "--object-streams=disable",
            "--compression-level=10",
        ])
        .arg(fixture_path())
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "stream will be re-processed without filtering",
        ));
    assert!(
        output.exists(),
        "the stream fallback must retain the output"
    );
}

#[test]
fn top_level_compression_level_overflow_is_a_cli_error() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--recompress-flate",
            "--object-streams=disable",
            "--compression-level=999999999999999999999",
        ])
        .arg(fixture_path())
        .arg(&output)
        .assert()
        .failure()
        .code(2);
}

#[cfg(feature = "qpdf-zlib-compat")]
fn qpdf_11_9_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
        .unwrap_or(false)
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn top_level_compression_levels_match_qpdf_11_9_byte_for_byte() {
    if !qpdf_11_9_available() {
        eprintln!("qpdf 11.9.0 not available; skipping byte differential");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = fixture_path();
    for level in ["1", "9"] {
        let qpdf_output = temp.path().join(format!("qpdf-{level}.pdf"));
        let flpdf_output = temp.path().join(format!("flpdf-{level}.pdf"));
        ProcessCommand::new("qpdf")
            .args([
                "--static-id",
                "--recompress-flate",
                "--object-streams=disable",
            ])
            .arg(format!("--compression-level={level}"))
            .arg(&input)
            .arg(&qpdf_output)
            .status()
            .expect("qpdf 11.9.0 must spawn")
            .success()
            .then_some(())
            .expect("qpdf 11.9.0 rewrite must succeed");
        Command::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--static-id",
                "--recompress-flate",
                "--object-streams=disable",
            ])
            .arg(format!("--compression-level={level}"))
            .arg(&input)
            .arg(&flpdf_output)
            .assert()
            .success();

        assert_eq!(
            std::fs::read(&flpdf_output).unwrap(),
            std::fs::read(&qpdf_output).unwrap(),
            "top-level compression level {level} must match qpdf 11.9.0"
        );
    }
}
