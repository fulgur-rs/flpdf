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
fn qdf_and_normalize_progress_mutations_are_visible_before_child_discovery() {
    // QPDFWriter::writeObject reports progress before the mode-specific
    // unparseObject body. QDF and normalize-content must keep that same live
    // boundary after leaving the planned emitter.
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let root = pdf.root_handle().unwrap();
        let child = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/QdfProgressChild".to_vec(),
                ObjectHandle::integer(42),
            )]))
            .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_extra_header_text("% qdf-normalize-live\n");
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                root.replace_key(b"/QdfProgressProbe", child.clone())?;
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} live write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"/QdfProgressProbe".len())
                .any(|window| window == b"/QdfProgressProbe"),
            "qdf={qdf} must retain the callback mutation"
        );
        assert!(
            output
                .windows(b"/QdfProgressChild 42".len())
                .any(|window| window == b"/QdfProgressChild 42"),
            "qdf={qdf} must discover the callback child after the root event"
        );
    }
}

#[test]
fn qdf_and_normalize_progress_stream_replacement_uses_live_payload() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let stream = pdf
            .new_stream_with_data(Rc::new(b"before".to_vec()))
            .unwrap();
        pdf.root_handle()
            .unwrap()
            .replace_key(b"/QdfProgressStream", stream.clone())
            .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_compress_streams(false);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                stream.replace_stream_data(Rc::new(b"after".to_vec()), None, None);
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} live stream write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"after".len())
                .any(|window| window == b"after"),
            "qdf={qdf} must emit the callback replacement"
        );
        assert!(!output
            .windows(b"before".len())
            .any(|window| window == b"before"));
    }
}

#[test]
fn qdf_and_normalize_progress_id_replacement_is_read_at_trailer_time() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let trailer = pdf.trailer();
        let replacement = ObjectHandle::array(vec![
            ObjectHandle::string(b"changed-id".to_vec()),
            ObjectHandle::string(b"changed-id".to_vec()),
        ]);
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                trailer.replace_key(b"/ID", replacement.clone())?;
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} late ID write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"<6368616e6765642d6964>".len())
                .any(|window| window == b"<6368616e6765642d6964>"),
            "qdf={qdf} must use the callback's live /ID[0]"
        );
    }
}

#[test]
fn qdf_and_normalize_progress_id_deletion_does_not_restore_the_setup_id() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/nonid-id0.pdf").to_vec(),
        ))
        .unwrap();
        let trailer = pdf.trailer();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                trailer.remove_key(b"/ID");
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} ID deletion write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"<31415926535897932384626433832795>".len())
                .any(|window| window == b"<31415926535897932384626433832795>"),
            "qdf={qdf} must generate a new static ID after callback deletion"
        );
        assert!(!output
            .windows(b"<aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa>".len())
            .any(|window| window == b"<aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa>"));
    }
}

#[test]
fn qdf_and_normalize_progress_trailer_child_gets_a_late_number() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let trailer = pdf.trailer();
        let child = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/LateTrailerChild".to_vec(),
                ObjectHandle::integer(42),
            )]))
            .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                trailer.replace_key(b"/Z", child.clone())?;
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} late trailer write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output.windows(b"/Z ".len()).any(|window| window == b"/Z "),
            "qdf={qdf} must serialize the callback's trailer child"
        );
    }
}

#[test]
fn qdf_late_trailer_streams_reserve_holders_and_ignore_xref_streams() {
    for xref in [false, true] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap();
        let trailer = pdf.trailer();
        let late_stream = pdf.new_stream_with_data(Rc::new(b"late".to_vec())).unwrap();
        if xref {
            late_stream
                .as_stream_dict()
                .unwrap()
                .replace_key(b"/Type", ObjectHandle::name(b"XRef".to_vec()))
                .unwrap();
        }
        let late_integer = pdf
            .make_indirect_object_handle(ObjectHandle::integer(42))
            .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(true);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                trailer.replace_key(b"/Y", late_stream.clone())?;
                trailer.replace_key(b"/Z", late_integer.clone())?;
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("xref={xref} late stream write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        if xref {
            assert!(output
                .windows(b"/Y 0 0 R".len())
                .any(|window| window == b"/Y 0 0 R"));
            assert!(output
                .windows(b"/Z 3 0 R".len())
                .any(|window| window == b"/Z 3 0 R"));
        } else {
            assert!(output
                .windows(b"/Y 3 0 R".len())
                .any(|window| window == b"/Y 3 0 R"));
            assert!(output
                .windows(b"/Z 5 0 R".len())
                .any(|window| window == b"/Z 5 0 R"));
        }
    }
}

#[test]
fn qdf_and_normalize_progress_direct_root_child_gets_a_late_number() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
        ))
        .unwrap();
        let root = pdf.root_handle().unwrap();
        let child = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/LateDirectRootChild".to_vec(),
                ObjectHandle::integer(42),
            )]))
            .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                root.replace_key(b"/LateDirectRoot", child.clone())?;
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} direct-root write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"/LateDirectRoot 3 0 R".len())
                .any(|window| window == b"/LateDirectRoot 3 0 R"),
            "qdf={qdf} must serialize the callback's direct-root child"
        );
        assert!(!output
            .windows(b"3 0 obj\n".len())
            .any(|window| window == b"3 0 obj\n"));
    }
}

#[test]
fn legacy_direct_root_is_copied_after_body_progress_callbacks() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
        ))
        .unwrap();
        let root = pdf.root_handle().unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_extra_header_text("% legacy direct-root callback\n");
        writer.set_static_id(true);
        writer.set_output_memory().unwrap();
        writer.register_progress_reporter(Box::new(move |percent| {
            if percent == 0 {
                root.replace_key(b"/CallbackMarker", ObjectHandle::integer(42))?;
            }
            Ok(())
        }));
        writer
            .write()
            .unwrap_or_else(|error| panic!("qdf={qdf} legacy direct-root write failed: {error}"));
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"/CallbackMarker 42".len())
                .any(|window| window == b"/CallbackMarker 42"),
            "qdf={qdf} must copy direct Root after body progress"
        );
    }
}

#[test]
fn encrypted_normalize_root_progress_failure_precedes_adbe_reconciliation() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-ext-indirect.pdf").to_vec(),
    ))
    .unwrap();
    let extensions = pdf
        .root_handle()
        .unwrap()
        .try_get_key(b"/Extensions")
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_content_normalization(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.force_pdf_version("1.7", 8);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(b"u", b"o"));
    writer.set_output_memory().unwrap();
    writer.register_progress_reporter(Box::new(|percent| {
        if percent == 0 {
            return Err(flpdf::Error::System("root progress failure".into()));
        }
        Ok(())
    }));

    let error = writer.write().unwrap_err();
    assert!(matches!(
        error,
        flpdf::Error::System(ref message) if message == "root progress failure"
    ));
    assert_eq!(
        extensions
            .try_get_key(b"/ADBE")
            .unwrap()
            .try_get_key(b"/ExtensionLevel")
            .unwrap()
            .try_get_int_value()
            .unwrap(),
        3,
        "a failed Root progress event must not leave output-only ADBE state"
    );
}

#[test]
fn encrypted_qdf_live_root_maps_an_extraneous_xref_child_to_null() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let xref = pdf.new_stream_with_data(Rc::new(Vec::new())).unwrap();
    xref.as_stream_dict()
        .unwrap()
        .replace_key(b"/Type", ObjectHandle::name(b"XRef".to_vec()))
        .unwrap();
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/ExtraneousXRef", xref)
        .unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_qdf_mode(true);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.force_pdf_version("1.7", 8);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(b"u", b"o"));
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/ExtraneousXRef 0 0 R".len())
        .any(|window| window == b"/ExtraneousXRef 0 0 R"));
    assert!(!output
        .windows(b"/Type /XRef".len())
        .any(|window| window == b"/Type /XRef"));
}

#[test]
fn encrypted_qdf_and_normalize_encrypt_direct_page_dictionary_strings() {
    for qdf in [true, false] {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/qdf-contents-ref-array.pdf").to_vec(),
        ))
        .unwrap();
        let page = pdf.get_object_handle(flpdf::ObjectRef::new(3, 0));
        let stream = pdf.new_stream_with_data(Rc::new(b"q Q".to_vec())).unwrap();
        page.replace_key(b"/Contents", ObjectHandle::array(vec![stream]))
            .unwrap();
        page.replace_key(
            b"/PieceInfo",
            ObjectHandle::dictionary(vec![(
                b"/App".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/Private".to_vec(),
                    ObjectHandle::string(b"SecretPageData".to_vec()),
                )]),
            )]),
        )
        .unwrap();

        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_qdf_mode(qdf);
        writer.set_content_normalization(!qdf);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_compress_streams(false);
        writer.set_static_id(true);
        writer.set_static_aes_iv(true);
        writer.force_pdf_version("1.7", 8);
        writer.set_encryption_parameters(EncryptParams::v4_aes128(b"u", b"o"));
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        let output = writer.get_buffer().unwrap();
        assert!(
            output
                .windows(b"/PieceInfo".len())
                .any(|window| window == b"/PieceInfo"),
            "qdf={qdf} must retain the page dictionary"
        );
        assert!(
            !output
                .windows(b"SecretPageData".len())
                .any(|window| window == b"SecretPageData"),
            "qdf={qdf} direct page dictionary strings must be encrypted"
        );
    }
}

#[test]
fn qdf_discovery_walks_a_direct_stream_dictionary_child() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let direct_stream = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Length".to_vec(), ObjectHandle::integer(4)),
            (
                b"/DirectQdfLabel".to_vec(),
                ObjectHandle::string(b"direct".to_vec()),
            ),
        ]),
        Rc::new(b"data".to_vec()),
    );
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/DirectQdfStream", direct_stream)
        .unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/DirectQdfStream".len())
        .any(|window| window == b"/DirectQdfStream"));
    assert!(output
        .windows(b"/DirectQdfLabel".len())
        .any(|window| window == b"/DirectQdfLabel"));
}

#[test]
fn qdf_crypt_cleanup_is_single_pass_for_stream_dictionary_state() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
    ))
    .unwrap();
    let stream = pdf.new_stream_with_data(Rc::new(b"raw".to_vec())).unwrap();
    let dictionary = stream.as_stream_dict().unwrap();
    dictionary
        .replace_key(
            b"/Filter",
            ObjectHandle::array(vec![ObjectHandle::name(b"Crypt".to_vec())]),
        )
        .unwrap();
    dictionary
        .replace_key(
            b"/DecodeParms",
            ObjectHandle::array(vec![ObjectHandle::dictionary(Vec::new())]),
        )
        .unwrap();
    stream.set_filter_on_write(false).unwrap();
    pdf.root_handle()
        .unwrap()
        .replace_key(b"/QdfCryptProbe", stream)
        .unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    let stream_ref = rewritten
        .root_handle()
        .unwrap()
        .try_get_key(b"/QdfCryptProbe")
        .unwrap()
        .object_ref()
        .unwrap();
    let rewritten_stream = rewritten.get_object_handle(stream_ref);
    rewritten_stream.get_raw_stream_data().unwrap();
    let rewritten_dictionary = rewritten_stream.as_stream_dict().unwrap();
    assert!(rewritten_dictionary
        .try_get_key(b"/Filter")
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert!(rewritten_dictionary
        .try_get_key(b"/DecodeParms")
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
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
