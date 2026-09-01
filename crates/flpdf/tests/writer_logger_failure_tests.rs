use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, Pdf, PdfWriter, QPDFLogger};
use std::collections::BTreeMap;
use std::io::Cursor;

struct FailOnceSink {
    failed: bool,
}

impl Pipeline for FailOnceSink {
    fn identifier(&self) -> &str {
        "writer fail-once warning sink"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        if !self.failed {
            self.failed = true;
            return Err(PipelineError::runtime("sink write failure 1"));
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn malformed_extensions_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    fn add_object(
        bytes: &mut Vec<u8>,
        offsets: &mut BTreeMap<u32, usize>,
        number: u32,
        body: &[u8],
    ) {
        offsets.insert(number, bytes.len());
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    offsets.insert(1, bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\n");
    add_object(
        &mut bytes,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    add_object(
        &mut bytes,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    offsets.insert(4, bytes.len());
    bytes.extend_from_slice(b"4 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>\n");

    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for number in 1..=4 {
        if let Some(offset) = offsets.get(&number) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn full_rewrite_propagates_extension_preflight_logger_failure() {
    let mut pdf = Pdf::open(Cursor::new(malformed_extensions_pdf()))
        .expect("malformed extension fixture should open lazily");
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailOnceSink { failed: false })));
    pdf.set_logger(logger);

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("memory output");
    let result = writer.write();

    assert!(matches!(
        &result,
        Err(Error::System(message)) if message == "sink write failure 1"
    ));
}

#[test]
fn linearized_rewrite_propagates_extension_preflight_logger_failure() {
    let mut pdf = Pdf::open(Cursor::new(malformed_extensions_pdf()))
        .expect("malformed extension fixture should open lazily");
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailOnceSink { failed: false })));
    pdf.set_logger(logger);

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_output_memory().expect("memory output");
    let result = writer.write();

    assert!(matches!(
        &result,
        Err(Error::System(message)) if message == "sink write failure 1"
    ));
}
