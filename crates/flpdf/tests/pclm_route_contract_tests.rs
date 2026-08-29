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

    let writer = include_str!("../src/writer.rs");
    let pclm_writer = writer
        .split_once("fn write_pclm")
        .and_then(|(_, rest)| rest.split_once("fn emit_canonical_pdf_inner"))
        .map(|(pclm, _)| pclm)
        .expect("writer source must contain the PCLm boundary");
    assert!(
        !pclm_writer.contains(".materialize()"),
        "PCLm emission must not rebuild a legacy Object snapshot"
    );
}
