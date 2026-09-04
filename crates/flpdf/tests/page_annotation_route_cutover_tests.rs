use flpdf::{
    AcroFormDocumentHelper, Error, FormFieldObjectHelper, ObjectRef, PageDocumentHelper,
    PageObjectHelper, Pdf, PdfOpenOptions, Pipeline, PipelineError, PipelineHandle, QPDFLogger,
};
use std::collections::BTreeMap;
use std::io::Cursor;

struct FailingWarningSink;

impl Pipeline for FailingWarningSink {
    fn identifier(&self) -> &str {
        "failing-warning-sink"
    }

    fn write(&mut self, _data: &[u8]) -> flpdf::PipelineResult<()> {
        Err(PipelineError::runtime("warning sink failed"))
    }

    fn finish(&mut self) -> flpdf::PipelineResult<()> {
        Ok(())
    }
}

fn build_non_array_fields_pdf() -> Vec<u8> {
    let objects: &[(u32, &[u8])] = &[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields 5 0 R >>"),
        (5, b"42"),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    for &(number, body) in objects {
        offsets.insert(number, bytes.len());
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = bytes.len();
    let size = objects.last().expect("fixture objects").0 + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..size {
        match offsets.get(&number) {
            Some(offset) => bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

#[test]
fn canonical_helpers_preserve_grouped_widget_field_association() {
    let bytes = include_bytes!("../../../tests/fixtures/compat/form-fields-and-annotations.pdf");
    let mut pdf = Pdf::open(Cursor::new(bytes.as_slice())).unwrap();
    let page_ref = PageDocumentHelper::new(&mut pdf).get_all_pages().unwrap()[0];
    let widgets = PageObjectHelper::new(page_ref, &mut pdf)
        .get_annotations_filtered(Some(b"/Widget"))
        .unwrap();
    assert_eq!(widgets.len(), 5);

    let annotation_to_field = {
        let mut acroform = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        acroform.annotation_to_field_map().unwrap()
    };
    let top_level_fields: Vec<ObjectRef> = widgets
        .into_iter()
        .map(|widget| {
            let annotation_ref = widget.object_ref().unwrap();
            FormFieldObjectHelper::new(annotation_to_field[&annotation_ref], &mut pdf)
                .get_top_level_field()
                .unwrap()
                .0
        })
        .collect();

    assert_eq!(
        top_level_fields
            .iter()
            .filter(|field_ref| **field_ref == ObjectRef::new(5, 0))
            .count(),
        3,
        "the three grouped widgets must resolve through the canonical qpdf helper composition"
    );
}

#[test]
fn canonical_acroform_analysis_warns_for_a_non_array_fields_value() {
    let mut pdf = Pdf::open_mem_owned_with_options(
        build_non_array_fields_pdf(),
        PdfOpenOptions {
            suppress_warnings: true,
            description: b"non-array-fields.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    let mut acroform = AcroFormDocumentHelper::new(&mut pdf).unwrap();
    assert!(acroform.get_form_fields().unwrap().is_empty());
    assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("/Fields key of /AcroForm dictionary is not an array; ignoring")
    }));
}

#[test]
fn canonical_acroform_analysis_propagates_a_warning_sink_failure() {
    let mut pdf = Pdf::open_mem_owned_with_options(
        build_non_array_fields_pdf(),
        PdfOpenOptions {
            suppress_warnings: false,
            description: b"non-array-fields.pdf".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingWarningSink)));
    pdf.set_logger(logger);

    let error = AcroFormDocumentHelper::new(&mut pdf)
        .err()
        .expect("warning sink failure must propagate from analyze");
    assert!(matches!(error, Error::System(message) if message == "warning sink failed"));
}
