//! qpdf writeObject progress precedes unparse of the live object.

use flpdf::{ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;

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
