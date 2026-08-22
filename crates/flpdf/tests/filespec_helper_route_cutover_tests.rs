use std::fs;
use std::path::Path;

#[test]
fn filespec_helpers_have_qpdf_owner_modules_and_no_old_facade() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!root.join("src/filespec_helper.rs").exists());
    let module = fs::read_to_string(root.join("src/filespec_helper/mod.rs")).unwrap();
    assert!(module.contains("mod filespec;"));
    assert!(module.contains("mod embedded_file_stream;"));
    assert!(root.join("src/filespec_helper/filespec.rs").exists());
    assert!(root
        .join("src/filespec_helper/embedded_file_stream.rs")
        .exists());

    let filespec = fs::read_to_string(root.join("src/filespec_helper/filespec.rs")).unwrap();
    let ef_stream =
        fs::read_to_string(root.join("src/filespec_helper/embedded_file_stream.rs")).unwrap();
    let job = fs::read_to_string(root.join("src/job/attachments.rs")).unwrap();
    assert!(filespec.contains("pub struct FileSpec"));
    assert!(filespec.contains("pub struct FileSpecBuilder"));
    assert!(ef_stream.contains("pub struct EmbeddedFileStream"));
    for function in [
        "pub fn add_attachment_from_path",
        "pub fn extract_attachment",
        "pub fn write_attachment",
        "pub fn extract_attachment_to_path",
    ] {
        assert!(!module.contains(function));
        assert!(!filespec.contains(function));
        assert!(job.contains(function));
    }
}
