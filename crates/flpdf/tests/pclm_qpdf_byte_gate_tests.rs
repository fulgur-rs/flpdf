//! PCLm output must be byte-identical to qpdf 11.9.0 across the writer
//! options qpdf leaves free in PCLm mode.
//!
//! `QPDFWriter::doWriteSetup` changes only `stream_decode_level`,
//! `compress_streams`, and `encrypted` for PCLm
//! (`libqpdf/QPDFWriter.cc:2071-2076`), so QDF and object-stream generation
//! stay observable and are gated here. The goldens and their regeneration
//! script live in `tests/fixtures/pclm/`.

use flpdf::{ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;

const MINI_INPUT: &[u8] = include_bytes!("../../../tests/fixtures/pclm/mini-pclm-in.pdf");
const MINI_DIRECT_ROOT_INPUT: &[u8] =
    include_bytes!("../../../tests/fixtures/pclm/mini-pclm-direct-root-in.pdf");

fn write_pclm(
    input: &[u8],
    configure: impl FnOnce(&mut PdfWriter<'_, Cursor<Vec<u8>>>),
) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(input.to_vec())).expect("open PCLm fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_static_id(true);
    configure(&mut writer);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write PCLm output");
    writer.get_buffer().expect("PCLm output bytes")
}

fn assert_matches_golden(actual: &[u8], golden: &[u8], label: &str) {
    if actual == golden {
        return;
    }
    let first_diff = actual
        .iter()
        .zip(golden.iter())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| actual.len().min(golden.len()));
    let start = first_diff.saturating_sub(120);
    panic!(
        "{label} must be byte-identical to qpdf 11.9.0\n\
         actual len {} golden len {} first diff at {first_diff}\n\
         actual  : {:?}\n\
         expected: {:?}",
        actual.len(),
        golden.len(),
        String::from_utf8_lossy(&actual[start..actual.len().min(first_diff + 120)]),
        String::from_utf8_lossy(&golden[start..golden.len().min(first_diff + 120)]),
    );
}

#[test]
fn pclm_multi_page_strip_order_matches_qpdf_11_9() {
    let actual = write_pclm(MINI_INPUT, |_| {});
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-out.pdf"),
        "PCLm",
    );
}

#[test]
fn pclm_qdf_matches_qpdf_11_9() {
    let actual = write_pclm(MINI_INPUT, |writer| writer.set_qdf_mode(true));
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-qdf.pdf"),
        "PCLm QDF",
    );
}

#[test]
fn pclm_generated_object_streams_match_qpdf_11_9() {
    let actual = write_pclm(MINI_INPUT, |writer| {
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
    });
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-objstm.pdf"),
        "PCLm generated object streams",
    );
}

#[test]
fn pclm_direct_root_matches_qpdf_11_9() {
    let actual = write_pclm(MINI_DIRECT_ROOT_INPUT, |_| {});
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-direct-root-out.pdf"),
        "PCLm direct Catalog",
    );
}
