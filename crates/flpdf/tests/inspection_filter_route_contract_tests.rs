use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn first_stream_filter_name_uses_canonical_accessors() {
    let source = fs::read_to_string(source_root().join("job/inspection.rs"))
        .expect("inspection.rs must be readable")
        .replace("\r\n", "\n");
    let start = source
        .find("fn first_stream_filter_name")
        .expect("first_stream_filter_name must exist");
    let body = &source[start..];
    let end = body
        .find("fn write_to_standard_output")
        .expect("next inspection helper must follow first_stream_filter_name");
    let body = &body[..end];

    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !body.contains(forbidden),
            "first_stream_filter_name retains non-canonical route {forbidden}"
        );
    }
    assert!(body.contains("stream_dictionary.try_get_key(b\"/Filter\")?"));
}
