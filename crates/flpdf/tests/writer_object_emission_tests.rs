//! qpdf writeObject progress precedes unparse of the live object.

use flpdf::{EncryptParams, ObjectHandle, ObjectStreamMode, Pdf, PdfOpenOptions, PdfWriter};
use std::io::Cursor;
use std::rc::Rc;

#[test]
fn progress_callback_mutation_is_visible_in_the_current_root_output() {
    // QPDFWriter.cc:1772 reports progress before unparseObject at :1794.
    // A qpdf 11.9.0 FunctionProgressReporter that adds this key at 0%
    // emits /ProgressProbe 42 in the Catalog on the same write.
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.register_progress_reporter(Box::new(move |percent| {
        if percent == 0 {
            root.replace_key(b"/ProgressProbe", ObjectHandle::integer(42))?;
        }
        Ok(())
    }));
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/ProgressProbe 42".len())
        .any(|window| window == b"/ProgressProbe 42"));
}

#[test]
fn progress_callback_stream_replacement_invalidates_the_planned_payload() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"before".to_vec()))
        .unwrap();
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/ProbeStream", stream.clone())
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_output_memory().unwrap();
    writer.register_progress_reporter(Box::new(move |percent| {
        if percent == 0 {
            stream.replace_stream_data(Rc::new(b"after".to_vec()), None, None);
        }
        Ok(())
    }));
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"stream\nafter".len())
        .any(|window| window == b"stream\nafter"));
    assert!(!output
        .windows(b"before".len())
        .any(|window| window == b"before"));
}

#[test]
fn progress_callback_attaches_a_new_indirect_child_to_the_live_root() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let child = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/ProgressChild".to_vec(),
            ObjectHandle::integer(42),
        )]))
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.register_progress_reporter(Box::new(move |percent| {
        if percent == 0 {
            root.replace_key(b"/ProgressProbeRef", child.clone())?;
        }
        Ok(())
    }));
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/ProgressProbeRef".len())
        .any(|window| window == b"/ProgressProbeRef"));
    assert!(output
        .windows(b"/ProgressChild 42".len())
        .any(|window| window == b"/ProgressChild 42"));
}

#[test]
fn specialized_progress_callback_attaches_a_new_child_to_a_future_object() {
    // qpdf's writeObject reports progress before unparseObject.  The
    // specialized non-linearized route must keep that same live queue
    // contract: a callback may mutate an object that is still waiting in the
    // queue, and the newly observed child is numbered at unparseChild time.
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let pages_ref = root
        .try_get_key(b"/Pages")
        .unwrap()
        .object_ref()
        .expect("fixture has an indirect Pages object");
    let pages = pdf.get_object_handle(pages_ref);
    let child = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/SpecializedProgressChild".to_vec(),
            ObjectHandle::integer(42),
        )]))
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_extra_header_text("% specialized-live-queue");
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    let mut called = false;
    writer.register_progress_reporter(Box::new(move |_percent| {
        if !called {
            called = true;
            pages.replace_key(b"/SpecializedProgressProbe", child.clone())?;
        }
        Ok(())
    }));
    writer
        .write()
        .expect("specialized writer must discover callback children live");
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/SpecializedProgressProbe".len())
        .any(|window| window == b"/SpecializedProgressProbe"));
    assert!(output
        .windows(b"/SpecializedProgressChild 42".len())
        .any(|window| window == b"/SpecializedProgressChild 42"));
}

#[test]
fn specialized_encrypted_live_queue_discovers_callback_children_in_each_mode() {
    for object_streams in [
        ObjectStreamMode::Disable,
        ObjectStreamMode::Preserve,
        ObjectStreamMode::Generate,
    ] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let root = pdf.root_handle().unwrap();
        let pages_ref = root
            .try_get_key(b"/Pages")
            .unwrap()
            .object_ref()
            .expect("fixture has an indirect Pages object");
        let pages = pdf.get_object_handle(pages_ref);
        let child = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (
                    b"/EncryptedSpecializedChild".to_vec(),
                    ObjectHandle::integer(42),
                ),
                (
                    b"/EncryptedSpecializedString".to_vec(),
                    ObjectHandle::string(b"dynamic-string".to_vec()),
                ),
            ]))
            .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(object_streams);
        writer.set_encryption_parameters(EncryptParams::v4_aes128(b"u", b"o"));
        writer.set_static_id(true);
        writer.set_static_aes_iv(true);
        writer.set_output_memory().unwrap();
        let mut called = false;
        writer.register_progress_reporter(Box::new(move |_percent| {
            if !called {
                called = true;
                pages.replace_key(b"/EncryptedSpecializedProbe", child.clone())?;
            }
            Ok(())
        }));
        writer.write().unwrap_or_else(|error| {
            panic!("encrypted specialized mode {object_streams:?}: {error}")
        });
        let output = writer.get_buffer().unwrap();
        let mut reopened = Pdf::open_with_options(
            Cursor::new(output),
            PdfOpenOptions {
                password: b"u".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();
        let rewritten_root = reopened.root_handle().unwrap();
        let rewritten_pages_ref = rewritten_root
            .try_get_key(b"/Pages")
            .unwrap()
            .object_ref()
            .expect("rewritten fixture has an indirect Pages object");
        let rewritten_pages = reopened.get_object_handle(rewritten_pages_ref);
        assert!(
            rewritten_pages
                .try_get_key(b"/EncryptedSpecializedProbe")
                .unwrap()
                .object_ref()
                .is_some(),
            "encrypted specialized mode {object_streams:?} must retain the callback child"
        );
        let child_ref = rewritten_pages
            .try_get_key(b"/EncryptedSpecializedProbe")
            .unwrap()
            .object_ref()
            .expect("callback child remains indirect");
        let rewritten_child = reopened.get_object_handle(child_ref);
        assert_eq!(
            rewritten_child
                .try_get_key(b"/EncryptedSpecializedString")
                .unwrap()
                .as_string(),
            Some(b"dynamic-string".to_vec()),
            "encrypted specialized mode {object_streams:?} must encrypt/decrypt dynamic child strings"
        );
    }
}

#[test]
fn specialized_generate_preserve_unreferenced_uses_setup_snapshot_and_deterministic_id() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_preserve_unreferenced_objects(true);
    writer.set_extra_header_text("% specialized-live-queue");
    writer.set_deterministic_id(true);
    writer.set_output_memory().unwrap();
    writer
        .write()
        .expect("specialized Generate with preserved objects succeeds");
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/Type /ObjStm".len())
        .any(|window| window == b"/Type /ObjStm"));
    assert!(output
        .windows(b"/ID [<".len())
        .any(|window| window == b"/ID [<"));
}

#[test]
fn specialized_objstm_member_progress_mutation_is_visible_on_the_second_pass() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let pages_ref = root
        .try_get_key(b"/Pages")
        .unwrap()
        .object_ref()
        .expect("fixture has an indirect Pages object");
    let pages = pdf.get_object_handle(pages_ref);
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_compress_streams(false);
    writer.set_extra_header_text("% specialized-live-queue");
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    let mut calls = 0_u8;
    writer.register_progress_reporter(Box::new(move |_percent| {
        calls = calls.saturating_add(1);
        if calls == 2 {
            pages.replace_key(b"/MemberProgressProbe", ObjectHandle::integer(42))?;
        }
        Ok(())
    }));
    writer
        .write()
        .expect("specialized Generate member progress mutation succeeds");
    let output = writer.get_buffer().unwrap();
    assert!(
        output
            .windows(b"/MemberProgressProbe 42".len())
            .any(|window| window == b"/MemberProgressProbe 42"),
        "a member mutation made before the second-pass unparse must be emitted"
    );
}

#[test]
fn specialized_nested_direct_stream_keeps_payload_and_framing() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let direct_stream = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Length".to_vec(), ObjectHandle::integer(999)),
            (
                b"/DirectStreamLabel".to_vec(),
                ObjectHandle::string(b"nested".to_vec()),
            ),
        ]),
        Rc::new(b"direct-payload".to_vec()),
    );
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/DirectStreamProbe", direct_stream)
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_extra_header_text("% specialized-live-queue");
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer
        .write()
        .expect("specialized direct-stream write succeeds");
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/DirectStreamLabel (nested)".len())
        .any(|window| window == b"/DirectStreamLabel (nested)"));
    assert!(output
        .windows(b"stream\ndirect-payloadendstream".len())
        .any(|window| window == b"stream\ndirect-payloadendstream"));
}

#[test]
fn specialized_encrypted_nested_direct_stream_keeps_payload_and_framing() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    pdf.root_handle()
        .unwrap()
        .replace_key(
            b"/EncryptedDirectStreamProbe",
            ObjectHandle::stream(
                ObjectHandle::dictionary(vec![
                    (b"/Length".to_vec(), ObjectHandle::integer(999)),
                    (
                        b"/DirectStreamLabel".to_vec(),
                        ObjectHandle::string(b"encrypted-nested".to_vec()),
                    ),
                ]),
                Rc::new(b"encrypted-direct-payload".to_vec()),
            ),
        )
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_extra_header_text("% specialized-live-queue");
    let mut encryption = EncryptParams::v4_aes128(b"u", b"o");
    encryption.encrypt_metadata = false;
    writer.set_encryption_parameters(encryption);
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.set_output_memory().unwrap();
    writer.write().expect("specialized encrypted stream write");
    let output = writer.get_buffer().unwrap();

    assert!(output
        .windows(b"/Length 48".len())
        .any(|window| window == b"/Length 48"));
    assert!(output
        .windows(b"stream\n".len())
        .any(|window| window == b"stream\n"));
    assert!(output
        .windows(b"endstream /Pages".len())
        .any(|window| window == b"endstream /Pages"));
    assert!(!output
        .windows(b"encrypted-direct-payload".len())
        .any(|window| window == b"encrypted-direct-payload"));
}

#[test]
fn specialized_direct_root_nested_stream_keeps_payload_and_framing() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
    ))
    .unwrap();
    assert!(
        pdf.root_ref().is_none(),
        "fixture must have a direct Catalog"
    );
    pdf.root_handle()
        .unwrap()
        .replace_key(
            b"/DirectRootStreamProbe",
            ObjectHandle::stream(
                ObjectHandle::dictionary(vec![
                    (b"/Length".to_vec(), ObjectHandle::integer(999)),
                    (
                        b"/DirectRootStreamLabel".to_vec(),
                        ObjectHandle::string(b"direct-root".to_vec()),
                    ),
                ]),
                Rc::new(b"direct-root-payload".to_vec()),
            ),
        )
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_compress_streams(false);
    writer.set_extra_header_text("% specialized-live-queue");
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer
        .write()
        .expect("specialized direct-root stream write succeeds");
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/DirectRootStreamLabel (direct-root)".len())
        .any(|window| window == b"/DirectRootStreamLabel (direct-root)"));
    assert!(output
        .windows(b"stream\ndirect-root-payloadendstream".len())
        .any(|window| window == b"stream\ndirect-root-payloadendstream"));
}
