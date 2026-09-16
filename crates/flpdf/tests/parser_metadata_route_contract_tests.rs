use std::fs;
use std::path::PathBuf;

fn source(path: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
        .expect("source file must be readable")
}

fn function_body<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_at = source
        .find(start)
        .expect("target source boundary must exist");
    let body = &source[start_at..];
    let end_at = body.find(end).expect("next source boundary must exist");
    &body[..end_at]
}

#[test]
fn live_parser_direct_values_use_one_metadata_construction_boundary() {
    let resolver = source("src/reader/resolver.rs");
    let child_handles = function_body(
        &resolver,
        "impl<R: Read + Seek> crate::parser::HandleResolver for ChildHandles",
        "impl<R: Read + Seek> DocumentResolver",
    );
    assert!(
        child_handles.contains("fn direct_handle_at"),
        "live parser must own the direct-value metadata construction boundary"
    );
    assert!(
        child_handles.contains("parsed_direct_object_handle_with_description"),
        "live parser direct values must initialize identity, description, and offset together"
    );
    assert!(
        !child_handles.contains("set_shared_description"),
        "live parser must not construct a direct value and then mutate its metadata"
    );
}
