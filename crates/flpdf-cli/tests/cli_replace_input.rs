use assert_cmd::Command;
use std::fs;
use std::process::Command as ProcessCommand;

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
}

#[test]
fn top_level_replace_input_rewrites_unicode_input() {
    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let input = directory.path().join("auto-ü.pdf");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    fs::copy(&fixture, &input).expect("copy replace-input fixture");
    let original = fs::read(&input).expect("read original input");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args([
            "--deterministic-id",
            "--object-streams=generate",
            "--replace-input",
            "./auto-ü.pdf",
        ])
        .assert()
        .success()
        .stdout("")
        .stderr("");

    let replaced = fs::read(&input).expect("read replaced input");
    assert_ne!(
        replaced, original,
        "replace-input must write the transformed PDF"
    );
    assert!(input.is_file(), "replace-input must keep the input path");
    assert!(!directory.path().join("auto-ü.pdf.~qpdf-orig").exists());
    assert!(!directory.path().join("auto-ü.pdf.~qpdf-orig#").exists());
    assert!(!directory.path().join("auto-ü.pdf.~qpdf-temp#").exists());
}

#[test]
fn top_level_replace_input_rejects_json_output_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input JSON differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf_input = directory.path().join("qpdf-input.pdf");
    let flpdf_input = directory.path().join("flpdf-input.pdf");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    fs::copy(&fixture, &qpdf_input).expect("copy qpdf input");
    fs::copy(&fixture, &flpdf_input).expect("copy flpdf input");

    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args(["--replace-input", "--json=2", "qpdf-input.pdf"])
        .output()
        .expect("run qpdf replace-input JSON oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(["--replace-input", "--json=2", "flpdf-input.pdf"])
        .output()
        .expect("run flpdf replace-input JSON");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
    assert!(flpdf_input.is_file(), "a rejected job must keep its input");
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn top_level_replace_input_matches_qpdf_bytes() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input byte differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf_directory = directory.path().join("qpdf");
    let flpdf_directory = directory.path().join("flpdf");
    fs::create_dir(&qpdf_directory).expect("create qpdf directory");
    fs::create_dir(&flpdf_directory).expect("create flpdf directory");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    let qpdf_input = qpdf_directory.join("auto-ü.pdf");
    let flpdf_input = flpdf_directory.join("auto-ü.pdf");
    fs::copy(&fixture, &qpdf_input).expect("copy qpdf input");
    fs::copy(&fixture, &flpdf_input).expect("copy flpdf input");

    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(&qpdf_directory)
        .args([
            "--deterministic-id",
            "--object-streams=generate",
            "--replace-input",
            "./auto-ü.pdf",
        ])
        .output()
        .expect("run qpdf replace-input oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(&flpdf_directory)
        .args([
            "--deterministic-id",
            "--object-streams=generate",
            "--replace-input",
            "./auto-ü.pdf",
        ])
        .output()
        .expect("run flpdf replace-input");

    assert_eq!(flpdf.status.code(), Some(0));
    assert_eq!(qpdf.status.code(), Some(0));
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        fs::read(&flpdf_input).unwrap(),
        fs::read(&qpdf_input).unwrap()
    );
}

#[test]
fn top_level_replace_input_rejects_split_pages_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input split differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf_input = directory.path().join("qpdf-input.pdf");
    let flpdf_input = directory.path().join("flpdf-input.pdf");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    fs::copy(&fixture, &qpdf_input).expect("copy qpdf input");
    fs::copy(&fixture, &flpdf_input).expect("copy flpdf input");

    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args(["--replace-input", "--split-pages=1", "qpdf-input.pdf"])
        .output()
        .expect("run qpdf replace-input split oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(["--replace-input", "--split-pages=1", "flpdf-input.pdf"])
        .output()
        .expect("run flpdf replace-input split");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
    assert!(flpdf_input.is_file(), "a rejected job must keep its input");
}

#[test]
fn top_level_replace_input_rejects_an_output_file_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input output differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf_input = directory.path().join("qpdf-input.pdf");
    let flpdf_input = directory.path().join("flpdf-input.pdf");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    fs::copy(&fixture, &qpdf_input).expect("copy qpdf input");
    fs::copy(&fixture, &flpdf_input).expect("copy flpdf input");

    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args(["qpdf-input.pdf", "output.pdf", "--replace-input"])
        .output()
        .expect("run qpdf replace-input output oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(["flpdf-input.pdf", "output.pdf", "--replace-input"])
        .output()
        .expect("run flpdf replace-input output");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
    assert!(flpdf_input.is_file(), "a rejected job must keep its input");
}

#[test]
fn top_level_replace_input_rejects_empty_input_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input empty differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args(["--empty", "--replace-input"])
        .output()
        .expect("run qpdf empty replace-input oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(["--empty", "--replace-input"])
        .output()
        .expect("run flpdf empty replace-input");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
}

#[test]
fn top_level_replace_input_rejects_inspection_like_qpdf() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input inspection differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf_input = directory.path().join("qpdf-input.pdf");
    let flpdf_input = directory.path().join("flpdf-input.pdf");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    fs::copy(&fixture, &qpdf_input).expect("copy qpdf input");
    fs::copy(&fixture, &flpdf_input).expect("copy flpdf input");

    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args(["--replace-input", "--check", "qpdf-input.pdf"])
        .output()
        .expect("run qpdf replace-input inspection oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(["--replace-input", "--check", "flpdf-input.pdf"])
        .output()
        .expect("run flpdf replace-input inspection");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
    assert!(flpdf_input.is_file(), "a rejected job must keep its input");
}

#[test]
fn top_level_replace_input_is_applied_after_job_json_file() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input job-json differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf_input = directory.path().join("qpdf-input.pdf");
    let flpdf_input = directory.path().join("flpdf-input.pdf");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf");
    fs::copy(&fixture, &qpdf_input).expect("copy qpdf input");
    fs::copy(&fixture, &flpdf_input).expect("copy flpdf input");
    fs::write(
        directory.path().join("job.json"),
        br#"{"deterministicId":"","objectStreams":"generate"}"#,
    )
    .expect("write partial job JSON");

    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args([
            "--job-json-file=job.json",
            "--replace-input",
            "qpdf-input.pdf",
        ])
        .output()
        .expect("run qpdf job-json replace-input oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args([
            "--job-json-file=job.json",
            "--replace-input",
            "flpdf-input.pdf",
        ])
        .output()
        .expect("run flpdf job-json replace-input");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
    assert!(
        flpdf_input.is_file(),
        "replace-input must keep the input path"
    );
}

#[test]
fn top_level_replace_input_empty_output_conflict_matches_qpdf_order() {
    if !qpdf_available() {
        eprintln!("qpdf 11.9.0 is unavailable; skipping replace-input empty-output differential");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary replace-input directory");
    let qpdf = ProcessCommand::new("qpdf")
        .current_dir(directory.path())
        .args(["--empty", "output.pdf", "--replace-input"])
        .output()
        .expect("run qpdf empty-output replace-input oracle");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(["--empty", "flpdf-output.pdf", "--replace-input"])
        .output()
        .expect("run flpdf empty-output replace-input");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr).replace("qpdf", "flpdf")
    );
}
