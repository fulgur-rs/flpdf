use flpdf::pipeline::{Pipeline, PipelineError, PipelineResult, PlString};

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
fn downstream_crates_can_implement_pipeline_and_construct_pl_string() {
    let mut captured = Vec::new();
    let mut sink = ExternalSink(Vec::new());
    {
        let mut stage = PlString::new("capture", Some(&mut sink), &mut captured);
        stage.write(b"payload").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(captured, b"payload");
    assert_eq!(sink.0, b"payload");
    assert_eq!(PipelineError::runtime("failure").message(), "failure");
}
