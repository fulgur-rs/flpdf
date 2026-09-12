//! qpdf getCompressibleObjGens removes stale generations from the live graph.

use flpdf::{EncryptParams, ObjectHandle, ObjectRef, ObjectStreamMode, Pdf, PdfWriter};
use std::fs;
use std::io::Cursor;
use std::process::Command;

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

#[test]
fn generate_preserve_does_not_seed_a_removed_generation() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .unwrap();
    let old = pdf
        .get_all_objects()
        .unwrap()
        .into_iter()
        .find(|object| object.as_stream_dict().is_some())
        .unwrap();
    let old_ref = old.object_ref().unwrap();
    pdf.replace_object(
        ObjectRef::new(old_ref.number, old_ref.generation + 1),
        ObjectHandle::integer(42),
    )
    .unwrap();
    let pending = ObjectHandle::array(vec![]);
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/ZPending", pending)
        .unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_preserve_unreferenced_objects(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();

    let actual = writer.get_buffer().unwrap();
    assert!(
        !actual
            .windows(b"\nnull\nendobj\n".len())
            .any(|window| { window == b"\nnull\nendobj\n" }),
        "qpdf removes the superseded generation instead of seeding an indirect null"
    );
}

#[test]
fn repeated_generate_preserve_does_not_resurrect_a_removed_generation() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .unwrap();
    let old = pdf
        .get_all_objects()
        .unwrap()
        .into_iter()
        .find(|object| object.as_stream_dict().is_some())
        .unwrap();
    let old_ref = old.object_ref().unwrap();
    pdf.replace_object(
        ObjectRef::new(old_ref.number, old_ref.generation + 1),
        ObjectHandle::integer(42),
    )
    .unwrap();
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/ZPending", ObjectHandle::array(vec![]))
        .unwrap();

    for write_index in 0..2 {
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
        writer.set_preserve_unreferenced_objects(true);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        let output = writer.get_buffer().unwrap();
        let null_count = output
            .windows(b"\nnull\nendobj\n".len())
            .filter(|window| *window == b"\nnull\nendobj\n")
            .count();
        // QPDFWriter keeps each prior makeIndirectObject(newNull()) in the
        // document cache after writer destruction; the next preserve write
        // therefore emits exactly the prior placeholder count. The removed
        // generation itself must still never reappear as the replacement 42.
        assert_eq!(null_count, write_index);
        assert!(!output
            .windows(b" 42\n".len())
            .any(|window| window == b" 42\n"));
    }
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn specialized_preserve_encryption_removes_stale_generation_like_qpdf() {
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("input.pdf");
    let qpdf_output = temporary.path().join("qpdf.pdf");
    fs::write(
        &input,
        include_bytes!("../../../tests/fixtures/compat/null-visible-stale-generation-objstm.pdf"),
    )
    .unwrap();
    let qpdf = Command::new("qpdf")
        .args([
            "--static-id",
            "--static-aes-iv",
            "--object-streams=preserve",
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "qpdf preserve/encrypt failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let mut pdf = Pdf::open(Cursor::new(fs::read(&input).unwrap())).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Preserve);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(b"u", b"o"));
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();

    assert_eq!(
        writer.get_buffer().unwrap(),
        fs::read(&qpdf_output).unwrap(),
        "specialized Preserve must carry qpdf's stale-generation removal into encrypted output"
    );
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
