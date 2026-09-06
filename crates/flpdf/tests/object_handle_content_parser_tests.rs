use flpdf::{
    parse_content_operations, pipeline::PlString, ContentToken, ContentTokenType, ObjectHandle,
    ObjectHandleParserCallbacks, ObjectRef, ParseControl, Pdf, PipelineResult, TokenFilter,
    TokenFilterOutput,
};
use std::cell::RefCell;
use std::rc::Rc;

fn stream(data: &[u8]) -> ObjectHandle {
    ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(data.to_vec()))
}

fn page_with_contents(contents: ObjectHandle) -> ObjectHandle {
    ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), contents)])
}

fn owned_page_with_contents(data: &[u8]) -> (Pdf<std::io::Cursor<Vec<u8>>>, ObjectHandle) {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let contents = pdf.new_stream_with_data(Rc::new(data.to_vec())).unwrap();
    page.replace_key(b"/Contents", contents).unwrap();
    (pdf, page)
}

fn indirect_content_shape_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let bodies = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n".to_vec(),
    ];
    let mut offsets = Vec::new();
    for body in &bodies {
        offsets.push(pdf.len());
        pdf.extend_from_slice(body);
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[derive(Default)]
struct RecordingCallbacks {
    size: Option<usize>,
    objects: Vec<(ObjectHandle, usize, usize)>,
    eof_calls: usize,
    stop_after: Option<usize>,
    fail: bool,
}

#[derive(Default)]
struct DefaultCallbacks {
    objects: Vec<ObjectHandle>,
    eof_calls: usize,
}

impl ObjectHandleParserCallbacks for DefaultCallbacks {
    fn handle_object(
        &mut self,
        object: ObjectHandle,
        _offset: usize,
        _length: usize,
    ) -> flpdf::Result<ParseControl> {
        self.objects.push(object);
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        self.eof_calls += 1;
        Ok(())
    }
}

impl ObjectHandleParserCallbacks for RecordingCallbacks {
    fn content_size(&mut self, size: usize) -> flpdf::Result<()> {
        self.size = Some(size);
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: ObjectHandle,
        offset: usize,
        length: usize,
    ) -> flpdf::Result<ParseControl> {
        if self.fail {
            return Err(flpdf::Error::System("parser callback failed".to_owned()));
        }
        self.objects.push((object, offset, length));
        Ok(if self.stop_after == Some(self.objects.len()) {
            ParseControl::Stop
        } else {
            ParseControl::Continue
        })
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        self.eof_calls += 1;
        Ok(())
    }
}

#[test]
fn parse_page_contents_delivers_object_handles_with_qpdf_spans_and_offsets() {
    let page = page_with_contents(stream(b" 1 2 cm\n"));
    let mut callbacks = RecordingCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.size, Some(8));
    assert_eq!(callbacks.eof_calls, 1);
    assert_eq!(
        callbacks
            .objects
            .iter()
            .map(|(_, offset, length)| (*offset, *length))
            .collect::<Vec<_>>(),
        vec![(1, 1), (3, 1), (5, 2)]
    );
    assert_eq!(callbacks.objects[0].0.as_integer(), Some(1));
    assert_eq!(callbacks.objects[0].0.get_parsed_offset(), 1);
    assert_eq!(
        callbacks.objects[2].0.as_operator().as_deref(),
        Some(b"cm".as_slice())
    );
}

#[test]
fn parse_as_contents_delivers_inline_image_handles() {
    let form = stream(b"BI /W 1 ID \0EI");
    let mut callbacks = RecordingCallbacks::default();

    form.parse_as_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.eof_calls, 1);
    assert!(callbacks
        .objects
        .iter()
        .any(|(object, _, _)| object.as_inline_image().is_some()));
}

#[test]
fn detached_parse_throws_the_first_recovery_warning_before_callbacks() {
    let form = stream(b"\r<0g");
    let mut callbacks = RecordingCallbacks::default();

    let error = form
        .parse_as_contents(&mut callbacks)
        .expect_err("a detached qpdf content parse throws its first warning");

    assert!(matches!(
        error,
        flpdf::Error::System(message)
            if message == "object 0 0 stream 0 0 (content, offset 1): invalid character (g) in hexstring"
    ));
    assert_eq!(callbacks.size, Some(4));
    assert!(callbacks.objects.is_empty());
    assert_eq!(callbacks.eof_calls, 0);
}

#[test]
fn parse_content_operations_ignores_inline_image_events() {
    let mut operators = Vec::new();
    parse_content_operations(b"BI /W 1 ID \0EI q", |_, operator| {
        operators.push(operator.to_vec());
        Ok(ParseControl::Continue)
    })
    .unwrap();

    assert_eq!(
        operators,
        vec![
            b"BI".to_vec(),
            b"ID".to_vec(),
            b"EI".to_vec(),
            b"q".to_vec()
        ]
    );
}

#[test]
fn detached_parse_throws_inline_image_eof_warning_before_normal_eof() {
    let form = stream(b"BI /W 1 ID \0");
    let mut callbacks = RecordingCallbacks::default();

    let error = form
        .parse_as_contents(&mut callbacks)
        .expect_err("a detached qpdf content parse throws an inline-image warning");

    assert!(matches!(
        error,
        flpdf::Error::System(message)
            if message == "object 0 0 stream 0 0 (stream data, offset 12): EOF found while reading inline image"
    ));
    assert!(!callbacks.objects.is_empty());
    assert!(!callbacks
        .objects
        .iter()
        .any(|(object, _, _)| object.as_inline_image().is_some()));
    assert_eq!(callbacks.eof_calls, 0);
}

#[test]
fn detached_parse_throws_id_at_eof_warning_before_normal_eof() {
    let form = stream(b"ID");
    let mut callbacks = RecordingCallbacks::default();

    let error = form
        .parse_as_contents(&mut callbacks)
        .expect_err("a detached qpdf content parse throws an ID-at-EOF warning");

    assert!(matches!(
        error,
        flpdf::Error::System(message)
            if message == "object 0 0 stream 0 0 (stream data, offset 2): EOF found while reading inline image"
    ));
    assert_eq!(callbacks.objects.len(), 1);
    assert_eq!(
        callbacks.objects[0].0.as_operator().as_deref(),
        Some(b"ID".as_slice())
    );
    assert_eq!(callbacks.eof_calls, 0);
}

#[test]
fn parse_page_contents_retains_qpdf_container_opening_offsets() {
    let page = page_with_contents(stream(b" [1] << /A 2 >>"));
    let mut callbacks = RecordingCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.objects[0].0.get_parsed_offset(), 1);
    assert_eq!(callbacks.objects[1].0.get_parsed_offset(), 5);
}

#[test]
fn parse_page_contents_uses_default_callback_hooks_and_all_scalar_handles() {
    let page = page_with_contents(stream(
        b"/Name (text) true false null .5 1.5 [42] << /A 3 >> cm",
    ));
    let mut callbacks = DefaultCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.eof_calls, 1);
    assert!(callbacks
        .iter()
        .any(|object| object.as_name() == Some(b"Name".to_vec())));
    assert!(callbacks
        .iter()
        .any(|object| object.as_string() == Some(b"text".to_vec())));
    assert!(callbacks
        .iter()
        .any(|object| object.as_boolean() == Some(true)));
    assert!(callbacks
        .iter()
        .any(|object| object.as_boolean() == Some(false)));
    assert!(callbacks.iter().any(ObjectHandle::is_null));
    assert!(callbacks.iter().any(|object| object.as_real() == Some(0.5)));
    assert!(callbacks.iter().any(|object| object.as_real() == Some(1.5)));
    assert!(callbacks
        .iter()
        .any(|object| object.as_array().is_some_and(|values| values.len() == 1)));
    assert!(callbacks.iter().any(|object| object
        .as_dictionary()
        .is_some_and(|values| values.contains_key(b"/A".as_slice()))));
}

impl DefaultCallbacks {
    fn iter(&self) -> impl Iterator<Item = &ObjectHandle> {
        self.objects.iter()
    }
}

#[test]
fn parse_page_contents_preserves_document_context_for_direct_handles() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let contents = pdf
        .new_stream_with_data(Rc::new(b"1 2 cm".to_vec()))
        .unwrap();
    page.replace_key(b"/Contents", contents).unwrap();
    let mut callbacks = RecordingCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.objects[0].0.as_integer(), Some(1));
    assert_eq!(callbacks.objects[0].0.get_parsed_offset(), 0);
}

#[test]
fn parse_page_contents_matches_qpdf_content_recovery_and_errors() {
    let (pdf, page) = owned_page_with_contents(b"q <0g> ] >>");
    let mut recovered = RecordingCallbacks::default();
    page.parse_page_contents(&mut recovered).unwrap();
    let diagnostics = pdf.repair_diagnostics();
    let messages = diagnostics
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(messages
        .iter()
        .any(|message| message.contains("invalid character (g) in hexstring")));
    assert!(messages.iter().any(|message| {
        message.contains("page object 3 0 stream 4 0 (content, offset ")
            && message.ends_with("treating unexpected array close token as null")
    }));
    assert!(messages.iter().any(|message| {
        message.contains("page object 3 0 stream 4 0 (content, offset ")
            && message.ends_with("unexpected dictionary close token")
    }));
    assert_eq!(recovered.eof_calls, 1);

    let (pdf, page) = owned_page_with_contents(b"<< /QPDFFake1 9 7 } /A >>");
    let mut dictionary = RecordingCallbacks::default();
    page.parse_page_contents(&mut dictionary).unwrap();
    assert!(dictionary.objects[0]
        .0
        .as_dictionary()
        .is_some_and(|values| values.contains_key(b"/QPDFFake2".as_slice())));
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("inserting key /QPDFFake2")));

    for input in [b"<< } } } } } } >>".as_slice(), b"<< /A [ } } } } } } ] >>"] {
        let (pdf, page) = owned_page_with_contents(input);
        let mut callbacks = RecordingCallbacks::default();
        page.parse_page_contents(&mut callbacks).unwrap();
        assert!(callbacks.objects[0].0.is_null());
        assert_eq!(callbacks.eof_calls, 1);
        assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
            diagnostic
                .message
                .ends_with("too many errors; giving up on reading object")
        }));
    }

    let (pdf, page) = owned_page_with_contents(b"[ } } } } } 1 2 3 4 } } } } } 6 ]");
    let mut streak = RecordingCallbacks::default();
    page.parse_page_contents(&mut streak).unwrap();
    assert!(
        !pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
            diagnostic
                .message
                .ends_with("too many errors; giving up on reading object")
        })
    );

    for input in [b"<< /A 1".as_slice(), b"[1"] {
        let error = page_with_contents(stream(input))
            .parse_page_contents(&mut RecordingCallbacks::default())
            .expect_err("detached content parsing must surface the qpdf EOF diagnostic");
        assert!(error
            .to_string()
            .contains("parse error while reading object"));
    }
}

#[test]
fn parse_page_contents_reports_non_bad_token_diagnostics_and_nesting_limit() {
    let (pdf, page) = owned_page_with_contents(b"/a#1x");
    let mut callbacks = RecordingCallbacks::default();
    page.parse_page_contents(&mut callbacks).unwrap();
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("name with stray #")));

    let mut nested = vec![b'['; 501];
    nested.extend(std::iter::repeat_n(b']', 501));
    let error = page_with_contents(stream(&nested))
        .parse_page_contents(&mut RecordingCallbacks::default())
        .expect_err("qpdf content parser must bound nesting");
    assert!(error.to_string().contains("object nesting too deep"));
}

#[test]
fn parse_page_contents_stop_skips_eof_and_callback_errors_propagate() {
    let page = page_with_contents(stream(b"1 2 cm"));
    let mut stopped = RecordingCallbacks {
        stop_after: Some(1),
        ..RecordingCallbacks::default()
    };
    page.parse_page_contents(&mut stopped).unwrap();
    assert_eq!(stopped.objects.len(), 1);
    assert_eq!(stopped.eof_calls, 0);

    let mut failed = RecordingCallbacks {
        fail: true,
        ..RecordingCallbacks::default()
    };
    let error = page
        .parse_page_contents(&mut failed)
        .expect_err("callback error must cross the ObjectHandle parser boundary");
    assert_eq!(error.to_string(), "parser callback failed");
}

#[test]
fn parse_page_contents_records_recovery_diagnostics_in_document_sink() {
    let (pdf, page) = owned_page_with_contents(b"<< /A >> cm");
    let mut callbacks = RecordingCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.eof_calls, 1);
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic
            .message
            .ends_with("dictionary ended prematurely; using null as value for last key")));
}

#[test]
fn parse_page_contents_records_qpdf_stream_diagnostic_context_in_order() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let first = pdf
        .new_stream_with_data(Rc::new(b"<< /A >> cm".to_vec()))
        .unwrap();
    let second = pdf
        .new_stream_with_data(Rc::new(b"1 2 cm".to_vec()))
        .unwrap();
    let first_ref = first.object_ref().expect("first stream is indirect");
    let second_ref = second.object_ref().expect("second stream is indirect");
    page.replace_key(
        b"/Contents",
        ObjectHandle::array(vec![first.clone(), second.clone()]),
    )
    .unwrap();
    let mut callbacks = RecordingCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    let expected_prefix = format!(
        "page object 3 0 stream {} {}, stream {} {} (content, offset ",
        first_ref.number, first_ref.generation, second_ref.number, second_ref.generation
    );
    assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
        diagnostic.message.starts_with(&expected_prefix)
            && diagnostic
                .message
                .ends_with("dictionary ended prematurely; using null as value for last key")
    }));
}

#[derive(Default)]
struct RecordingFilter {
    tokens: Vec<(ContentTokenType, Vec<u8>)>,
    eof_calls: usize,
    discard_words: bool,
    write_eof: bool,
}

impl TokenFilter for RecordingFilter {
    fn handle_token(
        &mut self,
        token: &ContentToken,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        self.tokens.push((token.token_type, token.raw.clone()));
        if !(self.discard_words && token.token_type == ContentTokenType::Word) {
            output.write_token(token)?;
        }
        Ok(())
    }

    fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        self.eof_calls += 1;
        if self.write_eof {
            output.write(b"!")?;
        }
        Ok(())
    }
}

#[test]
fn filter_page_contents_uses_the_canonical_pipeline_and_eof_lifecycle() {
    let page = page_with_contents(stream(b"1 2 cm"));
    let mut filter = RecordingFilter {
        write_eof: true,
        ..RecordingFilter::default()
    };
    let mut output = Vec::new();
    let mut sink = PlString::new("filtered page content", None, &mut output);

    page.filter_page_contents(&mut filter, Some(&mut sink))
        .unwrap();

    assert_eq!(output, b"1 2 cm!");
    assert_eq!(filter.eof_calls, 1);
    assert!(filter
        .tokens
        .iter()
        .any(|(token_type, raw)| *token_type == ContentTokenType::Word && raw == b"cm"));
}

#[test]
fn filter_as_contents_can_discard_tokens_and_add_content_filter_is_lazy() {
    let form = stream(b"q 1 cm");
    let mut filter = RecordingFilter {
        discard_words: true,
        ..RecordingFilter::default()
    };
    let mut output = Vec::new();
    let mut sink = PlString::new("filtered form content", None, &mut output);
    form.filter_as_contents(&mut filter, Some(&mut sink))
        .unwrap();
    assert_eq!(output, b" 1 ");
    assert_eq!(filter.eof_calls, 1);

    let stream = stream(b"q");
    let shared = Rc::new(RefCell::new(RecordingFilter::default()));
    stream.add_token_filter(shared.clone()).unwrap();
    assert_eq!(shared.borrow().tokens.len(), 0);
    let first = stream
        .get_stream_data(flpdf::DecodeLevel::Specialized)
        .unwrap();
    let second = stream
        .get_stream_data(flpdf::DecodeLevel::Specialized)
        .unwrap();
    assert_eq!(first.as_slice(), b"q");
    assert_eq!(second.as_slice(), b"q");
    assert_eq!(shared.borrow().eof_calls, 2);
}

#[test]
fn parse_page_contents_can_stop_on_the_inline_image_event() {
    let form = stream(b"BI /W 1 ID \0EI");
    let mut callbacks = RecordingCallbacks {
        stop_after: Some(5),
        ..RecordingCallbacks::default()
    };

    form.parse_as_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.objects.len(), 5);
    assert!(callbacks.objects[4].0.as_inline_image().is_some());
    assert_eq!(callbacks.eof_calls, 0);
}

#[test]
fn filter_as_contents_reports_failed_stream_decoding() {
    let pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let failing = pdf.new_stream().unwrap();
    failing
        .replace_stream_data_with_retry_callback(|_, _, _| Ok(false), None, None)
        .unwrap();
    let mut filter = RecordingFilter::default();

    let error = failing
        .filter_as_contents(&mut filter, None)
        .expect_err("qpdf must report an unsuccessful specialized stream pipe");
    assert!(error
        .to_string()
        .contains("errors while decoding content stream"));
}

#[test]
fn add_content_token_filter_coalesces_before_lazy_filter_execution() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let first = pdf.new_stream_with_data(Rc::new(b"q".to_vec())).unwrap();
    let second = pdf.new_stream_with_data(Rc::new(b"Q".to_vec())).unwrap();
    page.replace_key(b"/Contents", ObjectHandle::array(vec![first, second]))
        .unwrap();

    let shared = Rc::new(RefCell::new(RecordingFilter::default()));
    page.add_content_token_filter(shared.clone()).unwrap();

    let coalesced = page.get_key(b"/Contents");
    assert!(coalesced.is_indirect());
    assert!(coalesced.as_stream_data().is_none());
    assert_eq!(shared.borrow().eof_calls, 0);

    let data = coalesced
        .get_stream_data(flpdf::DecodeLevel::Specialized)
        .unwrap();
    assert_eq!(data.as_slice(), b"q\nQ");
    assert_eq!(shared.borrow().eof_calls, 1);
}
