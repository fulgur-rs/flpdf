//! qpdf getCompressibleObjGens removes stale generations from the live graph.

use flpdf::{ObjectRef, ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;

#[test]
fn generate_turns_a_retained_stale_generation_handle_into_direct_null() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/compressible-stale-generation-alias.pdf")
            .to_vec(),
    ))
    .unwrap();
    let versions = pdf
        .root_handle()
        .unwrap()
        .try_get_key(b"/Versions")
        .unwrap();
    let old = versions.try_get_array_item(0).unwrap();
    let current = versions.try_get_array_item(1).unwrap();
    assert_eq!(old.object_ref(), Some(ObjectRef::new(3, 0)));
    assert_eq!(current.object_ref(), Some(ObjectRef::new(3, 1)));

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();

    // QPDF.cc:2423-2430 calls removeObject. QPDF.cc:1996-2005 changes
    // the existing allocation into a floating null, including held aliases.
    assert!(old.is_direct());
    assert!(old.is_null());
    assert_eq!(old.object_ref(), None);
    assert_eq!(current.object_ref(), Some(ObjectRef::new(3, 1)));
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn generated_stale_generation_arrays_match_qpdf_in_both_visit_orders() {
    use std::process::Command;
    for reversed in [false, true] {
        let mut source = include_bytes!(
            "../../../tests/fixtures/compat/compressible-stale-generation-alias.pdf"
        )
        .to_vec();
        if reversed {
            let before = b"[3 0 R 3 1 R]";
            let offset = source
                .windows(before.len())
                .position(|window| window == before)
                .unwrap();
            source[offset..offset + before.len()].copy_from_slice(b"[3 1 R 3 0 R]");
        }
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.pdf");
        let expected = temp.path().join("qpdf.pdf");
        std::fs::write(&input, &source).unwrap();
        let status = Command::new("qpdf")
            .args(["--static-id", "--object-streams=generate"])
            .arg(&input)
            .arg(&expected)
            .status()
            .unwrap();
        assert!(status.success());
        let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        assert_eq!(
            writer.get_buffer().unwrap(),
            std::fs::read(expected).unwrap()
        );
    }
}
