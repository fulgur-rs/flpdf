//! Rebuilt `/Pages` root inheritable-key parity vs qpdf 11.9.0.
//!
//! `QPDF_optimization::pushInheritedAttributesToPage` pushes the effective
//! `/MediaBox`, `/CropBox`, `/Resources`, and `/Rotate` values to selected
//! leaves and removes those keys from the page-tree nodes. The `--pages` CLI
//! consumer must preserve that shape after rebuilding its page tree.

use assert_cmd::Command;
use flpdf::{Object, Pdf};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command as Shell;

const QPDF: &str = "/usr/bin/qpdf";
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn qpdf_available() -> bool {
    if !Path::new(QPDF).exists() {
        return false;
    }
    match Shell::new(QPDF).arg("--version").output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        _ => false,
    }
}

/// One page inherits all four qpdf page-tree attributes directly from root.
fn root_inheritable_fixture() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /CustomRoot /retained /MediaBox [0 0 612 792] /CropBox [10 20 500 700] /Resources << >> /Rotate 180 >>",
        ),
        (3, "<< /Type /Page /Parent 2 0 R >>"),
    ];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    for &(number, body) in objects {
        offsets.insert(number, bytes.len() as u64);
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    let xref_start = bytes.len() as u64;
    let size = objects.iter().map(|(number, _)| *number).max().unwrap() + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

fn resolve_to_terminal(pdf: &mut Pdf<BufReader<File>>, mut value: Object) -> Object {
    for _ in 0..64 {
        match value {
            Object::Reference(reference) => {
                value = pdf
                    .resolve(reference)
                    .expect("referenced value must resolve");
            }
            other => return other,
        }
    }
    panic!("object reference chain exceeded test depth bound");
}

#[derive(Debug, PartialEq)]
struct PageSnapshot {
    type_name: Option<Object>,
    media_box: Option<Object>,
    crop_box: Option<Object>,
    resources: Option<Object>,
    rotate: Option<Object>,
    parent_is_reference: bool,
}

#[derive(Debug, PartialEq)]
struct PageTreeSnapshot {
    root_type: Option<Object>,
    root_count: Option<Object>,
    root_custom: Option<Object>,
    root_inheritable_keys: [bool; 4],
    kids: Vec<PageSnapshot>,
}

fn page_snapshot(pdf: &mut Pdf<BufReader<File>>, value: Object) -> PageSnapshot {
    let page = resolve_to_terminal(pdf, value);
    let Object::Dictionary(page) = page else {
        panic!("page kid must resolve to a dictionary: {page:?}");
    };
    let mut resolved = |key: &str| {
        page.get(key)
            .cloned()
            .map(|value| resolve_to_terminal(pdf, value))
    };
    PageSnapshot {
        type_name: resolved("Type"),
        media_box: resolved("MediaBox"),
        crop_box: resolved("CropBox"),
        resources: resolved("Resources"),
        rotate: resolved("Rotate"),
        parent_is_reference: matches!(page.get("Parent"), Some(Object::Reference(_))),
    }
}

fn page_tree_snapshot(path: &Path) -> PageTreeSnapshot {
    let file = File::open(path).expect("output PDF must be readable");
    let mut pdf = Pdf::open(BufReader::new(file)).expect("output PDF must parse");
    let catalog_ref = pdf.root_ref().expect("output PDF must have a catalog");
    let catalog = pdf.resolve(catalog_ref).expect("catalog must resolve");
    let Object::Dictionary(catalog) = catalog else {
        panic!("catalog must be a dictionary: {catalog:?}");
    };
    let pages = catalog
        .get("Pages")
        .cloned()
        .expect("catalog must contain /Pages");
    let pages = resolve_to_terminal(&mut pdf, pages);
    let Object::Dictionary(root) = pages else {
        panic!("/Pages root must be a dictionary: {pages:?}");
    };
    let kids = root
        .get("Kids")
        .cloned()
        .map(|value| resolve_to_terminal(&mut pdf, value))
        .expect("rebuilt root must contain /Kids");
    let Object::Array(kids) = kids else {
        panic!("rebuilt root /Kids must be an array: {kids:?}");
    };
    PageTreeSnapshot {
        root_type: root.get("Type").cloned(),
        root_count: root.get("Count").cloned(),
        root_custom: root.get("CustomRoot").cloned(),
        root_inheritable_keys: ["MediaBox", "CropBox", "Resources", "Rotate"]
            .map(|key| root.get(key).is_some()),
        kids: kids
            .into_iter()
            .map(|kid| page_snapshot(&mut pdf, kid))
            .collect(),
    }
}

fn assert_rebuilt_root_shape(tree: &PageTreeSnapshot, tool: &str) {
    assert_eq!(
        tree.root_type,
        Some(Object::Name(b"Pages".to_vec())),
        "{tool}: rebuilt root must retain /Type /Pages"
    );
    assert_eq!(
        tree.root_count,
        Some(Object::Integer(1)),
        "{tool}: rebuilt root must retain /Count 1"
    );
    assert_eq!(
        tree.root_custom,
        Some(Object::Name(b"retained".to_vec())),
        "{tool}: rebuilt root must retain non-inheritable /CustomRoot"
    );
    assert_eq!(
        tree.root_inheritable_keys,
        [false, false, false, false],
        "{tool}: rebuilt root must not retain qpdf inheritable keys"
    );
    assert_eq!(
        tree.kids.len(),
        1,
        "{tool}: rebuilt root must retain one selected page"
    );
}

fn assert_materialized_page(tree: &PageTreeSnapshot, tool: &str) {
    let page = tree.kids.first().expect("root must have one page kid");
    assert_eq!(
        page.type_name,
        Some(Object::Name(b"Page".to_vec())),
        "{tool}: selected kid must remain /Type /Page"
    );
    assert_eq!(
        page.media_box,
        Some(Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ])),
        "{tool}: selected page must materialize /MediaBox"
    );
    assert_eq!(
        page.crop_box,
        Some(Object::Array(vec![
            Object::Integer(10),
            Object::Integer(20),
            Object::Integer(500),
            Object::Integer(700),
        ])),
        "{tool}: selected page must materialize /CropBox"
    );
    assert_eq!(
        page.resources,
        Some(Object::Dictionary(flpdf::Dictionary::new())),
        "{tool}: selected page must materialize /Resources"
    );
    assert_eq!(
        page.rotate,
        Some(Object::Integer(180)),
        "{tool}: selected page must materialize /Rotate"
    );
    assert!(
        page.parent_is_reference,
        "{tool}: selected page must be reparented to the rebuilt root"
    );
}

#[test]
fn cli_pages_removes_root_inheritable_attributes_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf {EXPECTED_QPDF_VERSION} unavailable; skipping root parity test");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory must be available");
    let input = temp.path().join("input.pdf");
    let qpdf_output = temp.path().join("qpdf-output.pdf");
    let flpdf_output = temp.path().join("flpdf-output.pdf");
    std::fs::write(&input, root_inheritable_fixture()).expect("fixture must be writable");

    let qpdf = Shell::new(QPDF)
        .args([
            input.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            qpdf_output.to_str().unwrap(),
        ])
        .output()
        .expect("qpdf must spawn");
    assert!(
        qpdf.status.success(),
        "qpdf --pages must succeed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .expect("flpdf binary must build")
        .args([
            input.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            flpdf_output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let qpdf_tree = page_tree_snapshot(&qpdf_output);
    let flpdf_tree = page_tree_snapshot(&flpdf_output);
    assert_rebuilt_root_shape(&qpdf_tree, "qpdf");
    assert_rebuilt_root_shape(&flpdf_tree, "flpdf");
    assert_materialized_page(&qpdf_tree, "qpdf");
    assert_materialized_page(&flpdf_tree, "flpdf");
    assert_eq!(
        flpdf_tree, qpdf_tree,
        "flpdf --pages must match qpdf's normalized root/kids/page shape"
    );
}
