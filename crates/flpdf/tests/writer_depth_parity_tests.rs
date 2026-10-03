//! qpdf-compatible writer behavior for programmatically built direct graphs.

use flpdf::{ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};

fn nested_direct_array(depth: usize) -> ObjectHandle {
    let mut nested = ObjectHandle::integer(1);
    for _ in 0..depth {
        nested = ObjectHandle::array(vec![nested]);
    }
    nested
}

#[test]
fn standard_writer_accepts_502_programmatic_direct_array_levels() {
    let mut pdf = Pdf::empty().expect("create an empty writer document");
    pdf.root_handle()
        .expect("get Catalog")
        .replace_key(b"/Deep", nested_direct_array(502))
        .expect("install programmatic direct graph");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("select memory output");
    writer
        .write()
        .expect("qpdf QPDFWriter::unparseObject has no 500-level writer cap");

    let output = writer.get_buffer().expect("completed write retains output");
    assert!(output.starts_with(b"%PDF-"));
    assert!(output.len() > 2_000);
}

#[test]
fn linearized_generate_accepts_502_programmatic_direct_array_levels() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    let mut pdf = Pdf::open_mem_owned(std::fs::read(fixture).expect("read one-page fixture"))
        .expect("open one-page fixture in memory");
    pdf.root_handle()
        .expect("get Catalog")
        .replace_key(b"/Deep", nested_direct_array(502))
        .expect("install programmatic direct graph");

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("select memory output");
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_linearization(true);
    writer
        .write()
        .expect("qpdf linearized writer has no direct nesting cap");

    let output = writer.get_buffer().expect("completed write retains output");
    assert!(output.starts_with(b"%PDF-"));
    assert!(output.len() > 3_000);
}
