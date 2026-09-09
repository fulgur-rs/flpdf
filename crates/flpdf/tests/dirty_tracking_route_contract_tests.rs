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

/// The live correspondence tables must not keep telling readers to call the
/// removed API. `docs/plans` and `docs/superpowers/plans` are historical design
/// records that document the tree as it was, so they stay out of scope.
///
/// A note that names the marker only to say it was removed is fine; what this
/// rejects is an instruction or a classification that still depends on it. The
/// audit-history line at the end of `e-job-cli-capi.md` is such a note.
#[test]
fn live_correspondence_docs_have_no_dirty_tracking_instructions() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut docs = vec![repo_root.join("docs/qpdf-correspondence.md")];
    for entry in
        fs::read_dir(repo_root.join("docs/qpdf-route-matrix")).expect("read route matrix directory")
    {
        let path = entry.expect("read route matrix entry").path();
        if path.extension().is_some_and(|extension| extension == "md") {
            docs.push(path);
        }
    }

    // Phrases that only make sense while the bridge exists: an instruction to
    // call it, or a route classification that rests on it.
    let forbidden = [
        "を要求する",
        "dirty propagation",
        "dirty bookkeeping を",
        "を呼ぶため `bridge`",
    ];
    // A line that dates the removal is a record, not a live instruction.
    let removal_note = "flpdf-3yn9.48.24";
    for path in docs {
        let source = fs::read_to_string(&path).expect("read documentation");
        for line in source.lines() {
            if line.contains(removal_note) {
                continue;
            }
            if !line.contains("mark_object_handle_dirty")
                && !line.contains("mark_object_dirty")
                && !line.contains("mark_object_handle_mutated")
                && !line.contains("dirty propagation")
                && !line.contains("dirty bookkeeping")
            {
                continue;
            }
            for marker in forbidden {
                assert!(
                    !line.contains(marker),
                    "line still depends on the removed dirty bridge ({marker:?}) in {}:\n  {line}",
                    path.display()
                );
            }
        }
    }
}
