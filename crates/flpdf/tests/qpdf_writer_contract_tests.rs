use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::process::Command;
use std::rc::Rc;

use flpdf::pipeline::{Pipeline, PipelineResult};
use flpdf::{write_pdf, DecodeLevel, ObjectRef, ObjectStreamMode, Pdf, QPDFWriter, StreamDataMode};

#[derive(Clone)]
struct SharedBytes(Rc<RefCell<Vec<u8>>>);

impl Write for SharedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct RecordingPipeline {
    bytes: Rc<RefCell<Vec<u8>>>,
    writes: Rc<RefCell<usize>>,
    finishes: Rc<RefCell<usize>>,
}

impl Pipeline for RecordingPipeline {
    fn identifier(&self) -> &str {
        "qpdf-writer-contract"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        *self.writes.borrow_mut() += 1;
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        *self.finishes.borrow_mut() += 1;
        Ok(())
    }
}

fn open_minimal_pdf() -> flpdf::Result<Pdf<BufReader<File>>> {
    let file = File::open("../../tests/fixtures/minimal.pdf")?;
    Pdf::open(BufReader::new(file))
}

fn qpdf_11_9_0() -> flpdf::Result<()> {
    let output = Command::new("qpdf").arg("--version").output()?;
    assert!(output.status.success(), "qpdf --version failed: {output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let version_line = text
        .lines()
        .find(|line| line.starts_with("qpdf version "))
        .expect("qpdf --version must report a standard version line");
    let reported_version = version_line
        .split_whitespace()
        .nth(2)
        .expect("qpdf version line must contain a version token");
    assert_eq!(
        reported_version, "11.9.0",
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

#[test]
fn set_output_file_writes_a_fresh_qpdf_checked_pdf() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("output-file.pdf");
    writer.set_output_file(&output_path)?;
    writer.write()?;

    let output = std::fs::read(&output_path)?;
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn set_output_writer_writes_to_an_owned_sink() -> flpdf::Result<()> {
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_writer(SharedBytes(Rc::clone(&bytes)))?;
    writer.write()?;

    let output = bytes.borrow();
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));
    Ok(())
}

#[test]
fn set_output_pipeline_writes_and_finishes_once() -> flpdf::Result<()> {
    qpdf_11_9_0()?;
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let writes = Rc::new(RefCell::new(0));
    let finishes = Rc::new(RefCell::new(0));
    let mut pdf = open_minimal_pdf()?;
    let mut writer = QPDFWriter::new(&mut pdf);
    writer.set_output_pipeline(RecordingPipeline {
        bytes: Rc::clone(&bytes),
        writes: Rc::clone(&writes),
        finishes: Rc::clone(&finishes),
    })?;
    writer.write()?;

    let output = bytes.borrow();
    assert_eq!(*writes.borrow(), 1);
    assert_eq!(*finishes.borrow(), 1);
    assert!(output.starts_with(b"%PDF-"));
    assert!(!output.windows(5).any(|window| window == b"/Prev"));
    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("pipeline.pdf");
    std::fs::write(&output_path, &*output)?;
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&output_path)
        .output()?;
    assert!(check.status.success(), "qpdf --check failed: {check:?}");
    Ok(())
}

#[test]
fn pdf_version_setters_are_reflected_in_the_output_header() -> flpdf::Result<()> {
    for (setter, expected) in [("minimum", "%PDF-1.7"), ("force", "%PDF-1.5")] {
        let mut pdf = open_minimal_pdf()?;
        let mut writer = QPDFWriter::new(&mut pdf);
        match setter {
            "minimum" => writer.set_minimum_pdf_version("1.7", 0)?,
            "force" => writer.force_pdf_version("1.5", 0)?,
            _ => unreachable!(),
        }
        writer.set_output_memory()?;
        writer.write()?;
        let output = writer.get_buffer()?;
        assert!(output.starts_with(expected.as_bytes()));
    }
    Ok(())
}
