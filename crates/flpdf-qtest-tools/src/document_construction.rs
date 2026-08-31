//! qpdf correspondence: `qpdf/pdf_from_scratch.cc` and
//! `qpdf/test_many_nulls.cc` document-construction helper responsibilities.
//!
//! These helpers intentionally own only the test programs' construction
//! recipes. Empty-document creation, indirect-object promotion, page-tree
//! insertion, stream creation, and PDF emission stay on the public canonical
//! [`flpdf`] API, matching the qpdf programs' use of `QPDF` and
//! `QPDFPageDocumentHelper` rather than shelling out to another executable.

use flpdf::{ObjectHandle, ObjectStreamMode, PageDocumentHelper, PageInput, Pdf, PdfWriter};
use std::io::Cursor;
use std::path::Path;
use std::rc::Rc;

const FROM_SCRATCH_OUTPUT: &str = "a.pdf";
const MANY_NULLS_OUTER_COUNT: usize = 20;
const MANY_NULLS_INNER_COUNT: usize = 20_000;

/// Run qpdf's `pdf_from_scratch` test 0.
///
/// qpdf correspondence: `qpdf/pdf_from_scratch.cc:31-74` constructs an empty
/// PDF, promotes a parsed font and procset, creates one content stream, adds a
/// page through `QPDFPageDocumentHelper`, and writes `a.pdf` with static IDs
/// and preserved stream data.
pub fn run_from_scratch(test_number: i32) -> flpdf::Result<()> {
    let mut pdf = Pdf::empty()?;
    if test_number != 0 {
        return Err(flpdf::Error::Unsupported(format!(
            "invalid test {test_number}"
        )));
    }

    let font = pdf.make_indirect_from_object_handle(ObjectHandle::parse(
        b"<< /Type /Font /Subtype /Type1 /Name /F1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    )?)?;
    let procset = pdf.make_indirect_from_object_handle(ObjectHandle::parse(b"[/PDF /Text]")?)?;
    let contents = pdf.new_stream_with_data(Rc::new(
        b"BT /F1 15 Tf 72 720 Td (First Page) Tj ET\n".to_vec(),
    ))?;
    let page = pdf.make_indirect_from_object_handle(ObjectHandle::dictionary(Vec::new()))?;
    let resources = ObjectHandle::dictionary(vec![
        (b"/ProcSet".to_vec(), procset),
        (
            b"/Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"/F1".to_vec(), font)]),
        ),
    ]);
    page.replace_key(b"/Type", ObjectHandle::parse(b"/Page")?)?;
    page.replace_key(b"/MediaBox", ObjectHandle::parse(b"[0 0 612 792]")?)?;
    page.replace_key(b"/Contents", contents)?;
    page.replace_key(b"/Resources", resources)?;
    pdf.mark_object_handle_dirty(&page)?;

    let page_ref = page
        .object_ref()
        .expect("make_indirect_from_object_handle returns an indirect page");
    PageDocumentHelper::new(&mut pdf).add_page(PageInput::existing(page_ref), true)?;

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(FROM_SCRATCH_OUTPUT)?;
    writer.set_static_id(true);
    writer.set_stream_data_mode(flpdf::StreamDataMode::Preserve);
    writer.write()
}

/// Run qpdf's `test_many_nulls` document generator.
///
/// qpdf correspondence: `qpdf/test_many_nulls.cc:18-40` creates twenty
/// twenty-thousand-item null arrays, stores their outer array under the
/// trailer's `/Nulls`, appends one page to `/Pages/Kids`, and writes with
/// generated object streams and a deterministic ID.
pub fn run_many_nulls(output: impl AsRef<Path>) -> flpdf::Result<()> {
    let mut pdf = build_many_nulls_document(MANY_NULLS_OUTER_COUNT, MANY_NULLS_INNER_COUNT)?;

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(output)?;
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_deterministic_id(true);
    writer.write()
}

fn build_many_nulls_document(
    outer_count: usize,
    inner_count: usize,
) -> flpdf::Result<Pdf<Cursor<Vec<u8>>>> {
    let mut pdf = Pdf::empty()?;
    let null = ObjectHandle::null();
    let outer = (0..outer_count)
        .map(|_| ObjectHandle::array((0..inner_count).map(|_| null.clone()).collect()))
        .collect();
    let top = pdf.make_indirect_from_object_handle(ObjectHandle::array(outer))?;
    pdf.trailer().replace_key(b"/Nulls", top)?;

    let root = pdf.root_handle()?;
    let pages = root.try_get_key(b"/Pages")?;
    let kids = pages.try_get_key(b"/Kids")?;
    let page = pdf.make_indirect_from_object_handle(ObjectHandle::parse(
        b"<< /Type /Page /MediaBox [0 0 612 792] >>",
    )?)?;
    kids.append_array_item(page)?;
    pdf.mark_object_handle_dirty(&kids)?;

    Ok(pdf)
}

#[cfg(test)]
mod tests {
    use super::build_many_nulls_document;
    use flpdf::{ObjectStreamMode, PageDocumentHelper, PdfWriter};

    #[test]
    fn small_many_nulls_document_preserves_the_qpdf_graph_shape() {
        let mut pdf = build_many_nulls_document(2, 3).expect("build small many-nulls document");
        let nulls = pdf.trailer().try_get_key(b"/Nulls").expect("read /Nulls");
        assert_eq!(nulls.try_get_array_n_items().expect("read outer array"), 2);
        for inner in nulls.try_get_array_as_vector().expect("read outer array") {
            assert_eq!(inner.try_get_array_n_items().expect("read inner array"), 3);
        }
        assert_eq!(
            PageDocumentHelper::new(&mut pdf)
                .get_all_pages()
                .expect("enumerate page tree")
                .len(),
            1
        );
    }

    #[test]
    fn small_many_nulls_writer_is_deterministic() {
        let mut first = build_many_nulls_document(2, 3).expect("build first document");
        let mut first_writer = PdfWriter::new(&mut first);
        first_writer
            .set_output_memory()
            .expect("configure first output");
        first_writer.set_object_stream_mode(ObjectStreamMode::Generate);
        first_writer.set_deterministic_id(true);
        first_writer.write().expect("write first document");
        let first_output = first_writer.get_buffer().expect("take first output");

        let mut second = build_many_nulls_document(2, 3).expect("build second document");
        let mut second_writer = PdfWriter::new(&mut second);
        second_writer
            .set_output_memory()
            .expect("configure second output");
        second_writer.set_object_stream_mode(ObjectStreamMode::Generate);
        second_writer.set_deterministic_id(true);
        second_writer.write().expect("write second document");
        let second_output = second_writer.get_buffer().expect("take second output");

        assert_eq!(first_output, second_output);
    }
}
