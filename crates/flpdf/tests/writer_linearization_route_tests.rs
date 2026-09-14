//! Route contracts for the writer and linearization ObjectHandle paths.

fn production_source(source: &str, test_module: &str) -> String {
    let source = source.replace("\r\n", "\n");
    source
        .split_once(test_module)
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn full_rewrite_root_reconciliation_has_no_legacy_snapshot_bridge() {
    let source = include_str!("../src/writer.rs").replace("\r\n", "\n");
    let route = source
        .split_once("pub(crate) fn emit_canonical_pdf")
        .and_then(|(_, rest)| rest.split_once("fn emit_canonical_pdf_inner"))
        .map(|(route, _)| route)
        .expect("full-rewrite writer route exists");

    assert!(route.contains("emit_canonical_pdf_inner"));
    assert!(!route.contains("snapshot_catalog_extensions"));
    assert!(!route.contains("restore_catalog_extensions"));
    assert!(
        !route.contains(".materialize()"),
        "Root reconciliation must not rebuild a legacy Object snapshot"
    );
    assert!(
        !route.contains("Object::Dictionary"),
        "Root reconciliation must remain on the canonical handle graph"
    );
    for legacy_bridge in [
        "snapshot_catalog_extensions",
        "restore_catalog_extensions",
        "inject_adbe_extension",
        "strip_adbe_extension",
    ] {
        assert!(
            !source.contains(legacy_bridge),
            "legacy ADBE bridge {legacy_bridge} must be removed after root cutover"
        );
    }
}

#[test]
fn linearization_id_construction_is_handle_native() {
    let source = production_source(
        include_str!("../src/linearization/writer.rs"),
        "\n#[cfg(test)]\nmod tests {",
    );

    let finalize = source
        .split_once("fn finalize_linearized_id")
        .and_then(|(_, rest)| rest.split_once("/// Build qpdf's pass-1 `/ID`"))
        .map(|(function, _)| function)
        .expect("linearization final ID helper exists");
    assert!(finalize.contains("-> ObjectHandle"));
    assert!(!finalize.contains("Object::"));

    let pass1 = source
        .split_once("fn linearization_pass1_id")
        .and_then(|(_, rest)| rest.split_once("/// Reserve the **first-page (Part-1)"))
        .map(|(function, _)| function)
        .expect("linearization pass-1 ID helper exists");
    assert!(pass1.contains("-> ObjectHandle"));
    assert!(!pass1.contains("Object::"));

    assert!(!source.contains("fn id_object_to_handle"));
    let implementation = source
        .split_once("fn write_linearized_impl")
        .map(|(_, rest)| rest)
        .expect("linearization implementation exists");
    assert!(!implementation.contains("id_object_to_handle"));
    assert!(
        implementation.contains("FinalLinearizedLayout"),
        "final linearization coordinates must be derived from pass-1 metadata"
    );
    assert!(
        implementation.contains("final_source_trailer"),
        "final IDs must be emitted through the final trailer view"
    );
    assert!(
        !implementation.contains("patch_linearized_deterministic_id"),
        "canonical final pass must not scan and patch an output buffer"
    );
}

#[test]
fn linearization_pass_one_is_metadata_only_and_uses_counted_output() {
    let source = include_str!("../src/linearization/writer.rs").replace("\r\n", "\n");
    let metadata = source
        .split_once("struct LinearizedPassOutput")
        .and_then(|(_, rest)| rest.split_once("/// Perform a complete single-pass write"))
        .map(|(definition, _)| definition)
        .expect("linearized pass metadata exists");
    assert!(
        !metadata.contains("bytes: Vec<u8>"),
        "pass metadata must not own the complete pass-1 body"
    );

    let write_pass = source
        .split_once("fn do_write_pass")
        .and_then(|(_, rest)| rest.split_once("/// Compute per-object byte lengths"))
        .map(|(function, _)| function)
        .expect("linearized pass writer exists");
    assert!(
        write_pass.contains("output: &mut OutputSink<'_>"),
        "both layout passes must use the counted output boundary"
    );
    assert!(
        !write_pass.contains("let mut bytes: Vec<u8>"),
        "do_write_pass must not allocate a document-sized body Vec"
    );
    assert!(
        !source.contains("pass1_output.bytes"),
        "pass-1 consumers must use metadata and incremental digest state"
    );
}

#[test]
fn linearization_final_route_does_not_clone_complete_xref_maps() {
    let source = production_source(
        include_str!("../src/linearization/writer.rs"),
        "\n#[cfg(test)]\nmod tests {",
    );
    let implementation = source
        .split_once("fn write_linearized_impl")
        .map(|(_, rest)| rest)
        .expect("linearization implementation exists");
    assert!(
        !implementation.contains("pass1_output.xref_offsets.clone()"),
        "the final layout must reuse the pass-1 xref owner"
    );
    assert!(
        !implementation.contains("xref_offsets: final_xref_offsets.clone()"),
        "Part-1 metadata must not clone the complete final xref map"
    );
    let pass = source
        .split_once("fn do_write_pass")
        .and_then(|(_, rest)| rest.split_once("/// Compute per-object byte lengths"))
        .map(|(function, _)| function)
        .expect("linearized pass writer exists");
    assert!(
        !pass.contains("layout.xref_offsets.clone()"),
        "the final pass must borrow the writer-owned xref map"
    );
    assert!(
        !source.contains("let mut virtual_offsets = xref_offsets.clone()"),
        "first-page xref encoding must not clone the complete physical xref map"
    );
}

#[test]
fn linearization_moves_the_writer_owned_renumber_map_into_the_two_pass_route() {
    let source = production_source(
        include_str!("../src/linearization/writer.rs"),
        "\n#[cfg(test)]\nmod tests {",
    );
    let implementation = source
        .split_once("fn write_linearized_impl")
        .map(|(_, rest)| rest)
        .expect("linearization implementation exists");
    assert!(
        implementation.contains("renumber: RenumberMap"),
        "the canonical two-pass writer must own the qpdf-shaped renumber map"
    );
    assert!(
        !implementation.contains("let mut local_renumber = renumber.clone()"),
        "linearization must not clone the complete renumber map before pass 1"
    );
}

#[test]
fn linearization_plan_does_not_retain_a_derived_page_user_inverse_map() {
    let source = production_source(
        include_str!("../src/linearization/plan.rs"),
        "\n#[cfg(test)]\nmod tests {",
    );
    assert!(
        source.contains("optimization.page_users("),
        "linearization planning must consume a borrowed view of the retained qpdf-shaped object-user map"
    );
    assert!(
        !source.contains("all_referenced_pages"),
        "the plan must not retain a second object-to-page inverse map"
    );
    assert!(
        !source.contains("referenced_pages("),
        "planning must not clone a page set for every object"
    );
}

#[test]
fn linearization_page_reach_uses_the_canonical_object_user_map() {
    let source = production_source(
        include_str!("../src/linearization/plan.rs"),
        "\n#[cfg(test)]\nmod tests {",
    );
    let page_partition = source
        .split_once("let mut page_hints")
        .and_then(|(_, rest)| rest.split_once("let provisional_set"))
        .map(|(section, _)| section)
        .expect("page partition route exists");
    assert!(
        page_partition.contains("optimization.page_users"),
        "page reach must come from qpdf-shaped object-user ownership"
    );
    assert!(
        !page_partition.contains("all_closures"),
        "page partition must not clone all page closures for reach counts"
    );
    assert!(
        !page_partition.contains("page_reach"),
        "page partition must not retain a second object-to-page map"
    );
}

#[test]
fn optimization_reverse_user_sets_use_compact_ordered_storage() {
    let source = include_str!("../src/optimization.rs").replace("\r\n", "\n");
    assert!(
        source.contains("CompactObjectUserSet {"),
        "object-user reverse values need a compact ordered-set owner"
    );
    assert!(
        source.contains("BTreeSet<ObjectUser>"),
        "large object-user cardinalities must retain a tree-backed ordered fallback"
    );
    assert!(
        !source.contains("object_to_users: BTreeMap<ObjectRef, BTreeSet<ObjectUser>>"),
        "the reverse table must not allocate a full BTreeSet value for every object"
    );
}

#[test]
fn prepare_file_for_write_is_owned_by_the_common_writer_boundary() {
    let writer_source = include_str!("../src/writer.rs").replace("\r\n", "\n");
    let write = writer_source
        .split_once("pub fn write(&mut self) -> Result<()>")
        .and_then(|(_, rest)| rest.split_once("    /// Return the output identity"))
        .map(|(function, _)| function)
        .expect("PdfWriter::write exists");
    assert_eq!(
        write.matches("prepare_file_for_write").count(),
        1,
        "the common PdfWriter route must prepare the graph exactly once"
    );

    let linearization_source = include_str!("../src/linearization/writer.rs").replace("\r\n", "\n");
    let linearization_route = linearization_source
        .split_once("pub(crate) fn write_linearized_for_pdf_writer")
        .and_then(|(_, rest)| rest.split_once("/// Append qpdf's pass-1 debugging comments"))
        .map(|(function, _)| function)
        .expect("PdfWriter linearization route exists");
    assert!(
        !linearization_route.contains("prepare_linearization_catalog"),
        "linearization must consume the common preparation boundary"
    );
}
