//! Route contracts for the writer and linearization ObjectHandle paths.

fn production_source(source: &str, test_module: &str) -> String {
    let source = source.replace("\r\n", "\n");
    source
        .split_once(test_module)
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn full_rewrite_catalog_restore_uses_the_live_handle() {
    let source = include_str!("../src/writer.rs").replace("\r\n", "\n");
    let route = source
        .split_once("pub(crate) fn emit_canonical_pdf")
        .and_then(|(_, rest)| rest.split_once("fn emit_canonical_pdf_inner"))
        .map(|(route, _)| route)
        .expect("full-rewrite writer route exists");

    assert!(route.contains("ObjectHandle"));
    assert!(route.contains("restore_key_raw"));
    assert!(
        !route.contains(".materialize()"),
        "Catalog restoration must not rebuild a legacy Object snapshot"
    );
    assert!(
        !route.contains("Object::Dictionary"),
        "Catalog restoration must remain on the canonical handle graph"
    );
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
        .and_then(|(_, rest)| rest.split_once("/// Overwrite every all-zero deterministic"))
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
        .and_then(|(_, rest)| rest.split_once("/// Write the pass-1 body"))
        .map(|(function, _)| function)
        .expect("PdfWriter linearization route exists");
    assert!(
        !linearization_route.contains("prepare_linearization_catalog"),
        "linearization must consume the common preparation boundary"
    );
}
