fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn resources_use_the_qpdf_generic_attribute_api_without_a_fixed_key_alias() {
    let helper = production_source(include_str!("../src/page_object_helper.rs"));
    assert!(helper.contains("pub fn get_attribute"));
    assert!(
        !helper.contains("pub fn get_resources"),
        "QPDFPageObjectHelper exposes getAttribute, not a getResources method"
    );

    let callers = [
        ("page_object_helper.rs", helper),
        ("resources.rs", include_str!("../src/resources.rs")),
        (
            "image_optimization.rs",
            include_str!("image_optimization.rs"),
        ),
        (
            "test_56_63.rs",
            include_str!("../../flpdf-qtest-tools/src/driver/test_56_63.rs"),
        ),
        (
            "test_64_71.rs",
            include_str!("../../flpdf-qtest-tools/src/driver/test_64_71.rs"),
        ),
    ];

    for (module, source) in callers {
        assert!(
            !source.contains(".get_resources("),
            "{module} retains the qpdf-less fixed-key resources alias"
        );
    }
}
