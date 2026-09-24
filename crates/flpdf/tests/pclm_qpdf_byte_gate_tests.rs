//! PCLm output must be byte-identical to qpdf 11.9.0 across the writer
//! options qpdf leaves free in PCLm mode.
//!
//! `QPDFWriter::doWriteSetup` changes only `stream_decode_level`,
//! `compress_streams`, and `encrypted` for PCLm
//! (`libqpdf/QPDFWriter.cc:2071-2076`), so QDF and object-stream generation
//! stay observable and are gated here. The goldens and their regeneration
//! script live in `tests/fixtures/pclm/`.

use flpdf::{ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;

const MINI_INPUT: &[u8] = include_bytes!("../../../tests/fixtures/pclm/mini-pclm-in.pdf");
const MINI_DIRECT_ROOT_INPUT: &[u8] =
    include_bytes!("../../../tests/fixtures/pclm/mini-pclm-direct-root-in.pdf");
const MINI_EXT_INDIRECT_INPUT: &[u8] =
    include_bytes!("../../../tests/fixtures/pclm/mini-pclm-ext-indirect-in.pdf");
const MINI_NONDICT_KID_INPUT: &[u8] =
    include_bytes!("../../../tests/fixtures/pclm/mini-pclm-nondict-kid-in.pdf");
const MINI_NONDICT_PAGE_INPUT: &[u8] =
    include_bytes!("../../../tests/fixtures/pclm/mini-pclm-nondict-page-in.pdf");
const MINI_TYPE_PAGE_KIDS_INPUT: &[u8] =
    include_bytes!("../../../tests/fixtures/pclm/mini-pclm-type-page-kids-in.pdf");

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

fn write_pclm_with_indirect_extensions() -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(MINI_INPUT.to_vec())).expect("open PCLm fixture");
    let extensions = pdf
        .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
            b"/Custom".to_vec(),
            ObjectHandle::integer(1),
        )]))
        .expect("create indirect Extensions dictionary");
    pdf.root_handle()
        .expect("resolve Catalog")
        .replace_key(b"/Extensions", extensions)
        .expect("attach indirect Extensions dictionary");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().expect("install memory output");
    writer.write().expect("write PCLm Generate output");
    writer.get_buffer().expect("PCLm Generate output bytes")
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
fn pclm_generate_captures_indirect_extensions_before_prepare() {
    let actual = write_pclm_with_indirect_extensions();
    assert!(
        actual
            .windows(b"/N 5".len())
            .any(|window| window == b"/N 5"),
        "PCLm Generate must keep the setup-time Extensions member in qpdf's five-member ObjStm"
    );
    assert!(
        actual
            .windows(b"/Extensions".len())
            .any(|window| window == b"/Extensions"),
        "PCLm Generate output must retain the indirect Catalog Extensions entry"
    );
}

#[test]
fn pclm_generate_indirect_extensions_match_qpdf_11_9() {
    let actual = write_pclm(MINI_EXT_INDIRECT_INPUT, |writer| {
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
    });
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-ext-indirect-objstm.pdf"),
        "PCLm generated object streams with indirect Extensions",
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

/// qpdf's `getAllPagesInternal` treats any `/Kids` entry without `/Kids` of
/// its own as a page leaf even when it is not a dictionary, and promotes a
/// direct kid to an indirect page object (`libqpdf/QPDF_pages.cc:91-131`).
/// The first kid here is the direct integer `42`.
#[test]
fn pclm_non_dictionary_kid_matches_qpdf_11_9() {
    let actual = write_pclm(MINI_NONDICT_KID_INPUT, |_| {});
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-nondict-kid-out.pdf"),
        "PCLm non-dictionary page-tree kid",
    );
}

/// A `/Kids` leaf that is not a dictionary stays in the page list.
///
/// `QPDF::getAllPagesInternal` classifies a kid by `kid.hasKey("/Kids")`
/// (`libqpdf/QPDF_pages.cc:100-103`), so an integer leaf takes the leaf arm and
/// is pushed into `all_pages` unchanged — the `/MediaBox` default and the
/// `/Type` override it attempts are both "ignoring key replacement request"
/// no-ops on a non-dictionary receiver. `enqueueObjectsPCLm` then numbers it
/// as the first PCLm object (`1 0 obj\n42\nendobj`).
#[test]
fn pclm_non_dictionary_page_leaf_matches_qpdf_11_9() {
    let actual = write_pclm(MINI_NONDICT_PAGE_INPUT, |_| {});
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-nondict-page-out.pdf"),
        "PCLm non-dictionary page leaf",
    );
}

/// `/Kids` determines subtree membership even when the dictionary says
/// `/Type /Page`; qpdf repairs it to `/Pages` and seeds PCLm from its children.
#[test]
fn pclm_page_typed_kid_with_kids_matches_qpdf_11_9() {
    let actual = write_pclm(MINI_TYPE_PAGE_KIDS_INPUT, |_| {});
    assert_matches_golden(
        &actual,
        include_bytes!("../../../tests/fixtures/pclm/mini-pclm-type-page-kids-out.pdf"),
        "PCLm /Type /Page subtree",
    );
}
