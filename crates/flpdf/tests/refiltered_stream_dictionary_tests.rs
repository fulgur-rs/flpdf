//! qpdf keeps external-file dictionary keys when refiltering stream bytes.

use flpdf::{
    ContentToken, ObjectHandle, Pdf, PdfWriter, PipelineResult, TokenFilter, TokenFilterOutput,
};
use std::cell::{Cell, RefCell};
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

#[test]
fn linearized_unencrypted_stream_uses_the_same_dictionary_owner() {
    let mut pdf = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/lone-flate-l9.pdf").to_vec(),
    )
    .unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"q Q\n".to_vec()))
        .unwrap();
    add_external_keys(&stream);
    pdf.get_object_handle(flpdf::ObjectRef::new(3, 0))
        .replace_key(b"/Contents", stream)
        .unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.set_compress_streams(true);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    let bytes = writer.get_buffer().unwrap();
    assert!(bytes
        .windows(b"external.bin".len())
        .any(|window| window == b"external.bin"));
}

fn rewrite_stream(
    configure_stream: impl FnOnce(&ObjectHandle),
    configure_writer: impl FnOnce(&mut PdfWriter<'_, std::io::Cursor<Vec<u8>>>),
) -> Vec<u8> {
    let mut pdf = Pdf::empty().unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"71>\n".to_vec()))
        .unwrap();
    configure_stream(&stream);
    pdf.trailer().replace_key(b"/Extra", stream).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    configure_writer(&mut writer);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    writer.get_buffer().unwrap()
}

fn emitted_stream_dict(bytes: Vec<u8>) -> ObjectHandle {
    let mut output = Pdf::open_mem_owned(bytes).unwrap();
    let extra = output.trailer().get_key(b"/Extra");
    extra.type_code().unwrap();
    extra.as_stream_dict().unwrap()
}

fn add_external_keys(stream: &ObjectHandle) {
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
}

#[test]
fn decoding_without_compression_removes_only_the_source_filter_keys() {
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            let dictionary = stream.as_stream_dict().unwrap();
            dictionary
                .replace_key(b"/Filter", ObjectHandle::name(b"ASCIIHexDecode".to_vec()))
                .unwrap();
        },
        |writer| {
            writer.set_compress_streams(false);
            writer.set_decode_level(flpdf::DecodeLevel::Generalized);
        },
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/Filter").unwrap().unparse(),
        b"null"
    );
    assert_eq!(
        dictionary.try_get_key(b"/DecodeParms").unwrap().unparse(),
        b"null"
    );
    assert_eq!(
        dictionary.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
    assert_eq!(
        dictionary.try_get_key(b"/FFilter").unwrap().unparse(),
        b"/ASCIIHexDecode"
    );
    assert_eq!(
        dictionary.try_get_key(b"/FDecodeParms").unwrap().unparse(),
        b"<< /Marker 42 >>"
    );
}

#[test]
fn filter_on_write_false_preserves_filter_and_external_file_keys() {
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            let dictionary = stream.as_stream_dict().unwrap();
            dictionary
                .replace_key(b"/Filter", ObjectHandle::name(b"ASCIIHexDecode".to_vec()))
                .unwrap();
            dictionary
                .replace_key(
                    b"/DecodeParms",
                    ObjectHandle::dictionary(vec![(
                        b"/Columns".to_vec(),
                        ObjectHandle::integer(1),
                    )]),
                )
                .unwrap();
            stream.set_filter_on_write(false).unwrap();
        },
        |writer| writer.set_compress_streams(true),
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/Filter").unwrap().unparse(),
        b"/ASCIIHexDecode"
    );
    assert_eq!(
        dictionary.try_get_key(b"/DecodeParms").unwrap().unparse(),
        b"<< /Columns 1 >>"
    );
    assert_eq!(
        dictionary.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
}

#[test]
fn metadata_filter_veto_removes_source_parameters_without_adding_flate() {
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            let dictionary = stream.as_stream_dict().unwrap();
            dictionary
                .replace_key(b"/Type", ObjectHandle::name(b"Metadata".to_vec()))
                .unwrap();
            dictionary
                .replace_key(b"/Filter", ObjectHandle::name(b"ASCIIHexDecode".to_vec()))
                .unwrap();
        },
        |writer| writer.set_compress_streams(true),
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/Filter").unwrap().unparse(),
        b"null"
    );
    assert_eq!(
        dictionary.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
    assert_eq!(
        dictionary.try_get_key(b"/Type").unwrap().unparse(),
        b"/Metadata"
    );
}

#[derive(Clone)]
struct PassThroughFilter {
    eof_calls: Rc<Cell<usize>>,
}

impl TokenFilter for PassThroughFilter {
    fn handle_token(
        &mut self,
        token: &ContentToken,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        output.write_token(token)
    }

    fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        self.eof_calls.set(self.eof_calls.get() + 1);
        Ok(())
    }
}

#[test]
fn token_filter_rewrite_keeps_external_keys_and_runs_once_per_pipe() {
    let eof_calls = Rc::new(Cell::new(0));
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            stream
                .add_token_filter(Rc::new(RefCell::new(PassThroughFilter {
                    eof_calls: Rc::clone(&eof_calls),
                })))
                .unwrap();
        },
        |writer| writer.set_compress_streams(true),
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
    assert_eq!(
        dictionary.try_get_key(b"/Filter").unwrap().unparse(),
        b"/FlateDecode"
    );
    assert_eq!(eof_calls.get(), 1);
}

#[test]
fn retrying_provider_preserves_external_keys_and_records_qpdf_retry_flags() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let call_log = Rc::clone(&calls);
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            stream
                .replace_stream_data_with_retry_callback(
                    move |pipeline, suppress_warnings, will_retry| {
                        call_log.borrow_mut().push((suppress_warnings, will_retry));
                        pipeline.write(b"provider")?;
                        pipeline.finish()?;
                        Ok(!will_retry)
                    },
                    None,
                    None,
                )
                .unwrap();
        },
        |writer| writer.set_compress_streams(true),
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
    assert_eq!(calls.borrow().as_slice(), &[(false, true), (false, false)]);
}

#[test]
fn unfiltered_stream_drops_only_an_empty_decode_parms_array_and_strips_crypt() {
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            let dictionary = stream.as_stream_dict().unwrap();
            dictionary
                .replace_key(
                    b"/Filter",
                    ObjectHandle::array(vec![
                        ObjectHandle::name(b"Crypt".to_vec()),
                        ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                    ]),
                )
                .unwrap();
            dictionary
                .replace_key(
                    b"/DecodeParms",
                    ObjectHandle::array(vec![
                        ObjectHandle::dictionary(Vec::new()),
                        ObjectHandle::dictionary(Vec::new()),
                    ]),
                )
                .unwrap();
            stream.set_filter_on_write(false).unwrap();
        },
        |writer| writer.set_compress_streams(true),
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/Filter").unwrap().unparse(),
        b"[ /ASCIIHexDecode ]"
    );
    assert_eq!(
        dictionary.try_get_key(b"/DecodeParms").unwrap().unparse(),
        b"[ << >> ]"
    );
    assert_eq!(
        dictionary.try_get_key(b"/F").unwrap().unparse(),
        b"(external.bin)"
    );
}

#[test]
fn missing_decode_parms_still_removes_a_crypt_filter() {
    let bytes = rewrite_stream(
        |stream| {
            add_external_keys(stream);
            stream
                .as_stream_dict()
                .unwrap()
                .replace_key(
                    b"/Filter",
                    ObjectHandle::array(vec![
                        ObjectHandle::name(b"Crypt".to_vec()),
                        ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                    ]),
                )
                .unwrap();
            stream.set_filter_on_write(false).unwrap();
        },
        |writer| writer.set_compress_streams(true),
    );
    let dictionary = emitted_stream_dict(bytes);
    assert_eq!(
        dictionary.try_get_key(b"/Filter").unwrap().unparse(),
        b"[ /ASCIIHexDecode ]"
    );
    assert_eq!(
        dictionary.try_get_key(b"/DecodeParms").unwrap().unparse(),
        b"null"
    );
}

#[test]
fn crypt_filter_cleanup_mutates_the_shared_source_array_like_qpdf() {
    let mut pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream_with_data(Rc::new(b"raw".to_vec())).unwrap();
    let dictionary = stream.as_stream_dict().unwrap();
    let filters = ObjectHandle::array(vec![
        ObjectHandle::name(b"Crypt".to_vec()),
        ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
    ]);
    let decode_parms = ObjectHandle::array(vec![
        ObjectHandle::dictionary(Vec::new()),
        ObjectHandle::dictionary(Vec::new()),
    ]);
    dictionary.replace_key(b"/Filter", filters.clone()).unwrap();
    dictionary
        .replace_key(b"/DecodeParms", decode_parms.clone())
        .unwrap();
    stream.set_filter_on_write(false).unwrap();
    pdf.trailer().replace_key(b"/Extra", stream).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_compress_streams(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    assert_eq!(filters.unparse(), b"[ /ASCIIHexDecode ]");
    assert_eq!(decode_parms.unparse(), b"[ << >> ]");
}

#[test]
fn provider_failure_stops_before_a_raw_retry_and_preserves_the_error_order() {
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    let mut pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    add_external_keys(&stream);
    stream
        .replace_stream_data_with_retry_callback(
            move |_pipeline, _suppress_warnings, _will_retry| {
                observed.set(observed.get() + 1);
                Err(flpdf::Error::System("provider failure".to_owned()))
            },
            None,
            None,
        )
        .unwrap();
    pdf.trailer().replace_key(b"/Extra", stream).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_compress_streams(true);
    writer.set_output_memory().unwrap();
    let error = writer
        .write()
        .expect_err("provider failure must cross writer boundary");
    assert_eq!(
        error.to_string(),
        "error while getting stream data for 3 0 R: provider failure"
    );
    assert_eq!(calls.get(), 1);
}
