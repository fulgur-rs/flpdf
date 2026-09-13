use flpdf::{ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;
use std::path::Path;

#[test]
fn plain_disable_uses_the_live_queue_consumer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plain = std::fs::read_to_string(root.join("writer/plain/mod.rs")).unwrap();
    let body = std::fs::read_to_string(root.join("writer/plain/body.rs")).unwrap();

    assert!(plain.contains("write_plain_live_disable"));
    assert!(body.contains("struct LiveQueue"));
    assert!(body.contains("emit_live_disable"));
    assert!(body.contains("enqueue_handle"));
}

#[test]
fn planned_preserve_uses_the_d9_source_membership_owner() {
    // qpdf has one source-membership owner, `getObjectStreamData`
    // (`QPDF.cc:2381-2390`); Preserve's early-return decision consumes that
    // map inside `preserveObjectStreams` (`QPDFWriter.cc:1939-1967`). The
    // planned consumer must not rederive the same fact from a second raw-xref
    // predicate.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plan = std::fs::read_to_string(root.join("writer/plain/plan.rs")).unwrap();

    assert!(!plan.contains("source_has_compressed_entries"));
    assert!(plan.contains("source_object_stream_data"));
    assert!(plan.contains("get_object_stream_data"));
}

#[test]
fn plain_disable_reconciles_direct_adbe_before_live_child_enqueue() {
    // qpdf 11.9.0: QPDFWriter.cc:1418-1430 replaces stale /ADBE before
    // unparseChild can enqueue the obsolete indirect /URL value.
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/adbe-orphan-url.pdf").to_vec(),
    ))
    .expect("open direct ADBE fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.force_pdf_version("1.7", 8);
    writer.set_static_id(true);
    writer.set_output_memory().expect("configure memory output");
    writer.write().expect("write live queue output");
    let output = writer.get_buffer().expect("read memory output");
    let expected = include_bytes!("../../../tests/fixtures/compat/golden/adbe-orphan-url.qpdf.pdf");
    assert_eq!(
        output, expected,
        "direct ADBE replacement must drop orphan URL"
    );
}
