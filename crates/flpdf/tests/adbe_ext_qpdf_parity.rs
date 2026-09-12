//! Byte-identity: flpdf plain full-rewrite emits qpdf's Catalog /Extensions
//! /ADBE mutations (removal AND injection) byte-for-byte.
//!
//! REMOVAL (QPDFWriter.cc L1408 whole /Extensions removal, L1432 /ADBE-only
//! removal): proves the root output-copy reconciliation matches qpdf's
//! `have_extensions_adbe = keys.count("/ADBE") > 0` (L1387) on inputs whose
//! source /ADBE dict lacks a valid `/ExtensionLevel`.
//!
//! INJECTION (`WriterTestSettings::min_extension_level`, qpdf
//! `--min-version=<v>.<ext>`) covers three shapes: (1) fresh /Extensions
//! creation when the source Catalog has none, (2) direct /Extensions with a
//! non-ADBE developer prefix (/XYZW) preserved, (3) indirect /Extensions
//! reference with existing /ADBE weak + /ACRO — inlined onto the Catalog,
//! /ADBE overwritten, /ACRO preserved.
//!
//! Fixtures are content-stream-free, so byte-identity is independent of the
//! deflate backend — this file is NOT gated on `qpdf-zlib-compat`.

use flpdf::{EncryptParams, NewlineBeforeEndstream, ObjectStreamMode, Pdf};
use std::path::Path;

/// STRIP-side WriterTestSettings (plain full rewrite, qpdf-matching newline/id).
///
/// Keep the non-default fields explicit so this parity helper documents the
/// exact writer settings used by the probe.
fn strip_options() -> WriterTestSettings {
    WriterTestSettings {
        static_id: true,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    }
}

/// INJECT-side WriterTestSettings: strip_options() + min-version 1.7 with extension
/// level 8 (mirrors `qpdf --min-version=1.7.8`).
fn inject_options() -> WriterTestSettings {
    let mut opts = strip_options();
    opts.min_version = Some("1.7".into());
    opts.min_extension_level = Some(8);
    opts
}

/// Plain full-rewrite of `fixture` with the given options; return bytes.
fn write_qpdf_equivalent(fixture: &str, options: &WriterTestSettings) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, options).unwrap();
    out
}

/// Read golden `references/<stem>/<golden_name>`.
fn golden(stem: &str, golden_name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(golden_name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_parity(fixture: &str, golden_name: &str, options: &WriterTestSettings) {
    let stem = fixture
        .strip_suffix(".pdf")
        .expect("fixture must end in .pdf");
    let actual = write_qpdf_equivalent(fixture, options);
    let expected = golden(stem, golden_name);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf golden {stem}/{golden_name} \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn whole_extensions_removed_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1408: /Extensions has only /ADBE and we don't want /ADBE → drop
    // whole /Extensions from Catalog.
    assert_parity(
        "one-page-stale-adbe-no-ext.pdf",
        "adbe-strip.pdf",
        &strip_options(),
    );
}

#[test]
fn non_adbe_prefix_preserved_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1432: /Extensions has /ADBE + non-ADBE prefix and we don't want
    // /ADBE → remove /ADBE key only, keep /Extensions with other keys.
    assert_parity(
        "one-page-stale-adbe-no-ext-vendor.pdf",
        "adbe-strip.pdf",
        &strip_options(),
    );
}

#[test]
fn fresh_extensions_adbe_injected_when_source_has_none_byte_identical_to_qpdf() {
    // qpdf --min-version=1.7.8 on a Catalog with no /Extensions must emit
    // a fresh /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>.
    // Verifies the root output-copy fresh-creation branch byte-for-byte.
    assert_parity("one-page-no-ext.pdf", "adbe-inject.pdf", &inject_options());
}

#[test]
fn non_adbe_prefix_preserved_when_source_lacks_adbe_and_min_ext_requests_injection_byte_identical_to_qpdf(
) {
    // qpdf --min-version=1.7.8 on a Catalog with /Extensions << /XYZW … >>
    // must add /ADBE before /XYZW (alphabetical) and preserve /XYZW verbatim.
    // The issue-focus case: non-ADBE developer prefix survives injection.
    assert_parity(
        "one-page-xyzw-only.pdf",
        "adbe-inject.pdf",
        &inject_options(),
    );
}

#[test]
fn indirect_extensions_inlined_and_adbe_overwritten_preserving_non_adbe_prefix_byte_identical_to_qpdf(
) {
    // qpdf --min-version=1.7.8 on a Catalog whose /Extensions is an indirect
    // reference (obj 3 = /ADBE weak + /ACRO) must inline obj 3 onto the
    // Catalog, overwrite /ADBE with (1.7, 8), and preserve /ACRO. Result key
    // order is alphabetical: /ACRO before /ADBE. obj 3 is dropped from the body.
    assert_parity(
        "one-page-ext-indirect.pdf",
        "adbe-inject.pdf",
        &inject_options(),
    );
}

#[test]
fn valid_zero_level_adbe_with_non_adbe_prefix_is_preserved_byte_identical_to_qpdf() {
    // qpdf preserves a valid /ADBE entry at extension level 0 when another
    // developer prefix remains under /Extensions. This also exercises the
    // indirect /Extensions source shape through the plain writer route.
    assert_parity(
        "linearize-indirect-extensions.pdf",
        "adbe-preserve.pdf",
        &strip_options(),
    );
}

#[test]
fn common_writer_preparation_keeps_indirect_extensions_direct_on_live_catalog() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearize-indirect-extensions.pdf");
    let file = std::fs::File::open(&path).expect("open indirect extensions fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("open fixture");

    let before = pdf
        .root_handle()
        .expect("fixture has a Catalog")
        .try_get_key(b"/Extensions")
        .expect("read source Extensions");
    assert!(
        before.is_indirect(),
        "fixture must start with indirect Extensions"
    );

    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &strip_options()).expect("rewrite succeeds");

    let after = pdf
        .root_handle()
        .expect("Catalog remains available after write")
        .try_get_key(b"/Extensions")
        .expect("read live Extensions after write");
    assert!(
        after.is_direct(),
        "qpdf prepareFileForWrite is permanent graph preparation, not an output snapshot"
    );
}

#[test]
fn common_writer_preparation_is_shared_by_linearized_output() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearize-indirect-extensions.pdf");
    let file = std::fs::File::open(&path).expect("open indirect extensions fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("open fixture");

    write_linearized_with_settings(&mut pdf, &strip_options()).expect("linearization succeeds");

    let extensions = pdf
        .root_handle()
        .expect("Catalog remains available after linearization")
        .try_get_key(b"/Extensions")
        .expect("read live Extensions after linearization");
    assert!(
        extensions.is_direct(),
        "linearized output must consume the same permanent graph preparation"
    );
}

fn specialized_mode_reconciles_shared_extensions_alias_on_live_catalog(
    object_streams: flpdf::ObjectStreamMode,
) -> flpdf::Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page-ext-indirect.pdf");
    let file = std::fs::File::open(&path).expect("open indirect Extensions fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("open fixture");

    let before = pdf
        .root_handle()
        .expect("fixture has a Catalog")
        .try_get_key(b"/Extensions")
        .expect("read source Extensions");
    let before_adbe = before.try_get_key(b"/ADBE").expect("read source ADBE");
    assert_eq!(
        before_adbe
            .try_get_key(b"/ExtensionLevel")
            .expect("read source extension level")
            .as_integer(),
        Some(3)
    );

    let settings = WriterTestSettings {
        object_streams,
        force_version: Some("1.4.2".to_owned()),
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings).expect("rewrite succeeds");

    let after = pdf
        .root_handle()
        .expect("Catalog remains available after write")
        .try_get_key(b"/Extensions")
        .expect("read live Extensions after write");
    assert!(
        after.as_dictionary().is_some(),
        "output-only ADBE mutation must not remove the live Extensions dictionary"
    );
    assert!(
        !after.try_has_key(b"/ADBE")?,
        "qpdf's shallow root copy shares an existing direct Extensions dictionary, so ADBE removal is observable on the live alias"
    );
    Ok(())
}

#[test]
fn specialized_generate_reconciles_shared_extensions_alias_on_live_catalog() {
    specialized_mode_reconciles_shared_extensions_alias_on_live_catalog(
        flpdf::ObjectStreamMode::Generate,
    )
    .expect("specialized Generate write succeeds");
}

#[test]
fn specialized_preserve_reconciles_shared_extensions_alias_on_live_catalog() {
    specialized_mode_reconciles_shared_extensions_alias_on_live_catalog(
        flpdf::ObjectStreamMode::Preserve,
    )
    .expect("specialized Preserve write succeeds");
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn specialized_standard_adbe_root_cutover_matches_qpdf_for_all_object_stream_modes() {
    use std::process::Command;

    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/adbe-orphan-url.pdf");
    let temp = tempfile::tempdir().expect("create qpdf comparison directory");
    for mode in [
        ObjectStreamMode::Disable,
        ObjectStreamMode::Preserve,
        ObjectStreamMode::Generate,
    ] {
        let mode_name = match mode {
            ObjectStreamMode::Disable => "disable",
            ObjectStreamMode::Preserve => "preserve",
            ObjectStreamMode::Generate => "generate",
        };
        let oracle_path = temp.path().join(format!("qpdf-{mode_name}.pdf"));
        let object_streams_arg = format!("--object-streams={mode_name}");
        let oracle = Command::new("qpdf")
            .args(["--static-id", "--static-aes-iv"])
            .arg(&object_streams_arg)
            .args([
                "--min-version=1.7.8",
                "--encrypt",
                "u",
                "o",
                "128",
                "--use-aes=y",
                "--",
            ])
            .arg(&input)
            .arg(&oracle_path)
            .output()
            .expect("run qpdf 11.9.0");
        assert!(oracle.status.success(), "qpdf failed: {:?}", oracle.stderr);

        let file = std::fs::File::open(&input).expect("open ADBE orphan fixture");
        let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("parse ADBE orphan fixture");
        let settings = WriterTestSettings {
            object_streams: mode,
            min_version: Some("1.7".to_owned()),
            min_extension_level: Some(8),
            static_id: true,
            static_aes_iv: true,
            encrypt: Some(EncryptParams::v4_aes128(b"u", b"o")),
            ..WriterTestSettings::default()
        };
        let mut actual = Vec::new();
        write_with_settings(&mut pdf, &mut actual, &settings).expect("specialized rewrite");
        let expected = std::fs::read(&oracle_path).expect("read qpdf output");
        assert_eq!(
            actual, expected,
            "specialized ADBE cutover mode={mode_name}"
        );
    }
}

fn assert_qdf_or_normalize_adbe_orphan_parity(content_normalization: bool) {
    use std::process::Command;

    if Command::new("qpdf").arg("--version").output().is_err() {
        eprintln!("qpdf is unavailable; skipping QDF/normalize ADBE orphan parity");
        return;
    }

    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/adbe-orphan-url.pdf");
    let temporary = tempfile::tempdir().expect("create qpdf comparison directory");
    let qpdf_output = temporary.path().join("qpdf.pdf");
    let mut qpdf = Command::new("qpdf");
    qpdf.args([
        "--static-id",
        "--object-streams=disable",
        "--force-version=1.7.8",
    ]);
    if content_normalization {
        qpdf.arg("--normalize-content=y");
    } else {
        qpdf.arg("--qdf");
    }
    qpdf.args(["--stream-data=uncompress"])
        .arg(&input)
        .arg(&qpdf_output);
    let result = qpdf.output().expect("run qpdf QDF/normalize rewrite");
    assert!(result.status.success(), "qpdf rewrite failed: {result:?}");

    let file = std::fs::File::open(&input).expect("open ADBE orphan fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("parse ADBE orphan fixture");
    let settings = WriterTestSettings {
        content_normalization,
        qdf: !content_normalization,
        object_streams: ObjectStreamMode::Disable,
        force_version: Some("1.7".to_owned()),
        force_extension_level: Some(8),
        stream_data: Some(flpdf::StreamDataMode::Uncompress),
        static_id: true,
        ..WriterTestSettings::default()
    };
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings).expect("QDF/normalize rewrite");
    let expected = std::fs::read(&qpdf_output).expect("read qpdf QDF/normalize output");
    assert_eq!(actual, expected, "QDF/normalize ADBE orphan parity");
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn qdf_adbe_orphan_root_cutover_matches_qpdf() {
    assert_qdf_or_normalize_adbe_orphan_parity(false);
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn normalize_adbe_orphan_root_cutover_matches_qpdf() {
    assert_qdf_or_normalize_adbe_orphan_parity(true);
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_normalize_adbe_orphan_root_cutover_matches_qpdf() {
    use std::process::Command;

    if !Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|result| result.status.success())
    {
        eprintln!("qpdf is unavailable; skipping encrypted normalize ADBE parity");
        return;
    }

    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/adbe-orphan-url.pdf");
    let temporary = tempfile::tempdir().expect("create qpdf comparison directory");
    for object_streams in [ObjectStreamMode::Disable, ObjectStreamMode::Preserve] {
        let mode = match object_streams {
            ObjectStreamMode::Disable => "disable",
            ObjectStreamMode::Preserve => "preserve",
            ObjectStreamMode::Generate => unreachable!(),
        };
        let qpdf_output = temporary.path().join(format!("qpdf-{mode}.pdf"));
        let result = Command::new("qpdf")
            .args([
                "--static-id",
                "--static-aes-iv",
                "--normalize-content=y",
                &format!("--object-streams={mode}"),
                "--force-version=1.7.8",
                "--stream-data=uncompress",
                "--encrypt",
                "u",
                "o",
                "128",
                "--use-aes=y",
                "--",
            ])
            .arg(&input)
            .arg(&qpdf_output)
            .output()
            .expect("run qpdf encrypted normalize rewrite");
        assert!(result.status.success(), "qpdf rewrite failed: {result:?}");

        let file = std::fs::File::open(&input).expect("open ADBE orphan fixture");
        let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("parse ADBE orphan fixture");
        let settings = WriterTestSettings {
            content_normalization: true,
            object_streams,
            force_version: Some("1.7".to_owned()),
            force_extension_level: Some(8),
            stream_data: Some(flpdf::StreamDataMode::Uncompress),
            static_id: true,
            static_aes_iv: true,
            encrypt: Some(EncryptParams::v4_aes128(b"u", b"o")),
            ..WriterTestSettings::default()
        };
        let mut actual = Vec::new();
        write_with_settings(&mut pdf, &mut actual, &settings).expect("encrypted normalize rewrite");
        assert_eq!(
            actual,
            std::fs::read(&qpdf_output).expect("read qpdf encrypted normalize output"),
            "encrypted normalize ADBE orphan parity mode={mode}"
        );
    }
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_qdf_adbe_orphan_root_cutover_matches_qpdf() {
    use std::process::Command;

    if !Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|result| result.status.success())
    {
        eprintln!("qpdf is unavailable; skipping encrypted QDF ADBE parity");
        return;
    }

    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/adbe-orphan-url.pdf");
    let temporary = tempfile::tempdir().expect("create qpdf comparison directory");
    let qpdf_output = temporary.path().join("qpdf.pdf");
    let result = Command::new("qpdf")
        .args([
            "--qdf",
            "--static-id",
            "--static-aes-iv",
            "--object-streams=disable",
            "--force-version=1.7.8",
            "--stream-data=uncompress",
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf encrypted QDF rewrite");
    assert!(result.status.success(), "qpdf rewrite failed: {result:?}");

    let file = std::fs::File::open(&input).expect("open ADBE orphan fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("parse ADBE orphan fixture");
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        force_version: Some("1.7".to_owned()),
        force_extension_level: Some(8),
        stream_data: Some(flpdf::StreamDataMode::Uncompress),
        static_id: true,
        static_aes_iv: true,
        encrypt: Some(EncryptParams::v4_aes128(b"u", b"o")),
        ..WriterTestSettings::default()
    };
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings).expect("encrypted QDF rewrite");
    assert_eq!(
        actual,
        std::fs::read(&qpdf_output).expect("read qpdf encrypted QDF output"),
        "encrypted QDF ADBE orphan parity"
    );
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn encrypted_qdf_content_streams_match_qpdf() {
    use std::process::Command;

    if !Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|result| result.status.success())
    {
        eprintln!("qpdf is unavailable; skipping encrypted QDF content parity");
        return;
    }

    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/qdf-contents-ref-array.pdf");
    let temporary = tempfile::tempdir().expect("create qpdf comparison directory");
    let qpdf_output = temporary.path().join("qpdf.pdf");
    let result = Command::new("qpdf")
        .args([
            "--qdf",
            "--static-id",
            "--static-aes-iv",
            "--object-streams=disable",
            "--force-version=1.7.8",
            "--stream-data=uncompress",
            "--encrypt",
            "u",
            "o",
            "128",
            "--use-aes=y",
            "--",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf encrypted QDF content rewrite");
    assert!(result.status.success(), "qpdf rewrite failed: {result:?}");

    let file = std::fs::File::open(&input).expect("open QDF content fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("parse QDF content fixture");
    let settings = WriterTestSettings {
        qdf: true,
        object_streams: ObjectStreamMode::Disable,
        force_version: Some("1.7".to_owned()),
        force_extension_level: Some(8),
        stream_data: Some(flpdf::StreamDataMode::Uncompress),
        static_id: true,
        static_aes_iv: true,
        encrypt: Some(EncryptParams::v4_aes128(b"u", b"o")),
        ..WriterTestSettings::default()
    };
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings).expect("encrypted QDF content rewrite");
    assert_eq!(
        actual,
        std::fs::read(&qpdf_output).expect("read qpdf encrypted QDF content output"),
        "encrypted QDF content stream parity"
    );
}

#[test]
fn direct_root_adbe_survives_forced_version_in_plain_disable_output() {
    for force_version in ["1.4", "1.7", "2.0"] {
        let object_streams = flpdf::ObjectStreamMode::Disable;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/direct-root-adbe.pdf");
        let file = std::fs::File::open(&path).expect("open direct-root Extensions fixture");
        let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("open fixture");
        let settings = WriterTestSettings {
            object_streams,
            force_version: Some(force_version.to_owned()),
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterTestSettings::default()
        };

        let mut output = Vec::new();
        write_with_settings(&mut pdf, &mut output, &settings).expect("rewrite succeeds");

        let mut rewritten = Pdf::open(std::io::Cursor::new(output)).expect("reopen rewritten PDF");
        let extensions = rewritten
            .root_handle()
            .expect("rewritten PDF has a Catalog")
            .try_get_key(b"/Extensions")
            .expect("read rewritten /Extensions");
        assert!(
            extensions
                .try_is_dictionary()
                .expect("inspect rewritten /Extensions"),
            "force {force_version}: rewritten Catalog keeps /Extensions"
        );
        let adbe = extensions
            .try_get_key(b"/ADBE")
            .expect("read rewritten /ADBE");
        assert!(
            adbe.try_is_dictionary().expect("inspect rewritten /ADBE"),
            "force {force_version}: rewritten /Extensions keeps /ADBE"
        );
        assert_eq!(
            adbe.try_get_key(b"/BaseVersion")
                .expect("read rewritten ADBE version")
                .as_name(),
            Some(b"1.7".to_vec())
        );
        assert_eq!(
            adbe.try_get_key(b"/ExtensionLevel")
                .expect("read rewritten ADBE level")
                .as_integer(),
            Some(8)
        );
    }
}

mod common;
#[allow(unused_imports)]
use common::{
    write_default, write_linearized_with_settings, write_with_settings, WriterTestSettings,
};
