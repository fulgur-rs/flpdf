//! Byte-identity coverage for qpdf's null-aware dictionary visibility.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, ObjectStreamMode, Pdf, WriteOptions};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

fn rewrite_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = mode;
    options.static_id = true;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();
    out
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_golden(actual: &[u8], golden_name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(golden_name);
    let expected =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read golden {path:?}: {error}"));
    if let Some(offset) = first_diff(actual, &expected) {
        let start = offset.saturating_sub(16);
        panic!(
            "{golden_name}: not byte-identical to qpdf golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {offset})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[start..(offset + 16).min(actual.len())],
            &expected[start..(offset + 16).min(expected.len())],
        );
    }
}

#[test]
fn disable_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix.pdf", ObjectStreamMode::Disable),
        "null-visible-matrix/disable.pdf",
    );
}

#[test]
fn preserve_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix-objstm.pdf", ObjectStreamMode::Preserve),
        "null-visible-matrix-objstm/preserve.pdf",
    );
}

#[test]
fn disable_null_visibility_cycle_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-cycle.pdf", ObjectStreamMode::Disable),
        "null-visible-cycle/disable.pdf",
    );
}

#[test]
fn preserve_filters_unreachable_sibling_from_source_container() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-mixed.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-mixed/preserve.pdf",
    );
}

#[test]
fn preserve_drops_fully_unreachable_source_container() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-unreachable.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-unreachable/preserve.pdf",
    );
}

#[test]
fn preserve_keeps_single_source_container_over_100_members() {
    assert_golden(
        &rewrite_mode(
            "null-visible-preserve-over-100.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-preserve-over-100/preserve.pdf",
    );
}
