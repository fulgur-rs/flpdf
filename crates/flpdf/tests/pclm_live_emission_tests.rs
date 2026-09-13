//! PCLm must discover callback-added children at qpdf's writeObject boundary.

use flpdf::{ObjectHandle, Pdf, PdfWriter};
use std::io::Cursor;
use std::rc::Rc;

#[test]
fn pclm_progress_callback_child_is_discovered_by_the_live_queue() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .unwrap();
    let page_ref = pdf
        .root_handle()
        .unwrap()
        .try_get_key(b"/Pages")
        .unwrap()
        .try_get_key(b"/Kids")
        .unwrap()
        .try_get_array_item(0)
        .unwrap()
        .object_ref()
        .expect("fixture page is indirect");
    let page = pdf.get_object_handle(page_ref);
    let child = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/PclmLiveChild".to_vec(),
            ObjectHandle::integer(42),
        )]))
        .unwrap();
    page.replace_key(
        b"/PclmDirectStream",
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(999))]),
            Rc::new(b"pclm-direct".to_vec()),
        ),
    )
    .unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    let mut called = false;
    writer.register_progress_reporter(Box::new(move |_percent| {
        if !called {
            called = true;
            page.replace_key(b"/PclmLiveProbe", child.clone())?;
        }
        Ok(())
    }));
    writer
        .write()
        .expect("PCLm must discover callback children at emission time");
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/PclmLiveProbe".len())
        .any(|window| window == b"/PclmLiveProbe"));
    assert!(output
        .windows(b"/PclmLiveChild 42".len())
        .any(|window| window == b"/PclmLiveChild 42"));
    assert!(output
        .windows(b"stream\npclm-directendstream".len())
        .any(|window| window == b"stream\npclm-directendstream"));
}

#[test]
fn pclm_does_not_consume_a_late_number_for_an_indirect_null_trailer_value() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .unwrap();
    let null = pdf
        .make_indirect_object_handle(ObjectHandle::null())
        .unwrap();
    let info = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/Producer".to_vec(),
            ObjectHandle::string(b"late-info".to_vec()),
        )]))
        .unwrap();
    pdf.trailer().replace_key(b"/A", null).unwrap();
    pdf.trailer().replace_key(b"/Info", info).unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();

    assert!(!output.windows(b"/A ".len()).any(|window| window == b"/A "));
    assert!(output
        .windows(b"/Info 7 0 R".len())
        .any(|window| window == b"/Info 7 0 R"));
}

#[test]
fn pclm_progress_callback_id_update_is_used_by_the_late_trailer() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .unwrap();
    let trailer = pdf.trailer();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    let mut called = false;
    writer.register_progress_reporter(Box::new(move |_percent| {
        if !called {
            called = true;
            trailer.replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"changed".to_vec()),
                    ObjectHandle::string(b"changed".to_vec()),
                ]),
            )?;
        }
        Ok(())
    }));
    writer.write().unwrap();
    let output = writer.get_buffer().unwrap();
    let mut rewritten = Pdf::open(Cursor::new(output.to_vec())).unwrap();
    let id = rewritten
        .trailer()
        .try_get_key(b"/ID")
        .unwrap()
        .try_get_array_item(0)
        .unwrap()
        .as_string()
        .unwrap();
    assert_eq!(id, b"changed");
}

#[test]
fn pclm_direct_root_progress_child_gets_qpdf_late_numbering() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let child = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/LateRootChild".to_vec(),
            ObjectHandle::integer(42),
        )]))
        .unwrap();
    root.replace_key(
        b"/PclmRootLabel",
        ObjectHandle::string(b"direct-root".to_vec()),
    )
    .unwrap();
    let trailer_child = pdf
        .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
            b"/TrailerChild".to_vec(),
            ObjectHandle::integer(7),
        )]))
        .unwrap();
    pdf.trailer().replace_key(b"/Z", trailer_child).unwrap();

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    let mut called = false;
    writer.register_progress_reporter(Box::new(move |_percent| {
        if !called {
            called = true;
            root.replace_key(b"/LateRootChild", child.clone())?;
        }
        Ok(())
    }));
    writer
        .write()
        .expect("direct-root callback child must use qpdf late numbering");
    let output = writer.get_buffer().unwrap();
    assert!(output
        .windows(b"/LateRootChild 3 0 R".len())
        .any(|window| window == b"/LateRootChild 3 0 R"));
    assert!(output
        .windows(b"/Z 4 0 R".len())
        .any(|window| window == b"/Z 4 0 R"));
    assert!(!output
        .windows(b"3 0 obj\n".len())
        .any(|window| window == b"3 0 obj\n"));
}

#[test]
fn pclm_preserves_external_file_trailer_keys() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/trailer-external-file-keys.pdf").to_vec(),
    ))
    .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().expect("PCLm write");

    let mut output = Pdf::open(Cursor::new(writer.get_buffer().unwrap())).unwrap();
    let trailer = output.trailer();
    let file_ref = trailer
        .try_get_key(b"/F")
        .unwrap()
        .object_ref()
        .expect("PCLm trailer /F must remain indirect");
    assert!(file_ref.number > 0);
    // PCLm's qpdf seed is page/contents/strip/root-only. The trailer is
    // written after that queue drains, so `unparseChild` assigns this late
    // reference without adding a body object (`QPDFWriter.cc:2928-2954,
    // 1144-1157`).
    let file_object = output.get_object_handle(file_ref);
    file_object.try_is_scalar().unwrap();
    assert!(file_object.is_null());
    assert_eq!(
        trailer.try_get_key(b"/FFilter").unwrap().unparse(),
        b"/ASCIIHexDecode"
    );
    assert_eq!(
        trailer.try_get_key(b"/FDecodeParms").unwrap().unparse(),
        b"<< /Columns 1 >>"
    );
}
