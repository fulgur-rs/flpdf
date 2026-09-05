use aes::cipher::{BlockCipherEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use cbc::cipher::{block_padding::Pkcs7, BlockModeEncrypt, KeyIvInit};
use cbc::Encryptor;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::{
    load_xref_and_trailer, DecodeLevel, EncryptMethod, EncryptParams, EncryptedError, Error,
    ObjectHandle, ObjectRef, Pdf, PdfOpenOptions, XrefEntry,
};
use md5::{Digest, Md5};
use std::fs::File;
use std::io::Write;
use std::io::{BufReader, Read, Seek};
use std::process::Command;

mod common;
use common::{write_with_settings, WriterTestSettings};

#[path = "common/ascii85.rs"]
mod ascii85;

fn resolved_handle<R>(pdf: &mut Pdf<R>, object_ref: ObjectRef) -> ObjectHandle
where
    R: Read + Seek,
{
    let handle = pdf.get_object_handle(object_ref);
    pdf.resolve(&handle).expect("resolve object");
    handle
}

fn resolved_key<R>(pdf: &mut Pdf<R>, object: &ObjectHandle, key: &[u8]) -> ObjectHandle
where
    R: Read + Seek,
{
    let value = object.get_key(key);
    pdf.resolve(&value).expect("resolve dictionary value");
    value
}

fn stream_dict(stream: &ObjectHandle) -> ObjectHandle {
    stream.as_stream_dict().expect("expected stream dictionary")
}

fn stream_data(stream: &ObjectHandle) -> Vec<u8> {
    stream
        .get_raw_stream_data()
        .expect("expected raw stream data")
        .as_ref()
        .clone()
}

fn assert_crypt_filter_shape<R>(
    pdf: &mut Pdf<R>,
    stream: &ObjectHandle,
    crypt_after: bool,
    other_filter: &[u8],
    crypt_name: &[u8],
) where
    R: Read + Seek,
{
    let dict = stream_dict(stream);
    let filters = resolved_key(pdf, &dict, b"/Filter")
        .as_array()
        .expect("filter array");
    assert_eq!(filters.len(), 2);
    let expected = if crypt_after {
        [other_filter, b"Crypt".as_slice()]
    } else {
        [b"Crypt".as_slice(), other_filter]
    };
    assert_eq!(filters[0].as_name().as_deref(), Some(expected[0]));
    assert_eq!(filters[1].as_name().as_deref(), Some(expected[1]));

    let decode_parms = resolved_key(pdf, &dict, b"/DecodeParms")
        .as_array()
        .expect("decode parms array");
    assert_eq!(decode_parms.len(), 2);
    let crypt_index = usize::from(crypt_after);
    let other_index = usize::from(!crypt_after);
    assert!(resolved_value(pdf, decode_parms[other_index].clone()).is_null());
    let crypt_params = resolved_value(pdf, decode_parms[crypt_index].clone());
    let name = resolved_key(pdf, &crypt_params, b"/Name");
    assert_eq!(name.as_name().as_deref(), Some(crypt_name));
}

fn resolved_value<R>(pdf: &mut Pdf<R>, value: ObjectHandle) -> ObjectHandle
where
    R: Read + Seek,
{
    pdf.resolve(&value).expect("resolve value");
    value
}

#[test]
fn opens_pdf_without_resolving_all_objects() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.resolved_count(), 0);
    assert_eq!(
        pdf.trailer().try_get_key(b"/Root").unwrap().object_ref(),
        Some(ObjectRef::new(1, 0))
    );
}

#[test]
fn open_options_default_enables_qpdf_recovery() {
    assert!(PdfOpenOptions::default().repair);
}

#[test]
fn open_with_options_uses_empty_password_by_default() {
    let file = File::open("../../tests/fixtures/compat/encrypted-r4-three-page.pdf").unwrap();
    let pdf = Pdf::open_with_options(BufReader::new(file), PdfOpenOptions::default()).unwrap();

    assert_eq!(pdf.version(), "1.6");
}

#[test]
fn open_with_options_rejects_wrong_password() {
    let file = File::open("../../tests/fixtures/compat/encrypted-r4-three-page.pdf").unwrap();
    let options = PdfOpenOptions {
        password: b"wrong".to_vec(),
        ..PdfOpenOptions::default()
    };
    let err = match Pdf::open_with_options(BufReader::new(file), options) {
        Ok(_) => panic!("wrong password should be rejected"),
        Err(err) => err,
    };

    assert!(matches!(err, Error::Encrypted(EncryptedError::BadPassword)));
}

#[test]
fn open_accepts_rc4_encryption_by_default() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v1-rc4-40-r2.pdf")),
        PdfOpenOptions {
            password: b"user-v1".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf accepts authenticated RC4 reads without a write opt-in");

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_with_options_accepts_owner_password() {
    let bytes = committed_encrypted_fixture("v1-rc4-40-r2.pdf");
    let options = PdfOpenOptions {
        password: b"owner-v1".to_vec(),
        ..PdfOpenOptions::default()
    };

    let pdf = Pdf::open_with_options(std::io::Cursor::new(bytes), options).unwrap();

    assert_eq!(pdf.version(), "1.7");
}

#[test]
fn resolves_indirect_object_on_access() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let root = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert_eq!(
        root.get_key(b"/Pages").object_ref(),
        Some(ObjectRef::new(2, 0))
    );
    assert!(root.is_resolved());
}

/// The canonical qpdf route reads a recovered object through the live parser,
/// just as `QPDF::readObjectAtOffset` calls `readObject` after validating the
/// object header (`libqpdf/QPDF.cc:1542-1645`). It does not impose the legacy
/// reader's bounded-window fallback cap. Keep this fixture modest: the
/// security regression for the removed legacy window belongs to the xref-repair
/// owner, while this test pins canonical recovery and warning-tolerant parsing.
#[test]
fn resolving_eof_running_objects_uses_live_qpdf_recovery() {
    const N: u32 = 256;
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let refs: String = (3..3 + N).map(|i| format!("{i} 0 R ")).collect();
    bytes.extend_from_slice(
        format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Junk [{refs}] >>\nendobj\n").as_bytes(),
    );
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    for i in 3..3 + N {
        bytes.extend_from_slice(format!("\n{i} 0 obj (").as_bytes());
    }
    // Balanced closing parens near EOF so each object's literal string is a
    // *valid* (terminated) object: resolution succeeds, so a fallback would run
    // to EOF for every reference were it not capped.
    bytes.push(b'\n');
    bytes.extend(std::iter::repeat_n(b')', N as usize + 2));
    let start_xref = bytes.len();
    bytes.extend_from_slice(b"\nzref\n0 1\n0000000000 65535 f \n");
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open_with_repair(std::io::Cursor::new(bytes)).unwrap();
    // Resolve every recovered object through the canonical qpdf-shaped route.
    let mut resolved = 0usize;
    for i in 3..3 + N {
        let handle = pdf.get_object_handle(ObjectRef::new(i, 0));
        if pdf.resolve(&handle).is_ok() {
            resolved += 1;
        }
    }
    assert_eq!(
        resolved, N as usize,
        "qpdf recovery resolves every recovered object"
    );
}

#[test]
fn resolve_returns_same_cached_handle() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let first = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert_eq!(
        first.get_key(b"/Pages").object_ref(),
        Some(ObjectRef::new(2, 0))
    );

    let second = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert!(first.is_same_object_as(&second));
}

#[test]
fn resolve_resolves_missing_reference_to_null() {
    let file = File::open(minimal_fixture_path()).unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(999, 0));

    assert!(object.is_null());
}

fn minimal_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf")
}

fn committed_encrypted_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted")
        .join(name);
    std::fs::read(&path)
        .unwrap_or_else(|err| panic!("read encrypted fixture {}: {err}", path.display()))
}

#[test]
fn resolve_resolves_compressed_entry_from_xref_stream() {
    let mut pdf = Pdf::open(std::io::Cursor::new(compressed_entry_pdf())).unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(2, 0));

    assert_eq!(object.as_integer(), Some(42));
}

#[test]
fn resolve_returns_null_for_mismatched_indirect_object() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let bad_offset = bytes.len();
    bytes.extend_from_slice(b"9 0 obj\ntrue\nendobj\n");
    let root_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{bad_offset:010} 00000 n \n{root_offset:010} 00000 n \ntrailer\n<< /Size 3 /Root 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(1, 0));

    assert!(object.is_null());
}

#[test]
fn resolve_returns_null_for_compressed_entry_with_missing_parent_stream() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        compressed_entry_with_missing_parent_pdf(),
    ))
    .unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(2, 0));

    assert!(object.is_null());
}

#[test]
fn resolve_returns_null_for_compressed_entry_with_non_stream_parent() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        compressed_entry_with_non_stream_parent_pdf(),
    ))
    .unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(2, 0));

    assert!(object.is_null());
}

#[test]
fn resolve_returns_null_for_compressed_entry_with_compressed_parent() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        compressed_entry_with_compressed_parent_pdf(),
    ))
    .unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(2, 0));

    assert!(object.is_null());
}

#[test]
fn resolve_returns_null_for_compressed_entry_with_mismatched_parent_ref() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        compressed_entry_with_mismatched_parent_ref_pdf(),
    ))
    .unwrap();

    let object = resolved_handle(&mut pdf, ObjectRef::new(2, 0));

    assert!(object.is_null());
}

#[test]
fn open_with_options_accepts_r5_by_default() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(5)),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf accepts authenticated R=5 reads without a write opt-in");

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_accepts_v4_rc4_crypt_filters_by_default() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v4-rc4-128-r4.pdf")),
        PdfOpenOptions {
            password: b"user-v4-rc4".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf accepts authenticated V=4 RC4 reads without a write opt-in");

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_with_options_accepts_v4_rc4_crypt_filters() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v4-rc4-128-r4.pdf")),
        PdfOpenOptions {
            password: b"user-v4-rc4".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_with_options_marks_rc4_as_weak_crypto() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v1-rc4-40-r2.pdf")),
        PdfOpenOptions {
            password: b"user-v1".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_with_options_accepts_r5_and_r6_by_default() {
    for (revision, password) in [
        (5, b"userpass".as_slice()),
        (5, b"ownerpass"),
        (6, b"userpass"),
        (6, b"ownerpass"),
    ] {
        let options = PdfOpenOptions {
            password: password.to_vec(),
            ..PdfOpenOptions::default()
        };

        let pdf = Pdf::open_with_options(
            std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(revision)),
            options,
        )
        .unwrap();

        assert_eq!(pdf.version(), "2.0");
    }
}

// ---------------------------------------------------------------------------
// authentication error parity for V=5 (and the V<5/V=4 path).
//
// Error behavior in reader.rs `authenticate_if_encrypted`:
//   - Password authentication runs FIRST. Both user+owner failing => BadPassword.
//   - On the V=5 R=5/R=6 auth path, a wrong-length /U or /O entry maps to
//     BadPassword (not Malformed), matching qpdf's "invalid password".
//
// `encryption.test` parity: subtest 7 (RC4 + wrong password without
// --allow-weak-crypto => BadPassword) and subtest 8 (V=5 with a /U shorter
// than 48 bytes => BadPassword) are reproduced fixture-by-fixture below,
// together with the four regression fences (a)-(d).
// ---------------------------------------------------------------------------

/// scenario 7: an RC4 (V=1/R=2, weak) file opened with the WRONG password
/// must report BadPassword. qpdf reports "invalid password" here.
#[test]
fn scenario7_rc4_wrong_password_without_weak_opt_in_is_bad_password() {
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v1-rc4-40-r2.pdf")),
        PdfOpenOptions {
            password: b"wrong".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("wrong password should be rejected"),
        Err(err) => err,
    };

    assert!(
        matches!(err, Error::Encrypted(EncryptedError::BadPassword)),
        "expected BadPassword from authentication, got {err:?}"
    );
}

/// scenario 8: a V=5 file whose /U entry is shorter than 48 bytes, opened on
/// the authentication path, must report BadPassword, not Malformed. qpdf
/// reports "invalid password" here.
#[test]
fn scenario8_v5_short_u_entry_is_bad_password() {
    let bytes = v5_pdf_with_truncated_u_entry();
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("a V=5 file with a short /U entry must not open"),
        Err(err) => err,
    };

    assert!(
        matches!(err, Error::Encrypted(EncryptedError::BadPassword)),
        "expected BadPassword for a wrong-length /U on the auth path, got {err:?}"
    );
}

/// Fence: a wrong-length /UE entry (not /U or /O) stays Malformed. The
/// reclassification is intentionally scoped to /U and /O only.
#[test]
fn fence_v5_short_ue_entry_stays_malformed() {
    let bytes = v5_pdf_with_truncated_ue_entry();
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("a V=5 file with a short /UE entry must not open"),
        Err(err) => err,
    };

    assert!(
        matches!(err, Error::Encrypted(EncryptedError::Malformed { .. })),
        "/UE length errors must remain Malformed (not reclassified), got {err:?}"
    );
}

/// A correct password against a weak (RC4) file is accepted without a
/// write-only weak-crypto opt-in, matching qpdf.
#[test]
fn fence_a_correct_password_weak_opens_without_write_opt_in() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v1-rc4-40-r2.pdf")),
        PdfOpenOptions {
            password: b"user-v1".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("correct password + weak encryption must open");

    assert!(pdf.uses_weak_crypto());
}

/// Regression fence (c): a well-formed V=5 file with a WRONG password still
/// returns BadPassword (unchanged).
#[test]
fn fence_c_v5_wellformed_wrong_password_is_bad_password() {
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(5)),
        PdfOpenOptions {
            password: b"definitely-wrong".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("wrong password against a well-formed V=5 file must fail"),
        Err(err) => err,
    };

    assert!(
        matches!(err, Error::Encrypted(EncryptedError::BadPassword)),
        "well-formed V=5 + wrong password must stay BadPassword, got {err:?}"
    );
}

/// Regression fence (d): a non-weak (AES, R=6) file with a WRONG password
/// still returns BadPassword (unchanged).
#[test]
fn fence_d_non_weak_aes_wrong_password_is_bad_password() {
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(6)),
        PdfOpenOptions {
            password: b"definitely-wrong".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("wrong password against an AES R=6 file must fail"),
        Err(err) => err,
    };

    assert!(
        matches!(err, Error::Encrypted(EncryptedError::BadPassword)),
        "non-weak AES + wrong password must stay BadPassword, got {err:?}"
    );
}

/// Build a well-formed V=5 R=5 fixture, then binary-edit the `/U <...>` hex
/// literal so the decoded string is 47 bytes (one byte short of the required
/// 48). The crafted file still parses; the short /U is detected on the
/// authentication path.
fn v5_pdf_with_truncated_u_entry() -> Vec<u8> {
    truncate_hex_entry(encrypted_r5_or_r6_minimal_pdf(5), b"/U <")
}

/// As [`v5_pdf_with_truncated_u_entry`] but truncates the `/UE` entry instead,
/// to exercise the fence that /UE length errors stay Malformed.
fn v5_pdf_with_truncated_ue_entry() -> Vec<u8> {
    truncate_hex_entry(encrypted_r5_or_r6_minimal_pdf(5), b"/UE <")
}

/// Drop the last hex byte (two hex chars) of the `<...>` string that follows
/// `marker` in `bytes`, shortening the decoded value by one byte.
fn truncate_hex_entry(bytes: Vec<u8>, marker: &[u8]) -> Vec<u8> {
    let start = find_subslice(&bytes, marker).expect("marker present in fixture") + marker.len();
    let end = start
        + bytes[start..]
            .iter()
            .position(|&b| b == b'>')
            .expect("closing > present");
    assert!(
        (end - start) >= 2 && (end - start).is_multiple_of(2),
        "hex literal must be a non-empty even number of nibbles"
    );
    let mut out = Vec::with_capacity(bytes.len() - 2);
    out.extend_from_slice(&bytes[..end - 2]);
    out.extend_from_slice(&bytes[end..]);
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn permissions_exposes_standard_flags() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(6)),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    let permissions = pdf.permissions().expect("encrypted fixture has /P");

    assert_eq!(permissions.raw(), -3904);
    assert!(!permissions.can_print());
    assert!(!permissions.can_modify());
    assert!(!permissions.can_copy());
    assert!(!permissions.can_annotate());
    assert!(!permissions.can_fill_forms());
    assert!(!permissions.can_extract_for_accessibility());
    assert!(!permissions.can_assemble());
    assert!(!permissions.can_print_high_quality());
}

#[test]
fn r6_perms_mismatch_warns_without_failing_open() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r6_pdf_with_perms(-4, true)),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
        entry.severity == flpdf::Severity::Warning && entry.message.contains("/Perms")
    }));
}

#[test]
fn r6_perms_wrong_length_warns_without_attempting_decryption() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_pdf(6, " /Perms <00>", &[])),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|entry| { entry.message.contains("R=6 /Perms entry is not 16 bytes") }));
}

#[test]
fn resolve_decrypts_encrypted_strings_after_authentication() {
    let bytes = writer_generated_rc4_reader_fixture(false);
    let mut pdf = Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"user-pw".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    let info_ref = pdf
        .trailer()
        .try_get_key(b"/Info")
        .expect("read writer fixture /Info")
        .object_ref()
        .expect("writer fixture has /Info");
    let info = resolved_handle(&mut pdf, info_ref);
    assert_eq!(
        resolved_key(&mut pdf, &info, b"/Title").as_string(),
        Some(b"TopSecretTitle".to_vec())
    );
}

#[test]
fn resolve_decrypts_object_stream_before_filter_decode() {
    let bytes = writer_generated_rc4_reader_fixture(true);
    let mut xref_reader = std::io::Cursor::new(bytes.clone());
    let xref = load_xref_and_trailer(&mut xref_reader).expect("load generated xref stream");
    let info_ref = xref
        .trailer
        .try_get_key(b"/Info")
        .expect("read writer fixture /Info")
        .object_ref()
        .expect("writer fixture has /Info");
    assert!(
        matches!(
            xref.entries.get(&info_ref),
            Some(XrefEntry::Compressed { .. })
        ),
        "/Info must be a compressed object-stream member"
    );

    let mut pdf = Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"user-pw".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    let info = resolved_handle(&mut pdf, info_ref);
    assert_eq!(
        resolved_key(&mut pdf, &info, b"/Value").as_integer(),
        Some(42)
    );
}

#[test]
fn v4_uses_separate_stream_and_string_crypt_filters() {
    let mut pdf = Pdf::open(std::io::Cursor::new(encrypted_v4_mixed_cf_reader_fixture())).unwrap();

    let dictionary = resolved_handle(&mut pdf, ObjectRef::new(3, 0));
    assert_eq!(
        resolved_key(&mut pdf, &dictionary, b"/Secret").as_string(),
        Some(b"plain text".to_vec())
    );

    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_eq!(stream_data(&stream), b"stream plain");
}

#[test]
fn v4_explicit_crypt_filter_decrypts_before_flate_when_crypt_is_first() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_fixture(false, false),
    ))
    .unwrap();

    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(4, 0))
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_ref(),
        b"explicit crypt stream"
    );
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, false, b"FlateDecode", b"StdCF");
}

#[test]
fn v4_explicit_crypt_filter_decrypts_before_flate_when_crypt_is_last() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_fixture(false, true),
    ))
    .unwrap();

    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(4, 0))
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_ref(),
        b"explicit crypt stream"
    );
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, true, b"FlateDecode", b"StdCF");
}

#[test]
fn canonical_stream_pipeline_decrypts_before_an_ascii_hex_filter() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_ascii_hex_fixture(),
    ))
    .unwrap();
    let stream = pdf.get_object_handle(ObjectRef::new(4, 0));

    assert_eq!(
        stream
            .get_stream_data(DecodeLevel::Generalized)
            .expect("canonical stream pipeline must decrypt before ASCIIHex")
            .as_slice(),
        b"explicit crypt ASCIIHex stream"
    );
}

#[test]
fn v4_explicit_identity_crypt_filter_is_noop_before_flate() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_fixture(true, false),
    ))
    .unwrap();

    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(4, 0))
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_ref(),
        b"explicit crypt stream"
    );
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, false, b"FlateDecode", b"Identity");
}

/// `/Crypt` in the first `/Filter` slot needs no prefix reconstruction, so the
/// ASCII85 filter that follows it stays fully supported. qpdf decodes this shape
/// too:
///
/// ```text
/// $ qpdf --show-object=4 --filtered-stream-data crypt_then_ascii85.pdf
/// explicit crypt stream
/// ```
#[test]
fn v4_explicit_crypt_filter_before_ascii85_decrypts_at_filter_slot() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_ascii85_crypt_filter_fixture(false),
    ))
    .unwrap();

    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(4, 0))
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_ref(),
        b"explicit crypt stream"
    );
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, false, b"ASCII85Decode", b"StdCF");
}

/// qpdf attaches the decryption pipeline to the raw stream bytes before any
/// `/Filter` stage, regardless of the `/Crypt` slot position
/// (`libqpdf/QPDF.cc:2489-2492`, `libqpdf/QPDF_encryption.cc:1065-1090`).
/// Therefore the ASCII85 representation is encrypted as a whole and remains
/// encoded after the compatibility path removes `/Crypt`.
#[test]
fn v4_explicit_crypt_filter_after_ascii85_decrypts_before_filter() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_ascii85_crypt_filter_fixture(true),
    ))
    .unwrap();

    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(4, 0))
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_ref(),
        b"explicit crypt stream"
    );
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, true, b"ASCII85Decode", b"StdCF");
}

/// qpdf prepends decryption to the raw stream and retries without filtering
/// when the downstream ASCII85 stage fails (`QPDF.cc:2489-2492`). The raw
/// shape here is ASCII85(encrypt(plaintext)), so decrypting it as an AES stream
/// is not a valid transform; qpdf still opens the document with a warning and
/// keeps the remaining `/ASCII85Decode` stage and raw bytes.
#[test]
fn v4_explicit_crypt_filter_failure_retries_unfiltered_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }
    let fixture = encrypted_v4_ascii85_crypt_filter_fixture_with_order(true, true);
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("wrong-order.pdf");
    let qpdf_output = temp.path().join("qpdf-decrypt.pdf");
    std::fs::write(&input, &fixture).unwrap();

    let qpdf = Command::new("qpdf")
        .args([
            "--warning-exit-0",
            "--password=",
            "--decrypt",
            "--static-id",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(qpdf.status.success(), "qpdf decrypt should write output");
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("error decoding stream data"),
        "qpdf must report the downstream filter warning: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let qpdf_qdf = temp.path().join("qpdf-decrypt.qdf.pdf");
    let qpdf_qdf_result = Command::new("qpdf")
        .args([
            "--warning-exit-0",
            "--password=",
            "--qdf",
            "--object-streams=disable",
            "--no-original-object-ids",
        ])
        .arg(&qpdf_output)
        .arg(&qpdf_qdf)
        .output()
        .expect("qpdf QDF inspection should spawn");
    assert!(qpdf_qdf_result.status.success());
    let qpdf_qdf = String::from_utf8_lossy(&std::fs::read(qpdf_qdf).unwrap()).into_owned();
    assert!(qpdf_qdf.contains("/ASCII85Decode"));
    assert!(!qpdf_qdf.contains("/Crypt"));

    // qpdf's own retained bytes, read straight back out of the file it just
    // wrote. This is the oracle: `Pl_AES_PDF` zero-pads the 46-byte tail that
    // follows the vector (`libqpdf/Pl_AES_PDF.cc:107-118`) and leaves a
    // trailer that is not valid padding in place (`:183-196`), so decryption
    // yields 48 bytes rather than failing.
    let qpdf_retained = qpdf_retained_raw_stream(&qpdf_output, "/ASCII85Decode");

    let mut pdf = Pdf::open(std::io::Cursor::new(fixture)).unwrap();
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, true, b"ASCII85Decode", b"StdCF");
    assert_eq!(
        stream_data(&stream),
        qpdf_retained,
        "retained stream bytes must match qpdf's byte for byte"
    );
}

/// The raw (still-encoded) bytes qpdf retained for the stream carrying
/// `filter_name` in `pdf_path`. qpdf renumbers objects when it writes, so the
/// stream is located by its dictionary rather than by an assumed number.
fn qpdf_retained_raw_stream(pdf_path: &std::path::Path, filter_name: &str) -> Vec<u8> {
    for object_number in 1..=10u32 {
        let shown = Command::new("qpdf")
            .arg("--warning-exit-0")
            .arg(format!("--show-object={object_number}"))
            .arg(pdf_path)
            .output()
            .expect("qpdf --show-object should spawn");
        if !shown.status.success() || !String::from_utf8_lossy(&shown.stdout).contains(filter_name)
        {
            continue;
        }
        let raw = Command::new("qpdf")
            .arg("--warning-exit-0")
            .arg(format!("--show-object={object_number}"))
            .arg("--raw-stream-data")
            .arg(pdf_path)
            .output()
            .expect("qpdf --raw-stream-data should spawn");
        assert!(
            raw.status.success(),
            "qpdf --raw-stream-data failed: {}",
            String::from_utf8_lossy(&raw.stderr)
        );
        return raw.stdout;
    }
    panic!("qpdf output has no stream carrying {filter_name}");
}

/// The same shape as the ASCII85 case with `/FlateDecode` in the filter slot:
/// the raw bytes are Flate(encrypt(plaintext)), so decrypting them as an AES
/// stream cannot produce valid Flate input. qpdf still keeps the decrypted
/// bytes and the remaining `/FlateDecode` stage.
#[test]
fn v4_explicit_crypt_filter_flate_failure_retries_unfiltered() {
    if !qpdf_available() {
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }
    let fixture = encrypted_v4_explicit_crypt_filter_fixture_with_order(false, true, true, true);
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("flate-wrong-order.pdf");
    let qpdf_output = temp.path().join("qpdf-decrypt.pdf");
    std::fs::write(&input, &fixture).unwrap();

    let qpdf = Command::new("qpdf")
        .args([
            "--warning-exit-0",
            "--password=",
            "--decrypt",
            "--static-id",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(qpdf.status.success(), "qpdf decrypt should write output");
    let qpdf_retained = qpdf_retained_raw_stream(&qpdf_output, "/FlateDecode");

    let mut pdf = Pdf::open(std::io::Cursor::new(fixture)).unwrap();
    let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
    assert_crypt_filter_shape(&mut pdf, &stream, true, b"FlateDecode", b"StdCF");
    assert_eq!(
        stream_data(&stream),
        qpdf_retained,
        "retained stream bytes must match qpdf's byte for byte"
    );
}

#[test]
fn r5_and_r6_identity_crypt_filters_leave_streams_and_strings_plaintext() {
    for revision in [5, 6] {
        let mut pdf = Pdf::open_with_options(
            std::io::Cursor::new(encrypted_r5_or_r6_identity_cf_minimal_pdf(revision)),
            PdfOpenOptions {
                password: b"userpass".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();

        let dict = resolved_handle(&mut pdf, ObjectRef::new(3, 0));
        assert_eq!(
            resolved_key(&mut pdf, &dict, b"/Secret").as_string(),
            Some(b"plain text".to_vec())
        );

        let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
        assert_eq!(stream_data(&stream), b"stream plain");
    }
}

#[test]
fn encryption_inspection_retains_parameters_after_bad_password_without_key() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf");
    let bytes = std::fs::read(path).expect("encrypted fixture");
    let pdf = Pdf::open_for_encryption_inspection(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"wrong".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("inspection must retain qpdf's parsed state after BadPassword");

    assert!(pdf.is_encrypted());
    assert_eq!(pdf.encryption_revision(), Some(3));
    assert_eq!(pdf.encryption_version(), Some(2));
    assert!(!pdf.user_password_matched());
    assert!(!pdf.owner_password_matched());
    assert!(pdf
        .trimmed_user_password()
        .is_some_and(|password| password.is_empty()));
    assert!(pdf.encryption_file_key().is_none());
}

#[test]
fn encryption_parameters_are_exposed_as_qpdf_individual_accessors() {
    let bytes = committed_encrypted_fixture("v2-rc4-128-r3.pdf");
    let pdf = Pdf::open_for_encryption_inspection(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"wrong".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("inspection must retain qpdf's parsed state after BadPassword");

    assert_eq!(pdf.encryption_version(), Some(2));
    assert_eq!(pdf.encryption_revision(), Some(3));
    assert_eq!(pdf.encryption_length_bits(), Some(128));
    assert_eq!(pdf.trimmed_user_password(), Some(Vec::new()));
    assert_eq!(pdf.encryption_methods(), Some(("none", "none", "none")));
}

/// `uses_weak_crypto()` must still classify a document as weak (RC4) after a
/// `BadPassword` open via `open_for_encryption_inspection`, which never
/// populates the authenticated `EncryptionState` `uses_weak_crypto` would
/// otherwise read -- it must fall back to the pre-authentication
/// `EncryptionInspectionState`, which computes the same RC4/R=5
/// classification from the parsed (password-independent) `/Encrypt` fields.
#[test]
fn uses_weak_crypto_reports_rc4_after_bad_password_inspection_open() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf");
    let bytes = std::fs::read(path).expect("encrypted fixture");
    let pdf = Pdf::open_for_encryption_inspection(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"wrong".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("inspection must retain qpdf's parsed state after BadPassword");

    assert!(
        pdf.uses_weak_crypto(),
        "RC4 must still be reported as weak crypto after a failed password, \
         from the retained inspection state alone"
    );
}

/// A `/V 5` document whose `/CF` names `/CFM /V2` is an RC4 crypt filter on a
/// modern revision. qpdf accepts it and reports RC4 rather than refusing the
/// document:
///
/// ```text
/// $ qpdf --static-id --encrypt --user-password=u --owner-password=o --bits=256 -- in.pdf r6.pdf
/// $ # substitute /CFM /AESV3 with /CFM /V2 in place, then:
/// $ qpdf --password=u --show-encryption r6_cfm_v2.pdf
/// R = 6
/// ...
/// stream encryption method: RC4
/// string encryption method: RC4
/// file encryption method: RC4
/// $ echo $?
/// 0
/// ```
///
/// flpdf used to reject the same document with `UnsupportedHandler`.
#[test]
fn r5_and_r6_accept_an_rc4_crypt_filter_method() {
    for revision in [5, 6] {
        let pdf = Pdf::open_with_options(
            std::io::Cursor::new(encrypted_r5_or_r6_unsupported_cf_minimal_pdf(revision)),
            PdfOpenOptions {
                password: b"userpass".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .unwrap_or_else(|err| panic!("qpdf opens this document; R={revision} got {err:?}"));

        assert_eq!(pdf.encryption_revision(), Some(revision));
        // `/StmF /StdCF` resolves through `/CF` to the RC4 method; `/StrF` is
        // the built-in `/Identity`, i.e. qpdf's `e_none`.
        let (stream_method, string_method, file_method) =
            pdf.encryption_methods().expect("document is encrypted");
        assert_eq!(stream_method, "RC4", "R={revision}");
        assert_eq!(string_method, "none", "R={revision}");
        // No `/EFF`, so qpdf mirrors the stream method into the file method.
        assert_eq!(file_method, "RC4", "R={revision}");
        // An R=6 document actively using an RC4 crypt filter is weak crypto
        // too, not just R=5: `uses_weak_crypto()` must look at the effective
        // crypt-filter methods, not only the revision number.
        assert!(
            pdf.uses_weak_crypto(),
            "R={revision} with an RC4 crypt filter must report weak crypto"
        );
    }
}

#[test]
fn v4_encrypt_metadata_false_leaves_metadata_stream_plaintext() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_plaintext_metadata_stream_fixture(),
    ))
    .unwrap();

    let stream = resolved_handle(&mut pdf, ObjectRef::new(3, 0));

    assert_eq!(stream_data(&stream), b"<xmpmeta>plain</xmpmeta>");
}

fn assert_encrypted_plaintext_stream_rewrite_restores_recovered_eol(
    bytes: Vec<u8>,
    catalog_key: &str,
    expected_payload: &[u8],
    expect_unfiltered: bool,
) {
    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).expect("encrypted source");
    let settings = WriterTestSettings {
        object_streams: flpdf::ObjectStreamMode::Disable,
        stream_data: Some(flpdf::StreamDataMode::Preserve),
        compress_streams: flpdf::CompressStreams::No,
        static_id: true,
        preserve_encryption: false,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings).expect("plaintext rewrite");

    let mut rewritten = Pdf::open(std::io::Cursor::new(output)).expect("rewritten output");
    let root = rewritten.root_ref().expect("root");
    let catalog = resolved_handle(&mut rewritten, root);
    let catalog_key = format!("/{catalog_key}");
    let stream_ref = resolved_key(&mut rewritten, &catalog, catalog_key.as_bytes())
        .object_ref()
        .expect("stream reference");
    let stream = resolved_handle(&mut rewritten, stream_ref);
    assert_eq!(
        stream_data(&stream),
        expected_payload,
        "rewritten payload must match qpdf's recovered-framing contract"
    );
    if expect_unfiltered {
        let dict = stream_dict(&stream);
        assert!(!dict.has_key(b"/Filter"));
        assert!(!dict.has_key(b"/DecodeParms"));
        assert_eq!(
            resolved_key(&mut rewritten, &dict, b"/Length").as_integer(),
            Some(expected_payload.len() as i64)
        );
    }
}

#[test]
fn encrypt_metadata_false_rewrite_restores_plaintext_recovered_eol() {
    assert_encrypted_plaintext_stream_rewrite_restores_recovered_eol(
        encrypted_v4_plaintext_metadata_recovered_eol_fixture(),
        "Metadata",
        b"A\n",
        false,
    );
}

#[test]
fn document_identity_stream_filter_rewrite_restores_plaintext_recovered_eol() {
    assert_encrypted_plaintext_stream_rewrite_restores_recovered_eol(
        encrypted_v4_identity_recovered_eol_fixture(false),
        "Data",
        b"A\n",
        false,
    );
}

#[test]
fn single_explicit_crypt_filter_without_type_falls_back_to_document_filter() {
    assert_encrypted_plaintext_stream_rewrite_restores_recovered_eol(
        encrypted_v4_identity_recovered_eol_fixture(true),
        "Data",
        b"",
        true,
    );
}

fn assert_explicit_identity_filter_chain_does_not_append_decoded_eol(crypt_after_flate: bool) {
    let bytes =
        encrypted_v4_explicit_crypt_filter_fixture_with_length(true, crypt_after_flate, false);
    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).expect("encrypted source");
    let settings = WriterTestSettings {
        object_streams: flpdf::ObjectStreamMode::Disable,
        stream_data: Some(flpdf::StreamDataMode::Preserve),
        compress_streams: flpdf::CompressStreams::No,
        static_id: true,
        preserve_encryption: false,
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings).expect("plaintext rewrite");

    // qpdf 11.9.0 removes only the explicit /Crypt slot. Even though /Length
    // was invalid and the source payload came from the endstream fallback, the
    // remaining one-element Flate chain and its raw encoded bytes survive.
    let stream_marker = find_subslice(&output, b"\nstream\n").expect("output stream marker");
    let dict_start = output[..stream_marker]
        .windows(2)
        .rposition(|window| window == b"<<")
        .expect("output stream dictionary");
    let raw_dict = ObjectHandle::parse(&output[dict_start..stream_marker])
        .expect("parse raw stream dictionary");
    let raw_filter = raw_dict.get_key(b"/Filter");
    let raw_filter_items = raw_filter.as_array().expect("filter array");
    assert_eq!(raw_filter_items.len(), 1);
    assert_eq!(
        raw_filter_items[0].as_name().as_deref(),
        Some(b"FlateDecode".as_slice()),
        "qpdf preserves the remaining filter as a one-element array"
    );
    let raw_decode_parms = raw_dict.get_key(b"/DecodeParms");
    let raw_decode_items = raw_decode_parms.as_array().expect("decode parms array");
    assert_eq!(raw_decode_items.len(), 1);
    assert!(
        raw_decode_items[0].is_null(),
        "qpdf removes only the DecodeParms slot paired with /Crypt"
    );
    let expected_raw = {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"explicit crypt stream").unwrap();
        let mut raw = encoder.finish().unwrap();
        raw.push(b'\n');
        raw
    };
    assert_eq!(
        raw_dict.get_key(b"/Length").as_integer(),
        Some(expected_raw.len() as i64),
        "qpdf rewrites /Length to the preserved Flate representation"
    );
    let raw_start = stream_marker + b"\nstream\n".len();
    assert_eq!(
        &output[raw_start..raw_start + expected_raw.len()],
        expected_raw,
        "qpdf preserves the exact recovered Flate representation after removing Identity /Crypt"
    );

    let mut rewritten = Pdf::open(std::io::Cursor::new(output)).expect("rewritten output");
    let root = rewritten.root_ref().expect("root");
    let catalog = resolved_handle(&mut rewritten, root);
    let stream_ref = resolved_key(&mut rewritten, &catalog, b"/Data")
        .object_ref()
        .expect("stream reference");
    let stream = resolved_handle(&mut rewritten, stream_ref);
    let decoded = stream
        .get_stream_data(DecodeLevel::Generalized)
        .expect("decode output stream");
    assert_eq!(
        decoded.as_ref(),
        b"explicit crypt stream",
        "the source framing EOL must be consumed before the remaining filter chain is decoded"
    );
}

#[test]
fn explicit_identity_before_flate_consumes_recovered_eol_in_source_representation() {
    assert_explicit_identity_filter_chain_does_not_append_decoded_eol(false);
}

#[test]
fn explicit_identity_after_flate_consumes_recovered_eol_in_source_representation() {
    assert_explicit_identity_filter_chain_does_not_append_decoded_eol(true);
}

#[test]
fn r5_and_r6_reject_malformed_encrypt_metadata() {
    for revision in [5, 6] {
        let err = match Pdf::open_with_options(
            std::io::Cursor::new(encrypted_r5_or_r6_pdf(
                revision,
                " /EncryptMetadata /false",
                &[],
            )),
            PdfOpenOptions {
                password: b"userpass".to_vec(),
                ..PdfOpenOptions::default()
            },
        ) {
            Ok(_) => panic!("malformed /EncryptMetadata should be rejected"),
            Err(err) => err,
        };

        assert!(
            matches!(err, Error::Encrypted(EncryptedError::Malformed { .. })),
            "expected Malformed for R={revision}, got {err:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// user_password_matched / owner_password_matched API parity
//
// Acceptance matrix: for each encryption version, opening with the USER
// password must yield (user=true, owner=false), opening with the OWNER
// password must yield (user=false, owner=true), and a plaintext document
// must yield (user=false, owner=false).
//
// Unauthenticated encrypted PDFs are untestable because `Pdf::open_with_options`
// returns `Err` on auth failure — there is no API that constructs an
// authenticated-but-failed `Pdf` — so that sub-case is skipped with this note.
//
// Encryption versions covered:
//   V=1/R=2  — qpdf-generated committed fixtures (user/owner passwords documented in README)
//   V=4/R=4  — encrypted_v4_aes_known_password_fixture (user="", owner="ownerpass")
//   V=5/R=5  — encrypted_r5_or_r6_minimal_pdf(5) (user="userpass", owner="ownerpass")
//   V=5/R=6  — encrypted_r5_or_r6_minimal_pdf(6) (user="userpass", owner="ownerpass")
// ---------------------------------------------------------------------------

#[test]
fn password_matched_flags_v1_r2_user_password() {
    // V=1/R=2 (40-bit RC4, "V=2" shorthand in the design).
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v1-rc4-40-r2.pdf")),
        PdfOpenOptions {
            password: b"user-v1".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        pdf.user_password_matched(),
        "documented user password should match /U"
    );
    assert!(
        !pdf.owner_password_matched(),
        "empty user password must not match /O"
    );
}

#[test]
fn password_matched_flags_v1_r2_owner_password() {
    // V=1/R=2 (40-bit RC4). Owner password is documented by the fixture README.
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(committed_encrypted_fixture("v1-rc4-40-r2.pdf")),
        PdfOpenOptions {
            password: b"owner-v1".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        !pdf.user_password_matched(),
        "owner password must not match /U"
    );
    assert!(
        pdf.owner_password_matched(),
        "owner password should match /O"
    );
}

#[test]
fn password_matched_flags_v4_r4_user_password() {
    // V=4/R=4 (AES-128). User password is "".
    let pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_aes_known_password_fixture(),
    ))
    .unwrap();

    assert!(
        pdf.user_password_matched(),
        "empty user password should match /U"
    );
    assert!(
        !pdf.owner_password_matched(),
        "empty user password must not match /O"
    );
}

#[test]
fn password_matched_flags_v4_r4_owner_password() {
    // V=4/R=4 (AES-128). Owner password is "ownerpass".
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_v4_aes_known_password_fixture()),
        PdfOpenOptions {
            password: b"ownerpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        !pdf.user_password_matched(),
        "owner password must not match /U"
    );
    assert!(
        pdf.owner_password_matched(),
        "owner password should match /O"
    );
}

#[test]
fn password_matched_flags_v5_r5_user_password() {
    // V=5/R=5 (AES-256, deprecated). User password is "userpass".
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(5)),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        pdf.user_password_matched(),
        "user password should match /U for R=5"
    );
    assert!(
        !pdf.owner_password_matched(),
        "user password must not match /O for R=5"
    );
}

#[test]
fn password_matched_flags_v5_r5_owner_password() {
    // V=5/R=5 (AES-256, deprecated). Owner password is "ownerpass".
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(5)),
        PdfOpenOptions {
            password: b"ownerpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        !pdf.user_password_matched(),
        "owner password must not match /U for R=5"
    );
    assert!(
        pdf.owner_password_matched(),
        "owner password should match /O for R=5"
    );
}

#[test]
fn password_matched_flags_v5_r6_user_password() {
    // V=5/R=6 (AES-256). User password is "userpass".
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(6)),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        pdf.user_password_matched(),
        "user password should match /U for R=6"
    );
    assert!(
        !pdf.owner_password_matched(),
        "user password must not match /O for R=6"
    );
}

#[test]
fn password_matched_flags_v5_r6_owner_password() {
    // V=5/R=6 (AES-256). Owner password is "ownerpass".
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(6)),
        PdfOpenOptions {
            password: b"ownerpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(
        !pdf.user_password_matched(),
        "owner password must not match /U for R=6"
    );
    assert!(
        pdf.owner_password_matched(),
        "owner password should match /O for R=6"
    );
}

#[test]
fn overlength_raw_hex_key_preserves_qpdf_reported_key_bits() {
    let bytes = std::fs::read("../../tests/fixtures/encrypted/v5-aes-256-r6.pdf")
        .expect("read encrypted fixture");
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            password: b"abababababababababababababababababababababababababababababababababababababababab"
                .to_vec(),
            password_is_hex_key: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf accepts an overlength raw key for this inspection fixture");

    assert_eq!(pdf.encryption_length_bits(), Some(320));
}

#[test]
fn password_matched_flags_plaintext_document() {
    // Unencrypted document: both flags must always be false.
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert!(
        !pdf.user_password_matched(),
        "plaintext PDF must have user_password_matched() == false"
    );
    assert!(
        !pdf.owner_password_matched(),
        "plaintext PDF must have owner_password_matched() == false"
    );
}

/// V=4/R=4 AES-128 fixture with a known user password (empty string) and a
/// known owner password ("ownerpass"). The public writer constructs `/O` and
/// `/U` through the production security-handler path.
fn encrypted_v4_aes_known_password_fixture() -> Vec<u8> {
    let input = std::fs::read(minimal_fixture_path()).expect("read minimal fixture");
    let mut pdf = Pdf::open(std::io::Cursor::new(input)).expect("open minimal fixture");
    let mut output = Vec::new();
    let settings = WriterTestSettings {
        encrypt: Some(EncryptParams::v4_aes128(Vec::new(), b"ownerpass".to_vec())),
        ..WriterTestSettings::default()
    };
    write_with_settings(&mut pdf, &mut output, &settings).expect("write V=4 AES fixture");
    output
}

fn encrypted_r5_or_r6_minimal_pdf(revision: i64) -> Vec<u8> {
    encrypted_r5_or_r6_pdf(revision, "", &[])
}

fn encrypted_r5_or_r6_identity_cf_minimal_pdf(revision: i64) -> Vec<u8> {
    encrypted_r5_or_r6_pdf(
        revision,
        " /CF << /StdCF << /CFM /AESV3 /Length 256 >> >> /StmF /Identity /StrF /Identity",
        &[
            b"3 0 obj\n<< /Secret (plain text) >>\nendobj\n".as_slice(),
            b"4 0 obj\n<< /Length 12 >>\nstream\nstream plain\nendstream\nendobj\n".as_slice(),
        ],
    )
}

fn encrypted_r5_or_r6_unsupported_cf_minimal_pdf(revision: i64) -> Vec<u8> {
    encrypted_r5_or_r6_pdf(
        revision,
        " /CF << /StdCF << /CFM /V2 /Length 128 >> >> /StmF /StdCF /StrF /Identity",
        &[],
    )
}

fn encrypted_r6_pdf_with_perms(perms_p: i32, perms_encrypt_metadata: bool) -> Vec<u8> {
    let perms = r6_perms_entry(perms_p, perms_encrypt_metadata);
    encrypted_r5_or_r6_pdf(6, &format!(" /Perms <{}>", hex_string(&perms)), &[])
}

fn r6_perms_entry(p: i32, encrypt_metadata: bool) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[..4].copy_from_slice(&p.to_le_bytes());
    block[4..8].copy_from_slice(&[0xff; 4]);
    block[8] = if encrypt_metadata { b'T' } else { b'F' };
    block[9..12].copy_from_slice(b"adb");
    block[12..16].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);

    let file_key: [u8; 32] = std::array::from_fn(|i| i as u8);
    let cipher = Aes256::new((&file_key).into());
    let mut encrypted = block.into();
    cipher.encrypt_block(&mut encrypted);
    encrypted.into()
}

fn encrypted_r5_or_r6_pdf(revision: i64, encrypt_suffix: &str, extra_objects: &[&[u8]]) -> Vec<u8> {
    let (u, o, ue, oe) = match revision {
        5 => (
            "97e87734dfa9d2a69a7e7326ce3fabd944a3e718602d1bc4171df8a2736c6cbe00112233445566778899aabbccddeeff",
            "d95e9aa87833363eccce3e1ba1161b87fcc36c3a2e144b199ddd543db3ad480a102132435465768798a9bacbdcedfe0f",
            "08030d6f64d3cf8bc22a9ec592a44da03b019659444bbb14111ea6f021b3bdac",
            "f8e5af968015e82307b0f2c725cb2641a22dd792ec33c4b104fd5d685f2bba41",
        ),
        6 => (
            "6ce813242d7505a42af6eb24292ac1fe9c8de1a21f598c5205b39d9e9a5ba7bf00112233445566778899aabbccddeeff",
            "b03bdf6b914364dcdecf182d4cc04bacff9e9a38ea5fd1af31acd59c654495e1102132435465768798a9bacbdcedfe0f",
            "4ca56fc060201d966373508e0d5970b65f7581d8f6ff46ee6a3755b623b8379b",
            "b2ee22084804dbe76635580e7caeb3ba9069d40184ae4ec16eee7aca91d05936",
        ),
        _ => panic!("unsupported revision"),
    };

    let mut bytes = b"%PDF-2.0\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let mut offsets = vec![obj1_offset, obj2_offset];
    for object in extra_objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref_offset = bytes.len();
    let size = offsets.len() + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Encrypt << /Filter /Standard /V 5 /R {revision} /Length 256 /P -3904 /O <{o}> /U <{u}> /OE <{oe}> /UE <{ue}>{encrypt_suffix} >> /ID [<000102030405060708090a0b0c0d0e0f><000102030405060708090a0b0c0d0e0f>] >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

/// Algorithm 5 `/U` known answers for the fixed synthetic V=4 fixtures below.
/// The fixtures use `/O` = `0x42` × 32, `/P` = -3904, an empty user password,
/// and `/ID[0]` = `000102...0f`; only `/EncryptMetadata` varies.
fn fixed_r4_u_entry(encrypt_metadata: bool) -> Vec<u8> {
    let hex = if encrypt_metadata {
        "6f5935609a5209f12cc39a027555ab0a00000000000000000000000000000000"
    } else {
        "5a8642e22c26e8ad7f09b3da4a7ca48f00000000000000000000000000000000"
    };
    decode_hex_fixture(hex)
}

fn encrypted_v4_mixed_cf_reader_fixture() -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = fixed_r4_u_entry(true);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");

    let string_key = aes128_object_key(&per_object_aes_key(&file_key, 3, 0));
    let encrypted_secret = aes128_cbc_encrypt_with_iv(&string_key, &[0x11; 16], b"plain text");
    let obj3_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Secret <{}> >>\nendobj\n",
            hex_string(&encrypted_secret)
        )
        .as_bytes(),
    );

    let obj4_offset = bytes.len();
    bytes
        .extend_from_slice(b"4 0 obj\n<< /Length 12 >>\nstream\nstream plain\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n{obj3_offset:010} 00000 n \n{obj4_offset:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESV2 /Length 128 >> >> /StmF /Identity /StrF /StdCF >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

/// qpdf requires `/V` and `/R` together, before any password work, and throws
/// `damagedPDF` when either is missing (`libqpdf/QPDF_encryption.cc:770-777`).
/// Observed by blanking `/V 5` in place in a `qpdf --encrypt --bits=256`
/// output:
///
/// ```text
/// $ qpdf --password=u --show-encryption no_v.pdf
/// qpdf: no_v.pdf (encryption dictionary, offset 1033): some encryption
///   dictionary parameters are missing or the wrong type
/// $ echo $?
/// 2
/// ```
///
/// The password path already read `/V` through `standard_handler_r5_inputs`;
/// the `--password-is-hex-key` path did not, and used to open such a document.
#[test]
fn an_encrypt_dictionary_without_v_is_rejected_on_every_path() {
    let mut fixture = encrypted_r5_or_r6_pdf(6, "", &[]);
    let at = fixture
        .windows(4)
        .position(|window| window == b"/V 5")
        .expect("fixture declares /V 5");
    fixture[at..at + 4].copy_from_slice(b"    ");

    let paths = [
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
        PdfOpenOptions {
            password: b"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_vec(),
            password_is_hex_key: true,
            ..PdfOpenOptions::default()
        },
    ];
    for options in paths {
        let hex_key = options.password_is_hex_key;
        let err = Pdf::open_with_options(std::io::Cursor::new(fixture.clone()), options)
            .err()
            .unwrap_or_else(|| panic!("qpdf rejects this document (hex key path: {hex_key})"));
        assert!(
            matches!(
                err,
                Error::Encrypted(EncryptedError::Malformed { ref reason })
                    if reason == "missing /V entry"
            ),
            "hex key path: {hex_key}, got {err:?}"
        );
    }
}

/// A `/V 4` document whose one crypt filter names a `/CFM` qpdf does not
/// recognise, carrying two strings and two streams.
///
/// qpdf opens it, warns once for strings and once for streams, and decrypts
/// everything as AES because its `default:` arm rewrites the filter to
/// `e_aes` (`libqpdf/QPDF_encryption.cc:1121-1133`). Observed on a real
/// `qpdf --encrypt --use-aes=y` output whose `/CFM` was rewritten to `/AESVX`.
fn encrypted_v4_unknown_cfm_fixture() -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = fixed_r4_u_entry(true);

    let aes_string = |object_number: u32, plaintext: &[u8]| {
        let key = aes128_object_key(&per_object_aes_key(&file_key, object_number, 0));
        hex_string(&aes128_cbc_encrypt_with_iv(&key, &[0x11; 16], plaintext))
    };
    let aes_stream = |object_number: u32, plaintext: &[u8]| {
        let key = aes128_object_key(&per_object_aes_key(&file_key, object_number, 0));
        aes128_cbc_encrypt_with_iv(&key, &[0x22; 16], plaintext)
    };

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    let mut push = |bytes: &mut Vec<u8>, object: Vec<u8>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(&object);
    };
    push(
        &mut bytes,
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
    );
    push(
        &mut bytes,
        b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n".to_vec(),
    );
    push(
        &mut bytes,
        format!(
            "3 0 obj\n<< /Secret <{}> >>\nendobj\n",
            aes_string(3, b"first")
        )
        .into_bytes(),
    );
    push(
        &mut bytes,
        format!(
            "4 0 obj\n<< /Secret <{}> >>\nendobj\n",
            aes_string(4, b"second")
        )
        .into_bytes(),
    );
    for (object_number, plaintext) in [(5u32, b"stream one".as_slice()), (6, b"stream two")] {
        let body = aes_stream(object_number, plaintext);
        let mut object = format!(
            "{object_number} 0 obj\n<< /Length {} >>\nstream\n",
            body.len()
        )
        .into_bytes();
        object.extend_from_slice(&body);
        object.extend_from_slice(b"\nendstream\nendobj\n");
        push(&mut bytes, object);
    }

    let xref_offset = bytes.len();
    let size = offsets.len() + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 \
             /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESVX /Length 128 >> >> \
             /StmF /StdCF /StrF /StdCF /EFF /StdCF >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

/// An unrecognised `/CFM` used to make flpdf refuse the document with
/// `UnsupportedHandler`. qpdf opens it, decrypts as AES, and warns once per
/// kind — strings before streams.
#[test]
fn an_unknown_crypt_filter_warns_once_per_kind_and_still_decrypts() {
    let mut pdf = Pdf::open(std::io::Cursor::new(encrypted_v4_unknown_cfm_fixture()))
        .expect("qpdf opens a document with an unknown /CFM");

    let secret = |pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>, object_number: u32| {
        let dict = resolved_handle(pdf, ObjectRef::new(object_number, 0));
        resolved_key(pdf, &dict, b"/Secret")
            .as_string()
            .unwrap_or_else(|| panic!("object {object_number} has a /Secret string"))
    };
    assert_eq!(secret(&mut pdf, 3), b"first");
    assert_eq!(secret(&mut pdf, 4), b"second");

    for (object_number, plaintext) in [(5u32, b"stream one".as_slice()), (6, b"stream two")] {
        let stream = resolved_handle(&mut pdf, ObjectRef::new(object_number, 0));
        assert_eq!(stream_data(&stream), plaintext);
    }

    let messages: Vec<String> = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|entry| entry.message.to_string())
        .filter(|message| message.contains("unknown encryption filter"))
        .collect();
    assert_eq!(
        messages,
        vec![
            "unknown encryption filter for strings (check /StrF in /Encrypt dictionary); \
             strings may be decrypted improperly"
                .to_string(),
            "unknown encryption filter for streams (check /StmF from /Encrypt dictionary); \
             streams may be decrypted improperly"
                .to_string(),
        ],
        "qpdf warns once per kind, strings first, because each arm resets its crypt filter"
    );
}

/// The same document reported through `--show-encryption`. qpdf prints
/// `unknown` for all three methods (`show_encryption_method`,
/// `libqpdf/QPDFJob.cc:682-684`) and exits 0; the fixture's `/EFF` names the
/// same crypt filter, which is the branch where qpdf calls `interpretCF` on
/// `/EFF` rather than mirroring `cf_stream` (`QPDF_encryption.cc:891-904`).
///
/// Observed on a `/V 4` qpdf output whose `/CFM` was rewritten to `/AESVX`:
///
/// ```text
/// stream encryption method: unknown
/// string encryption method: unknown
/// file encryption method: unknown
/// ```
#[test]
fn an_unknown_crypt_filter_is_reported_as_unknown() {
    let pdf = Pdf::open(std::io::Cursor::new(encrypted_v4_unknown_cfm_fixture()))
        .expect("qpdf opens a document with an unknown /CFM");

    assert_eq!(
        pdf.encryption_methods(),
        Some(("unknown", "unknown", "unknown"))
    );
}

fn encrypted_v4_explicit_crypt_filter_fixture(identity: bool, crypt_after_flate: bool) -> Vec<u8> {
    encrypted_v4_explicit_crypt_filter_fixture_with_length(identity, crypt_after_flate, true)
}

fn encrypted_v4_explicit_crypt_filter_ascii_hex_fixture() -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = fixed_r4_u_entry(true);

    let mut ascii_hex = Vec::new();
    for byte in b"explicit crypt ASCIIHex stream" {
        ascii_hex.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    let stream_key = aes128_object_key(&per_object_aes_key(&file_key, 4, 0));
    let encrypted = aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], &ascii_hex);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Data 4 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let obj4_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter [/ASCIIHexDecode /Crypt] /DecodeParms [null << /Name /StdCF >>] >>\nstream\n",
            encrypted.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&encrypted);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n0000000000 65535 f \n{obj4_offset:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESV2 /Length 128 >> >> /StmF /Identity /StrF /Identity >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );

    bytes
}

fn encrypted_v4_explicit_crypt_filter_fixture_with_length(
    identity: bool,
    crypt_after_flate: bool,
    valid_length: bool,
) -> Vec<u8> {
    encrypted_v4_explicit_crypt_filter_fixture_with_order(
        identity,
        crypt_after_flate,
        valid_length,
        false,
    )
}

fn encrypted_v4_explicit_crypt_filter_fixture_with_order(
    identity: bool,
    crypt_after_flate: bool,
    valid_length: bool,
    flate_after_crypt: bool,
) -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = fixed_r4_u_entry(true);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Data 4 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let stream_key = aes128_object_key(&per_object_aes_key(&file_key, 4, 0));
    encoder.write_all(b"explicit crypt stream").unwrap();
    let compressed = encoder.finish().unwrap();
    let stream_data = if identity {
        compressed
    } else if flate_after_crypt {
        let ciphertext =
            aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], b"explicit crypt stream");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&ciphertext).unwrap();
        encoder.finish().unwrap()
    } else {
        // qpdf prepends stream decryption before it constructs the filter
        // chain, so the ciphertext is the encrypted Flate representation even
        // when /Crypt appears after /FlateDecode in the dictionary.
        aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], &compressed)
    };
    let decode_parms = if identity { "Identity" } else { "StdCF" };
    let (filters, decode_parms_array) = if crypt_after_flate {
        (
            "[/FlateDecode /Crypt]",
            format!("[null << /Name /{decode_parms} >>]"),
        )
    } else {
        (
            "[/Crypt /FlateDecode]",
            format!("[<< /Name /{decode_parms} >> null]"),
        )
    };
    let obj4_offset = bytes.len();
    let length = if valid_length {
        stream_data.len().to_string()
    } else {
        "[]".to_string()
    };
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {length} /Filter {filters} /DecodeParms {decode_parms_array} >>\nstream\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n0000000000 65535 f \n{obj4_offset:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESV2 /Length 128 >> >> /StmF /Identity /StrF /Identity >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

/// Build the `/Crypt`-plus-ASCII85 counterpart of
/// [`encrypted_v4_explicit_crypt_filter_fixture`].
///
/// Stream bytes follow the `/Filter` array's decode order, so
/// `crypt_after_ascii85` decides which of the two transforms wraps the other.
fn encrypted_v4_ascii85_crypt_filter_fixture(crypt_after_ascii85: bool) -> Vec<u8> {
    encrypted_v4_ascii85_crypt_filter_fixture_with_order(crypt_after_ascii85, false)
}

fn encrypted_v4_ascii85_crypt_filter_fixture_with_order(
    crypt_after_ascii85: bool,
    ascii85_after_crypt: bool,
) -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = fixed_r4_u_entry(true);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Data 4 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");

    let stream_key = aes128_object_key(&per_object_aes_key(&file_key, 4, 0));
    let plaintext = b"explicit crypt stream";
    let encoded = ascii85::fixture_bytes(plaintext);
    let stream_data = if ascii85_after_crypt {
        let ciphertext = aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], plaintext);
        ascii85::fixture_bytes(&ciphertext)
    } else {
        aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], &encoded)
    };
    let (filters, decode_parms_array) = if crypt_after_ascii85 {
        ("[/ASCII85Decode /Crypt]", "[null << /Name /StdCF >>]")
    } else {
        ("[/Crypt /ASCII85Decode]", "[<< /Name /StdCF >> null]")
    };
    let obj4_offset = bytes.len();
    let length = stream_data.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {length} /Filter {filters} /DecodeParms {decode_parms_array} >>\nstream\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n0000000000 65535 f \n{obj4_offset:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESV2 /Length 128 >> >> /StmF /Identity /StrF /Identity >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

fn encrypted_v4_plaintext_metadata_stream_fixture() -> Vec<u8> {
    encrypted_v4_plaintext_metadata_stream_fixture_with_body(b"<xmpmeta>plain</xmpmeta>", true)
}

fn encrypted_v4_plaintext_metadata_recovered_eol_fixture() -> Vec<u8> {
    encrypted_v4_plaintext_metadata_stream_fixture_with_body(b"A", false)
}

fn encrypted_v4_plaintext_metadata_stream_fixture_with_body(
    metadata: &[u8],
    valid_length: bool,
) -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let u = fixed_r4_u_entry(false);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let obj3_offset = bytes.len();
    let length = if valid_length {
        metadata.len().to_string()
    } else {
        "[]".to_string()
    };
    bytes.extend_from_slice(
        format!("3 0 obj\n<< /Type /Metadata /Subtype /XML /Length {length} >>\nstream\n")
            .as_bytes(),
    );
    bytes.extend_from_slice(metadata);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n{obj3_offset:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /EncryptMetadata false /CF << /StdCF << /CFM /AESV2 /Length 128 >> >> /StmF /StdCF /StrF /StdCF >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

fn encrypted_v4_identity_recovered_eol_fixture(explicit_crypt_identity: bool) -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let u = fixed_r4_u_entry(true);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Data 4 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let obj4_offset = bytes.len();
    let stream_dict = if explicit_crypt_identity {
        "<< /Length [] /Filter /Crypt /DecodeParms << /Name /Identity >> >>"
    } else {
        "<< /Length [] >>"
    };
    bytes.extend_from_slice(
        format!("4 0 obj\n{stream_dict}\nstream\nA\nendstream\nendobj\n").as_bytes(),
    );

    let stream_filter = if explicit_crypt_identity {
        "StdCF"
    } else {
        "Identity"
    };
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n0000000000 65535 f \n{obj4_offset:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESV2 /Length 128 >> >> /StmF /{stream_filter} /StrF /Identity >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

/// Encrypt a fixture through the public writer. The ordinary-object case uses
/// a string, while the ObjStm case uses an integer member so the test isolates
/// container decryption-before-Flate behavior from member-string handling.
fn writer_generated_rc4_reader_fixture(use_object_stream: bool) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let obj3_offset = bytes.len();
    if use_object_stream {
        bytes.extend_from_slice(b"3 0 obj\n<< /Value 42 >>\nendobj\n");
    } else {
        bytes.extend_from_slice(b"3 0 obj\n<< /Title (TopSecretTitle) >>\nendobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n{obj3_offset:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).expect("open plaintext RC4 fixture");
    let settings = WriterTestSettings {
        object_streams: if use_object_stream {
            flpdf::ObjectStreamMode::Generate
        } else {
            flpdf::ObjectStreamMode::Disable
        },
        encrypt: Some(EncryptParams::rc4(
            EncryptMethod::V1Rc440,
            b"user-pw".to_vec(),
            b"owner-pw".to_vec(),
        )),
        ..WriterTestSettings::default()
    };
    let mut output = Vec::new();
    write_with_settings(&mut pdf, &mut output, &settings).expect("write RC4 object-stream fixture");
    output
}

fn r4_file_key(password: &[u8], o: &[u8], p: i32, id0: &[u8]) -> Vec<u8> {
    r4_file_key_with_encrypt_metadata(password, o, p, id0, true)
}

fn r4_file_key_with_encrypt_metadata(
    password: &[u8],
    o: &[u8],
    p: i32,
    id0: &[u8],
    encrypt_metadata: bool,
) -> Vec<u8> {
    let mut padded = [0u8; 32];
    let password_len = password.len().min(32);
    padded[..password_len].copy_from_slice(&password[..password_len]);
    padded[password_len..].copy_from_slice(&PASSWORD_PADDING[..32 - password_len]);

    let mut hasher = Md5::new();
    hasher.update(padded);
    hasher.update(o);
    hasher.update(p.to_le_bytes());
    hasher.update(id0);
    if !encrypt_metadata {
        hasher.update([0xff; 4]);
    }
    let mut digest = hasher.finalize().to_vec();
    for _ in 0..50 {
        let mut hasher = Md5::new();
        hasher.update(&digest[..16]);
        digest = hasher.finalize().to_vec();
    }
    digest[..16].to_vec()
}

const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

fn per_object_aes_key(file_key: &[u8], object_number: u32, generation: u32) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(file_key);
    hasher.update(&object_number.to_le_bytes()[..3]);
    hasher.update(&generation.to_le_bytes()[..2]);
    hasher.update([0x73, 0x41, 0x6c, 0x54]);
    let digest = hasher.finalize();
    digest[..(file_key.len() + 5).min(16)].to_vec()
}

fn aes128_object_key(key: &[u8]) -> [u8; 16] {
    key.try_into().unwrap()
}

fn aes128_cbc_encrypt_with_iv(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    let mut data = vec![0u8; plaintext.len() + 16];
    data[..plaintext.len()].copy_from_slice(plaintext);
    let encrypted = <Encryptor<Aes128> as KeyIvInit>::new(key.into(), iv.into())
        .encrypt_padded::<Pkcs7>(&mut data, plaintext.len())
        .unwrap();
    let mut out = iv.to_vec();
    out.extend_from_slice(encrypted);
    out
}

/// RC4, implemented here rather than reached for through `flpdf`, so the
/// fixtures below are an independent check on the reader's decryption rather
/// than a round trip through the same code.
fn rc4_process(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            byte ^ s[(s[i as usize].wrapping_add(s[j as usize])) as usize]
        })
        .collect()
}

/// Algorithm 3.1's RC4 variant: no `"sAlT"`, truncated to `min(len + 5, 16)`.
fn per_object_rc4_key(file_key: &[u8], object_number: u32, generation: u32) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(file_key);
    hasher.update(&object_number.to_le_bytes()[..3]);
    hasher.update(&generation.to_le_bytes()[..2]);
    let digest = hasher.finalize();
    digest[..(file_key.len() + 5).min(16)].to_vec()
}

/// Algorithm 2 for `/R 2`, which skips the 50 rehash rounds `r4_file_key` does,
/// truncated to `length_bytes`.
fn r2_file_key(password: &[u8], o: &[u8], p: i32, id0: &[u8], length_bytes: usize) -> Vec<u8> {
    let mut padded = [0u8; 32];
    let password_len = password.len().min(32);
    padded[..password_len].copy_from_slice(&password[..password_len]);
    padded[password_len..].copy_from_slice(&PASSWORD_PADDING[..32 - password_len]);

    let mut hasher = Md5::new();
    hasher.update(padded);
    hasher.update(o);
    hasher.update(p.to_le_bytes());
    hasher.update(id0);
    hasher.finalize()[..length_bytes].to_vec()
}

/// Algorithm 4 (`/R 2`): RC4 the padding string with the file key.
fn r2_u_entry(file_key: &[u8]) -> Vec<u8> {
    rc4_process(file_key, &PASSWORD_PADDING)
}

/// Algorithm 5 (`/R 3` and up): MD5 of the padding and `/ID[0]`, RC4 with the
/// file key, then 19 more rounds under the key XORed with the round number.
fn r3_u_entry(file_key: &[u8], id0: &[u8]) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(PASSWORD_PADDING);
    hasher.update(id0);
    let mut block = rc4_process(file_key, &hasher.finalize()).to_vec();
    for round in 1u8..=19 {
        let key: Vec<u8> = file_key.iter().map(|byte| byte ^ round).collect();
        block = rc4_process(&key, &block);
    }
    // The trailing 16 bytes are arbitrary padding per the algorithm.
    block.extend_from_slice(&[0u8; 16]);
    block
}

/// The plaintext every pre-`/V 4` stream fixture below carries.
const LEGACY_STREAM_PLAINTEXT: &[u8] = b"BT /F1 12 Tf 20 100 Td (legacy rc4) Tj ET\n";

/// A `/V 1` or `/V 2` document whose one content stream is RC4-encrypted.
///
/// The committed `tests/fixtures/encrypted/v*-rc4-*.pdf` fixtures have no
/// pages and therefore no streams, so nothing else in the suite exercises
/// stream decryption below `/V 4` — the exact path qpdf reaches by leaving
/// `use_aes` false and skipping the crypt-filter switch entirely
/// (`libqpdf/QPDF_encryption.cc:1062-1063`).
fn encrypted_legacy_rc4_stream_fixture(version: i64) -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let (revision, length_bits, file_key, u) = if version == 1 {
        let file_key = r2_file_key(b"", &o, p, &id0, 5);
        let u = r2_u_entry(&file_key);
        (2, 40, file_key, u)
    } else {
        let file_key = r4_file_key(b"", &o, p, &id0);
        let u = r3_u_entry(&file_key, &id0);
        (3, 128, file_key, u)
    };

    let stream_key = per_object_rc4_key(&file_key, 4, 0);
    let body = rc4_process(&stream_key, LEGACY_STREAM_PLAINTEXT);

    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let mut push = |bytes: &mut Vec<u8>, object: &[u8]| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };
    push(
        &mut bytes,
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
    );
    push(
        &mut bytes,
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
    );
    push(
        &mut bytes,
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n",
    );
    let stream_object = {
        let mut object = format!("4 0 obj\n<< /Length {} >>\nstream\n", body.len()).into_bytes();
        object.extend_from_slice(&body);
        object.extend_from_slice(b"\nendstream\nendobj\n");
        object
    };
    push(&mut bytes, &stream_object);

    let xref_offset = bytes.len();
    let size = offsets.len() + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Encrypt << /Filter /Standard /V {version} \
             /R {revision} /Length {length_bits} /P {p} /O <{}> /U <{}> >> \
             /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0),
        )
        .as_bytes(),
    );
    bytes
}

/// The regression this whole change most risks: making `cf_stream` faithful
/// leaves it at qpdf's `e_none` for `/V 1` and `/V 2`, and a consumer that
/// read it without qpdf's `/V >= 4` gate would silently stop decrypting.
#[test]
fn pre_v4_documents_still_decrypt_their_streams_with_rc4() {
    for version in [1, 2] {
        let mut pdf = Pdf::open_with_options(
            std::io::Cursor::new(encrypted_legacy_rc4_stream_fixture(version)),
            PdfOpenOptions {
                ..PdfOpenOptions::default()
            },
        )
        .unwrap_or_else(|err| panic!("/V {version} fixture must authenticate: {err:?}"));

        let stream = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
        assert_eq!(
            stream_data(&stream),
            LEGACY_STREAM_PLAINTEXT,
            "/V {version} stream must be RC4-decrypted"
        );
    }
}

/// The same documents report qpdf's stored crypt-filter state: `e_none`,
/// because `interpretCF` never runs below `/V 4`
/// (`libqpdf/QPDF_encryption.cc:860`). `qpdf --show-encryption` prints no
/// method lines at all for these, since it gates the display on `V >= 4`
/// (`libqpdf/QPDFJob.cc:736-740`).
#[test]
fn pre_v4_documents_report_no_crypt_filter_methods() {
    for version in [1, 2] {
        let pdf = Pdf::open_with_options(
            std::io::Cursor::new(encrypted_legacy_rc4_stream_fixture(version)),
            PdfOpenOptions {
                ..PdfOpenOptions::default()
            },
        )
        .expect("fixture must authenticate");

        assert_eq!(pdf.encryption_version(), Some(version));
        assert_eq!(pdf.encryption_methods(), Some(("none", "none", "none")));
    }
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[test]
fn missing_reference_resolves_to_null() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert!(resolved_handle(&mut pdf, ObjectRef::new(99, 0)).is_null());
}

#[test]
fn resolves_compressed_entry_from_xref_stream() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(&catalog);

    let obj3_offset = bytes.len();
    let obj_stream_body = b"2 0 42";
    let obj3 = format!(
        "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        obj_stream_body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&obj3);
    bytes.extend_from_slice(obj_stream_body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(2, 0)).as_integer(),
        Some(42)
    );
}

#[test]
fn resolves_compressed_entry_with_flate_decode_from_xref_stream() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>| {
        bytes.extend_from_slice(object);
    };

    add_object(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", &mut bytes);

    let member1 = format!("<< /Type /Packed /Payload ({}) >>", "A".repeat(400),).into_bytes();
    let member2 = format!("<< /Type /Packed /Payload ({}) >>", "B".repeat(420),).into_bytes();

    let (stream_data, first) = encode_flate_objstm(&[(2, &member1[..]), (3, &member2[..])]);
    let obj_stream_offset = bytes.len();
    let obj_stream = format!(
        "4 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
        first,
        stream_data.len(),
    )
    .into_bytes();
    bytes.extend_from_slice(&obj_stream);
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 1, obj_stream_offset as u32, 0);

    let xref_stream_object = format!(
        "5 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(2, 0)).unparse_resolved(),
        ObjectHandle::parse(&member1).unwrap().unparse_resolved()
    );
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(3, 0)).unparse_resolved(),
        ObjectHandle::parse(&member2).unwrap().unparse_resolved()
    );
}

#[test]
fn resolves_compressed_entry_declared_in_extended_object_stream() {
    let fixture = objstm_extends_chain_pdf();
    assert_qpdf_object_contains(&fixture, 2, "null");
    assert_qpdf_object_contains(&fixture, 3, "99");

    let mut pdf = Pdf::open(std::io::Cursor::new(fixture)).unwrap();

    assert!(resolved_handle(&mut pdf, ObjectRef::new(2, 0)).is_null());
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(3, 0)).as_integer(),
        Some(99)
    );
}

#[test]
fn objstm_direct_container_qpdf_contract() {
    let fixture = objstm_direct_container_pdf();
    assert_qpdf_object_contains(&fixture, 2, "null");
    assert_qpdf_object_contains(&fixture, 3, "null");
    assert_qpdf_object_contains(&fixture, 10, "/V 100");
    assert_qpdf_object_contains(&fixture, 11, "/V 200");
    assert_qpdf_object_contains(&fixture, 12, "/V 300");

    let mut pdf = Pdf::open(std::io::Cursor::new(fixture)).unwrap();
    assert!(
        resolved_handle(&mut pdf, ObjectRef::new(2, 0)).is_null(),
        "a header object number, not the xref field2, controls resolution"
    );
    assert!(
        resolved_handle(&mut pdf, ObjectRef::new(3, 0)).is_null(),
        "a header object number, not the xref field2, controls resolution"
    );
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(10, 0)).unparse_resolved(),
        ObjectHandle::parse(b"<< /V 100 >>")
            .unwrap()
            .unparse_resolved()
    );
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(11, 0)).unparse_resolved(),
        ObjectHandle::parse(b"<< /V 200 >>")
            .unwrap()
            .unparse_resolved()
    );
    assert_eq!(
        resolved_handle(&mut pdf, ObjectRef::new(12, 0)).unparse_resolved(),
        ObjectHandle::parse(b"<< /V 300 >>")
            .unwrap()
            .unparse_resolved(),
        "a child xref entry must resolve against the child container directly"
    );
}

#[test]
fn objstm_direct_container_rejects_effective_xref_source_mismatch() {
    let fixture = objstm_direct_container_pdf_with_child_source(4);
    assert_qpdf_object_contains(&fixture, 12, "null");

    let mut pdf = Pdf::open(std::io::Cursor::new(fixture)).unwrap();
    assert!(
        resolved_handle(&mut pdf, ObjectRef::new(12, 0)).is_null(),
        "a child header must not be used when effective xref points at another stream"
    );
}

fn compressed_entry_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let catalog = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(&catalog);

    let obj3_offset = bytes.len();
    let obj_stream_body = b"2 0 42";
    let obj3 = format!(
        "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
        obj_stream_body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&obj3);
    bytes.extend_from_slice(obj_stream_body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();

    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn compressed_entry_with_missing_parent_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();
    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn compressed_entry_with_non_stream_parent_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let obj3_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /NotObjStm >>\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();
    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn compressed_entry_with_compressed_parent_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let obj4_offset = bytes.len();
    bytes.extend_from_slice(
        b"4 0 obj\n<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream\nendobj\n",
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj4_offset as u32, 0);

    let xref_stream_object = format!(
        "5 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();
    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn compressed_entry_with_mismatched_parent_ref_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let bad_parent_offset = bytes.len();
    bytes.extend_from_slice(
        b"9 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 6 >>\nstream\n2 0 42\nendstream\nendobj\n",
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, bad_parent_offset as u32, 0);

    let xref_stream_object = format!(
        "4 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
        xref_entries.len()
    )
    .into_bytes();
    let startxref = bytes.len();
    bytes.extend_from_slice(&xref_stream_object);
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn objstm_extends_chain_pdf() -> Vec<u8> {
    decode_hex_fixture(include_str!(
        "../../../tests/fixtures/compat/objstm-extends-chain.pdf.hex"
    ))
}

fn objstm_direct_container_pdf() -> Vec<u8> {
    objstm_direct_container_pdf_with_child_source(5)
}

fn objstm_direct_container_pdf_with_child_source(child_source: u32) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let (parent_data, parent_first) =
        encode_plain_objstm(&[(10, b"<< /V 100 >>"), (11, b"<< /V 200 >>")]);
    let parent_offset = bytes.len();
    let parent_header = format!(
        "4 0 obj\n<< /Type /ObjStm /N 2 /First {parent_first} /Length {} >>\nstream\n",
        parent_data.len()
    );
    bytes.extend_from_slice(parent_header.as_bytes());
    bytes.extend_from_slice(&parent_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let (child_data, child_first) = encode_plain_objstm(&[(12, b"<< /V 300 >>")]);
    let child_offset = bytes.len();
    let child_header = format!(
        "5 0 obj\n<< /Type /ObjStm /N 1 /First {child_first} /Length {} /Extends 4 0 R >>\nstream\n",
        child_data.len()
    );
    bytes.extend_from_slice(child_header.as_bytes());
    bytes.extend_from_slice(&child_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_stream_offset = bytes.len();
    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 5, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 1, parent_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, child_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, xref_stream_offset as u32, 0);
    for _ in 7..10 {
        append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    }
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 2, child_source, 0);

    let xref_stream = format!(
        "6 0 obj\n<< /Type /XRef /Size 13 /Root 1 0 R /W [1 3 1] /Index [0 13] /Length {} >>\nstream\n",
        xref_entries.len()
    );
    bytes.extend_from_slice(xref_stream.as_bytes());
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_stream_offset}\n%%EOF\n").as_bytes());
    bytes
}

fn assert_qpdf_object_contains(fixture: &[u8], object_number: u32, expected: &str) {
    let Some(output) = qpdf_show_object(fixture, object_number) else {
        return;
    };
    assert!(
        output.contains(expected),
        "qpdf --show-object={object_number} output {output:?} must contain {expected:?}"
    );
}

/// Whether qpdf 11.9.0 is on `PATH`, for tests that spawn it as a live
/// differential oracle rather than hand-authoring the expected bytes.
fn qpdf_available() -> bool {
    Command::new("qpdf")
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

fn qpdf_show_object(fixture: &[u8], object_number: u32) -> Option<String> {
    let version = match Command::new("qpdf").arg("--version").output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("qpdf --version failed to start: {error}"),
    };
    assert!(
        version.status.success(),
        "qpdf --version failed: {}",
        String::from_utf8_lossy(&version.stderr)
    );
    let version_line = String::from_utf8_lossy(&version.stdout);
    if version_line
        .lines()
        .next()
        .is_none_or(|line| line.trim() != "qpdf version 11.9.0")
    {
        return None;
    }

    let directory = tempfile::tempdir().expect("create qpdf differential fixture directory");
    let input = directory.path().join("fixture.pdf");
    std::fs::write(&input, fixture).expect("write qpdf differential fixture");
    let output = Command::new("qpdf")
        .arg("--warning-exit-0")
        .arg(format!("--show-object={object_number}"))
        .arg(&input)
        .output()
        .expect("run qpdf --show-object");
    assert!(
        output.status.success(),
        "qpdf --show-object={object_number} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn decode_hex_fixture(hex: &str) -> Vec<u8> {
    let digits: Vec<u8> = hex
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    assert!(digits.len().is_multiple_of(2));

    digits
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let bytes_u24 = value.to_be_bytes();
    bytes.extend_from_slice(&bytes_u24[1..]);
}

fn encode_flate_objstm(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    let mut header = String::new();
    let mut body = Vec::new();

    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{} {} ", number, offset));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }

    let mut decoded = Vec::new();
    decoded.extend_from_slice(header.as_bytes());
    decoded.extend_from_slice(&body);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&decoded).unwrap();
    let encoded = encoder.finish().unwrap();

    (encoded, header.len())
}

fn encode_plain_objstm(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    let mut header = String::new();
    let mut body = Vec::new();

    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{number} {offset} "));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }

    let header_len = header.len();
    let mut data = header.into_bytes();
    data.extend_from_slice(&body);
    (data, header_len)
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}

// ── Authoritative indirect /Length via xref ────────────────────────────────

/// When `/Length` is an indirect reference, the reader resolves the holder
/// via the xref and slices EXACTLY that many content bytes. Here the
/// authoritative length (2) is shorter than what the `endstream`-scan
/// fallback would yield (`ab\n`, after trimming one of the two trailing
/// EOLs), so without xref resolution the stream would carry a spurious
/// trailing newline; resolving the holder makes it exactly `ab`.
#[test]
fn indirect_length_resolved_via_xref_is_authoritative() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let off1 = bytes.len();
    // Stream payload is `ab\n\n` (4 bytes) before `endstream`; the indirect
    // /Length holder (obj 4) says the real content is only 2 bytes (`ab`).
    bytes.extend_from_slice(b"1 0 obj\n<< /Length 4 0 R >>\nstream\nab\n\nendstream\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
    let off3 = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let off4 = bytes.len();
    bytes.extend_from_slice(b"4 0 obj\n2\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 2 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    let stream = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert_eq!(
        stream_data(&stream),
        b"ab",
        "indirect /Length must be resolved to the authoritative 2 bytes \
         (got {:?})",
        String::from_utf8_lossy(&stream_data(&stream))
    );
}

/// The reader re-slices on the still-encrypted object bytes BEFORE
/// `decrypt_resolved_object`. Opening an encrypted fixture and resolving its
/// objects must still yield correctly decrypted streams (no double-decrypt /
/// ciphertext leakage from the new path).
#[test]
fn encrypted_fixture_streams_decrypt_correctly_with_indirect_length_path() {
    let file = File::open("../../tests/fixtures/compat/encrypted-r4-three-page.pdf").unwrap();
    let mut pdf = Pdf::open_with_options(BufReader::new(file), PdfOpenOptions::default()).unwrap();
    // Resolve every object; none must error and no panic.
    let mut stream_seen = false;
    for r in pdf.object_refs() {
        let object = resolved_handle(&mut pdf, r);
        if object.as_stream_dict().is_some() {
            stream_seen = true;
            // A correctly decrypted content/metadata stream is decodable and
            // not obviously ciphertext garbage of the wrong length.
            let data = stream_data(&object);
            assert!(
                !data.is_empty() || stream_dict(&object).has_key(b"/Length"),
                "decrypted stream {r:?} unexpectedly empty with no /Length"
            );
        }
    }
    assert!(stream_seen, "fixture must contain at least one stream");
}

/// A cyclic indirect-/Length holder chain
/// (obj 1's /Length -> obj 2 -> obj 1) must NOT recurse forever. The
/// in-progress `Reserved` guard breaks the cycle; resolution terminates and
/// the stream falls back to the endstream-scan length.
#[test]
fn cyclic_indirect_length_holder_terminates() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let off1 = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Length 2 0 R >>\nstream\nAAA\nendstream\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Length 1 0 R >>\nstream\nBBBB\nendstream\nendobj\n");
    let off3 = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Catalog /Pages 4 0 R >>\nendobj\n");
    let off4 = bytes.len();
    bytes.extend_from_slice(b"4 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 3 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    // Must terminate (no stack overflow / hang) and yield a stream.
    let object = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert!(
        object.as_stream_dict().is_some(),
        "cyclic /Length holder must still resolve to a stream (endstream-scan fallback)"
    );
}

// ── Whole-file QDF detection for exact-window indirect /Length ─────────────

fn exact_window_indirect_length_pdf(header: &[u8]) -> Vec<u8> {
    // obj 1: stream `ab\n` followed *directly* by `endstream` (non-conformant:
    // no mandatory pre-endstream EOL). Indirect /Length holder (obj 4) gives
    // the spec content length 3 — auth_end == endstream_pos (exact window).
    let mut bytes = header.to_vec();
    let off1 = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Length 4 0 R >>\nstream\nab\nendstream\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
    let off3 = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let off4 = bytes.len();
    bytes.extend_from_slice(b"4 0 obj\n3\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for off in [off1, off2, off3, off4] {
        bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 2 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

/// Non-QDF file: the authoritative indirect /Length (3 bytes) is honoured
/// verbatim even in the exact-window case, so the stream keeps its trailing
/// newline (`ab\n`) instead of the endstream-scan dropping it.
#[test]
fn non_qdf_exact_window_indirect_length_preserves_trailing_newline() {
    let pdf = exact_window_indirect_length_pdf(b"%PDF-1.7\n");
    let mut pdf = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let stream = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert_eq!(
        stream_data(&stream),
        b"ab\n",
        "non-QDF exact-window indirect /Length must keep the trailing newline, got {:?}",
        String::from_utf8_lossy(&stream_data(&stream))
    );
}

/// Same object bytes but the header carries `%QDF-1.0`: qpdf's indirect
/// holder still gives the authoritative logical payload length, so an exact
/// endpoint keeps the trailing LF just as it does for an ordinary PDF.
#[test]
fn qdf_exact_window_indirect_length_preserves_trailing_newline() {
    let pdf = exact_window_indirect_length_pdf(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n");
    let mut pdf = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let stream = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert_eq!(
        stream_data(&stream),
        b"ab\n",
        "QDF exact-window indirect /Length must keep the trailing newline, got {:?}",
        String::from_utf8_lossy(&stream_data(&stream))
    );
}

/// The exact-window indirect length path must also work through a reader that
/// returns one byte at a time.
#[test]
fn qdf_exact_window_indirect_length_works_through_short_reads() {
    use std::io::{Read, Seek, SeekFrom};

    struct OneByteReader(std::io::Cursor<Vec<u8>>);
    impl Read for OneByteReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            self.0.read(&mut buf[..1])
        }
    }
    impl Seek for OneByteReader {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(pos)
        }
    }

    let pdf = exact_window_indirect_length_pdf(b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n%QDF-1.0\n");
    let mut pdf = Pdf::open(OneByteReader(std::io::Cursor::new(pdf))).unwrap();
    let stream = resolved_handle(&mut pdf, ObjectRef::new(1, 0));
    assert_eq!(
        stream_data(&stream),
        b"ab\n",
        "QDF exact-window indirect /Length must survive short reads (got {:?})",
        String::from_utf8_lossy(&stream_data(&stream))
    );
}
