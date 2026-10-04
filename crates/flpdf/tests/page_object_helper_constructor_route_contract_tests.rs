fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn page_helper_exposes_the_qpdf_handle_constructor_without_an_object_ref_overload() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = production_source(source);

    assert!(production.contains("pub fn from_object_handle(object: ObjectHandle"));
    assert!(
        !production.contains("pub fn new(page_ref: ObjectRef"),
        "qpdf constructs QPDFPageObjectHelper from QPDFObjectHandle, not ObjectRef"
    );
}
