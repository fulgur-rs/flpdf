//! Differential coverage for qpdf's writers with an inline Catalog.

use flpdf::Pdf;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::{write_with_settings, WriterTestSettings};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn flpdf_rewrite(path: &Path, mode: flpdf::ObjectStreamMode) -> flpdf::Result<Vec<u8>> {
    let mut pdf = Pdf::open(BufReader::new(File::open(path)?))?;
    let settings = WriterTestSettings {
        static_id: true,
        object_streams: mode,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)?;
    Ok(output)
}

fn qpdf_rewrite(path: &Path, output: &Path, mode: &str) {
    let result = Command::new("qpdf")
        .args(["--static-id", &format!("--object-streams={mode}")])
        .arg(path)
        .arg(output)
        .output()
        .expect("run qpdf standard rewrite");
    assert!(
        result.status.success(),
        "qpdf rewrite failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn qpdf_check(path: &Path) {
    let result = Command::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .expect("run qpdf --check");
    assert!(
        result.status.success(),
        "qpdf --check failed for {path:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn standard_writer_rewrites_direct_root_like_qpdf_and_keeps_it_inline() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping direct-root writer differential");
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    for name in ["direct-root-adbe.pdf", "direct-root-one-page.pdf"] {
        let input = fixture(name);
        for (mode, mode_name) in [
            (flpdf::ObjectStreamMode::Disable, "disable"),
            (flpdf::ObjectStreamMode::Preserve, "preserve"),
            (flpdf::ObjectStreamMode::Generate, "generate"),
        ] {
            let qpdf_output = temporary.path().join(format!("qpdf-{mode_name}-{name}"));
            let flpdf_output = temporary.path().join(format!("flpdf-{mode_name}-{name}"));
            qpdf_rewrite(&input, &qpdf_output, mode_name);

            let actual = flpdf_rewrite(&input, mode)?;
            std::fs::write(&flpdf_output, &actual)?;
            qpdf_check(&qpdf_output);
            qpdf_check(&flpdf_output);

            let rewritten = Pdf::open(BufReader::new(File::open(&flpdf_output)?))?;
            assert!(
                rewritten.root_ref().is_none(),
                "{name}/{mode_name}: flpdf must retain the inline Catalog in the output trailer"
            );
            let qpdf_trailer = Command::new("qpdf")
                .args(["--show-object=trailer", qpdf_output.to_str().unwrap()])
                .output()
                .expect("inspect qpdf trailer");
            assert!(qpdf_trailer.status.success());
            let qpdf_trailer = String::from_utf8_lossy(&qpdf_trailer.stdout);
            assert!(
                qpdf_trailer.contains("/Root <<"),
                "{name}/{mode_name}: qpdf must retain the inline Catalog"
            );
            if mode == flpdf::ObjectStreamMode::Preserve {
                assert_eq!(
                    actual,
                    std::fs::read(&qpdf_output)?,
                    "{name}: plain direct-root rewrite must match qpdf --static-id"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn qdf_generate_writer_rewrites_a_reachable_direct_root() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping direct-root QDF differential");
        return Ok(());
    }

    let input = fixture("direct-root-one-page.pdf");
    let temporary = tempfile::tempdir()?;
    for (mode, mode_name) in [
        (flpdf::ObjectStreamMode::Disable, "disable"),
        (flpdf::ObjectStreamMode::Generate, "generate"),
    ] {
        let output_path = temporary.path().join(format!("qdf-{mode_name}.pdf"));
        let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
        let settings = WriterTestSettings {
            static_id: true,
            qdf: true,
            object_streams: mode,
            ..WriterTestSettings::default()
        };
        let mut output = Vec::new();
        write_with_settings(&mut pdf, &mut output, &settings)?;
        std::fs::write(&output_path, &output)?;
        qpdf_check(&output_path);

        let rewritten = Pdf::open(BufReader::new(File::open(&output_path)?))?;
        assert!(rewritten.root_ref().is_none());
        let pages = Command::new("qpdf")
            .args(["--show-npages", output_path.to_str().unwrap()])
            .output()
            .expect("inspect direct-root QDF page count");
        assert!(pages.status.success());
        assert_eq!(String::from_utf8_lossy(&pages.stdout).trim(), "1");
    }
    Ok(())
}

#[test]
fn specialized_xref_stream_writer_rewrites_a_direct_root() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping direct-root xref-stream differential");
        return Ok(());
    }

    let input = fixture("direct-root-one-page.pdf");
    let temporary = tempfile::tempdir()?;
    let output_path = temporary.path().join("specialized-xref.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let settings = WriterTestSettings {
        static_id: true,
        object_streams: flpdf::ObjectStreamMode::Generate,
        extra_header_text: "% specialized\n".to_string(),
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)?;
    std::fs::write(&output_path, &output)?;
    qpdf_check(&output_path);

    let rewritten = Pdf::open(BufReader::new(File::open(&output_path)?))?;
    assert!(rewritten.root_ref().is_none());
    assert!(
        output
            .windows(b"/Type /XRef".len())
            .any(|window| window == b"/Type /XRef"),
        "specialized Generate output must use an xref stream"
    );
    Ok(())
}

#[test]
fn pclm_writer_rewrites_a_reachable_direct_root() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping direct-root PCLm differential");
        return Ok(());
    }

    let input = fixture("direct-root-one-page.pdf");
    let temporary = tempfile::tempdir()?;
    let output_path = temporary.path().join("pclm.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let settings = WriterTestSettings {
        static_id: true,
        pclm: true,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)?;
    std::fs::write(&output_path, &output)?;
    qpdf_check(&output_path);
    assert!(output.starts_with(b"%PDF-1.4\n%PCLm 1.0\n"));

    let rewritten = Pdf::open(BufReader::new(File::open(&output_path)?))?;
    assert!(
        rewritten.root_ref().is_none(),
        "PCLm must retain the inline Catalog in the output trailer"
    );
    let pages = Command::new("qpdf")
        .args(["--show-npages", output_path.to_str().unwrap()])
        .output()
        .expect("inspect direct-root PCLm page count");
    assert!(pages.status.success());
    assert_eq!(String::from_utf8_lossy(&pages.stdout).trim(), "1");
    let trailer = Command::new("qpdf")
        .args(["--show-object=trailer", output_path.to_str().unwrap()])
        .output()
        .expect("inspect direct-root PCLm trailer");
    assert!(trailer.status.success());
    let trailer = String::from_utf8_lossy(&trailer.stdout);
    assert!(trailer.contains("/Root <<"));
    assert!(
        trailer.contains("/Pages 2 0 R"),
        "the inline Catalog's indirect /Pages child must be remapped in PCLm"
    );
    Ok(())
}

#[test]
fn pclm_writer_preserves_a_direct_catalog_with_extensions() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping direct-root PCLm extension differential");
        return Ok(());
    }

    let input = fixture("direct-root-adbe.pdf");
    let temporary = tempfile::tempdir()?;
    let output_path = temporary.path().join("pclm-adbe.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let settings = WriterTestSettings {
        static_id: true,
        pclm: true,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)?;
    std::fs::write(&output_path, &output)?;
    qpdf_check(&output_path);

    let rewritten = Pdf::open(BufReader::new(File::open(&output_path)?))?;
    assert!(rewritten.root_ref().is_none());
    let trailer = Command::new("qpdf")
        .args(["--show-object=trailer", output_path.to_str().unwrap()])
        .output()
        .expect("inspect direct-root PCLm trailer");
    assert!(trailer.status.success());
    let trailer = String::from_utf8_lossy(&trailer.stdout);
    assert!(trailer.contains("/Root <<"));
    assert!(trailer.contains("/ExtensionLevel 8"));
    Ok(())
}

#[test]
fn pclm_writer_rewrites_a_direct_root_with_deterministic_id() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping deterministic direct-root PCLm differential");
        return Ok(());
    }

    let input = fixture("direct-root-one-page.pdf");
    let temporary = tempfile::tempdir()?;
    let output_path = temporary.path().join("pclm-deterministic.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let settings = WriterTestSettings {
        deterministic_id: true,
        pclm: true,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings)?;
    std::fs::write(&output_path, &output)?;
    qpdf_check(&output_path);

    let rewritten = Pdf::open(BufReader::new(File::open(&output_path)?))?;
    assert!(rewritten.root_ref().is_none());
    Ok(())
}
