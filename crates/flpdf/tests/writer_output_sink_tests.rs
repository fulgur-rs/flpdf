//! Output-sink failure routing for `PdfWriter`.
//!
//! qpdf wraps its output handle in `Pl_StdioFile("qpdf output", file)`
//! (`QPDFWriter.cc:101-110`), so a failed write names that pipeline and never a
//! file. These tests pin the routes that carry a sink failure out of
//! `PdfWriter::write`.

use flpdf::pipeline::{Pipeline, PipelineError, PipelineResult};
use flpdf::{Error, Pdf, PdfWriter};
use std::cell::RefCell;
use std::io::{Cursor, Write};
use std::rc::Rc;

fn minimal_pdf() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/minimal.pdf"
    ))
    .expect("read the minimal fixture")
}

fn one_page_pdf() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/compat/one-page.pdf"
    ))
    .expect("read the one-page fixture")
}

struct FailOnFinish;

impl Pipeline for FailOnFinish {
    fn identifier(&self) -> &str {
        "fail-on-finish sink"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Err(PipelineError::runtime("sink finish failure"))
    }
}

#[test]
fn a_pipeline_finish_failure_leaves_the_write() {
    let mut pdf = Pdf::open(Cursor::new(minimal_pdf())).expect("open the fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer
        .set_output_pipeline(FailOnFinish)
        .expect("configure the failing sink");

    let error = writer
        .write()
        .expect_err("finish failure must escape write");
    assert!(
        error.to_string().contains("sink finish failure"),
        "{error:?}"
    );
}

#[test]
fn get_buffer_rejects_a_writer_sink() {
    let mut pdf = Pdf::open(Cursor::new(minimal_pdf())).expect("open the fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer
        .set_output_writer(Vec::new())
        .expect("configure a writer sink");
    writer.write().expect("write into the writer sink");

    let error = writer
        .get_buffer()
        .expect_err("only a memory output can hand back a buffer");
    assert!(matches!(&error, Error::Unsupported(message)
        if message.contains("requires a successful memory output")));
}

struct RecordingWriter {
    bytes: Rc<RefCell<Vec<u8>>>,
    write_lengths: Rc<RefCell<Vec<usize>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.write_lengths.borrow_mut().push(data.len());
        self.bytes.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn linearized_writer_streams_final_pass_to_writer_sink() {
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let write_lengths = Rc::new(RefCell::new(Vec::new()));
    let sink = RecordingWriter {
        bytes: Rc::clone(&bytes),
        write_lengths: Rc::clone(&write_lengths),
    };
    let mut pdf = Pdf::open(Cursor::new(one_page_pdf())).expect("open the fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer
        .set_output_writer(sink)
        .expect("configure a recording writer sink");
    writer.set_linearization(true);
    writer.set_static_id(true);
    writer
        .write()
        .expect("linearized output must reach the writer sink");

    let output_len = bytes.borrow().len();
    let writes = write_lengths.borrow();
    assert!(output_len > 0, "linearized writer must emit bytes");
    assert!(
        writes.len() > 1,
        "linearized final pass must stream multiple chunks, got {writes:?}"
    );
    assert!(
        writes.iter().any(|&length| length < output_len),
        "no single sink write should own the complete final PDF: {writes:?}"
    );
}

struct FailOnFlush;

impl Write for FailOnFlush {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("sink flush failure"))
    }
}

#[test]
fn a_writer_sink_flush_failure_leaves_the_write() {
    // A sink qpdf does not name keeps the bare I/O error: only the file sink
    // carries qpdf's `qpdf output` pipeline identity
    // (`QPDFWriter.cc:101-110`).
    let mut pdf = Pdf::open(Cursor::new(minimal_pdf())).expect("open the fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer
        .set_output_writer(FailOnFlush)
        .expect("configure the failing sink");

    let error = writer
        .write()
        .expect_err("a flush failure must escape write");
    assert!(
        matches!(&error, Error::Io(source) if source.to_string().contains("sink flush failure")),
        "{error:?}"
    );
}
