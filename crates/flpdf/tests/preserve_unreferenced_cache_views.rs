//! qpdf's preserve-unreferenced write seeds from the complete object cache
//! (`libqpdf/QPDFWriter.cc:2909-2915`), so a document-allocated reserved
//! sentinel and a value swapped into a previously unknown generation must both
//! stay visible to the writer.

use flpdf::{ObjectRef, ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;

fn one_page() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf"),
    )
    .expect("read one-page fixture")
}

fn matching_raw_generation_orphan_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let orphan_offset = bytes.len();
    bytes.extend_from_slice(b"5 65536 obj\n45\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n0000000000 00000 f \n0000000000 00000 f \n");
    bytes.extend_from_slice(format!("{orphan_offset:010} 65536 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn write_preserve_unreferenced(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    mode: ObjectStreamMode,
    probe: ObjectRef,
) -> std::result::Result<Option<ObjectRef>, String> {
    let mut writer = PdfWriter::new(pdf);
    writer.set_static_id(true);
    writer.set_object_stream_mode(mode);
    writer.set_preserve_unreferenced_objects(true);
    writer.set_output_memory().expect("configure memory output");
    match writer.write() {
        Ok(()) => Ok(writer
            .get_renumbered_obj_gen(probe)
            .expect("query renumbering")),
        Err(error) => Err(error.to_string()),
    }
}

/// `QPDF_Reserved::unparse` throws when an unreplaced reserved object reaches
/// the writer (`libqpdf/QPDF_Reserved.cc:22-26`). Because qpdf's
/// preserve-unreferenced walk enqueues every `getAllObjects()` entry
/// (`libqpdf/QPDFWriter.cc:2909-2915`), the generated-object-stream route must
/// report that error too, not drop the object.
#[test]
fn unreplaced_reserved_object_errors_on_every_preserve_unreferenced_route() {
    for mode in [ObjectStreamMode::Generate, ObjectStreamMode::Disable] {
        let mut pdf = Pdf::open(Cursor::new(one_page())).expect("open fixture");
        let reserved = pdf.new_reserved().expect("allocate a reserved object");
        let reserved_ref = reserved.object_ref().expect("reserved identity");
        let error = write_preserve_unreferenced(&mut pdf, mode, reserved_ref)
            .expect_err("an unreplaced reserved object must not be silently dropped");
        assert!(
            error.contains("attempting to unparse a reserved object"),
            "{mode:?} must report QPDF_Reserved::unparse, got {error}"
        );
    }
}

/// `QPDF::swapObjects` resolves both identities before swapping
/// (`libqpdf/QPDF.cc:2284-2291`), so a generation that had no xref row owns a
/// cache cell afterwards and stays enqueueable by a preserve-unreferenced
/// write.
#[test]
fn value_swapped_into_an_unknown_generation_survives_preserve_unreferenced() {
    let unknown = ObjectRef::new(900, 0);
    let source = ObjectRef::new(1, 0);
    for mode in [ObjectStreamMode::Generate, ObjectStreamMode::Disable] {
        let mut pdf = Pdf::open(Cursor::new(one_page())).expect("open fixture");
        pdf.get_all_objects().expect("prepare the canonical cache");
        pdf.swap_objects(source, unknown)
            .expect("swap a live value into an unknown generation");
        let renumbered = write_preserve_unreferenced(&mut pdf, mode, unknown)
            .expect("preserve-unreferenced write succeeds");
        assert!(
            renumbered.is_some(),
            "{mode:?} must emit the value swapped into {unknown}"
        );
    }
}

#[test]
fn raw_generation_orphan_survives_preserve_unreferenced() {
    for mode in [ObjectStreamMode::Disable, ObjectStreamMode::Generate] {
        let mut pdf =
            Pdf::open(Cursor::new(matching_raw_generation_orphan_pdf())).expect("open PDF");
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_static_id(true);
        writer.set_object_stream_mode(mode);
        writer.set_preserve_unreferenced_objects(true);
        writer.set_output_memory().expect("configure memory output");
        writer
            .write()
            .expect("preserve-unreferenced write succeeds");

        let output = writer.get_buffer().expect("writer output");
        let value_count = output
            .windows(b"\n45\n".len())
            .filter(|window| *window == b"\n45\n")
            .count();
        assert!(
            value_count == 1,
            "raw-generation orphan must be emitted once in {mode:?} mode, got {value_count}"
        );
    }
}
