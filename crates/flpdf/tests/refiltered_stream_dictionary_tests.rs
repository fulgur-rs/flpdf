//! qpdf keeps external-file dictionary keys when refiltering stream bytes.

use flpdf::{ObjectHandle, Pdf, PdfWriter};
use std::rc::Rc;

#[test]
fn refiltered_stream_retains_external_file_dictionary_keys() {
    let mut pdf = Pdf::empty().unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"q Q\n".to_vec()))
        .unwrap();
    let dictionary = stream.as_stream_dict().unwrap();
    dictionary
        .replace_key(b"/F", ObjectHandle::string(b"external.bin".to_vec()))
        .unwrap();
    dictionary
        .replace_key(b"/FFilter", ObjectHandle::name(b"ASCIIHexDecode".to_vec()))
        .unwrap();
    dictionary
        .replace_key(
            b"/FDecodeParms",
            ObjectHandle::dictionary(vec![(b"/Marker".to_vec(), ObjectHandle::integer(42))]),
        )
        .unwrap();
    pdf.trailer().replace_key(b"/Extra", stream).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_compress_streams(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let mut output = Pdf::open_mem_owned(writer.get_buffer().unwrap()).unwrap();
    let extra = output.trailer().try_get_key(b"/Extra").unwrap();
    extra.type_code().unwrap();
    let actual = extra.as_stream_dict().unwrap();
    assert_eq!(
        actual.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
    assert_eq!(
        actual.try_get_key(b"/FFilter").unwrap().unparse(),
        b"/ASCIIHexDecode"
    );
    assert_eq!(
        actual.try_get_key(b"/FDecodeParms").unwrap().unparse(),
        b"<< /Marker 42 >>"
    );
    assert_eq!(
        actual.try_get_key(b"/Filter").unwrap().unparse(),
        b"/FlateDecode"
    );
}
