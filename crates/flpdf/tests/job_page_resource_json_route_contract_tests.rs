//! Route contracts for the remaining A6/A7/A8 Job, page, resource, and JSON consumers.

use std::fs;
use std::path::PathBuf;

fn production_source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
        .replace("\r\n", "\n");
    source
        .split_once("\n#[cfg(test)]\nmod ")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn remaining_job_page_resource_and_json_consumers_use_canonical_routes() {
    let files = [
        "src/job/page_merge.rs",
        "src/job/page_specs.rs",
        "src/job/resource_pruning.rs",
        "src/pages.rs",
        "src/pages/repair.rs",
        "src/resources.rs",
        "src/overlay_appearance_stream.rs",
        "src/document_json.rs",
    ];
    let forbidden = [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
        ".as_dictionary(",
        ".as_array(",
        ".as_integer(",
        ".as_name(",
        ".is_null(",
    ];

    for file in files {
        let source = production_source(file);
        assert!(
            source.contains(".try_"),
            "{file} production must use a canonical fallible accessor"
        );
        for pattern in forbidden {
            assert!(
                !source.contains(pattern),
                "{file} production retains non-canonical route {pattern}"
            );
        }
    }
}
