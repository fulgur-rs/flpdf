//! Byte-identity: generated object streams keep the membership qpdf fixes
//! during writer setup, before `prepareFileForWrite` rewrites the Catalog.
//!
//! `QPDFWriter::doWriteSetup` reaches `generateObjectStreams` through a bare
//! `switch (m->object_stream_mode)` that no QDF, encryption, or content
//! normalization predicate guards (`libqpdf/QPDFWriter.cc:2125-2139`).
//! `QPDFWriter::write` only calls `prepareFileForWrite` afterwards
//! (`libqpdf/QPDFWriter.cc:2195`), and that call makes an indirect Catalog
//! `/Extensions` dictionary direct (`libqpdf/QPDFWriter.cc:2039-2046`).
//! Membership therefore still contains the `/Extensions` object, which qpdf
//! emits as an object-stream member even though the Catalog now inlines it.
//!
//! Both fixtures carry an indirect `/Extensions` dictionary. Encryption is what
//! separates these routes from the plain `--object-streams=generate` goldens in
//! `cmp_generate_objstm_tests`.
//!
//! `QPDFWriter::setExtraHeaderText` (`libqpdf/QPDFWriter.cc:269`) selects the
//! same route without encryption; it has no qpdf command-line binding, so that
//! golden comes from the C++ API through
//! `scripts/generate-qpdf-extra-header-golden.sh`.
//!
//! The encrypted goldens use RC4-128 rather than AES so the bytes are
//! reproducible (AES adds a random IV to every stream), and every option set
//! emits uncompressed streams — QDF suppresses compression on its own and the
//! content normalization route passes `--compress-streams=n` — so these gates
//! are independent of the DEFLATE backend and need no `qpdf-zlib-compat`
//! feature. Regenerate with `tests/golden/regenerate.sh`.

mod common;

use common::{write_with_settings, WriterTestSettings};
use flpdf::{CompressStreams, EncryptMethod, EncryptParams, ObjectStreamMode, Pdf};
use std::path::Path;

fn fixture_path(stem: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(format!("{stem}.pdf"))
}

fn golden(stem: &str, name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read golden {path:?}: {error}"))
}

fn rc4_128_generate_settings() -> WriterTestSettings {
    WriterTestSettings {
        object_streams: ObjectStreamMode::Generate,
        static_id: true,
        encrypt: Some(EncryptParams::rc4(EncryptMethod::V2Rc4128, b"u", b"o")),
        ..WriterTestSettings::default()
    }
}

fn write_generate(stem: &str, settings: &WriterTestSettings) -> Vec<u8> {
    let path = fixture_path(stem);
    let file = std::fs::File::open(&path).unwrap_or_else(|error| panic!("open {path:?}: {error}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("open fixture");
    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, settings).expect("Generate write");
    out
}

fn assert_byte_identical(actual: &[u8], expected: &[u8], label: &str) {
    if actual == expected {
        return;
    }
    let common = actual.len().min(expected.len());
    let offset = (0..common)
        .find(|index| actual[*index] != expected[*index])
        .unwrap_or(common);
    let low = offset.saturating_sub(48);
    panic!(
        "{label} must be byte-identical to qpdf 11.9.0\n\
         actual len {} expected len {} first diff at {offset}\n\
         actual  : {:?}\n\
         expected: {:?}",
        actual.len(),
        expected.len(),
        String::from_utf8_lossy(&actual[low..actual.len().min(offset + 48)]),
        String::from_utf8_lossy(&expected[low..expected.len().min(offset + 48)]),
    );
}

fn assert_qdf_route(stem: &str) {
    let settings = WriterTestSettings {
        qdf: true,
        ..rc4_128_generate_settings()
    };
    assert_byte_identical(
        &write_generate(stem, &settings),
        &golden(stem, "encrypt-qdf-generate.pdf"),
        &format!("{stem} encrypted QDF Generate"),
    );
}

fn assert_normalize_content_route(stem: &str) {
    let settings = WriterTestSettings {
        content_normalization: true,
        compress_streams: CompressStreams::No,
        ..rc4_128_generate_settings()
    };
    assert_byte_identical(
        &write_generate(stem, &settings),
        &golden(stem, "encrypt-normalize-generate.pdf"),
        &format!("{stem} encrypted normalized Generate"),
    );
}

/// The header text qpdf wrote into `extra-header-qdf-generate.pdf`. qpdf
/// emits it verbatim right after the file header (`QPDFWriter.cc:3000-3001`).
const EXTRA_HEADER_TEXT: &str = "%% flpdf extra header\n";

#[test]
fn extra_header_text_qdf_generate_keeps_indirect_extensions_compressed() {
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Generate,
        static_id: true,
        extra_header_text: EXTRA_HEADER_TEXT.to_string(),
        ..WriterTestSettings::default()
    };
    let stem = "one-page-ext-indirect";
    assert_byte_identical(
        &write_generate(stem, &settings),
        &golden(stem, "extra-header-qdf-generate.pdf"),
        "extra-header QDF Generate",
    );
}

#[test]
fn encrypted_qdf_generate_keeps_indirect_extensions_compressed() {
    assert_qdf_route("one-page-ext-indirect");
}

#[test]
fn encrypted_qdf_generate_keeps_linearization_fixture_extensions_compressed() {
    assert_qdf_route("linearize-indirect-extensions");
}

#[test]
fn encrypted_normalized_generate_keeps_indirect_extensions_compressed() {
    assert_normalize_content_route("one-page-ext-indirect");
}

#[test]
fn encrypted_normalized_generate_keeps_linearization_fixture_extensions_compressed() {
    assert_normalize_content_route("linearize-indirect-extensions");
}
