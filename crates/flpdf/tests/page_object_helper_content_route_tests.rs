#[test]
fn content_stream_object_events_are_handle_native() {
    let source = include_str!("../src/page_object_helper.rs");
    assert!(
        source.contains("objects: Vec<ObjectHandle>"),
        "content callbacks must retain live ObjectHandle events"
    );
    assert!(
        source.contains("pub fn content_stream_objects(&mut self) -> Result<Vec<ObjectHandle>>"),
        "the public content event API must return ObjectHandle values"
    );
    for forbidden in [
        "objects: Vec<Object>",
        "pub fn content_stream_objects(&mut self) -> Result<Vec<Object>>",
        "callbacks.objects.push(object.materialize())",
    ] {
        assert!(
            !source.contains(forbidden),
            "content event route still contains the raw projection: {forbidden}"
        );
    }
}
