//! Route contracts for the qpdf-shaped PCLm writer boundary.

fn production_source(source: &str) -> &str {
    source
        .split("\n#[cfg(test)]")
        .next()
        .expect("source must contain a test boundary")
}

#[test]
fn pclm_planning_and_emission_do_not_materialize_legacy_objects() {
    let pclm = production_source(include_str!("../src/writer/pclm.rs"));
    assert!(
        !pclm.contains(".materialize()"),
        "PCLm planning must walk live ObjectHandle values"
    );

    let pclm_writer = include_str!("../src/writer/pclm_live.rs");
    assert!(
        !pclm_writer.contains(".materialize()"),
        "PCLm emission must not rebuild a legacy Object snapshot"
    );
}

#[test]
fn pclm_planning_uses_canonical_resolving_accessors() {
    let pclm = production_source(include_str!("../src/writer/pclm.rs"));

    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".is_null(",
    ] {
        assert!(
            !pclm.contains(forbidden),
            "PCLm planning retains legacy accessor route {forbidden}"
        );
    }
    assert!(
        pclm.contains("try_dereference") && pclm.contains("try_is_null"),
        "PCLm planning must use canonical resolving accessors"
    );
}
