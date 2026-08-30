//! Final zero-route contracts for the qpdf ObjectHandle object model.

use std::fs;
use std::path::{Path, PathBuf};

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn source(path: impl AsRef<Path>) -> String {
    fs::read_to_string(source_root().join(path.as_ref()))
        .unwrap_or_else(|error| panic!("unable to read {}: {error}", path.as_ref().display()))
        .replace("\r\n", "\n")
}

fn production_source(path: impl AsRef<Path>) -> String {
    let source = source(path);
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn raw_object_module_and_exports_are_deleted() {
    let root = source_root();
    assert!(
        !root.join("object.rs").exists(),
        "the legacy raw object module must be removed"
    );
    assert!(
        root.join("object_ref.rs").is_file(),
        "ObjectRef must live in its own canonical module"
    );

    let lib = source("lib.rs");
    assert!(!lib.contains("pub mod object;"));
    assert!(!lib.contains("pub use object::{"));
    assert!(!lib.contains("pub use parser::parse_object"));
    assert!(!lib.contains("pub use content_stream::ParserCallbacks"));
}

#[test]
fn handle_model_has_no_materialization_or_reference_value_route() {
    let object_handle = source("object_handle.rs");
    assert!(!object_handle.contains("pub fn materialize"));
    assert!(!object_handle.contains("materialize_bounded"));
    assert!(!object_handle.contains("ObjectValue::Reference"));
}

#[test]
fn parser_and_content_streams_have_only_handle_callbacks() {
    let parser = production_source("parser.rs");
    assert!(!parser.contains("fn parse_qpdf_direct_object("));
    assert!(!parser.contains("-> Result<Object>"));

    let content_stream = production_source("content_stream.rs");
    assert!(!content_stream.contains("trait ParserCallbacks"));
    assert!(!content_stream.contains("parse_content_stream_data"));
}

#[test]
fn reader_and_xref_have_no_legacy_replacement_cache() {
    let reader = production_source("reader.rs");
    for legacy in [
        "lift_object_to_handle",
        "pub fn set_object(",
        "resolve_borrowed",
        "resolve_object",
    ] {
        assert!(
            !reader.contains(legacy),
            "reader retains legacy route {legacy}"
        );
    }

    let xref = production_source("xref.rs");
    for legacy in ["XrefObjectCache", "crate::object::", "use crate::object::"] {
        assert!(!xref.contains(legacy), "xref retains legacy route {legacy}");
    }
}

#[test]
fn canonical_writer_and_pdf_surfaces_do_not_import_raw_object_types() {
    for path in [
        "pdf.rs",
        "writer.rs",
        "writer/encrypted_strings.rs",
        "writer/object.rs",
        "writer/object_streams/emission.rs",
        "writer/rewrite_renumber.rs",
        "writer/serialize.rs",
    ] {
        let production = production_source(path);
        for legacy in [
            "crate::object::",
            "use crate::object::",
            "ObjectValue::Reference",
            "materialize(",
        ] {
            assert!(
                !production.contains(legacy),
                "{path} retains raw object surface {legacy}"
            );
        }
    }
}
