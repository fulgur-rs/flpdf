use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, NameTree, ObjectRef, Pdf, QPDFLogger};
use std::collections::BTreeMap;
use std::io::Cursor;

struct FailOnceSink {
    failed: bool,
}

impl Pipeline for FailOnceSink {
    fn identifier(&self) -> &str {
        "nntree fail-once warning sink"
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

fn malformed_child_name_tree_pdf() -> Vec<u8> {
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

    add_object(
        &mut bytes,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    add_object(
        &mut bytes,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [] /Count 0 >>",
    );
    add_object(&mut bytes, &mut offsets, 4, b"<< /Kids [5 0 R] >>");
    offsets.insert(5, bytes.len());
    bytes.extend_from_slice(b"5 0 obj\n<< /Names [(key) 6 0 R >>\n");
    add_object(&mut bytes, &mut offsets, 6, b"<< /Marker 1 >>");

    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for number in 1..=6 {
        if let Some(offset) = offsets.get(&number) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn name_tree_does_not_downgrade_a_child_resolution_logger_failure() {
    let mut pdf = Pdf::open(Cursor::new(malformed_child_name_tree_pdf()))
        .expect("malformed name-tree fixture should open lazily");
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailOnceSink { failed: false })));
    pdf.set_logger(logger);

    let mut tree = NameTree::new(pdf.get_object_handle(ObjectRef::new(4, 0)), true);
    let result = tree.begin(&mut pdf);

    assert!(matches!(
        &result,
        Err(Error::System(message)) if message == "sink write failure 1"
    ));
}

#[test]
fn name_tree_structural_non_dictionary_is_downgraded_to_a_warning() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec(),
    ))
    .expect("minimal fixture should open");
    let scalar = pdf
        .make_indirect_from_object_handle(flpdf::ObjectHandle::integer(1))
        .expect("scalar tree root should be allocatable");
    let mut tree = NameTree::new(scalar, true);

    let cursor = tree
        .begin(&mut pdf)
        .expect("structural tree error is recoverable");

    assert!(!cursor.valid());
    assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
        entry
            .message
            .contains("non-dictionary node while traversing name/number tree")
    }));
}

#[test]
fn name_tree_structural_warning_logger_failure_is_propagated() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec(),
    ))
    .expect("minimal fixture should open");
    let scalar = pdf
        .make_indirect_from_object_handle(flpdf::ObjectHandle::integer(1))
        .expect("scalar tree root should be allocatable");
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailOnceSink { failed: false })));
    pdf.set_logger(logger);
    let mut tree = NameTree::new(scalar, true);

    let result = tree.begin(&mut pdf);

    assert!(matches!(
        result,
        Err(Error::System(message)) if message == "sink write failure 1"
    ));
}
