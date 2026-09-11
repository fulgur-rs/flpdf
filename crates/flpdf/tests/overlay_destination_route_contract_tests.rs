fn overlay_page_handle_source() -> String {
    let source = include_str!("../src/job/overlay.rs").replace("\r\n", "\n");
    let start = source
        .find("fn overlay_page_handle")
        .expect("overlay_page_handle must exist");
    let body = &source[start..];
    let end = body
        .find("\n// Feature-gated byte-identity gate")
        .expect("overlay byte gate must follow overlay_page_handle");
    body[..end].to_owned()
}

#[test]
fn overlay_destination_uses_the_canonical_handle_resolver() {
    let source = overlay_page_handle_source();
    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !source.contains(forbidden),
            "overlay_page_handle retains non-canonical route {forbidden}"
        );
    }
    assert!(source.contains(".try_dereference()?"));
    assert!(source.contains(".try_as_dictionary()?"));
}
