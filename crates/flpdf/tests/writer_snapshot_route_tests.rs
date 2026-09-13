use std::path::Path;

#[test]
fn preserve_membership_snapshot_reaches_every_planned_writer_consumer() {
    // qpdf captures source ObjStm membership during writer setup and reuses
    // that state after preparation (`QPDFWriter.cc:1939-1967,2114-2140`).
    // These route contracts keep planned and linearized production consumers
    // from silently falling back to a second raw-xref read.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let writer = std::fs::read_to_string(root.join("writer.rs")).unwrap();
    let linearization_plan = std::fs::read_to_string(root.join("linearization/plan.rs")).unwrap();
    let linearization_writer =
        std::fs::read_to_string(root.join("linearization/writer.rs")).unwrap();

    assert!(writer.contains("plan_object_streams_with_reachability_and_source_membership("));
    assert!(writer.contains("Some(&source_object_stream_data)"));
    assert!(linearization_plan.contains("from_pdf_with_writer_options_and_source_membership("));
    assert!(linearization_writer.contains("source_container_by_member"));
    assert!(!linearization_writer.contains("source_xref_entries()"));
}
