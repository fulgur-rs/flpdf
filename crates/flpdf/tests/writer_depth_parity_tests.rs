//! qpdf-compatible writer behavior for programmatically built direct graphs.

use flpdf::{ObjectHandle, ObjectStreamMode, Pdf, PdfWriter};

fn nested_direct_array(depth: usize) -> ObjectHandle {
    let mut nested = ObjectHandle::integer(1);
    for _ in 0..depth {
        nested = ObjectHandle::array(vec![nested]);
    }
    nested
}

fn serialized_deep_value_depth(output: &[u8]) -> usize {
    let key_start = output
        .windows(b"/Deep".len())
        .position(|window| window == b"/Deep")
        .expect("writer output retains the /Deep dictionary entry");
    let value_start = key_start + b"/Deep".len();
    let array_start = value_start
        + output[value_start..]
            .iter()
            .position(|&byte| byte == b'[')
            .expect("/Deep value starts with an array");

    let mut current_depth = 0;
    let mut maximum_depth = 0;
    for &byte in &output[array_start..] {
        match byte {
            b'[' => {
                current_depth += 1;
                maximum_depth = maximum_depth.max(current_depth);
            }
            b']' => {
                current_depth -= 1;
                if current_depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    assert_eq!(current_depth, 0, "/Deep array output is balanced");
    maximum_depth
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
    assert_eq!(serialized_deep_value_depth(&output), 502);
}

#[test]
fn linearized_writer_serializes_502_programmatic_direct_array_levels() {
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
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_linearization(true);
    writer
        .write()
        .expect("qpdf linearized writer has no direct nesting cap");

    let output = writer.get_buffer().expect("completed write retains output");
    assert!(output.starts_with(b"%PDF-"));
    assert_eq!(serialized_deep_value_depth(&output), 502);
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
    assert_eq!(serialized_deep_value_depth(&output), 502);
}
