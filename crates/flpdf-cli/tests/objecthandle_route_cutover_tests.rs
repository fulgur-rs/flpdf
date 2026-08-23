#[test]
fn cli_production_has_no_legacy_object_route() {
    let source = include_str!("../src/main.rs").replace("\r\n", "\n");
    let production = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("main.rs test module marker");

    for forbidden in ["Object::", "resolve_borrowed"] {
        assert!(
            !production.contains(forbidden),
            "flpdf-cli production still uses legacy {forbidden} route"
        );
    }
}
