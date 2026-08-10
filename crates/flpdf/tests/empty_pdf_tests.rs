//! Integration tests for [`flpdf::Pdf::empty`].
//!
//! Mirrors qpdf's `QPDF::emptyPDF()` (`libqpdf/QPDF.cc:34-51,290-293`): a
//! fixed minimal PDF read through the normal parser, usable with the same
//! mutation, page-document-helper, and writer APIs as any other opened
//! document.

use flpdf::{ObjectHandle, PageDocumentHelper, PageInput, Pdf};
use std::process::Command;

fn write_static_id(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> Vec<u8> {
    let options = WriterTestSettings {
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut out = Vec::new();
    write_with_settings(pdf, &mut out, &options).expect("write empty document");
    out
}

/// Golden bytes captured from qpdf 11.9.0 (`qpdf --static-id --empty`).
/// Pins the writer's key ordering, the classic-xref layout, and the
/// `--static-id` constant even when the `qpdf` binary is unavailable
/// locally.
fn golden_static_id_empty() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.3\n");
    out.extend_from_slice(&[b'%', 0xbf, 0xf7, 0xa2, 0xfe, b'\n']);
    out.extend_from_slice(b"1 0 obj\n<< /Pages 2 0 R /Type /Catalog >>\nendobj\n");
    out.extend_from_slice(b"2 0 obj\n<< /Count 0 /Kids [ ] /Type /Pages >>\nendobj\n");
    out.extend_from_slice(
        b"xref\n0 3\n0000000000 65535 f \n0000000015 00000 n \n0000000064 00000 n \n",
    );
    out.extend_from_slice(
        b"trailer << /Root 1 0 R /Size 3 /ID [<31415926535897932384626433832795>\
<31415926535897932384626433832795>] >>\n",
    );
    out.extend_from_slice(b"startxref\n117\n%%EOF\n");
    out
}

#[test]
fn empty_document_write_matches_qpdf_static_id_empty_golden() {
    let mut pdf = Pdf::empty().unwrap();
    let actual = write_static_id(&mut pdf);
    assert_eq!(
        actual,
        golden_static_id_empty(),
        "flpdf Pdf::empty() + --static-id write diverged from the qpdf 11.9.0 `--static-id --empty` golden"
    );
}

/// Re-runs the golden comparison live against the `qpdf` binary when it is
/// on `PATH`; a no-op (not a failure) when qpdf is unavailable, since the
/// golden-bytes test above already pins the same bytes.
#[test]
fn empty_document_write_matches_live_qpdf_static_id_empty() {
    if Command::new("qpdf").arg("--version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let qpdf_out = dir.path().join("qpdf-empty.pdf");
    let status = Command::new("qpdf")
        .arg("--static-id")
        .arg("--empty")
        .arg(&qpdf_out)
        .status()
        .expect("invoking qpdf");
    assert!(status.success(), "qpdf --static-id --empty failed");
    let qpdf_bytes = std::fs::read(&qpdf_out).unwrap();

    let mut pdf = Pdf::empty().unwrap();
    let actual = write_static_id(&mut pdf);
    assert_eq!(
        actual, qpdf_bytes,
        "flpdf Pdf::empty() + --static-id write diverged from a live qpdf --static-id --empty run"
    );
}

#[test]
fn empty_document_accepts_added_page_via_page_document_helper_and_passes_qpdf_check() {
    let mut pdf = Pdf::empty().unwrap();
    let direct_page = ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (
            b"MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(612),
                ObjectHandle::integer(792),
            ]),
        ),
    ]);
    PageDocumentHelper::new(&mut pdf)
        .add_page(PageInput::direct(direct_page), false)
        .unwrap();

    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap();
    assert_eq!(pages.len(), 1);

    if Command::new("qpdf").arg("--version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("empty-plus-page.pdf");
    let options = WriterTestSettings {
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut bytes = Vec::new();
    write_with_settings(&mut pdf, &mut bytes, &options).unwrap();
    std::fs::write(&input_path, &bytes).unwrap();

    let output = Command::new("qpdf")
        .arg("--check")
        .arg(&input_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "qpdf --check rejected the written document: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

mod common;
#[allow(unused_imports)]
use common::{write_default, write_with_settings, WriterTestSettings};
