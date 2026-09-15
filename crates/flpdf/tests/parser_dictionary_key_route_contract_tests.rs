use std::fs;
use std::path::PathBuf;

fn parser_source() -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/parser.rs"))
        .expect("parser.rs must be readable")
}

fn function_body<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_at = source.find(start).expect("target function must exist");
    let body = &source[start_at..];
    let end_at = body.find(end).expect("next function boundary must exist");
    &body[..end_at]
}

#[test]
fn parser_dictionary_warnings_use_canonical_raw_key_bytes() {
    let source = parser_source();
    let finish_dictionary = function_body(&source, "fn finish_dictionary", "fn parse_scalar_token");
    let duplicate = function_body(&source, "fn insert_dictionary_value", "fn integer_or_ref");

    for body in [finish_dictionary, duplicate] {
        for forbidden in [
            "legacy_dictionary_key",
            "String::from_utf8_lossy",
            "format!(\"/{}",
        ] {
            assert!(
                !body.contains(forbidden),
                "parser dictionary warning retains legacy/lossy route {forbidden}"
            );
        }
    }
    assert!(duplicate.contains("key.as_slice()"));
}

#[test]
fn parser_signature_probe_uses_type_only_handle_predicates() {
    let source = parser_source();
    let finish_dictionary = function_body(&source, "fn finish_dictionary", "fn parse_scalar_token");

    assert!(
        finish_dictionary.contains("try_is_name_and_equals(b\"Sig\")"),
        "signature type detection must use the resolving name predicate"
    );
    assert!(
        finish_dictionary.contains("map(ObjectHandle::try_is_string)"),
        "signature contents detection must use the type-only string predicate"
    );
    assert!(
        !finish_dictionary.contains("and_then(ObjectHandle::as_name)"),
        "signature type detection must not clone name payloads"
    );
    assert!(
        !finish_dictionary.contains("and_then(ObjectHandle::as_string)"),
        "signature contents detection must not clone string payloads"
    );
}
