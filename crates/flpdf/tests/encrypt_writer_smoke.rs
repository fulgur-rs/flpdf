//! End-to-end smoke tests for the writer-side `--encrypt` path
//! (V=4 AES-128 only).
//!
//! Each test builds an encrypted PDF via `write_with_settings` with
//! `WriterTestSettings.encrypt = Some(EncryptParams::v4_aes128(...))`, then
//! validates the result by re-opening it through `Pdf::open_with_options`
//! and checking the encrypted-document invariants. Where a plaintext
//! input fixture has identifiable content (strings or stream payloads),
//! we also verify the resolved contents after decryption match the
//! original — proving the per-object key derivation, AES IV/padding, and
//! `/Length` updates all line up with what the reader path expects.

use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::process::Command;

use flpdf::{
    load_xref_and_trailer, CopyEncryptionSource, DecodeLevel, EncryptMethod, EncryptParams,
    ObjectHandle, ObjectKeyAlg, ObjectRef, ObjectStreamMode, PageDocumentHelper, Pdf,
    PdfOpenOptions, R2PermissionsConfig, StreamDataMode, XrefEntry,
};

const INFO_PLAINTEXT: &[u8] = b"Task4NestedPrintable";
const STREAM_DICT_PLAINTEXT: &[u8] = b"Task4StreamPrintable";

fn fixture(rel: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    fs::read(&path).unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()))
}

fn encrypt_to_bytes(input: &[u8], params: EncryptParams) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(input.to_vec())).expect("open plaintext input");
    let mut out = Vec::new();
    let options = WriterTestSettings {
        encrypt: Some(params),
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut out, &options).expect("encrypted write");
    out
}

fn open_encrypted(bytes: &[u8], password: &[u8]) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open_with_options(
        Cursor::new(bytes.to_vec()),
        PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("open encrypted output with password")
}

/// A one-page PDF whose trailer `/Info` dictionary contains a nested printable
/// string and whose Catalog-reachable stream dictionary contains another one.
/// The stream keeps the QDF indirect `/Length` holder route observable while
/// the nested `/Info` value is eligible for generated ObjStm membership.
///
/// The page intentionally has no `/Resources`. qpdf 11.9.0 preserves that
/// omission in standard, QDF, and generated-ObjStm writes: only the linearized
/// writer calls `QPDF::optimize()`, and inherited-attribute pushing copies an
/// ancestor value rather than synthesizing an empty resource dictionary.
fn nested_string_fixture(info_value: &[u8]) -> Vec<u8> {
    let mut info_hex = String::with_capacity(info_value.len() * 2);
    for byte in info_value {
        use std::fmt::Write as _;
        write!(&mut info_hex, "{byte:02x}").expect("write to String");
    }

    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] >>".to_vec(),
        format!("<< /Nested << /Marker <{info_hex}> >> >>").into_bytes(),
        [
            format!(
                "<< /Type /Metadata /Subtype /XML /Label ({}) /Length 5 >>\nstream\n",
                String::from_utf8_lossy(STREAM_DICT_PLAINTEXT)
            )
            .into_bytes(),
            b"hello\nendstream".to_vec(),
        ]
        .concat(),
    ];
    fixture_from_objects(&objects, 4)
}

fn rc4_printable_ciphertext_fixture() -> Vec<u8> {
    let mut candidates = String::new();
    for byte in 0u8..=u8::MAX {
        use std::fmt::Write as _;
        write!(&mut candidates, " /C{byte:02x} <{byte:02x}>").expect("write to String");
    }
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] >>".to_vec(),
        format!("<< /Nested <<{candidates} >> >>").into_bytes(),
    ];
    fixture_from_objects(&objects, 4)
}

fn fixture_from_objects(objects: &[Vec<u8>], info_number: u32) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {info_number} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1,
        )
        .as_bytes(),
    );
    bytes
}

fn encrypted_options() -> WriterTestSettings {
    WriterTestSettings {
        static_id: true,
        static_aes_iv: true,
        encrypt: Some(EncryptParams::v4_aes128(Vec::new(), Vec::new())),
        ..WriterTestSettings::default()
    }
}

fn rewrite_fixture(input: &[u8], options: &WriterTestSettings) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(input.to_vec())).expect("open nested string fixture");
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, options).expect("encrypted full rewrite");
    output
}

fn assert_named_string_token_is_hex(bytes: &[u8], key: &[u8]) {
    let key_offset = bytes
        .windows(key.len())
        .position(|part| part == key)
        .unwrap_or_else(|| panic!("missing {} in output", String::from_utf8_lossy(key)));
    let mut token_offset = key_offset + key.len();
    while bytes
        .get(token_offset)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        token_offset += 1;
    }
    assert_eq!(
        bytes.get(token_offset),
        Some(&b'<'),
        "{} string token must use hexadecimal syntax",
        String::from_utf8_lossy(key)
    );
    let token_end = bytes[token_offset + 1..]
        .iter()
        .position(|byte| *byte == b'>')
        .map(|relative| token_offset + 1 + relative)
        .expect("hexadecimal string token must close");
    assert!(
        bytes[token_offset + 1..token_end]
            .iter()
            .all(u8::is_ascii_hexdigit),
        "{} hexadecimal token contains a non-hex byte",
        String::from_utf8_lossy(key)
    );
}

fn run_qpdf_check(bytes: &[u8]) -> Option<std::process::Output> {
    if Command::new("qpdf").arg("--version").output().is_err() {
        return None;
    }

    let dir = tempfile::tempdir().expect("create qpdf check directory");
    let path = dir.path().join("encrypted.pdf");
    fs::write(&path, bytes).expect("write qpdf check input");
    Some(
        Command::new("qpdf")
            .arg("--password=")
            .arg("--check")
            .arg(&path)
            .output()
            .expect("run qpdf --check"),
    )
}

fn qpdf_check_result_is_acceptable(exit_code: Option<i32>, stderr: &[u8]) -> bool {
    if exit_code == Some(0) {
        return true;
    }
    if exit_code != Some(3) {
        return false;
    }

    // qpdf 12.x added this page validation after the pinned 11.9.0 oracle.
    // Permit only that version-skew warning. Other repair warnings still fail
    // the test so damaged xrefs, syntax, and stream data cannot be hidden.
    let Ok(stderr) = std::str::from_utf8(stderr) else {
        return false;
    };
    let mut saw_resources_warning = false;
    let mut saw_warning_summary = false;
    for line in stderr.lines().map(|line| line.trim_end_matches('\r')) {
        if line.is_empty() {
            continue;
        }
        if line == "qpdf: operation succeeded with warnings" {
            if saw_warning_summary {
                return false;
            }
            saw_warning_summary = true;
        } else if line.starts_with("WARNING: ")
            && line.ends_with(" Resources is missing or invalid; repairing")
        {
            saw_resources_warning = true;
        } else {
            return false;
        }
    }
    saw_resources_warning && saw_warning_summary
}

fn assert_qpdf_check(bytes: &[u8]) {
    let Some(output) = run_qpdf_check(bytes) else {
        eprintln!("qpdf not available; skipping qpdf --check verification");
        return;
    };
    assert!(
        qpdf_check_result_is_acceptable(output.status.code(), &output.stderr),
        "qpdf --check failed:\nstdout:\n{}\nstderr:\n{}\nPDF:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(bytes)
    );
}

fn qpdf_qdf_encrypted_rewrite(input: &[u8]) -> Option<Vec<u8>> {
    if Command::new("qpdf").arg("--version").output().is_err() {
        return None;
    }

    let directory = tempfile::tempdir().expect("create qpdf rewrite directory");
    let input_path = directory.path().join("input.pdf");
    let output_path = directory.path().join("output.pdf");
    fs::write(&input_path, input).expect("write qpdf rewrite input");
    let result = Command::new("qpdf")
        .args([
            "--static-id",
            "--qdf",
            "--object-streams=generate",
            "--allow-weak-crypto",
            "--encrypt",
            "",
            "x",
            "128",
            "--",
        ])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .expect("run qpdf encrypted QDF rewrite");
    assert!(
        result.status.success(),
        "qpdf encrypted QDF rewrite failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Some(fs::read(output_path).expect("read qpdf encrypted QDF rewrite"))
}

#[test]
fn qpdf_check_helper_rejects_recoverable_xref_warnings_and_errors() {
    let mut warning_only = nested_string_fixture(INFO_PLAINTEXT);
    let marker = b"\nstartxref\n";
    let marker_offset = warning_only
        .windows(marker.len())
        .rposition(|part| part == marker)
        .expect("fixture has startxref");
    let value_start = marker_offset + marker.len();
    let value_end = value_start
        + warning_only[value_start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .expect("startxref value is newline terminated");
    warning_only.splice(value_start..value_end, *b"0");

    let Some(warning_output) = run_qpdf_check(&warning_only) else {
        eprintln!("qpdf not available; skipping qpdf --check verification");
        return;
    };
    assert!(
        !warning_output.status.success(),
        "qpdf repair warnings must remain distinguishable from clean output: {}",
        String::from_utf8_lossy(&warning_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&warning_output.stderr).contains("WARNING"),
        "damaged startxref must exercise qpdf's warning-only path"
    );

    let error_output = run_qpdf_check(b"not a PDF file").expect("qpdf remains available");
    assert!(
        !error_output.status.success(),
        "--warning-exit-0 must not hide qpdf errors"
    );
}

#[test]
fn qpdf_check_acceptance_allows_only_qpdf_12_missing_resources_warning() {
    let resource_warning = b"WARNING: /tmp/output.pdf object stream 6, object 5 0 at offset 116: kid 0 (from 0) Resources is missing or invalid; repairing\nqpdf: operation succeeded with warnings\n";
    assert!(qpdf_check_result_is_acceptable(Some(3), resource_warning));

    let xref_warning = b"WARNING: /tmp/output.pdf: file is damaged\nWARNING: /tmp/output.pdf: can't find startxref\nWARNING: /tmp/output.pdf: Attempting to reconstruct cross-reference table\nqpdf: operation succeeded with warnings\n";
    assert!(!qpdf_check_result_is_acceptable(Some(3), xref_warning));
    assert!(!qpdf_check_result_is_acceptable(
        Some(3),
        b"WARNING: /tmp/output.pdf: stream data is damaged; repairing\nqpdf: operation succeeded with warnings\n",
    ));
    assert!(!qpdf_check_result_is_acceptable(Some(2), resource_warning));
}

#[test]
fn encrypted_standard_writes_preserve_missing_page_resources_like_qpdf_11_9() {
    let input = nested_string_fixture(INFO_PLAINTEXT);
    for (qdf, object_streams) in [
        (false, ObjectStreamMode::Disable),
        (true, ObjectStreamMode::Disable),
        (false, ObjectStreamMode::Generate),
    ] {
        let mut options = encrypted_options();
        options.qdf = qdf;
        options.object_streams = object_streams;
        let bytes = rewrite_fixture(&input, &options);
        let mut reopened = open_encrypted(&bytes, b"");
        let pages = PageDocumentHelper::new(&mut reopened)
            .get_all_pages()
            .expect("enumerate rewritten pages");
        assert_eq!(pages.len(), 1);
        let page = reopened
            .resolve_canonical_object(pages[0])
            .expect("resolve rewritten page");
        assert!(
            page.as_dictionary()
                .is_some_and(|dict| !dict.contains_key(b"/Resources".as_slice())),
            "qdf={qdf}, object_streams={object_streams:?}: qpdf 11.9 standard writes do not synthesize /Resources"
        );
    }
}

fn resolve_nested_info_marker(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<u8> {
    let info_ref = pdf
        .trailer()
        .try_get_key(b"/Info")
        .expect("read trailer /Info")
        .object_ref()
        .expect("trailer /Info must be an indirect reference");
    let info = pdf
        .resolve_canonical_object(info_ref)
        .expect("resolve /Info");
    let nested = info
        .try_get_key(b"/Nested")
        .expect("read /Nested")
        .as_dictionary()
        .expect("/Info /Nested dictionary");
    nested
        .get(b"/Marker".as_slice())
        .and_then(|value| value.as_string())
        .expect("/Info /Nested /Marker must be a string")
}

fn resolve_metadata_label(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<u8> {
    let root_ref = pdf.root_ref().expect("encrypted output has /Root");
    let metadata_ref = pdf
        .resolve_canonical_object(root_ref)
        .expect("resolve Catalog")
        .try_get_key(b"/Metadata")
        .expect("read Catalog /Metadata")
        .object_ref()
        .expect("Catalog /Metadata must be an indirect reference");
    let metadata = pdf
        .resolve_canonical_object(metadata_ref)
        .expect("resolve Metadata stream");
    metadata
        .as_stream_dict()
        .and_then(|dict| dict.get_key(b"/Label").as_string())
        .expect("Metadata /Label must be a string")
}

fn object_number_before_marker(bytes: &[u8], marker: &[u8]) -> u32 {
    let marker_offset = bytes
        .windows(marker.len())
        .position(|part| part == marker)
        .expect("object marker must be present");
    let object_suffix = b" 0 obj\n";
    let suffix_offset = bytes[..marker_offset]
        .windows(object_suffix.len())
        .rposition(|part| part == object_suffix)
        .expect("marker must follow an indirect object header");
    let mut number_start = suffix_offset;
    while number_start > 0 && bytes[number_start - 1].is_ascii_digit() {
        number_start -= 1;
    }
    std::str::from_utf8(&bytes[number_start..suffix_offset])
        .expect("object number is ASCII")
        .parse()
        .expect("object number is decimal")
}

fn indirect_object_numbers(bytes: &[u8]) -> Vec<u32> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let mut words = line.split(|byte| *byte == b' ');
            let number = words.next()?;
            if words.next()? != b"0" || words.next()? != b"obj" || words.next().is_some() {
                return None;
            }
            std::str::from_utf8(number).ok()?.parse().ok()
        })
        .collect()
}

/// Encrypting `minimal.pdf` (no strings / no streams) must still produce
/// a structurally valid encrypted file: `/Encrypt` present in the
/// trailer, the password authenticates as a user password, and the
/// resulting reader-side qpdf encryption accessors report V=4 R=4 AESv2.
#[test]
fn v4_aes128_encrypts_minimal_fixture_and_authenticates_user_password() {
    let input = fixture("tests/fixtures/minimal.pdf");
    let encrypted = encrypt_to_bytes(
        &input,
        EncryptParams::v4_aes128(b"user-pw".to_vec(), b"owner-pw".to_vec()),
    );

    // Sanity: /Encrypt is present in the output bytes.
    assert!(
        encrypted
            .windows(b"/Encrypt".len())
            .any(|w| w == b"/Encrypt"),
        "encrypted output must carry /Encrypt"
    );

    // Open with the user password and verify the reader-side view.
    let pdf = open_encrypted(&encrypted, b"user-pw");
    assert!(pdf.is_encrypted(), "reader must report is_encrypted=true");
    assert!(
        pdf.user_password_matched(),
        "user password should authenticate"
    );
    assert_eq!(pdf.encryption_version(), Some(4));
    assert_eq!(pdf.encryption_revision(), Some(4));
    assert_eq!(pdf.encryption_length_bits(), Some(128));
    assert_eq!(pdf.encryption_methods(), Some(("AESv2", "AESv2", "AESv2")));
}

/// Owner password also authenticates against the same encrypted output —
/// not just the user password. Covers Algorithm 7 round-trip via the
/// writer-built `/O` entry.
#[test]
fn v4_aes128_owner_password_also_authenticates() {
    let input = fixture("tests/fixtures/minimal.pdf");
    let encrypted = encrypt_to_bytes(
        &input,
        EncryptParams::v4_aes128(b"user-pw".to_vec(), b"owner-pw".to_vec()),
    );
    let pdf = open_encrypted(&encrypted, b"owner-pw");
    assert!(
        pdf.owner_password_matched(),
        "owner password should authenticate"
    );
}

/// A wrong password is rejected, proving the encryption is real and the
/// reader is genuinely validating against `/U` / `/O` — not just
/// accepting any byte sequence as a password.
#[test]
fn v4_aes128_wrong_password_is_rejected() {
    let input = fixture("tests/fixtures/minimal.pdf");
    let encrypted = encrypt_to_bytes(
        &input,
        EncryptParams::v4_aes128(b"correct-pw".to_vec(), b"correct-owner".to_vec()),
    );
    let result = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: b"WRONG".to_vec(),
            ..PdfOpenOptions::default()
        },
    );
    let err = match result {
        Ok(_) => panic!("wrong password must fail to open"),
        Err(e) => e,
    };
    let display = format!("{err:?}");
    assert!(
        display.contains("BadPassword") || display.contains("Encrypted"),
        "expected BadPassword error variant, got: {display}"
    );
}

/// Richer round-trip on `compat/one-page.pdf` (has streams + content
/// strings): after encryption + decryption via the reader path, the
/// resolved `/Root` is a valid `/Catalog`. This exercises per-object key
/// derivation + AES IV/padding + `/Length` update on a non-trivial stream
/// payload.
///
/// The full-rewrite writer renumbers objects Catalog-first, so
/// the output `/Root` number is NOT the input's; the round-trip property is
/// that the trailer's `/Root` still resolves to the document catalog.
#[test]
fn v4_aes128_round_trip_on_one_page_resolves_to_same_root() {
    let input = fixture("tests/fixtures/compat/one-page.pdf");

    let encrypted = encrypt_to_bytes(
        &input,
        EncryptParams::v4_aes128(b"u".to_vec(), b"o".to_vec()),
    );
    let mut enc_pdf = open_encrypted(&encrypted, b"u");
    let enc_root = enc_pdf.root_ref().expect("encrypted output has /Root");

    // Resolve the catalog dictionary and verify it carries /Type /Catalog
    // after decryption (proves at least one full object decrypts cleanly).
    let catalog = enc_pdf
        .resolve_canonical_object(enc_root)
        .expect("decrypted /Catalog object resolves");
    assert_eq!(
        catalog
            .as_dictionary()
            .and_then(|dict| dict.get(b"/Type".as_slice()).cloned())
            .and_then(|value| value.as_name()),
        Some(b"Catalog".to_vec()),
        "/Catalog /Type must round-trip across encrypt + decrypt"
    );
}

/// AES string syntax is selected at scalar emission time on both compact and
/// QDF full rewrites. The nested `/Info` value and stream-dictionary label must
/// both be encrypted, forced to hexadecimal syntax, accepted by qpdf, and
/// recoverable through flpdf's public reader.
#[test]
fn aes_encrypted_strings_use_hex_in_compact_and_qdf() {
    let input = nested_string_fixture(INFO_PLAINTEXT);
    for qdf in [false, true] {
        let mut options = encrypted_options();
        options.qdf = qdf;
        let bytes = rewrite_fixture(&input, &options);

        for plaintext in [INFO_PLAINTEXT, STREAM_DICT_PLAINTEXT] {
            assert!(
                !bytes.windows(plaintext.len()).any(|part| part == plaintext),
                "{plaintext:?} leaked from the {qdf:?} encrypted output"
            );
        }
        assert_named_string_token_is_hex(&bytes, b"/Marker");
        assert_named_string_token_is_hex(&bytes, b"/Label");
        if qdf {
            let object_numbers = indirect_object_numbers(&bytes);
            let unique: BTreeSet<_> = object_numbers.iter().copied().collect();
            assert_eq!(
                object_numbers.len(),
                unique.len(),
                "QDF object and /Length-holder numbers must be unique: {object_numbers:?}"
            );
            assert!(
                bytes
                    .windows(b"\nendobj\n\nxref\n".len())
                    .any(|part| part == b"\nendobj\n\nxref\n"),
                "QDF /Encrypt object must keep the blank line before xref"
            );
        }
        assert_qpdf_check(&bytes);

        let mut reopened = open_encrypted(&bytes, b"");
        assert!(reopened.is_encrypted());
        assert!(reopened.user_password_matched());
        let encrypt_ref = reopened
            .trailer()
            .try_get_key(b"/Encrypt")
            .expect("read trailer /Encrypt")
            .object_ref()
            .expect("trailer /Encrypt must be an indirect reference");
        let encrypt_dict = reopened
            .resolve_canonical_object(encrypt_ref)
            .expect("resolve dedicated /Encrypt object");
        assert_eq!(
            encrypt_dict
                .as_dictionary()
                .and_then(|dict| dict.get(b"/Filter".as_slice()).cloned())
                .and_then(|value| value.as_name()),
            Some(b"Standard".to_vec()),
            "trailer /Encrypt must reference the dedicated encryption dictionary"
        );
        assert_eq!(resolve_nested_info_marker(&mut reopened), INFO_PLAINTEXT);
        assert_eq!(resolve_metadata_label(&mut reopened), STREAM_DICT_PLAINTEXT);
    }
}

/// Generated ObjStm members do not receive an individual string data key. The
/// enclosing ObjStm stream remains payload-encrypted, while its decrypted and
/// decoded member body retains the original plaintext marker.
#[test]
fn generated_objstm_member_strings_are_encrypted_only_by_the_container() {
    let input = nested_string_fixture(INFO_PLAINTEXT);
    let mut options = encrypted_options();
    options.object_streams = ObjectStreamMode::Generate;
    let bytes = rewrite_fixture(&input, &options);

    assert!(
        bytes
            .windows(b"/Type /ObjStm".len())
            .any(|part| part == b"/Type /ObjStm"),
        "generate mode must emit an ObjStm container"
    );
    assert!(
        !bytes
            .windows(INFO_PLAINTEXT.len())
            .any(|part| part == INFO_PLAINTEXT),
        "the ObjStm container payload must be encrypted"
    );
    assert_qpdf_check(&bytes);

    let mut reopened = open_encrypted(&bytes, b"");
    let info_ref = reopened
        .trailer()
        .try_get_key(b"/Info")
        .expect("read trailer /Info")
        .object_ref()
        .expect("trailer /Info must be an indirect reference");
    let object_header = format!("\n{} 0 obj\n", info_ref.number);
    assert!(
        !bytes
            .windows(object_header.len())
            .any(|part| part == object_header.as_bytes()),
        "/Info must be stored as an ObjStm member, not a plain indirect object"
    );

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes.as_slice()))
        .expect("load encrypted output xref");
    let root_ref = reopened.root_ref().expect("encrypted output has /Root");
    assert!(
        !matches!(
            loaded.entries.get(&root_ref),
            Some(XrefEntry::Compressed { .. })
        ),
        "encrypted output must keep the Catalog outside ObjStm"
    );
    let (container_number, member_index) = match loaded.entries.get(&info_ref) {
        Some(XrefEntry::Compressed { stream, index }) => (*stream, *index),
        other => panic!("/Info must have a type-2 xref entry, got {other:?}"),
    };
    assert_eq!(
        container_number,
        object_number_before_marker(&bytes, b"/Type /ObjStm"),
        "/Info type-2 xref entry must name the emitted ObjStm container"
    );
    let container = reopened
        .resolve_canonical_object(ObjectRef::new(container_number, 0))
        .expect("resolve and decrypt ObjStm container");
    let stream_dict = container
        .as_stream_dict()
        .expect("ObjStm object is a stream");
    let decoded = container
        .get_stream_data(DecodeLevel::Generalized)
        .expect("decode decrypted ObjStm payload");
    let first = usize::try_from(
        stream_dict
            .get_key(b"/First")
            .as_integer()
            .expect("ObjStm /First must be an integer"),
    )
    .expect("non-negative /First");
    let member_count = u32::try_from(
        stream_dict
            .get_key(b"/N")
            .as_integer()
            .expect("ObjStm /N must be an integer"),
    )
    .expect("non-negative /N");
    assert!(
        member_index < member_count,
        "type-2 index must be within /N"
    );
    let header_fields: Vec<usize> = std::str::from_utf8(&decoded[..first])
        .expect("ObjStm header is ASCII")
        .split_ascii_whitespace()
        .map(|field| field.parse().expect("ObjStm header field is decimal"))
        .collect();
    let member_pair = usize::try_from(member_index).expect("member index fits usize") * 2;
    assert_eq!(
        header_fields.get(member_pair).copied(),
        Some(usize::try_from(info_ref.number).expect("object number fits usize")),
        "type-2 index must select the /Info member header"
    );
    let member_start = first
        + header_fields
            .get(member_pair + 1)
            .copied()
            .expect("member offset follows object number");
    let member_end = if member_index + 1 < member_count {
        first
            + header_fields
                .get(member_pair + 3)
                .copied()
                .expect("next member offset follows next object number")
    } else {
        decoded.len()
    };
    assert!(
        decoded[member_start..member_end]
            .windows(INFO_PLAINTEXT.len())
            .any(|part| part == INFO_PLAINTEXT),
        "ObjStm member string must stay plaintext inside the encrypted container"
    );
}

/// qpdf assigns the generated ObjStm container before the ordinary objects
/// even when the source was encrypted and the output is decrypted. The
/// source encryption state must not route this standard Generate rewrite
/// through the catalog-first legacy placement.
#[test]
fn decrypting_encrypted_source_generate_uses_container_first_numbering() {
    let input = fixture("tests/fixtures/compat/one-page-enc-u.pdf");
    let mut pdf = Pdf::open_with_options(
        Cursor::new(input),
        PdfOpenOptions {
            password: b"u".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("open encrypted source");
    let options = WriterTestSettings {
        static_id: true,
        object_streams: ObjectStreamMode::Generate,
        preserve_encryption: false,
        ..WriterTestSettings::default()
    };
    let bytes = {
        let mut output = Vec::new();
        write_with_settings(&mut pdf, &mut output, &options).expect("decrypt and generate");
        output
    };

    assert_eq!(
        object_number_before_marker(&bytes, b"/Type /ObjStm"),
        1,
        "qpdf assigns the generated ObjStm before ordinary objects for decrypted encrypted input"
    );
}

/// Keep the existing QDF encrypted ObjStm serializer covered while the
/// non-QDF Generate route adopts qpdf's container-first numbering.
#[test]
fn encrypted_qdf_generate_objstm_remains_checkable() {
    let input = nested_string_fixture(INFO_PLAINTEXT);
    let mut options = encrypted_options();
    options.qdf = true;
    options.object_streams = ObjectStreamMode::Generate;

    let bytes = rewrite_fixture(&input, &options);
    assert!(bytes
        .windows(b"%QDF-1.0".len())
        .any(|part| part == b"%QDF-1.0"));
    assert!(bytes
        .windows(b"/Type /ObjStm".len())
        .any(|part| part == b"/Type /ObjStm"));
    assert_qpdf_check(&bytes);

    let reopened = open_encrypted(&bytes, b"");
    assert!(reopened.is_encrypted());
    assert!(reopened.root_ref().is_some());
}

#[test]
fn encrypted_qdf_objstm_dictionary_matches_qpdf() {
    let input = nested_string_fixture(INFO_PLAINTEXT);
    let options = WriterTestSettings {
        static_id: true,
        qdf: true,
        object_streams: ObjectStreamMode::Generate,
        encrypt: Some(EncryptParams::rc4(
            EncryptMethod::V2Rc4128,
            Vec::new(),
            b"x".to_vec(),
        )),
        ..WriterTestSettings::default()
    };
    let actual = rewrite_fixture(&input, &options);
    let Some(expected) = qpdf_qdf_encrypted_rewrite(&input) else {
        eprintln!("qpdf not available; skipping encrypted QDF byte parity");
        return;
    };

    assert_eq!(
        actual, expected,
        "encrypted QDF ObjStm output must be byte-identical to qpdf 11.9.0"
    );
}

#[test]
fn copy_encryption_rejects_short_public_file_key_in_compact_and_qdf() {
    let input = nested_string_fixture(INFO_PLAINTEXT);

    for qdf in [false, true] {
        let mut pdf = Pdf::open(Cursor::new(input.clone())).expect("open copy-encryption fixture");
        let mut output = Vec::new();
        let mut options = WriterTestSettings {
            qdf,
            static_id: true,
            static_aes_iv: true,
            ..WriterTestSettings::default()
        };
        let encrypt_dict = ObjectHandle::dictionary(vec![
            (b"/V".to_vec(), ObjectHandle::integer(4)),
            (b"/R".to_vec(), ObjectHandle::integer(4)),
            (b"/Length".to_vec(), ObjectHandle::integer(128)),
        ]);
        options.copy_encryption = Some(CopyEncryptionSource {
            encrypt_dict,
            file_key: vec![0x31; 15],
            id0: b"0123456789abcdef".to_vec(),
            object_key_alg: ObjectKeyAlg::Aes,
        });

        let error = write_with_settings(&mut pdf, &mut output, &options)
            .expect_err("short public copy-encryption key must be rejected");
        assert!(matches!(
            error,
            flpdf::Error::Unsupported(message)
                if message
                    == "copy-encryption V=4 R=4 file key must be 16 bytes; got 15"
        ));
        assert!(
            output.is_empty(),
            "{qdf:?} copy-encryption validation must precede output emission"
        );
    }
}

#[test]
fn linearized_cleartext_metadata_resolves_direct_and_copied_encryption() {
    let input = nested_string_fixture(INFO_PLAINTEXT);
    let mut params = EncryptParams::v4_aes128(b"user", b"owner");
    params.encrypt_metadata = false;

    let mut direct_pdf = Pdf::open(Cursor::new(input.clone())).expect("open direct input");
    let direct_settings = WriterTestSettings {
        static_id: true,
        static_aes_iv: true,
        encrypt: Some(params.clone()),
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut direct_pdf, &direct_settings)
        .expect("linearized direct encryption should support cleartext metadata");

    let donor_bytes = encrypt_to_bytes(&input, params);
    let mut donor = open_encrypted(&donor_bytes, b"user");
    let copy_source = donor
        .writer_copy_encryption_source()
        .expect("authenticated donor should expose copy-encryption parameters")
        .expect("donor should be encrypted");
    let mut copied_pdf = Pdf::open(Cursor::new(input)).expect("open copied input");
    let copied_settings = WriterTestSettings {
        static_id: true,
        static_aes_iv: true,
        copy_encryption: Some(copy_source),
        ..WriterTestSettings::default()
    };
    write_linearized_with_settings(&mut copied_pdf, &copied_settings)
        .expect("linearized copied encryption should support cleartext metadata");
}

/// RC4-128 keeps qpdf's normal content heuristic after encryption. Cover every
/// possible one-byte plaintext under one deterministic object key: RC4's XOR
/// mapping is a permutation, so at least one resulting ciphertext byte is
/// printable and must therefore use literal-string syntax rather than a
/// cipher-wide hexadecimal override.
#[test]
fn rc4_128_printable_ciphertext_uses_literal_string_syntax() {
    let input = rc4_printable_ciphertext_fixture();
    let options = WriterTestSettings {
        static_id: true,
        encrypt: Some(EncryptParams::rc4(
            EncryptMethod::V4Rc4128,
            Vec::new(),
            Vec::new(),
        )),
        ..WriterTestSettings::default()
    };

    let bytes = rewrite_fixture(&input, &options);
    let repeated = rewrite_fixture(&input, &options);
    assert_eq!(bytes, repeated, "RC4 + static ID must be deterministic");

    let literal_candidate = (0u8..=u8::MAX).find(|byte| {
        let needle = format!("/C{byte:02x} (");
        bytes
            .windows(needle.len())
            .any(|part| part == needle.as_bytes())
    });
    assert!(
        literal_candidate.is_some(),
        "at least one printable RC4 ciphertext must use literal syntax"
    );

    let reopened = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            password: Vec::new(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("reopen RC4-128 output");
    assert!(reopened.is_encrypted());
    assert!(reopened.user_password_matched());
}

/// Resolve the JavaScript stream (catalog `/OpenAction` -> action `/JS`) in the
/// re-opened encrypted `pdf` and return its `/Length` dictionary entry.
fn js_stream_length(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> ObjectHandle {
    let root = pdf.root_ref().expect("/Root");
    let catalog = pdf.resolve_canonical_object(root).expect("catalog");
    let open_action = catalog
        .try_get_key(b"/OpenAction")
        .expect("read /OpenAction");
    pdf.resolve(&open_action).expect("resolve /OpenAction");
    let action = open_action;
    let js_ref = action.try_get_key(b"/JS").expect("read /JS");
    pdf.resolve(&js_ref).expect("resolve /JS");
    let stream_dict = js_ref.as_stream_dict().expect("/JS is a stream");
    stream_dict
        .try_get_key(b"/Length")
        .expect("/Length present")
}

/// `--stream-data=preserve` + `--encrypt`: the
/// orphan-/Length-holder drop must still fire under encryption. Before this,
/// the preserve gate (`effective_stream_policy().is_some()`) was false
/// for preserve, so the holder survived; the old in-place stream encryption
/// path then direct-ized `/Length` anyway, leaving a stale orphan emitted as a
/// real object.
/// The gate now keys on `!options.qdf`, so the holder is dropped and `/Length` is
/// direct.
///
/// Asserted via flpdf's own reader (no garbage collection) on the live encrypted
/// output — NOT via `qpdf --decrypt`, which would GC the stale holder and mask
/// the bug. Pre-fix this output reopens with 8 live objects (6 logical + stale
/// holder + /Encrypt); post-fix with 7 (6 logical + /Encrypt, holder dropped).
/// Structural assertion only — AES IVs are random, so byte-identity is not
/// available for AES.
#[test]
fn v4_aes128_preserve_drops_orphan_length_holder() {
    let input = fixture("tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
    let mut pdf = Pdf::open(Cursor::new(input)).expect("open plaintext input");
    let mut out = Vec::new();
    let options = WriterTestSettings {
        stream_data: Some(StreamDataMode::Preserve),
        encrypt: Some(EncryptParams::v4_aes128(
            b"user-pw".to_vec(),
            b"owner-pw".to_vec(),
        )),
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut out, &options).expect("encrypted preserve write");

    let mut pdf = open_encrypted(&out, b"user-pw");
    // 6 logical objects (Catalog, Pages, Page, content stream, Action, JS stream)
    // with the orphan holder dropped, plus the /Encrypt dictionary object = 7.
    // (Pre-fix: 8, the stale holder still live.)
    assert_eq!(
        pdf.live_object_refs().len(),
        7,
        "preserve + encrypt must drop the orphaned indirect /Length holder \
         (6 logical objects + /Encrypt = 7; the stale holder is gone)"
    );
    assert!(
        js_stream_length(&mut pdf).as_integer().is_some(),
        "preserve + encrypt must direct-ize the JS stream's /Length"
    );
}

#[test]
fn v1_r2_writer_uses_the_separate_r2_permission_bits() {
    let mut params =
        EncryptParams::rc4(EncryptMethod::V1Rc440, b"user".to_vec(), b"owner".to_vec());
    params.r2_permissions = R2PermissionsConfig {
        print: false,
        modify: true,
        extract: true,
        annotate: true,
    };

    let bytes = encrypt_to_bytes(&fixture("tests/fixtures/compat/one-page.pdf"), params);
    let pdf = open_encrypted(&bytes, b"user");
    assert_eq!(pdf.encryption_version(), Some(1));
    assert_eq!(pdf.encryption_revision(), Some(2));
    assert_eq!(
        pdf.permissions().expect("R=2 output is encrypted").raw(),
        -8
    );
}

mod common;
use common::PdfCanonicalTestExt;
#[allow(unused_imports)]
use common::{
    write_default, write_linearized_with_settings, write_with_settings, WriterTestSettings,
};
