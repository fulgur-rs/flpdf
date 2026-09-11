use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn signature_eligibility_uses_canonical_type_accessors() {
    let source = fs::read_to_string(source_root().join("writer/object_streams/eligibility.rs"))
        .expect("eligibility.rs must be readable")
        .replace("\r\n", "\n");
    let start = source
        .find("pub(crate) fn is_qpdf_signature_dict")
        .expect("signature eligibility function must exist");
    let body = &source[start..];
    let end = body
        .find("/// Push an object's child values")
        .expect("child traversal must follow signature eligibility");
    let body = &body[..end];

    for forbidden in [
        "pdf.resolve(",
        "resolve_handle(",
        "resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !body.contains(forbidden),
            "signature eligibility retains non-canonical route {forbidden}"
        );
    }
    assert!(body.contains("type_value.try_is_name_and_equals(b\"Sig\")?"));
}
