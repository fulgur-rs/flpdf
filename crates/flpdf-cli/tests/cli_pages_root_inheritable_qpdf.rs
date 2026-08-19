//! Rebuilt `/Pages` root inheritable-key parity vs qpdf 11.9.0.
//!
//! `QPDF_optimization::pushInheritedAttributesToPage` pushes the effective
//! `/MediaBox`, `/CropBox`, `/Resources`, and `/Rotate` values to selected
//! leaves and removes those keys from the retained root. The `--pages` CLI
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
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] /CropBox [10 20 500 700] /Resources << >> /Rotate 180 >>",
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

fn pages_root(path: &Path) -> flpdf::Dictionary {
    let file = File::open(path).expect("output PDF must be readable");
    let mut pdf = Pdf::open(BufReader::new(file)).expect("output PDF must parse");
    let catalog_ref = pdf.root_ref().expect("output PDF must have a catalog");
    let catalog = pdf.resolve(catalog_ref).expect("catalog must resolve");
    let pages_ref = match catalog {
        Object::Dictionary(dictionary) => match dictionary.get("Pages") {
            Some(Object::Reference(reference)) => *reference,
            other => panic!("catalog /Pages must be an indirect reference: {other:?}"),
        },
        other => panic!("catalog must be a dictionary: {other:?}"),
    };
    match pdf.resolve(pages_ref).expect("/Pages root must resolve") {
        Object::Dictionary(dictionary) => dictionary,
        other => panic!("/Pages root must be a dictionary: {other:?}"),
    }
}

fn assert_rebuilt_root_shape(root: &flpdf::Dictionary, tool: &str) {
    assert_eq!(
        root.get("Type"),
        Some(&Object::Name(b"Pages".to_vec())),
        "{tool}: rebuilt root must retain /Type /Pages"
    );
    assert!(
        root.get("Kids").is_some(),
        "{tool}: rebuilt root needs /Kids"
    );
    assert_eq!(
        root.get("Count"),
        Some(&Object::Integer(1)),
        "{tool}: rebuilt root must retain /Count 1"
    );
    for key in ["MediaBox", "CropBox", "Resources", "Rotate"] {
        assert!(
            root.get(key).is_none(),
            "{tool}: rebuilt root must not retain inheritable /{key}: {root:?}"
        );
    }
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

    let qpdf_root = pages_root(&qpdf_output);
    let flpdf_root = pages_root(&flpdf_output);
    assert_rebuilt_root_shape(&qpdf_root, "qpdf");
    assert_rebuilt_root_shape(&flpdf_root, "flpdf");
}
