//! Route contracts for the qpdf `asString` caller cutover.

use std::fs;
use std::path::PathBuf;

fn source(path: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
        .expect("production source must be readable")
        .replace("\r\n", "\n")
}

fn function_body<'a>(source: &'a str, signature: &str, next: &str) -> &'a str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once(next))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("missing route body for {signature}"))
}

#[test]
fn encryption_string_observers_use_the_canonical_as_string_route() {
    let source = source("src/encryption/state.rs");
    for (signature, next) in [
        (
            "fn required_32_byte_string_from_handle(",
            "fn required_v_lt_5_32_byte_string_from_handle(",
        ),
        (
            "fn required_v_lt_5_32_byte_string_from_handle(",
            "fn required_48_byte_string_from_handle(",
        ),
        (
            "fn required_48_byte_string_from_handle(",
            "fn encrypt_metadata_flag_from_handle(",
        ),
        (
            "fn r6_perms_warning_from_handle(",
            "/// qpdf's `/ID[0]` value",
        ),
    ] {
        let body = function_body(&source, signature, next);
        assert!(
            body.contains("try_as_string()?"),
            "{signature} must use ObjectHandle::try_as_string"
        );
        assert!(
            !body.contains("try_dereference()?") && !body.contains(".as_string()"),
            "{signature} must not duplicate qpdf asString's resolve/cast pair"
        );
    }
}

#[test]
fn utf8_string_accessors_use_the_canonical_as_string_route() {
    let source = source("src/object_handle.rs");
    for (signature, next) in [
        (
            "pub fn try_get_value_as_utf8(",
            "pub fn try_get_value_as_operator(",
        ),
        (
            "pub fn try_get_utf8_value(",
            "pub fn try_get_operator_value(",
        ),
    ] {
        let body = function_body(&source, signature, next);
        assert!(
            body.contains("try_as_string()?"),
            "{signature} must use ObjectHandle::try_as_string"
        );
        assert!(
            !body.contains("try_dereference()?") && !body.contains("try_get_value_as_string()?"),
            "{signature} must not duplicate qpdf asString's resolve/cast pair"
        );
    }
}

#[test]
fn page_label_prefix_uses_the_silent_canonical_as_string_route() {
    let source = source("src/page_label_document_helper.rs");
    let body = function_body(&source, "fn from_handle(", "fn label_for_page(");
    assert!(
        body.contains("try_as_string()?"),
        "page-label /P must use ObjectHandle::try_as_string"
    );
    assert!(
        !body.contains("prefix_handle.try_dereference()?")
            && !body.contains("prefix_handle.as_string()"),
        "page-label /P must not duplicate qpdf asString's resolve/cast pair"
    );
}
