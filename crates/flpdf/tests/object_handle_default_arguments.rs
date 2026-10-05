use flpdf::tokenizer::Token;
use flpdf::{ObjectHandle, Pdf, Pipeline, PipelineResult, TokenFilter, TokenFilterOutput};
use std::rc::Rc;

#[derive(Default)]
struct Sink(Vec<u8>);

impl Pipeline for Sink {
    fn identifier(&self) -> &str {
        "ObjectHandle default-argument test sink"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

struct ForwardTokens;

impl TokenFilter for ForwardTokens {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        output.write_token(token)
    }
}

#[test]
fn object_handle_public_defaults_match_qpdf_call_shapes(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dictionary = ObjectHandle::dictionary(vec![(
        b"/Type".to_vec(),
        ObjectHandle::name(b"Example".to_vec()),
    )]);
    assert!(dictionary.try_is_dictionary_of_type(b"Example")?);

    let mut pdf = Pdf::empty()?;
    let stream = pdf.new_stream_with_data(Rc::new(b"1 2 m\n".to_vec()))?;
    stream
        .try_get_stream_dict()?
        .replace_key(b"/Type", ObjectHandle::name(b"Example".to_vec()))?;
    assert!(stream.try_is_stream_of_type(b"Example")?);

    let mut direct = ObjectHandle::dictionary(Vec::new());
    direct.make_direct()?;
    let resources = ObjectHandle::dictionary(Vec::new());
    resources.merge_resources(&ObjectHandle::dictionary(Vec::new()))?;
    let mut min_suffix = 1;
    assert_eq!(
        resources.get_unique_resource_name(b"/X", &mut min_suffix)?,
        b"/X1"
    );
    assert!(!ObjectHandle::null().is_image()?);

    let value = ObjectHandle::integer(42);
    let _json = value.get_json(2)?;
    let mut sink = Sink::default();
    value.write_json(2, &mut sink)?;
    assert_eq!(sink.0, b"42");

    let indirect = pdf.make_indirect_object_handle(value)?;
    let object_ref = indirect
        .object_ref()
        .expect("new indirect object reference");
    let reference = format!("{} {} R", object_ref.number, object_ref.generation);
    assert_eq!(
        indirect.get_json(2)?.get_string(),
        Some(reference.as_bytes().to_vec())
    );
    let mut sink = Sink::default();
    indirect.write_json(2, &mut sink)?;
    assert_eq!(sink.0, format!("\"{reference}\"").as_bytes());

    let mut filter = ForwardTokens;
    let page = ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), stream.clone())]);
    page.filter_page_contents(&mut filter)?;
    stream.filter_as_contents(&mut filter)?;
    Ok(())
}
