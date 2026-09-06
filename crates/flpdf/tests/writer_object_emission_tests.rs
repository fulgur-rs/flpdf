//! qpdf writeObject progress precedes unparse of the live object.

use flpdf::{ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};
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
