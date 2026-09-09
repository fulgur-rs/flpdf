use std::fs;
use std::path::Path;

fn production_source(path: &str) -> String {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
        .replace("\r\n", "\n");
    source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or_else(|| panic!("{path} has no production section"))
        .to_owned()
}

#[test]
fn acroform_filespec_embedded_and_signature_production_routes_are_handle_native() {
    for path in [
        "acroform_document_helper.rs",
        "filespec_helper/filespec.rs",
        "filespec_helper/embedded_file_stream.rs",
        "embedded_files.rs",
        "signatures.rs",
    ] {
        let source = production_source(path);
        for forbidden in [
            ".resolve(",
            "resolve_handle(",
            "resolve_handle_ref(",
            ".get_key(",
            ".has_key(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} still contains the legacy resolver/accessor bridge {forbidden}"
            );
        }
    }
}
