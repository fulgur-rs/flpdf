//! Byte-identity for later-page object ordering in the linearized body.
//!
//! qpdf pushes a later page's own dictionary into part7 *before* it iterates
//! that page's entry in the object-user map
//! (`libqpdf/QPDF_linearization.cc:1231-1255`): `pages.at(i)` is placed and
//! erased from `lc_other_page_private`, so the map loop can no longer emit it.
//! The map itself is ordered by object id, so consuming it without placing the
//! page first writes the page dictionary in the middle of its own descendants.
//!
//! `--object-streams=disable` is what makes the order observable. These
//! fixtures carry a source object stream, so under the default preserve mode
//! the members stay packed inside it and the sequence cannot be seen.

#![cfg(feature = "qpdf-zlib-compat")]

mod common;

use common::{write_linearized_with_settings, WriterTestSettings};
use flpdf::{NewlineBeforeEndstream, ObjectStreamMode, Pdf};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

fn linearize_disable(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let mut pdf =
        Pdf::open(BufReader::new(File::open(&path).expect("open fixture"))).expect("parse fixture");
    let settings = WriterTestSettings {
        object_streams: ObjectStreamMode::Disable,
        deterministic_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut pdf, &settings).expect("linearized write")
}

fn assert_golden(actual: &[u8], golden_name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(golden_name);
    let expected =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read golden {path:?}: {error}"));
    let offset = actual
        .iter()
        .zip(expected.iter())
        .position(|(a, b)| a != b)
        .or_else(|| (actual.len() != expected.len()).then_some(actual.len().min(expected.len())));
    if let Some(offset) = offset {
        let start = offset.saturating_sub(16);
        panic!(
            "{golden_name}: not byte-identical to qpdf golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {offset})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            String::from_utf8_lossy(&actual[start..(offset + 16).min(actual.len())]),
            String::from_utf8_lossy(&expected[start..(offset + 16).min(expected.len())]),
        );
    }
}

#[test]
fn later_page_dictionary_precedes_its_exclusive_font() {
    assert_golden(
        &linearize_disable("primary-objstm-exclusive-font.pdf"),
        "primary-objstm-exclusive-font/linearize-disable.pdf",
    );
}

#[test]
fn later_page_dictionary_precedes_untyped_container_members() {
    assert_golden(
        &linearize_disable("untyped-objstm-container.pdf"),
        "untyped-objstm-container/linearize-disable.pdf",
    );
}
