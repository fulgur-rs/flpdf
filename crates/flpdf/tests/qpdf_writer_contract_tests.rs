use std::fs::File;
use std::io::{BufRead, BufReader, Cursor};

use flpdf::{DecodeLevel, ObjectRef, ObjectStreamMode, Pdf, QPDFWriter, StreamDataMode};

fn open_minimal_pdf() -> flpdf::Result<Pdf<BufReader<File>>> {
    let file = File::open("../../tests/fixtures/minimal.pdf")?;
    Pdf::open(BufReader::new(file))
}

#[test]
fn memory_full_rewrite_has_fresh_output() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.set_static_id(true);
    writer.write()?;

    let buffer = writer.get_buffer()?;
    assert!(buffer.starts_with(b"%PDF-"));
    assert!(!buffer.windows(5).any(|window| window == b"/Prev"));

    Ok(())
}

#[test]
fn final_version_is_available_before_write() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    let version = writer.get_final_version()?;
    writer.set_output_memory()?;
    writer.write()?;

    let buffer = writer.get_buffer()?;
    let mut first_line = String::new();
    BufReader::new(Cursor::new(buffer)).read_line(&mut first_line)?;
    assert!(first_line.contains(&version));

    Ok(())
}

#[test]
fn qpdf_setter_surface_compiles() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Disable);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.set_decode_level(DecodeLevel::Generalized);
    writer.set_compress_streams(false);
    writer.set_recompress_flate(true);
    writer.set_content_normalization(false);
    writer.set_qdf_mode(true);
    writer.set_preserve_unreferenced_objects(true);
    writer.set_newline_before_endstream(false);
    writer.set_deterministic_id(true);
    writer.set_static_id(true);
    writer.set_linearization(false);
    writer.set_output_memory()?;

    Ok(())
}

#[test]
fn writer_result_surface_compiles() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;

    let _ = writer.get_renumbered_obj_gen(ObjectRef::new(1, 0));
    let _ = writer.get_written_xref_table();

    Ok(())
}
