use flpdf::{
    pipeline::PlString, ContentToken, ContentTokenType, ObjectHandle, ObjectHandleParserCallbacks,
    ObjectRef, ParseControl, Pdf, PipelineResult, TokenFilter, TokenFilterOutput,
};
use std::cell::RefCell;
use std::rc::Rc;

fn stream(data: &[u8]) -> ObjectHandle {
    ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(data.to_vec()))
}

fn page_with_contents(contents: ObjectHandle) -> ObjectHandle {
    ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), contents)])
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
    diagnostics: Vec<(usize, String)>,
    eof_calls: usize,
    stop_after: Option<usize>,
    fail: bool,
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

    fn handle_diagnostic(&mut self, offset: usize, message: &str) -> flpdf::Result<()> {
        self.diagnostics.push((offset, message.to_owned()));
        Ok(())
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
    assert_eq!(callbacks.diagnostics, Vec::new());
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
fn parse_as_contents_recovers_inline_image_eof_before_normal_eof() {
    let form = stream(b"BI /W 1 ID \0");
    let mut callbacks = RecordingCallbacks::default();

    form.parse_as_contents(&mut callbacks).unwrap();

    assert_eq!(callbacks.eof_calls, 1);
    assert!(callbacks
        .diagnostics
        .iter()
        .any(|(_, message)| message == "EOF found while reading inline image"));
    assert!(!callbacks
        .objects
        .iter()
        .any(|(object, _, _)| object.as_inline_image().is_some()));
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
fn parse_page_contents_forwards_recovery_diagnostics_before_eof() {
    let page = page_with_contents(stream(b"<< /A >> cm"));
    let mut callbacks = RecordingCallbacks::default();

    page.parse_page_contents(&mut callbacks).unwrap();

    assert!(!callbacks.diagnostics.is_empty());
    assert_eq!(callbacks.diagnostics[0].0, 2);
    assert_eq!(callbacks.eof_calls, 1);
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
