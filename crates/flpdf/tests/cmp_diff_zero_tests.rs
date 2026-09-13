//! Byte-identity demonstration: flpdf plain rewrite == `qpdf --static-id`.
//!
//! This is the capstone of the qpdf byte-identical roadmap. It is gated on the
//! `qpdf-zlib-compat` feature because byte-identity requires flpdf's deflate
//! output to match qpdf's classic-libz output (the Pure-Rust miniz_oxide default
//! produces equivalent but not byte-identical compression). Three independent
//! pieces must all line up:
//!
//!   1. Stream-dictionary key order — `/Length` pulled out, `/Filter` last on
//!      re-filtered streams (matches `QPDFWriter::unparseObject`).
//!   2. Trailer on the `trailer ` line with keys sorted and `/ID` last.
//!   3. No newline before `endstream` ([`NewlineBeforeEndstream::Never`]) —
//!      qpdf's default output writes exactly `/Length` bytes then `endstream`.
//!
//! plus deflate parity (this feature) and the deterministic `--static-id` trailer
//! `/ID`. With all of these, flpdf's full rewrite is `cmp`-diff-0 against the
//! committed `qpdf --static-id` golden references.
//!
//! CAVEAT: byte-identity pins to the linked libz version (captured with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 / qpdf 11.9.0); a different libz may shift the deflate
//! bytes and require re-blessing the goldens.

#![cfg(feature = "qpdf-zlib-compat")]

mod common;
use common::PdfCanonicalTestExt;

use common::{write_with_settings, WriterTestSettings};
use flpdf::{ObjectRef, ObjectStreamMode, Pdf, StreamDataMode, XrefEntry};
use std::io::Cursor;
use std::path::Path;

/// Full-rewrite `fixture` with the qpdf-matching option set and return the bytes.
fn rewrite_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode::Disable)
}

/// Full-rewrite `fixture` with an explicit object-stream mode and the
/// qpdf-matching option set.
fn rewrite_qpdf_equivalent_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        object_streams: mode,
        static_id: true,
        // qpdf's default output writes no newline before endstream.
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    // compress_streams defaults to Yes (decode + re-encode to single FlateDecode).

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("static-id.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Report the first differing byte offset for a readable failure message.
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

fn assert_cmp_diff_zero(fixture: &str, stem: &str) {
    let actual = rewrite_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --static-id golden \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

fn assert_cmp_diff_zero_mode_named(fixture: &str, mode: ObjectStreamMode, stem: &str, name: &str) {
    let actual = rewrite_qpdf_equivalent_mode(fixture, mode);
    assert_cmp_diff_zero_named(&actual, stem, name);
}

fn assert_mode_cmp_diff_zero(fixture: &str, mode: ObjectStreamMode) {
    assert_cmp_diff_zero_mode_named(&format!("{fixture}.pdf"), mode, fixture, "static-id.pdf");
}

/// Full-rewrite `fixture` with an explicit object-stream `mode` + forced version
/// (qpdf-matching options).
fn rewrite_mode_force_qpdf_equivalent(
    fixture: &str,
    mode: ObjectStreamMode,
    force: &str,
) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        object_streams: mode,
        force_version: Some(force.to_string()),
        static_id: true,
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
    out
}

/// Read a named golden under `references/<stem>/`.
fn golden_named(stem: &str, name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Full-rewrite `fixture` in `--stream-data=preserve` mode with the qpdf-matching
/// option set (matches `qpdf --static-id --stream-data=preserve`).
fn rewrite_preserve_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        static_id: true,
        stream_data: Some(StreamDataMode::Preserve),
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
    out
}

/// Assert `actual` is byte-identical to the named golden under `references/<stem>/`.
fn assert_cmp_diff_zero_named(actual: &[u8], stem: &str, name: &str) {
    let expected = golden_named(stem, name);
    if let Some(off) = first_diff(actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{stem}/{name}: not byte-identical to qpdf golden \
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
fn force_below_1_5_downgrades_xref_stream_source_byte_identical_to_qpdf() {
    // The xref-stream -> classic-table DOWNGRADE path is new (ipc6 only ever did
    // table -> stream UPGRADES). Anchor it to qpdf: flpdf preserve+force1.4 on an
    // ObjStm/xref-stream source must be byte-identical to qpdf's classic-table
    // output (qpdf --object-streams=preserve --force-version=1.4 --static-id).
    let actual = rewrite_mode_force_qpdf_equivalent(
        "three-page-objstm.pdf",
        ObjectStreamMode::Preserve,
        "1.4",
    );
    let expected = golden_named("three-page-objstm", "downgrade-force14.pdf");
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "xref-stream downgrade not byte-identical to qpdf golden \
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
fn one_two_three_page_mode_matrix_is_byte_identical_to_qpdf() {
    for fixture in ["one-page", "two-page", "three-page"] {
        for mode in [ObjectStreamMode::Disable, ObjectStreamMode::Preserve] {
            assert_mode_cmp_diff_zero(fixture, mode);
        }
    }
}

/// A direct `/Root` survives when Preserve selects a cross-reference stream.
///
/// `canonical_trailer_entries` omits `/Root`, and the cross-reference stream
/// serializer reads it only from the trailer plan, so the live route has to
/// serialize an inline Catalog itself. Before this cutover the live route was
/// always classic-table, where the trailer handle carried `/Root`; selecting a
/// stream without wiring the direct Catalog drops it and makes the output
/// unreadable.
#[test]
fn direct_root_survives_a_preserve_cross_reference_stream() {
    let Some(oracle) = pinned_qpdf() else {
        eprintln!("[SKIP cmp_diff_zero_tests] qpdf 11.9.0 is unavailable");
        return;
    };
    let directory = tempfile::tempdir().expect("tempdir");
    let input = directory.path().join("direct-root-objstm.pdf");
    std::fs::write(&input, direct_root_object_stream_pdf()).expect("write fixture");
    let expected_path = directory.path().join("qpdf.pdf");

    let status = std::process::Command::new(oracle)
        .args(["--deterministic-id", "--object-streams=preserve"])
        .arg(&input)
        .arg(&expected_path)
        .status()
        .expect("qpdf runs");
    assert!(
        status.success(),
        "qpdf must rewrite the direct-root fixture"
    );

    let file = std::fs::File::open(&input).expect("open fixture");
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("fixture parses");
    let opts = WriterTestSettings {
        object_streams: ObjectStreamMode::Preserve,
        deterministic_id: true,
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &opts).expect("rewrite succeeds");

    let expected = std::fs::read(&expected_path).expect("qpdf output");
    if let Some(off) = first_diff(&actual, &expected) {
        panic!(
            "direct-root Preserve output diverged from qpdf 11.9.0 \
             (flpdf={} bytes, qpdf={} bytes, first diff at byte {off})",
            actual.len(),
            expected.len(),
        );
    }
}

/// A PDF whose trailer holds an inline Catalog and whose cross-reference stream
/// marks one object as a member of a source object stream.
fn direct_root_object_stream_pdf() -> Vec<u8> {
    fn deflate(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).expect("deflate write");
        encoder.finish().expect("deflate finish")
    }

    let member = b"<< /Type /Metadata /Note (in-objstm) >>";
    let first = b"5 0 ";
    let mut objstm_data = first.to_vec();
    objstm_data.extend_from_slice(member);
    let mut out = b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let bodies: [(u32, Vec<u8>); 2] = [
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Meta 5 0 R >>"
                .to_vec(),
        ),
    ];
    for (number, body) in bodies {
        offsets.insert(number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\nendobj\n");
    }
    offsets.insert(4, out.len());
    out.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
            first.len(),
            objstm_data.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&objstm_data);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = out.len();
    let mut rows: Vec<[u8; 7]> = Vec::new();
    let row = |kind: u8, field2: u32, field3: u16| {
        let mut entry = [0u8; 7];
        entry[0] = kind;
        entry[1..5].copy_from_slice(&field2.to_be_bytes());
        entry[5..7].copy_from_slice(&field3.to_be_bytes());
        entry
    };
    rows.push(row(0, 0, 65535));
    rows.push(row(0, 0, 65535));
    for number in [2u32, 3, 4] {
        rows.push(row(1, offsets[&number] as u32, 0));
    }
    rows.push(row(2, 4, 0));
    rows.push(row(1, xref_offset as u32, 0));
    let table: Vec<u8> = rows.concat();
    let compressed = deflate(&table);
    out.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 7 /W [1 4 2] \
             /Root << /Type /Catalog /Pages 2 0 R >> /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out
}

/// `--preserve-unreferenced` with source object streams, compared against the
/// pinned qpdf 11.9.0 binary rather than a committed golden.
///
/// This axis has no golden reference, and it is the axis this cutover actually
/// changes: with `preserve_unreferenced` the planner skips its eligibility
/// filter, so every source container survives into the live walk. Run the
/// oracle at test time for the ObjStm fixtures instead of leaving the axis
/// unpinned.
#[test]
fn preserve_unreferenced_with_source_object_streams_matches_qpdf_11_9() {
    let Some(oracle) = pinned_qpdf() else {
        eprintln!("[SKIP cmp_diff_zero_tests] qpdf 11.9.0 is unavailable");
        return;
    };
    let directory = tempfile::tempdir().expect("tempdir");
    for fixture in [
        "null-visible-matrix-objstm.pdf",
        "null-visible-preserve-empty-removed.pdf",
        "null-visible-stale-generation-objstm.pdf",
        "null-visible-preserve-signature.pdf",
        "three-page-objstm.pdf",
        "shared-stream-objstm.pdf",
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(fixture);
        let expected_path = directory.path().join(format!("{fixture}.qpdf"));
        let status = std::process::Command::new(oracle)
            .args([
                "--deterministic-id",
                "--object-streams=preserve",
                "--preserve-unreferenced",
            ])
            .arg(&input)
            .arg(&expected_path)
            .status()
            .expect("qpdf runs");
        // Exit 3 is qpdf's "succeeded with warnings"; it still writes the file
        // and is the expected status for the damaged fixtures here.
        assert!(
            matches!(status.code(), Some(0 | 3)),
            "{fixture}: qpdf --preserve-unreferenced must produce output, got {:?}",
            status.code()
        );

        let actual = rewrite_qpdf_equivalent_preserve_unreferenced(fixture);
        let expected = std::fs::read(&expected_path).expect("qpdf output");
        if let Some(off) = first_diff(&actual, &expected) {
            panic!(
                "{fixture}: --preserve-unreferenced output diverged from qpdf 11.9.0 \
                 (flpdf={} bytes, qpdf={} bytes, first diff at byte {off})",
                actual.len(),
                expected.len(),
            );
        }
    }
}

/// The QDF form of the source-backed Preserve route must admit the same
/// signature dictionary that qpdf keeps when `preserve-unreferenced` disables
/// the ordinary compressible-object eligibility filter.
///
/// Keep the two already-fixed damaged ObjStm fixtures in this exact-option
/// comparison as regression controls. The signature fixture was the RED case
/// for `flpdf-lhzo`: qpdf succeeded and emitted the member, while the old body
/// validator rejected it before emission.
#[test]
fn qdf_preserve_unreferenced_signature_objstm_matches_qpdf_11_9() {
    let Some(oracle) = pinned_qpdf() else {
        eprintln!("[SKIP cmp_diff_zero_tests] qpdf 11.9.0 is unavailable");
        return;
    };
    let directory = tempfile::tempdir().expect("tempdir");
    for fixture in [
        "null-visible-preserve-signature.pdf",
        "null-visible-preserve-empty-removed.pdf",
        "null-visible-stale-generation-objstm.pdf",
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(fixture);
        let expected_path = directory.path().join(format!("{fixture}.qpdf"));
        let status = std::process::Command::new(oracle)
            .args(["--static-id", "--qdf", "--preserve-unreferenced"])
            .arg(&input)
            .arg(&expected_path)
            .status()
            .expect("qpdf runs");
        if fixture == "null-visible-preserve-signature.pdf" {
            assert_eq!(
                status.code(),
                Some(0),
                "{fixture}: qpdf signature rewrite must succeed without warnings"
            );
        }
        assert!(
            matches!(status.code(), Some(0 | 3)),
            "{fixture}: qpdf --qdf --preserve-unreferenced must produce output, got {:?}",
            status.code()
        );

        let actual = rewrite_qdf_preserve_unreferenced(fixture).unwrap_or_else(|error| {
            panic!("{fixture}: flpdf --qdf --preserve-unreferenced must succeed like qpdf: {error}")
        });
        let expected = std::fs::read(&expected_path).expect("qpdf output");
        if let Some(off) = first_diff(&actual, &expected) {
            panic!(
                "{fixture}: --qdf --preserve-unreferenced output diverged from qpdf 11.9.0 \
                 (flpdf={} bytes, qpdf={} bytes, first diff at byte {off})",
                actual.len(),
                expected.len(),
            );
        }
    }
}

/// qpdf's `QPDF_Stream::pipeStreamData` reads the parsed in-body payload even
/// when the stream dictionary also carries the external-file keys `/F`,
/// `/FFilter`, and `/FDecodeParms`; `QPDFWriter::unparseObject` preserves those
/// keys while replacing `/Length` with the emitted payload length
/// (`QPDF_Stream.cc:605-620`, `QPDFWriter.cc:1239-1314,1440-1455`). Keep this
/// preserve-mode edge compared against the live qpdf 11.9.0 oracle.
#[test]
fn preserve_external_file_stream_matches_qpdf_11_9() {
    let Some(oracle) = pinned_qpdf() else {
        eprintln!("[SKIP cmp_diff_zero_tests] qpdf 11.9.0 is unavailable");
        return;
    };
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/external-file-stream.pdf");
    let directory = tempfile::tempdir().expect("tempdir");
    let expected_path = directory.path().join("qpdf.pdf");
    let status = std::process::Command::new(oracle)
        .args(["--static-id", "--stream-data=preserve"])
        .arg(&input)
        .arg(&expected_path)
        .status()
        .expect("qpdf runs");
    assert!(
        matches!(status.code(), Some(0 | 3)),
        "qpdf preserve rewrite must produce output, got {:?}",
        status.code()
    );

    let actual = rewrite_preserve_qpdf_equivalent("external-file-stream.pdf");
    let expected = std::fs::read(&expected_path).expect("qpdf output");
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "external-file stream preserve output diverged from qpdf 11.9.0 \
             (flpdf={} bytes, qpdf={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\nqpdf  : {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

/// The pinned qpdf 11.9.0 oracle, or `None` when it is unavailable. Comparing
/// against a different qpdf is not a parity result, so the version is checked.
fn pinned_qpdf() -> Option<&'static str> {
    let output = std::process::Command::new("qpdf").arg("--version").output();
    output.ok().and_then(|output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .is_some_and(|line| line == "qpdf version 11.9.0")
            .then_some("qpdf")
    })
}

fn rewrite_qpdf_equivalent_preserve_unreferenced(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let opts = WriterTestSettings {
        object_streams: ObjectStreamMode::Preserve,
        // qpdf is invoked with `--deterministic-id`, whose content hash differs
        // from `--static-id`'s fixed value; setting both here would pick the
        // static one and diverge in the trailer `/ID` alone.
        deterministic_id: true,
        preserve_unreferenced_objects: true,
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn rewrite_qdf_preserve_unreferenced(fixture: &str) -> flpdf::Result<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file))?;

    let opts = WriterTestSettings {
        object_streams: ObjectStreamMode::Preserve,
        qdf: true,
        static_id: true,
        preserve_unreferenced_objects: true,
        newline_before_endstream: flpdf::NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };

    let mut out = Vec::new();
    write_with_settings(&mut pdf, &mut out, &opts)?;
    Ok(out)
}

/// Preserve mode on a source with no object streams has nothing to preserve:
/// `preserveObjectStreams` returns before it builds any mapping
/// (`QPDFWriter.cc:1941-1945`), so the walk, the version floor and the
/// cross-reference form all match Disable (`:1097-1106`, `:2172-2173`,
/// `:3023-3025`).
///
/// This states the equivalence directly rather than leaving it implied by the
/// two separate golden comparisons above. It does not, on its own, pin which
/// internal route serves the case: both routes match the same golden bytes for
/// these fixtures, so reverting the routing keeps this test green. The
/// `--preserve-unreferenced` oracle comparison above does pin the routing --
/// reverting it makes that test fail -- because that axis skips the planner's
/// eligibility filter and reaches the live walk with every source container.
#[test]
fn preserve_with_no_source_object_streams_matches_disable_byte_for_byte() {
    for fixture in [
        "one-page",
        "two-page",
        "three-page",
        "preserve-no-source-objstm-xref",
    ] {
        let disable =
            rewrite_qpdf_equivalent_mode(&format!("{fixture}.pdf"), ObjectStreamMode::Disable);
        let preserve =
            rewrite_qpdf_equivalent_mode(&format!("{fixture}.pdf"), ObjectStreamMode::Preserve);
        assert_eq!(
            disable, preserve,
            "{fixture}: Preserve-with-no-source-ObjStm diverged from Disable"
        );
    }
}

/// A source xref stream with no type-2 rows must not make Preserve retain an
/// object-stream layout. qpdf's `preserveObjectStreams` returns at its empty
/// source-membership check, so both Preserve and Disable emit the same
/// classic-xref rewrite (`QPDFWriter.cc:1939-1945,2172-2173,3023-3025`).
/// Compare both modes with the live qpdf 11.9.0 output as well as with each
/// other so this is an observable source-xref-form regression case, not only a
/// comparison against a shared golden.
#[test]
fn preserve_no_source_objstm_xref_stream_matches_qpdf_11_9() {
    let fixture = "preserve-no-source-objstm-xref.pdf";

    // The local mode and output-form assertions do not need the oracle, so
    // they run everywhere. Only the byte comparison against qpdf is gated;
    // otherwise an environment without the pinned binary would silently stop
    // checking that a source xref stream is not carried into the rewrite.
    let preserve = rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode::Preserve);
    let disable = rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode::Disable);
    assert_eq!(
        preserve, disable,
        "empty source membership must produce the Disable-equivalent bytes"
    );
    // `startxref\n` also contains `xref\n`, so anchor on a standalone section
    // header followed by its first subsection. Without the leading newline and
    // the `0 ` subsection start, an output with no xref section at all would
    // still satisfy this assertion.
    assert!(
        preserve
            .windows(b"\nxref\n0 ".len())
            .any(|window| window == b"\nxref\n0 "),
        "an empty source membership must use a classic xref table"
    );
    assert!(
        !preserve
            .windows(b"/Type /XRef".len())
            .any(|window| window == b"/Type /XRef"),
        "the source xref stream must not be carried into the rewritten output"
    );

    let Some(oracle) = pinned_qpdf() else {
        eprintln!("[SKIP cmp_diff_zero_tests] qpdf 11.9.0 is unavailable for the byte comparison");
        return;
    };
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let directory = tempfile::tempdir().expect("tempdir");
    let expected_path = directory.path().join("qpdf.pdf");
    let status = std::process::Command::new(oracle)
        .args(["--static-id", "--object-streams=preserve"])
        .arg(&input)
        .arg(&expected_path)
        .status()
        .expect("qpdf runs");
    assert_eq!(status.code(), Some(0), "qpdf preserve rewrite must succeed");
    let expected = std::fs::read(&expected_path).expect("qpdf output");
    assert_eq!(preserve, expected, "Preserve output must match qpdf 11.9.0");
}

#[test]
fn disable_xref_stream_source_downgrades_to_classic_table_byte_identical_to_qpdf() {
    assert_cmp_diff_zero_mode_named(
        "null-visible-matrix-objstm.pdf",
        ObjectStreamMode::Disable,
        "null-visible-matrix-objstm",
        "disable.pdf",
    );
}

#[test]
fn preserve_object_stream_mode_is_byte_identical_to_qpdf_static_id_matrix() {
    let cases = [
        ("three-page-objstm.pdf", "three-page-objstm", "preserve.pdf"),
        (
            "objstm-lin-od-indirect-length.pdf",
            "objstm-lin-od-indirect-length",
            "static-id.pdf",
        ),
        (
            "objstm-lin-od-indirect-length-flate.pdf",
            "objstm-lin-od-indirect-length-flate",
            "static-id.pdf",
        ),
        (
            "kept-indirect-length.pdf",
            "kept-indirect-length",
            "static-id.pdf",
        ),
    ];
    for (fixture, stem, name) in cases {
        assert_cmp_diff_zero_mode_named(fixture, ObjectStreamMode::Preserve, stem, name);
    }
}

#[test]
fn preserve_nonmonotonic_source_indices_match_qpdf_source_number_order() {
    let fixture = "nonmonotonic-objstm-index.pdf";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let source = std::fs::read(path).unwrap();
    let source_pdf = Pdf::open(Cursor::new(source)).unwrap();
    let source_xref = source_pdf.get_xref_table();
    assert_eq!(
        source_xref.get(&ObjectRef::new(3, 0)),
        Some(&XrefEntry::Compressed {
            stream: 4,
            index: 0,
        })
    );
    assert_eq!(
        source_xref.get(&ObjectRef::new(2, 0)),
        Some(&XrefEntry::Compressed {
            stream: 4,
            index: 1,
        })
    );

    let actual = rewrite_qpdf_equivalent_mode(fixture, ObjectStreamMode::Preserve);
    assert_cmp_diff_zero_named(&actual, "nonmonotonic-objstm-index", "preserve.pdf");

    let output_pdf = Pdf::open(Cursor::new(actual.clone())).unwrap();
    let output_xref = output_pdf.get_xref_table();
    assert_eq!(
        output_xref.get(&ObjectRef::new(3, 0)),
        Some(&XrefEntry::Compressed {
            stream: 2,
            index: 0,
        })
    );
    assert_eq!(
        output_xref.get(&ObjectRef::new(4, 0)),
        Some(&XrefEntry::Compressed {
            stream: 2,
            index: 1,
        })
    );

    let mut rewritten = Pdf::open(Cursor::new(actual)).unwrap();
    let catalog = rewritten
        .resolve_canonical_object(rewritten.root_ref().unwrap())
        .unwrap();
    assert_eq!(
        catalog.try_get_key(b"/Pages").unwrap().object_ref(),
        Some(ObjectRef::new(3, 0))
    );
    assert_eq!(
        catalog.try_get_key(b"/Extra").unwrap().object_ref(),
        Some(ObjectRef::new(4, 0))
    );
}

#[test]
fn lone_flate_l9_plain_rewrite_is_byte_identical_to_qpdf_static_id() {
    // A lone /FlateDecode source compressed at level 9: flpdf must preserve the
    // bytes verbatim (qpdf default), so re-encoding at level 6 would diverge.
    assert_cmp_diff_zero("lone-flate-l9.pdf", "lone-flate-l9");
}

#[test]
fn od_indirect_length_plain_rewrite_drops_orphan_holder_byte_identical_to_qpdf() {
    // The catalog's /OpenAction reaches a JavaScript stream (obj 6) with an
    // INDIRECT /Length (7 0 R); the holder (obj 7) is reachable ONLY through
    // that /Length edge. Once /Length is normalized to a direct integer the
    // holder orphans, and qpdf garbage-collects it. The plain full-rewrite path
    // must drop it too, shifting object numbers contiguously — not
    // emit it as a trailing integer object.
    assert_cmp_diff_zero(
        "objstm-lin-od-indirect-length.pdf",
        "objstm-lin-od-indirect-length",
    );
}

#[test]
fn od_indirect_length_flate_plain_rewrite_drops_orphan_holder_byte_identical_to_qpdf() {
    // Same orphan structure, but the JS stream is a lone /FlateDecode (the
    // writer's verbatim-preserve path): /Length is direct-ized to the
    // compressed byte count when the holder is dropped.
    assert_cmp_diff_zero(
        "objstm-lin-od-indirect-length-flate.pdf",
        "objstm-lin-od-indirect-length-flate",
    );
}

#[test]
fn od_indirect_length_preserve_drops_orphan_holder_byte_identical_to_qpdf() {
    // --stream-data=preserve keeps the stream bytes verbatim, but qpdf still
    // direct-izes every stream's /Length and GCs the orphaned holder.
    // The orphan-drop gate must therefore fire for preserve too, not only when
    // streams are recompressed.
    let actual = rewrite_preserve_qpdf_equivalent("objstm-lin-od-indirect-length.pdf");
    assert_cmp_diff_zero_named(&actual, "objstm-lin-od-indirect-length", "preserve.pdf");
}

#[test]
fn od_indirect_length_flate_preserve_drops_orphan_holder_byte_identical_to_qpdf() {
    // Same as above with a lone /FlateDecode JS stream: under preserve the
    // compressed bytes are kept verbatim and /Length is direct-ized to the
    // compressed byte count when the holder is dropped.
    let actual = rewrite_preserve_qpdf_equivalent("objstm-lin-od-indirect-length-flate.pdf");
    assert_cmp_diff_zero_named(
        &actual,
        "objstm-lin-od-indirect-length-flate",
        "preserve.pdf",
    );
}

#[test]
fn kept_indirect_length_preserve_directizes_kept_holder_byte_identical_to_qpdf() {
    // The dual of the orphan case: the image XObject's indirect /Length holder is
    // ALSO referenced by the catalog (/KeepHolder), so it stays live. qpdf still
    // direct-izes the stream's /Length even under preserve (it normalizes EVERY
    // stream's /Length to a direct integer), keeping the holder as a now
    // length-unreferenced live integer. flpdf must match.
    let actual = rewrite_preserve_qpdf_equivalent("kept-indirect-length.pdf");
    assert_cmp_diff_zero_named(&actual, "kept-indirect-length", "preserve.pdf");
}

#[test]
fn kept_indirect_length_plain_rewrite_directizes_length_keeps_holder_byte_identical_to_qpdf() {
    // Dual of the orphan case: an image XObject (obj 5) declares
    // /Filter /DCTDecode — which flpdf cannot decode, so it is passed through
    // verbatim — and carries an INDIRECT /Length (6 0 R) whose holder (obj 6) is
    // ALSO referenced by the catalog (/KeepHolder 6 0 R). qpdf direct-izes the
    // /Length to the raw byte count (every emitted stream gets a direct /Length)
    // while KEEPING the holder (it has another live reference). The decode-failure
    // passthrough path used to leak the renumbered indirect /Length; this pins it
    // byte-identical to qpdf.
    assert_cmp_diff_zero("kept-indirect-length.pdf", "kept-indirect-length");
}
