//! Output-sink failure routing for `PdfWriter`.
//!
//! qpdf wraps its output handle in `Pl_StdioFile("qpdf output", file)`
//! (`QPDFWriter.cc:101-110`), so a failed write names that pipeline and never a
//! file. These tests pin the routes that carry a sink failure out of
//! `PdfWriter::write`.

use flpdf::pipeline::{Pipeline, PipelineError, PipelineResult};
use flpdf::{Error, Pdf, PdfWriter};
use std::io::{Cursor, Write};

fn minimal_pdf() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/minimal.pdf"
    ))
    .expect("read the minimal fixture")
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
