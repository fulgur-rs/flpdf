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
    let duplicate = function_body(&source, "fn insert_dictionary_value", "fn real");

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

/// The signature raw-capture branch only runs on an encrypted document -- it
/// is gated on the string decrypter (`parser.rs`, `capture_raw_signature_contents`)
/// -- and only there does resolving an indirect `/Type` trip the parse guard.
///
/// On a document-owned resolver the re-entrant error never reaches the caller:
/// `ResolverHandle::resolve_indirect_inner` catches it, records a warning,
/// resolves the handle to null and returns `Ok(())`. qpdf behaves the same
/// way, reporting the re-entrancy as a warning and exiting 3. Only a
/// `DocumentResolver` test double propagates the error directly.
#[test]
fn indirect_type_in_an_encrypted_signature_dictionary_warns_instead_of_failing() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/encrypted-indirect-type-signature.pdf");
    let Ok(bytes) = fs::read(&path) else {
        eprintln!("fixture {path:?} is absent; skipping");
        return;
    };

    let options = flpdf::PdfOpenOptions {
        password: b"o".to_vec(),
        ..flpdf::PdfOpenOptions::default()
    };
    let mut pdf = flpdf::Pdf::open_mem_owned_with_options(bytes, options)
        .expect("the encrypted fixture must authenticate");
    let probe = pdf
        .trailer()
        .try_get_key(b"/Probe")
        .expect("trailer lookup succeeds");

    // The parse guard fires while resolving the indirect /Type, but the
    // document resolver swallows it, so this call still succeeds.
    let byte_range = probe
        .try_get_key(b"/ByteRange")
        .expect("the caught re-entrancy must not propagate to the caller");
    assert!(
        byte_range.try_is_array().expect("array check succeeds"),
        "the signature-shaped dictionary must still parse into a usable handle"
    );
}
