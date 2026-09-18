use std::path::Path;

#[test]
fn preserve_membership_snapshot_reaches_every_planned_writer_consumer() {
    // qpdf captures source ObjStm membership during writer setup and reuses
    // that state after preparation (`QPDFWriter.cc:1939-1967,2114-2140`).
    // These route contracts keep planned and linearized production consumers
    // from silently falling back to a second raw-xref read.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plain = std::fs::read_to_string(root.join("writer/plain/mod.rs")).unwrap();
    let plan = std::fs::read_to_string(root.join("writer/plain/plan.rs")).unwrap();
    let linearization_plan = std::fs::read_to_string(root.join("linearization/plan.rs")).unwrap();
    let linearization_writer =
        std::fs::read_to_string(root.join("linearization/writer.rs")).unwrap();

    assert!(plain.contains("build_live_object_stream_plan("));
    assert!(plain.contains("source_object_stream_data"));
    assert!(plain.contains("generated_compressible"));
    assert!(plain.contains("generated_object_stream_sources"));
    assert!(plan.contains("Some(source_object_stream_data)"));
    assert!(linearization_plan.contains("from_pdf_with_writer_options_and_source_membership("));
    assert!(linearization_writer.contains("source_container_by_member"));
    assert!(!linearization_writer.contains("source_xref_entries()"));
}

#[test]
fn special_stream_snapshot_reaches_the_linearized_plan() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let writer = std::fs::read_to_string(root.join("writer.rs")).unwrap();
    let linearization_plan = std::fs::read_to_string(root.join("linearization/plan.rs")).unwrap();
    let linearization_writer =
        std::fs::read_to_string(root.join("linearization/writer.rs")).unwrap();

    assert!(
        writer.contains("special_streams.as_ref()"),
        "the setup-owned special-stream snapshot must cross the linearized writer boundary"
    );
    assert!(
        linearization_writer.contains("special_streams: Option"),
        "the linearized writer must accept the setup snapshot"
    );
    assert!(
        linearization_plan.contains("normalized_streams_snapshot"),
        "the plan must consume setup normalized-stream membership"
    );
    assert!(
        writer.contains("normalized_streams_raw"),
        "the setup snapshot must retain qpdf's raw object-generation identity"
    );
    assert!(
        linearization_writer.contains("SpecialStreams::normalized_streams_raw"),
        "the linearized consumer must pass the raw setup identity to the plan"
    );
}
