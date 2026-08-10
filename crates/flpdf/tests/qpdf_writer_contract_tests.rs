use std::fs::File;
use std::io::{BufRead, BufReader, Cursor};
use std::process::Command;

use flpdf::{write_pdf, DecodeLevel, ObjectRef, ObjectStreamMode, Pdf, QPDFWriter, StreamDataMode};

fn open_minimal_pdf() -> flpdf::Result<Pdf<BufReader<File>>> {
    let file = File::open("../../tests/fixtures/minimal.pdf")?;
    Pdf::open(BufReader::new(file))
}

fn qpdf_11_9_0() -> flpdf::Result<()> {
    let output = Command::new("qpdf").arg("--version").output()?;
    assert!(output.status.success(), "qpdf --version failed: {output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("11.9.0"),
        "unexpected qpdf version output: {text}"
    );
    Ok(())
}

#[test]
fn write_before_output_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    assert!(writer.write().is_err());
    Ok(())
}

#[test]
fn get_buffer_before_write_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn output_configuration_can_happen_only_once() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    assert!(writer.set_output_memory().is_err());
    Ok(())
}

#[test]
fn write_can_happen_only_once() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    assert!(writer.write().is_err());
    Ok(())
}

#[test]
fn get_buffer_from_non_memory_writer_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_writer(Cursor::new(Vec::new()))?;
    writer.write()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn get_buffer_after_first_retrieval_returns_err() -> flpdf::Result<()> {
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let _ = writer.get_buffer()?;
    assert!(writer.get_buffer().is_err());
    Ok(())
}

#[test]
fn qpdf_checks_qpdf_writer_memory_output() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let buffer = writer.get_buffer()?;

    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("rewrite.pdf");
    std::fs::write(&output_path, buffer)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn qpdf_writer_full_rewrite_removes_prev_from_incremental_source() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let source = std::fs::read("../../tests/fixtures/minimal.pdf")?;
    let mut incremental_pdf = Pdf::open(Cursor::new(source))?;
    let root_ref = incremental_pdf.root_ref().expect("minimal.pdf has /Root");
    let root = incremental_pdf.resolve(root_ref)?;
    incremental_pdf.set_object(root_ref, root);
    let mut incremental = Vec::new();
    write_pdf(&mut incremental_pdf, &mut incremental)?;
    assert!(incremental.windows(5).any(|window| window == b"/Prev"));

    let mut rewritten_pdf = Pdf::open(Cursor::new(incremental))?;
    let mut writer = QPDFWriter::new(&mut rewritten_pdf);
    writer.set_output_memory()?;
    writer.write()?;
    let output = writer.get_buffer()?;
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));

    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("full-rewrite.pdf");
    std::fs::write(&output_path, output)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
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
