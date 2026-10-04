fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn page_helper_uses_qpdf_callbacks_for_recursive_xobjects_only() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = production_source(source);

    assert!(production.contains("pub fn for_each_image<F>"));
    assert!(production.contains("pub fn for_each_form_xobject<F>"));
    assert!(production.contains("pub fn get_images(&mut self)"));
    assert!(production.contains("pub fn get_form_xobjects(&mut self)"));
    assert!(
        !production.contains("pub fn get_images_recursive"),
        "qpdf has recursive forEachImage, not a recursive map getter"
    );
    assert!(
        !production.contains("pub fn get_form_xobjects_recursive"),
        "qpdf has recursive forEachFormXObject, not a recursive map getter"
    );
}
