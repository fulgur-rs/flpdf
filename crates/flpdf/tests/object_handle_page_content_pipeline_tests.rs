use flpdf::{pipeline::PlString, DecodeLevel, ObjectHandle, ObjectRef, PageObjectHelper, Pdf};
use std::rc::Rc;

fn stream(data: &[u8]) -> ObjectHandle {
    ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(data.to_vec()))
}

fn filtered_stream(filter: &[u8], data: &[u8]) -> ObjectHandle {
    ObjectHandle::stream(
        ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(filter.to_vec()),
        )]),
        Rc::new(data.to_vec()),
    )
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

#[test]
fn add_page_contents_always_installs_an_array_in_qpdf_order() {
    let first = stream(b"first");
    let second = stream(b"second");
    let page = page_with_contents(ObjectHandle::array(vec![first.clone()]));

    let prepended = stream(b"prepended");
    page.add_page_contents(prepended.clone(), true).unwrap();
    let contents = page.get_key(b"/Contents").as_array().unwrap();
    assert_eq!(contents.len(), 2);
    assert!(contents[0].is_same_object_as(&prepended));
    assert!(contents[1].is_same_object_as(&first));

    page.add_page_contents(second.clone(), false).unwrap();
    let contents = page.get_key(b"/Contents").as_array().unwrap();
    assert_eq!(contents.len(), 3);
    assert!(contents[2].is_same_object_as(&second));
}

#[test]
fn add_page_contents_rejects_a_non_stream_like_qpdf_assert_stream() {
    let page = ObjectHandle::dictionary(Vec::new());
    let error = page
        .add_page_contents(ObjectHandle::integer(1), false)
        .expect_err("qpdf assertStream rejects non-stream content");
    assert!(error.to_string().contains("operation for stream"));
}

#[test]
fn rotate_page_uses_the_nearest_valid_inherited_angle_and_normalizes() {
    let parent = ObjectHandle::dictionary(vec![(b"/Rotate".to_vec(), ObjectHandle::integer(90))]);
    let page = ObjectHandle::dictionary(vec![(b"/Parent".to_vec(), parent)]);

    page.rotate_page(180, true).unwrap();
    assert_eq!(page.get_key(b"/Rotate").as_integer(), Some(270));

    page.rotate_page(-90, false).unwrap();
    assert_eq!(page.get_key(b"/Rotate").as_integer(), Some(270));
}

#[test]
fn rotate_page_rejects_non_quarter_turns() {
    let error = ObjectHandle::dictionary(Vec::new())
        .rotate_page(45, false)
        .expect_err("qpdf rejects angles that are not multiples of 90");
    assert_eq!(
        error.to_string(),
        "QPDF::rotatePage called with an angle that is not a multiple of 90"
    );
}

#[test]
fn rotate_page_ignores_a_non_quarter_turn_inherited_angle() {
    let parent = ObjectHandle::dictionary(vec![(b"/Rotate".to_vec(), ObjectHandle::integer(45))]);
    let page = ObjectHandle::dictionary(vec![(b"/Parent".to_vec(), parent)]);

    page.rotate_page(90, true).unwrap();

    assert_eq!(page.get_key(b"/Rotate").as_integer(), Some(90));
}

#[test]
fn rotate_page_stops_when_parent_is_not_a_dictionary() {
    let page = ObjectHandle::dictionary(vec![(b"/Parent".to_vec(), ObjectHandle::integer(1))]);

    page.rotate_page(90, true).unwrap();

    assert_eq!(page.get_key(b"/Rotate").as_integer(), Some(90));
}

#[test]
fn pipe_page_contents_decodes_and_joins_streams_with_qpdf_newline_rules() {
    let page = page_with_contents(ObjectHandle::array(vec![
        stream(b"q"),
        stream(b"Q\n"),
        stream(b""),
        stream(b"x"),
    ]));
    let mut output = Vec::new();
    let mut pipeline = PlString::new("page content output", None, &mut output);

    page.pipe_page_contents(&mut pipeline).unwrap();

    assert_eq!(output, b"q\nQ\n\nx");
}

#[test]
fn pipe_page_contents_uses_qpdf_specialized_filter_decoding() {
    let page = page_with_contents(ObjectHandle::array(vec![
        filtered_stream(b"ASCIIHexDecode", b"71>"),
        stream(b"Q"),
    ]));
    let mut output = Vec::new();
    let mut pipeline = PlString::new("filtered page content output", None, &mut output);

    page.pipe_page_contents(&mut pipeline).unwrap();

    assert_eq!(output, b"q\nQ");
}

#[test]
fn pipe_page_contents_reports_a_provider_decode_failure_at_the_stream_boundary() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let failing = pdf.new_stream().unwrap();
    failing
        .replace_stream_data_with_retry_callback(|_, _, _| Ok(false), None, None)
        .unwrap();
    page.replace_key(b"/Contents", failing).unwrap();

    let mut output = Vec::new();
    let mut pipeline = PlString::new("failing page content output", None, &mut output);
    let error = page
        .pipe_page_contents(&mut pipeline)
        .expect_err("qpdf reports a failed stream provider as a damaged content stream");

    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: content stream object 4 0: errors while decoding content stream"
    );
}

#[test]
fn form_pipe_contents_ignores_an_unsuccessful_stream_pipe_like_qpdf() {
    let mut pdf = Pdf::empty().unwrap();
    let form = pdf.new_stream().unwrap();
    let dictionary = form.as_stream_dict().unwrap();
    dictionary
        .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
        .unwrap();
    dictionary
        .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
        .unwrap();
    form.replace_stream_data_with_retry_callback(|_, _, _| Ok(false), None, None)
        .unwrap();

    let mut helper = PageObjectHelper::from_object_handle(form, &mut pdf);
    let mut output = Vec::new();
    let mut pipeline = PlString::new("form content output", None, &mut output);
    helper
        .pipe_contents(&mut pipeline)
        .expect("qpdf ignores false from Form pipeStreamData");
    assert!(output.is_empty());
}

#[test]
fn form_pipe_contents_propagates_provider_errors() {
    let mut pdf = Pdf::empty().unwrap();
    let form = pdf.new_stream().unwrap();
    let dictionary = form.as_stream_dict().unwrap();
    dictionary
        .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
        .unwrap();
    dictionary
        .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
        .unwrap();
    form.replace_stream_data_with_callback(
        |_| Err(flpdf::Error::System("provider failure".to_owned())),
        None,
        None,
    )
    .unwrap();

    let mut helper = PageObjectHelper::from_object_handle(form, &mut pdf);
    let mut output = Vec::new();
    let mut pipeline = PlString::new("form content output", None, &mut output);
    let error = helper
        .pipe_contents(&mut pipeline)
        .expect_err("provider exceptions must cross Form pipeContents");
    assert_eq!(error.to_string(), "provider failure");
}

#[test]
fn pipe_page_contents_propagates_provider_errors_before_content_diagnostics() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let failing = pdf.new_stream().unwrap();
    failing
        .replace_stream_data_with_callback(
            |_| Err(flpdf::Error::System("provider failure".to_owned())),
            None,
            None,
        )
        .unwrap();
    page.replace_key(b"/Contents", failing).unwrap();

    let mut output = Vec::new();
    let mut pipeline = PlString::new("erroring page content output", None, &mut output);
    let error = page
        .pipe_page_contents(&mut pipeline)
        .expect_err("provider errors must cross the canonical pipe boundary");

    assert_eq!(error.to_string(), "provider failure");
}

#[test]
fn pipe_content_streams_updates_the_full_qpdf_stream_description() {
    let contents = ObjectHandle::array(vec![stream(b"a"), stream(b"b")]);
    let mut output = Vec::new();
    let mut pipeline = PlString::new("content output", None, &mut output);
    let mut all_description = String::new();

    contents
        .pipe_content_streams(&mut pipeline, "page object 7 0", &mut all_description)
        .unwrap();

    assert_eq!(output, b"a\nb");
    assert_eq!(all_description, "page object 7 0 stream 0 0, stream 0 0");
}

#[test]
fn coalesce_content_streams_installs_a_lazy_document_owned_provider() {
    let mut pdf = Pdf::open_mem_owned(indirect_content_shape_pdf()).unwrap();
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    let first = pdf.new_stream_with_data(Rc::new(b"q".to_vec())).unwrap();
    let second = pdf.new_stream_with_data(Rc::new(b"Q\n".to_vec())).unwrap();
    page.replace_key(b"/Contents", ObjectHandle::array(vec![first, second]))
        .unwrap();

    page.coalesce_content_streams().unwrap();
    let coalesced = page.get_key(b"/Contents");
    assert!(coalesced.is_indirect());
    assert!(coalesced.as_stream_data().is_none());
    assert!(!coalesced.as_stream_dict().unwrap().has_key(b"/Length"));

    assert_eq!(
        coalesced
            .get_stream_data(DecodeLevel::Specialized)
            .unwrap()
            .as_slice(),
        b"q\nQ\n"
    );
}

#[test]
fn coalesce_content_streams_is_a_noop_for_a_single_stream() {
    let content = stream(b"q");
    let page = page_with_contents(content.clone());

    page.coalesce_content_streams().unwrap();

    let current = page.get_key(b"/Contents");
    assert!(current.is_same_object_as(&content));
    assert_eq!(current.as_stream_data().unwrap().as_slice(), b"q");
}

#[test]
fn coalesce_content_streams_requires_an_owning_pdf_for_an_array() {
    let page = page_with_contents(ObjectHandle::array(vec![stream(b"q"), stream(b"Q")]));

    let error = page
        .coalesce_content_streams()
        .expect_err("qpdf cannot create a replacement stream without a QPDF");
    assert_eq!(
        error.to_string(),
        "coalesceContentStreams called on object  with no associated PDF file"
    );
}
