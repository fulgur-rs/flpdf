//! Route contracts for encrypted dictionary serialization.

fn production_function_body<'a>(source: &'a str, signature: &str, end: &str) -> &'a str {
    source
        .split_once(signature)
        .and_then(|(_, tail)| tail.split_once(end))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("production route {signature:?} must exist"))
}

#[test]
fn encryption_dictionary_serialization_resolves_dictionary_and_string_handles() {
    let source = include_str!("../src/writer/encrypted_strings.rs").replace("\r\n", "\n");
    let route = production_function_body(
        &source,
        "pub(crate) fn write_encryption_dictionary_handle(",
        "\n#[cfg(test)]",
    );

    assert!(
        route.contains("handle.try_as_dictionary()?"),
        "the Encrypt dictionary handle must use the resolving dictionary projection"
    );
    assert!(
        route.contains("value.try_as_string()?"),
        "indirect encryption-key strings must use the resolving string projection"
    );
    assert!(!route.contains("handle.as_dictionary()"));
    assert!(!route.contains("value.as_string()"));
}
