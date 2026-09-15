//! Attachment routes must apply page selection and rotation like qpdf.
//!
//! qpdf runs `handlePageSpecs` and `handleRotations` before
//! `handleTransformations`, which is where `addAttachments` and
//! `copyAttachments` live (`libqpdf/QPDFJob.cc:465-474,2243,2246`). The
//! attachment routes in this binary configure their own job, so they need the
//! same page-spec and rotation plumbing the rewrite route has -- otherwise a
//! run that also selects pages or rotates exits 0 while silently emitting an
//! unrotated, unselected document.

#[path = "support/mod.rs"]
#[allow(dead_code, unused_imports)]
mod support;

use assert_cmd::Command as CargoCommand;
use std::io::Write;
use std::path::Path;

fn two_page_pdf() -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(include_bytes!(
        "../../../tests/fixtures/compat/attachment-two-page.pdf"
    ))
    .unwrap();
    file
}

fn rotate_values(path: &Path) -> Vec<i64> {
    let bytes = std::fs::read(path).unwrap();
    let mut values = Vec::new();
    let needle = b"/Rotate ";
    let mut index = 0;
    while let Some(found) = bytes[index..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let start = index + found + needle.len();
        let end = bytes[start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit() && *byte != b'-')
            .map_or(bytes.len(), |offset| start + offset);
        if let Ok(text) = std::str::from_utf8(&bytes[start..end]) {
            if let Ok(value) = text.parse::<i64>() {
                values.push(value);
            }
        }
        index = end;
    }
    values
}

fn page_count(path: &Path) -> usize {
    let bytes = std::fs::read(path).unwrap();
    bytes
        .windows(b"/Type /Page".len())
        .filter(|window| *window == b"/Type /Page")
        .count()
        .saturating_sub(
            bytes
                .windows(b"/Type /Pages".len())
                .filter(|window| *window == b"/Type /Pages")
                .count(),
        )
}

fn attachment_temp(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("probe.txt");
    std::fs::write(&path, b"probe payload").unwrap();
    path
}

#[test]
fn add_attachment_applies_rotation() {
    let temp = tempfile::tempdir().unwrap();
    let input = two_page_pdf();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
            "--rotate=90",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let rotations = rotate_values(&output);
    assert!(
        !rotations.is_empty() && rotations.iter().all(|value| *value == 90),
        "every page must carry the requested rotation, got {rotations:?}"
    );
}

#[test]
fn add_attachment_applies_page_selection() {
    let temp = tempfile::tempdir().unwrap();
    let input = two_page_pdf();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        page_count(&output),
        1,
        "the single selected page must be the only one written"
    );
}

#[test]
fn remove_attachment_applies_rotation() {
    let temp = tempfile::tempdir().unwrap();
    let input = two_page_pdf();
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            "--remove-attachment=attachment.txt",
            "--rotate=90",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let rotations = rotate_values(&output);
    assert!(
        !rotations.is_empty() && rotations.iter().all(|value| *value == 90),
        "every page must carry the requested rotation, got {rotations:?}"
    );
}

#[test]
fn copy_attachments_applies_page_selection() {
    let temp = tempfile::tempdir().unwrap();
    let input = two_page_pdf();
    let donor = two_page_pdf();
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            "--copy-attachments-from",
            donor.path().to_str().unwrap(),
            "--prefix=P",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        page_count(&output),
        1,
        "the single selected page must be the only one written"
    );
}

#[test]
fn attachment_route_without_page_ops_still_writes() {
    let temp = tempfile::tempdir().unwrap();
    let input = two_page_pdf();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    // An empty page-spec list must not initialize the page-selection
    // machinery: qpdf calls handlePageSpecs only when page_specs is non-empty
    // (`libqpdf/QPDFJob.cc:466`).
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(page_count(&output), 2);
    assert!(rotate_values(&output).iter().all(|value| *value == 0));
}

#[test]
fn add_attachment_applies_collate() {
    let temp = tempfile::tempdir().unwrap();
    let input = two_page_pdf();
    let donor = two_page_pdf();
    let attachment = attachment_temp(temp.path());
    let collated = temp.path().join("collated.pdf");
    let sequential = temp.path().join("sequential.pdf");

    // `--collate` is consumed inside handlePageSpecs itself
    // (`libqpdf/QPDFJob.cc:2474-2502`), so it has to reach the job alongside
    // the specs. Interleaved and sequential runs over the same two sources
    // hold the same pages in a different order, so byte-inequality is what
    // proves the flag arrived.
    for (output, collate) in [(&collated, true), (&sequential, false)] {
        let mut command = CargoCommand::cargo_bin("flpdf").unwrap();
        command.arg("--static-id");
        if collate {
            command.arg("--collate");
        }
        command
            .args([
                input.path().to_str().unwrap(),
                "--pages",
                ".",
                "1-z",
                donor.path().to_str().unwrap(),
                "1-z",
                "--",
                "--add-attachment",
                attachment.to_str().unwrap(),
                "--",
                output.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    assert_eq!(page_count(&collated), 4);
    assert_eq!(page_count(&sequential), 4);
    assert_ne!(
        std::fs::read(&collated).unwrap(),
        std::fs::read(&sequential).unwrap(),
        "--collate must reorder the selected pages"
    );
}
