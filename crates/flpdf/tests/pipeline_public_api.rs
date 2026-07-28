use flpdf::pipeline::{
    Base64Action, Pipeline, PipelineError, PipelineResult, PlBase64, PlConcatenate, PlString,
};

struct ExternalSink(Vec<u8>);

impl Pipeline for ExternalSink {
    fn identifier(&self) -> &str {
        "external"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

#[test]
fn downstream_crates_can_implement_pipeline_and_construct_public_pipeline_stages() {
    let mut captured = Vec::new();
    let mut sink = ExternalSink(Vec::new());
    {
        let mut stage = PlString::new("capture", Some(&mut sink), &mut captured);
        stage.write(b"payload").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(captured, b"payload");
    assert_eq!(sink.0, b"payload");

    let mut concatenate = PlConcatenate::new("concatenate", &mut sink);
    assert_eq!(concatenate.identifier(), "concatenate");
    concatenate.finish().unwrap();
    concatenate.manual_finish().unwrap();

    let mut base64 = PlBase64::new("base64", &mut sink, Base64Action::Encode);
    assert_eq!(base64.identifier(), "base64");
    base64.write(b"M").unwrap();
    base64.finish().unwrap();

    assert_eq!(PipelineError::runtime("failure").message(), "failure");
}
