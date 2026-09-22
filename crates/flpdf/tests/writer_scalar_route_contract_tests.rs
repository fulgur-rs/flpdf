use std::fs;
use std::path::PathBuf;

#[test]
fn dynamic_child_writer_has_a_direct_scalar_boundary() {
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/writer/object.rs"))
            .expect("writer/object.rs must be readable");
    let start = source
        .find("fn unparse_child_with_dynamic_ref_map_and_string_writer<F>(")
        .expect("dynamic child writer must exist");
    let end = start
        + source[start..]
            .find("fn unparse_object_walk_with_dynamic_ref_map_and_string_writer<F>(")
            .expect("dynamic child writer boundary must exist");
    let body = &source[start..end];
    assert!(
        body.contains("write_direct_child_with_dynamic_ref_map_and_string_writer"),
        "direct scalar children must avoid the recursive object walker"
    );
}

#[test]
fn ordinary_child_walkers_have_direct_scalar_boundaries() {
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/writer/object.rs"))
            .expect("writer/object.rs must be readable");
    for (start_marker, end_marker, helper) in [
        (
            "pub(crate) fn unparse_child(handle: &ObjectHandle",
            "fn visible_dict_entries",
            "write_direct_child(",
        ),
        (
            "fn unparse_child_with_ref_map(",
            "fn unparse_object_walk_with_ref_map(",
            "write_direct_child_with_ref_map(",
        ),
        (
            "fn unparse_child_with_dynamic_ref_map(",
            "fn unparse_object_walk_with_dynamic_ref_map(",
            "write_direct_child_with_dynamic_ref_map(",
        ),
    ] {
        let start = source.find(start_marker).expect("ordinary child writer");
        let end = start
            + source[start..]
                .find(end_marker)
                .expect("child writer boundary");
        assert!(
            source[start..end].contains(helper),
            "ordinary child writer must use {helper}"
        );
    }
}
