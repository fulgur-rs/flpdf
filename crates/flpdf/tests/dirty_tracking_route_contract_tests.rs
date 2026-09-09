use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read source directory") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn production_sources_have_no_dirty_tracking_bridge() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        manifest_dir.join("src"),
        manifest_dir.join("../flpdf-cli/src"),
        manifest_dir.join("../flpdf-qtest-tools/src"),
    ];
    let forbidden = [
        "dirty_object_refs",
        "mark_object_handle_dirty",
        "mark_object_dirty",
        "mark_object_handle_mutated",
        "is_dirty(",
        "clear_dirty(",
    ];

    let mut files = Vec::new();
    for root in roots {
        rust_sources(&root, &mut files);
    }
    for path in files {
        let source = fs::read_to_string(&path).expect("read Rust source");
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "dirty tracking marker {marker:?} remains in {}",
                path.display()
            );
        }
    }
}
