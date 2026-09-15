//! Linearization must preserve qpdf's foreign-object writer boundary.

use flpdf::{Error, ObjectHandle, PageDocumentHelper, PageInput, Pdf, PdfWriter};

const FOREIGN_OBJECT_ERROR: &str = "QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file.";

fn one_page() -> ObjectHandle {
    ObjectHandle::dictionary(vec![
        (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
        (
            b"/MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(612),
                ObjectHandle::integer(792),
            ]),
        ),
    ])
}

#[test]
fn linearization_rejects_a_foreign_indirect_descendant_in_a_direct_container() {
    let mut foreign = Pdf::empty().expect("foreign PDF");
    let foreign_root = foreign.root_handle().expect("foreign root");

    let mut destination = Pdf::empty().expect("destination PDF");
    let destination_root = destination.root_handle().expect("destination root");
    let direct_container = ObjectHandle::dictionary(vec![(b"/Foreign".to_vec(), foreign_root)]);
    destination_root
        .replace_key(b"/ForeignContainer", direct_container)
        .expect("qpdf's shallow mutation ownership check accepts the direct container");
    PageDocumentHelper::new(&mut destination)
        .add_page(PageInput::direct(one_page()), false)
        .expect("destination page");

    let mut writer = PdfWriter::new(&mut destination);
    writer.set_linearization(true);
    writer.set_output_memory().expect("memory output");
    let error = writer
        .write()
        .expect_err("linearization must reject the foreign descendant");

    assert!(matches!(error, Error::Internal(message) if message == FOREIGN_OBJECT_ERROR));
}
