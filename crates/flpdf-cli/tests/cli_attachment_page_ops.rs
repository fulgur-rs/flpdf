//! Attachment routes must apply page selection and rotation like qpdf.
//!
//! qpdf runs `handlePageSpecs` and `handleRotations` before
//! `handleTransformations`, which is where `addAttachments` and
//! `copyAttachments` live (`libqpdf/QPDFJob.cc:465-474,2243,2246`). The
//! attachment routes in this binary configure their own job, so they need the
//! same page-spec and rotation plumbing the rewrite route has -- otherwise a
//! run that also selects pages or rotates exits 0 while silently emitting an
//! unrotated, unselected document.

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

/// Return each page's `/Contents` object number in file order, which reflects
/// the order the pages were laid down.
fn page_content_order(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap();
    let needle = b"/Contents ";
    let mut order = Vec::new();
    let mut index = 0;
    while let Some(found) = bytes[index..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let start = index + found + needle.len();
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'/' || *byte == b'>')
            .map_or(bytes.len(), |offset| start + offset);
        order.push(bytes[start..end].to_vec());
        index = end;
    }
    order
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
                // The two runs are separate processes; without fixed dates
                // each embeds its own wall-clock timestamp and a run that
                // straddles a second boundary would differ for that reason
                // alone, masking a dropped --collate.
                "--creationdate=D:20200102030405Z",
                "--moddate=D:20200102030405Z",
                "--",
                output.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    assert_eq!(page_count(&collated), 4);
    assert_eq!(page_count(&sequential), 4);
    // Compare the actual page order, not just byte inequality: with the dates
    // pinned the only remaining difference is the interleave itself.
    let collated_order = page_content_order(&collated);
    let sequential_order = page_content_order(&sequential);
    assert_eq!(collated_order.len(), 4);
    assert_eq!(sequential_order.len(), 4);
    assert_ne!(
        collated_order, sequential_order,
        "--collate must interleave the sources instead of concatenating them"
    );
    assert_ne!(
        std::fs::read(&collated).unwrap(),
        std::fs::read(&sequential).unwrap(),
        "--collate must reorder the selected pages"
    );
}

/// Run qpdf with `args` plus a temporary output path and return the bytes, or
/// `None` when qpdf is unavailable.
fn qpdf_output(args: &[&str]) -> Option<Vec<u8>> {
    let directory = tempfile::tempdir().ok()?;
    let output = directory.path().join("oracle.pdf");
    let status = std::process::Command::new("qpdf")
        .args(args)
        .arg(&output)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&output).ok()
}

fn preserve_unreachable_pdf() -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(include_bytes!(
        "../../../tests/fixtures/compat/null-visible-preserve-unreachable.pdf"
    ))
    .unwrap();
    file
}

fn one_page_pdf() -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(include_bytes!(
        "../../../tests/fixtures/compat/one-page.pdf"
    ))
    .unwrap();
    file
}

/// Collect the `N 0 obj` numbers a written file defines, in ascending order.
/// This is independent of the DEFLATE backend, unlike a byte comparison.
fn object_numbers(bytes: &[u8]) -> Vec<u32> {
    let needle = b" 0 obj";
    let mut numbers = Vec::new();
    let mut index = 0;
    while let Some(found) = bytes[index..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let end = index + found;
        let start = bytes[..end]
            .iter()
            .rposition(|byte| !byte.is_ascii_digit())
            .map_or(0, |offset| offset + 1);
        if let Ok(text) = std::str::from_utf8(&bytes[start..end]) {
            if let Ok(value) = text.parse::<u32>() {
                numbers.push(value);
            }
        }
        index = end + needle.len();
    }
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

#[test]
fn add_attachment_honors_preserve_unreferenced() {
    let temp = tempfile::tempdir().unwrap();
    let input = preserve_unreachable_pdf();
    let donor = one_page_pdf();
    let attachment = attachment_temp(temp.path());
    let preserved = temp.path().join("preserved.pdf");
    let pruned = temp.path().join("pruned.pdf");

    // The page merge inside create_qpdf consults the writer configuration, so
    // the configuration has to be installed before any page spec reaches the
    // job -- otherwise the merge runs against the defaults and drops orphans.
    for (output, preserve) in [(&preserved, true), (&pruned, false)] {
        let mut command = CargoCommand::cargo_bin("flpdf").unwrap();
        command.arg("--static-id");
        if preserve {
            command.arg("--preserve-unreferenced");
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
                "--creationdate=D:20200102030405Z",
                "--moddate=D:20200102030405Z",
                "--",
                output.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    // Object counts alone are too weak: installing the writer configuration
    // after the merge still yields one extra object while dropping different
    // ones. Pin the surviving object numbers against qpdf instead -- a byte
    // comparison would fail on DEFLATE output alone unless this binary is
    // built with the zlib-compatible backend.
    let Some(oracle) = qpdf_output(&[
        "--static-id",
        "--preserve-unreferenced",
        input.path().to_str().unwrap(),
        "--pages",
        ".",
        "1-z",
        donor.path().to_str().unwrap(),
        "1-z",
        "--",
        "--add-attachment",
        attachment.to_str().unwrap(),
        "--creationdate=D:20200102030405Z",
        "--moddate=D:20200102030405Z",
        "--",
    ]) else {
        eprintln!("qpdf not available; skipping the oracle comparison");
        return;
    };
    let oracle_objects = object_numbers(&oracle);
    assert_eq!(
        object_numbers(&std::fs::read(&preserved).unwrap()),
        oracle_objects,
        "--preserve-unreferenced must retain exactly the objects qpdf keeps"
    );
    assert_ne!(
        object_numbers(&std::fs::read(&pruned).unwrap()),
        oracle_objects,
        "the default run must drop objects the preserved run keeps"
    );
}

#[test]
fn add_attachment_honors_remove_unreferenced_resources() {
    let temp = tempfile::tempdir().unwrap();
    let mut input = tempfile::NamedTempFile::new().unwrap();
    input
        .write_all(include_bytes!(
            "../../../tests/fixtures/compat/inherited-resources-one-page.pdf"
        ))
        .unwrap();
    let attachment = attachment_temp(temp.path());
    let kept = temp.path().join("kept.pdf");
    let removed = temp.path().join("removed.pdf");

    for (output, mode) in [(&kept, "no"), (&removed, "yes")] {
        CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--static-id",
                input.path().to_str().unwrap(),
                "--pages",
                ".",
                "1-z",
                "--",
                &format!("--remove-unreferenced-resources={mode}"),
                "--add-attachment",
                attachment.to_str().unwrap(),
                "--creationdate=D:20200102030405Z",
                "--moddate=D:20200102030405Z",
                "--",
                output.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    assert_ne!(
        std::fs::read(&kept).unwrap(),
        std::fs::read(&removed).unwrap(),
        "an explicit --remove-unreferenced-resources value must reach the merge"
    );
}

fn encrypted_donor() -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(include_bytes!(
        "../../../tests/fixtures/encrypted/v4-aes-128-r4.pdf"
    ))
    .unwrap();
    file
}

/// Whether a qpdf binary can be run at all. Kept separate from
/// [`encryption_revision`] so a missing `R = ...` line -- which is exactly what
/// the regression under test produces -- can never be mistaken for an absent
/// oracle.
fn qpdf_available() -> bool {
    std::process::Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Report the `R = N` revision qpdf sees. Panics when the inspection itself
/// fails or prints no revision: both mean the output is not the encrypted
/// document the caller asked for.
fn encryption_revision(path: &Path, password: &str) -> u32 {
    let output = std::process::Command::new("qpdf")
        .args([&format!("--password={password}"), "--show-encryption"])
        .arg(path)
        .output()
        .expect("qpdf runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "qpdf --show-encryption must succeed; status={:?}\nstdout: {stdout}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("R = "))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| panic!("qpdf reported no encryption revision\nstdout: {stdout}"))
}

#[test]
fn add_attachment_lets_a_later_encrypt_win_over_copy_encryption() {
    let temp = tempfile::tempdir().unwrap();
    let input = one_page_pdf();
    let donor = encrypted_donor();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    // A later --encrypt overrides --copy-encryption, so the job-level donor
    // fields must not be forwarded: write_qpdf would otherwise reapply the
    // AES-128 donor over the requested AES-256 writer options.
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            &format!("--copy-encryption={}", donor.path().to_str().unwrap()),
            "--encryption-file-password=user-v4-aes",
            "--encrypt",
            "--user-password=newu",
            "--owner-password=newo",
            "--bits=256",
            "--",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=D:20200102030405Z",
            "--moddate=D:20200102030405Z",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    if !qpdf_available() {
        eprintln!("qpdf not available; skipping the revision check");
        return;
    }
    assert_eq!(
        encryption_revision(&output, "newo"),
        6,
        "--bits=256 must win over the AES-128 copy-encryption donor"
    );
}

/// Build an AES-128 encrypted single-page donor with qpdf. Returns `None` when
/// qpdf is unavailable.
fn encrypted_paged_donor(directory: &Path) -> Option<std::path::PathBuf> {
    let source = one_page_pdf();
    let donor = directory.join("donor.pdf");
    let status = std::process::Command::new("qpdf")
        .args([
            "--static-id",
            "--allow-weak-crypto",
            "--encrypt",
            "--user-password=dpw",
            "--owner-password=dow",
            "--bits=128",
            "--use-aes=y",
            "--",
        ])
        .arg(source.path())
        .arg(&donor)
        .status()
        .ok()?;
    status.success().then_some(donor)
}

#[test]
fn add_attachment_keeps_donor_credentials_for_page_sources() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping the page-source credential check");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = one_page_pdf();
    let attachment = attachment_temp(temp.path());
    let donor = encrypted_paged_donor(temp.path()).expect("qpdf builds the encrypted donor");
    let output = temp.path().join("out.pdf");

    // qpdf keeps `encryption_file` and its password for the page-spec fallback
    // even when a later --encrypt takes over the output mode: only the
    // `copy_encryption` flag that gates the writer is cleared
    // (`libqpdf/QPDFJob_config.cc:146-148,1164-1166`,
    // `QPDFJob.cc:2405-2410,2891-2900`). Selecting a page from the donor
    // without a per-spec password must therefore still authenticate.
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            &format!("--copy-encryption={}", donor.to_str().unwrap()),
            "--encryption-file-password=dow",
            "--pages",
            donor.to_str().unwrap(),
            "1",
            "--",
            "--encrypt",
            "--user-password=newu",
            "--owner-password=newo",
            "--bits=256",
            "--",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=D:20200102030405Z",
            "--moddate=D:20200102030405Z",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        encryption_revision(&output, "newo"),
        6,
        "the later --encrypt must own the output mode while the donor \
         credentials stay available to the page source"
    );
}

#[test]
fn add_attachment_opens_the_copy_encryption_donor_once() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping the donor reopen check");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = one_page_pdf();
    let attachment = attachment_temp(temp.path());
    let donor = encrypted_paged_donor(temp.path()).expect("qpdf builds the encrypted donor");

    // Break startxref so opening the donor emits repair warnings. The writer
    // configuration this route installs already carries the opened donor, so
    // letting the job reopen it would parse the file twice and repeat every
    // warning.
    let mut bytes = std::fs::read(&donor).unwrap();
    let marker = b"startxref";
    let start = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("donor has startxref")
        + marker.len();
    let end = bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n' && start + 1 < bytes.len())
        .map(|offset| start + offset)
        .expect("startxref is followed by a newline");
    bytes.splice(start..end, b"\n999999".iter().copied());
    let broken = temp.path().join("donor-broken.pdf");
    std::fs::write(&broken, &bytes).unwrap();

    let output = temp.path().join("out.pdf");
    let assert = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            &format!("--copy-encryption={}", broken.to_str().unwrap()),
            "--encryption-file-password=dow",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=D:20200102030405Z",
            "--moddate=D:20200102030405Z",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    let flpdf_warnings = stderr
        .lines()
        .filter(|line| line.contains("WARNING"))
        .count();

    let oracle = std::process::Command::new("qpdf")
        .args([
            "--static-id",
            input.path().to_str().unwrap(),
            &format!("--copy-encryption={}", broken.to_str().unwrap()),
            "--encryption-file-password=dow",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=D:20200102030405Z",
            "--moddate=D:20200102030405Z",
            "--",
        ])
        .arg(temp.path().join("oracle.pdf"))
        .output()
        .expect("qpdf runs");
    let qpdf_warnings = String::from_utf8_lossy(&oracle.stderr)
        .lines()
        .filter(|line| line.contains("WARNING"))
        .count();

    assert!(qpdf_warnings > 0, "the broken donor must warn at all");
    assert_eq!(
        flpdf_warnings, qpdf_warnings,
        "the donor must be opened once, like qpdf\nflpdf stderr:\n{stderr}"
    );
}

/// `--empty` replaces the input file, so the sole positional path is the
/// output (`libqpdf/QPDFJob_config.cc:27-40`). The attachment routes used to
/// consume it as the input and then fail with "an output file name is
/// required".
#[test]
fn add_attachment_honors_empty_input() {
    let temp = tempfile::tempdir().unwrap();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--empty",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=D:20200102030405Z",
            "--moddate=D:20200102030405Z",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        std::fs::metadata(&output).is_ok_and(|meta| meta.len() > 0),
        "--empty must write the document to the sole positional path"
    );
}

#[test]
fn add_attachment_honors_empty_input_with_page_selection() {
    let temp = tempfile::tempdir().unwrap();
    let source = one_page_pdf();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--empty",
            "--pages",
            source.path().to_str().unwrap(),
            "1",
            "--",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--creationdate=D:20200102030405Z",
            "--moddate=D:20200102030405Z",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        page_count(&output),
        1,
        "the page selected from the external source must be written"
    );
}

#[test]
fn copy_attachments_honors_empty_input() {
    let temp = tempfile::tempdir().unwrap();
    let donor = one_page_pdf();
    let output = temp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--empty",
            "--copy-attachments-from",
            donor.path().to_str().unwrap(),
            "--prefix=P",
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        std::fs::metadata(&output).is_ok_and(|meta| meta.len() > 0),
        "--empty must write the document to the sole positional path"
    );
}

/// The `--empty` resolver rejects the same shapes the `--pages` route does.
///
/// qpdf reports a different diagnostic for two of these — its parser accepts
/// only one positional path, so a second one becomes `unknown argument` before
/// the configuration check runs. That difference predates this route: the
/// existing `--pages` route diverges identically, so it is not introduced here.
#[test]
fn empty_input_rejects_conflicting_shapes() {
    let temp = tempfile::tempdir().unwrap();
    let source = one_page_pdf();
    let attachment = attachment_temp(temp.path());
    let output = temp.path().join("out.pdf");

    // --replace-input with --empty: qpdf refuses this combination outright
    // (`libqpdf/QPDFJob.cc:590-591`).
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--empty",
            "--replace-input",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--replace-input may not be used with --empty",
        ));

    // Both an input and an output alongside --empty.
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--empty",
            source.path().to_str().unwrap(),
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "empty input can't be used since input file has already been given",
        ));

    // Neither: qpdf asks for the output name (`QPDFJob.cc:592-593`).
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--empty",
            "--add-attachment",
            attachment.to_str().unwrap(),
            "--",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "an output file name is required; use - for standard output",
        ));
}
