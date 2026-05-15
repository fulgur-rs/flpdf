use aes::cipher::{BlockEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use cbc::Encryptor;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::{parse_object, EncryptedError, Error, Object, ObjectRef, Pdf, PdfOpenOptions};
use md5::{Digest, Md5};
use std::fs::File;
use std::io::BufReader;
use std::io::Write;

#[test]
fn opens_pdf_without_resolving_all_objects() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let pdf = Pdf::open(BufReader::new(file)).unwrap();

    assert_eq!(pdf.version(), "1.7");
    assert_eq!(pdf.resolved_count(), 0);
    assert_eq!(pdf.trailer().get_ref("Root"), Some(ObjectRef::new(1, 0)));
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
fn open_rejects_rc4_encryption_by_default() {
    let err = match Pdf::open(std::io::Cursor::new(encrypted_r2_reader_fixture())) {
        Ok(_) => panic!("RC4 encryption should be rejected by default"),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        Error::Encrypted(EncryptedError::WeakCryptoNotAllowed)
    ));
}

#[test]
fn open_with_options_accepts_owner_password() {
    let bytes = encrypted_v1_owner_password_fixture();
    let options = PdfOpenOptions {
        allow_weak_crypto: true,
        password: b"owner".to_vec(),
        ..PdfOpenOptions::default()
    };

    let pdf = Pdf::open_with_options(std::io::Cursor::new(bytes), options).unwrap();

    assert_eq!(pdf.version(), "1.7");
}

#[test]
fn resolves_indirect_object_on_access() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let root = pdf.resolve(ObjectRef::new(1, 0)).unwrap();
    let Object::Dictionary(dict) = root else {
        panic!("expected catalog dictionary")
    };

    assert_eq!(dict.get_ref("Pages"), Some(ObjectRef::new(2, 0)));
    assert_eq!(pdf.resolved_count(), 1);
}

#[test]
fn open_with_options_rejects_r5_by_default() {
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(5)),
        PdfOpenOptions {
            password: b"userpass".to_vec(),
            ..PdfOpenOptions::default()
        },
    ) {
        Ok(_) => panic!("deprecated R=5 encryption should be rejected by default"),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        Error::Encrypted(EncryptedError::WeakCryptoNotAllowed)
    ));
}

#[test]
fn open_rejects_v4_rc4_crypt_filters_by_default() {
    let err = match Pdf::open(std::io::Cursor::new(encrypted_v4_rc4_cf_fixture())) {
        Ok(_) => panic!("V=4 /CFM /V2 encryption should be rejected by default"),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        Error::Encrypted(EncryptedError::WeakCryptoNotAllowed)
    ));
}

#[test]
fn open_with_options_accepts_v4_rc4_crypt_filters_with_weak_crypto_opt_in() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_v4_rc4_cf_fixture()),
        PdfOpenOptions {
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_with_options_marks_rc4_opt_in_as_weak_crypto() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r2_reader_fixture()),
        PdfOpenOptions {
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn open_with_options_accepts_r5_with_weak_crypto_opt_in_and_r6_by_default() {
    for (revision, password) in [
        (5, b"userpass".as_slice()),
        (5, b"ownerpass"),
        (6, b"userpass"),
        (6, b"ownerpass"),
    ] {
        let options = PdfOpenOptions {
            allow_weak_crypto: revision == 5,
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
// flpdf-9hc.3.21: authentication error parity for V=5 (and the V<5/V=4 path).
//
// Error-variant firing order in reader.rs `authenticate_if_encrypted`:
//   - Password authentication runs FIRST. Both user+owner failing => BadPassword.
//   - The weak-crypto gate (WeakCryptoNotAllowed) is applied only AFTER a
//     password authenticates.
//   - On the V=5 R=5/R=6 auth path, a wrong-length /U or /O entry maps to
//     BadPassword (not Malformed), matching qpdf's "invalid password".
//
// `encryption.test` parity: subtest 7 (RC4 + wrong password without
// --allow-weak-crypto => BadPassword) and subtest 8 (V=5 with a /U shorter
// than 48 bytes => BadPassword) are reproduced fixture-by-fixture below,
// together with the four regression fences (a)-(d).
// ---------------------------------------------------------------------------

/// scenario 7: an RC4 (V=1/R=2, weak) file opened with the WRONG password and
/// WITHOUT --allow-weak-crypto must report BadPassword, not
/// WeakCryptoNotAllowed. qpdf reports "invalid password" here.
#[test]
fn scenario7_rc4_wrong_password_without_weak_opt_in_is_bad_password() {
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r2_reader_fixture()),
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
        "expected BadPassword (password checked before weak-crypto gate), got {err:?}"
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
            allow_weak_crypto: true,
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
            allow_weak_crypto: true,
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

/// Regression fence (a): a CORRECT password against a weak (RC4) file WITHOUT
/// --allow-weak-crypto must still return WeakCryptoNotAllowed. Only the
/// ordering relative to BadPassword changed, not this behaviour.
#[test]
fn fence_a_correct_password_weak_not_allowed_still_weak_crypto() {
    // encrypted_r2_reader_fixture is built with the empty user password, so
    // the default (empty) password authenticates successfully.
    let err = match Pdf::open(std::io::Cursor::new(encrypted_r2_reader_fixture())) {
        Ok(_) => panic!("RC4 must be rejected without --allow-weak-crypto"),
        Err(err) => err,
    };

    assert!(
        matches!(err, Error::Encrypted(EncryptedError::WeakCryptoNotAllowed)),
        "correct password + weak + not allowed must stay WeakCryptoNotAllowed, got {err:?}"
    );
}

/// Regression fence (b): a CORRECT password against a weak (RC4) file WITH
/// --allow-weak-crypto must still open.
#[test]
fn fence_b_correct_password_weak_allowed_still_opens() {
    let pdf = Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r2_reader_fixture()),
        PdfOpenOptions {
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .expect("correct password + weak + --allow-weak-crypto must open");

    assert!(pdf.uses_weak_crypto());
}

/// Regression fence (c): a well-formed V=5 file with a WRONG password still
/// returns BadPassword (unchanged).
#[test]
fn fence_c_v5_wellformed_wrong_password_is_bad_password() {
    let err = match Pdf::open_with_options(
        std::io::Cursor::new(encrypted_r5_or_r6_minimal_pdf(5)),
        PdfOpenOptions {
            allow_weak_crypto: true,
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
fn resolve_decrypts_encrypted_strings_after_authentication() {
    let bytes = encrypted_r2_reader_fixture();
    let mut pdf = Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    let Object::Dictionary(dict) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("expected dictionary");
    };

    assert_eq!(
        dict.get("Secret"),
        Some(&Object::String(b"plain text".to_vec()))
    );
}

#[test]
fn resolve_decrypts_object_stream_before_filter_decode() {
    let bytes = encrypted_r2_reader_fixture();
    let mut pdf = Pdf::open_with_options(
        std::io::Cursor::new(bytes),
        PdfOpenOptions {
            allow_weak_crypto: true,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        pdf.resolve(ObjectRef::new(5, 0)).unwrap(),
        Object::String(b"plain text".to_vec())
    );
}

#[test]
fn v4_uses_separate_stream_and_string_crypt_filters() {
    let mut pdf = Pdf::open(std::io::Cursor::new(encrypted_v4_mixed_cf_reader_fixture())).unwrap();

    let Object::Dictionary(dict) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("expected dictionary");
    };
    assert_eq!(
        dict.get("Secret"),
        Some(&Object::String(b"plain text".to_vec()))
    );

    let Object::Stream(stream) = pdf.resolve(ObjectRef::new(4, 0)).unwrap() else {
        panic!("expected stream");
    };
    assert_eq!(stream.data, b"stream plain".to_vec());
}

#[test]
fn v4_explicit_crypt_filter_decrypts_at_filter_slot_before_flate() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_fixture(false, false),
    ))
    .unwrap();

    let Object::Stream(stream) = pdf.resolve(ObjectRef::new(4, 0)).unwrap() else {
        panic!("expected stream");
    };

    assert_eq!(stream.data, b"explicit crypt stream".to_vec());
    assert_eq!(stream.dict.get("Filter"), None);
    assert_eq!(stream.dict.get("DecodeParms"), None);
}

#[test]
fn v4_explicit_crypt_filter_decrypts_at_filter_slot_after_flate() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_fixture(false, true),
    ))
    .unwrap();

    let Object::Stream(stream) = pdf.resolve(ObjectRef::new(4, 0)).unwrap() else {
        panic!("expected stream");
    };

    assert_eq!(stream.data, b"explicit crypt stream".to_vec());
}

#[test]
fn v4_explicit_identity_crypt_filter_is_noop_before_flate() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_explicit_crypt_filter_fixture(true, false),
    ))
    .unwrap();

    let Object::Stream(stream) = pdf.resolve(ObjectRef::new(4, 0)).unwrap() else {
        panic!("expected stream");
    };

    assert_eq!(stream.data, b"explicit crypt stream".to_vec());
}

#[test]
fn r5_and_r6_identity_crypt_filters_leave_streams_and_strings_plaintext() {
    for revision in [5, 6] {
        let mut pdf = Pdf::open_with_options(
            std::io::Cursor::new(encrypted_r5_or_r6_identity_cf_minimal_pdf(revision)),
            PdfOpenOptions {
                allow_weak_crypto: revision == 5,
                password: b"userpass".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .unwrap();

        let Object::Dictionary(dict) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
            panic!("expected dictionary");
        };
        assert_eq!(
            dict.get("Secret"),
            Some(&Object::String(b"plain text".to_vec()))
        );

        let Object::Stream(stream) = pdf.resolve(ObjectRef::new(4, 0)).unwrap() else {
            panic!("expected stream");
        };
        assert_eq!(stream.data, b"stream plain".to_vec());
    }
}

#[test]
fn r5_and_r6_reject_unsupported_crypt_filter_methods() {
    for revision in [5, 6] {
        let err = match Pdf::open_with_options(
            std::io::Cursor::new(encrypted_r5_or_r6_unsupported_cf_minimal_pdf(revision)),
            PdfOpenOptions {
                password: b"userpass".to_vec(),
                ..PdfOpenOptions::default()
            },
        ) {
            Ok(_) => panic!("unsupported crypt filter should be rejected"),
            Err(err) => err,
        };

        assert!(
            matches!(
                err,
                Error::Encrypted(EncryptedError::UnsupportedHandler { .. })
            ),
            "expected UnsupportedHandler for R={revision}, got {err:?}"
        );
    }
}

#[test]
fn v4_encrypt_metadata_false_leaves_metadata_stream_plaintext() {
    let mut pdf = Pdf::open(std::io::Cursor::new(
        encrypted_v4_plaintext_metadata_stream_fixture(),
    ))
    .unwrap();

    let Object::Stream(stream) = pdf.resolve(ObjectRef::new(3, 0)).unwrap() else {
        panic!("expected metadata stream");
    };

    assert_eq!(stream.data, b"<xmpmeta>plain</xmpmeta>".to_vec());
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
                allow_weak_crypto: revision == 5,
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

fn encrypted_v1_owner_password_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let xref_offset = bytes.len();
    let trailer = b"trailer\n<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 /P -3904 /O <94e8094419662a774442fb072e3d9f19e9d130ec09a4d0061e78fe920f7ab62f> /U <13f520c882d052bf57b416b747c13979bded7ea31240fe41928852aca3894c49> >> /ID [<000102030405060708090a0b0c0d0e0f><000102030405060708090a0b0c0d0e0f>] >>\nstartxref\n";
    bytes.extend_from_slice(format!("xref\n0 3\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(trailer);
    bytes.extend_from_slice(xref_offset.to_string().as_bytes());
    bytes.extend_from_slice(b"\n%%EOF\n");
    bytes
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

fn encrypted_v4_mixed_cf_reader_fixture() -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = r4_user_key(&file_key, &id0);

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

fn encrypted_v4_rc4_cf_fixture() -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = r4_user_key(&file_key, &id0);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{obj1_offset:010} 00000 n \n{obj2_offset:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 4 /R 4 /Length 128 /P {p} /O <{}> /U <{}> /CF << /StdCF << /CFM /V2 /Length 128 >> >> /StmF /StdCF /StrF /StdCF >> /ID [<{}><{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes
}

fn encrypted_v4_explicit_crypt_filter_fixture(identity: bool, crypt_after_flate: bool) -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key(b"", &o, p, &id0);
    let u = r4_user_key(&file_key, &id0);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let stream_key = aes128_object_key(&per_object_aes_key(&file_key, 4, 0));
    let stream_data = if crypt_after_flate {
        let encrypted =
            aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], b"explicit crypt stream");
        encoder.write_all(&encrypted).unwrap();
        encoder.finish().unwrap()
    } else {
        encoder.write_all(b"explicit crypt stream").unwrap();
        let compressed = encoder.finish().unwrap();
        if identity {
            compressed
        } else {
            aes128_cbc_encrypt_with_iv(&stream_key, &[0x22; 16], &compressed)
        }
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
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter {filters} /DecodeParms {decode_parms_array} >>\nstream\n",
            stream_data.len()
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
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r4_file_key_with_encrypt_metadata(b"", &o, p, &id0, false);
    let u = r4_user_key(&file_key, &id0);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    let metadata = b"<xmpmeta>plain</xmpmeta>";
    let obj3_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n",
            metadata.len()
        )
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

fn encrypted_r2_reader_fixture() -> Vec<u8> {
    let id0 = decode_hex_fixture("000102030405060708090a0b0c0d0e0f");
    let o = [0x42u8; 32];
    let p = -3904i32;
    let file_key = r2_file_key(b"", &o, p, &id0);
    let u = rc4_crypt(&file_key, &PASSWORD_PADDING);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");

    let encrypted_secret = rc4_crypt(&per_object_key(&file_key, 3, 0), b"plain text");
    let obj3_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Secret <{}> >>\nendobj\n",
            hex_string(&encrypted_secret)
        )
        .as_bytes(),
    );

    let compressed_string = rc4_crypt(&per_object_key(&file_key, 5, 0), b"plain text");
    let obj_stream_plaintext = format!("5 0 <{}>", hex_string(&compressed_string));
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(obj_stream_plaintext.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    let encrypted_stream = rc4_crypt(&per_object_key(&file_key, 4, 0), &compressed);
    let obj4_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} /Filter /FlateDecode >>\nstream\n",
            encrypted_stream.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&encrypted_stream);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj2_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, obj4_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 3 1] /Index [0 6] /Length {} /Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 /P {p} /O <{}> /U <{}> >> /ID [<{}><{}>] >>\nstream\n",
            xref_entries.len(),
            hex_string(&o),
            hex_string(&u),
            hex_string(&id0),
            hex_string(&id0)
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

fn r2_file_key(password: &[u8], o: &[u8], p: i32, id0: &[u8]) -> Vec<u8> {
    let mut padded = [0u8; 32];
    let password_len = password.len().min(32);
    padded[..password_len].copy_from_slice(&password[..password_len]);
    padded[password_len..].copy_from_slice(&PASSWORD_PADDING[..32 - password_len]);

    let mut hasher = Md5::new();
    hasher.update(padded);
    hasher.update(o);
    hasher.update(p.to_le_bytes());
    hasher.update(id0);
    hasher.finalize()[..5].to_vec()
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

fn r4_user_key(file_key: &[u8], id0: &[u8]) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(PASSWORD_PADDING);
    hasher.update(id0);
    let mut data = hasher.finalize().to_vec();
    data = rc4_crypt(file_key, &data);
    for i in 1u8..=19 {
        let xor_key: Vec<u8> = file_key.iter().map(|byte| byte ^ i).collect();
        data = rc4_crypt(&xor_key, &data);
    }
    data.resize(32, 0);
    data
}

const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

fn per_object_key(file_key: &[u8], object_number: u32, generation: u32) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(file_key);
    hasher.update(&object_number.to_le_bytes()[..3]);
    hasher.update(&generation.to_le_bytes()[..2]);
    let digest = hasher.finalize();
    digest[..(file_key.len() + 5).min(16)].to_vec()
}

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
        .encrypt_padded_mut::<Pkcs7>(&mut data, plaintext.len())
        .unwrap();
    let mut out = iv.to_vec();
    out.extend_from_slice(encrypted);
    out
}

fn rc4_crypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut state = [0u8; 256];
    for (i, value) in state.iter_mut().enumerate() {
        *value = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256usize {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, j as usize);
    }
    let mut out = data.to_vec();
    let mut i = 0u8;
    j = 0;
    for byte in &mut out {
        i = i.wrapping_add(1);
        j = j.wrapping_add(state[i as usize]);
        state.swap(i as usize, j as usize);
        let idx = state[i as usize].wrapping_add(state[j as usize]) as usize;
        *byte ^= state[idx];
    }
    out
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

    assert_eq!(pdf.resolve(ObjectRef::new(99, 0)).unwrap(), Object::Null);
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
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(42)
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
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        parse_object(&member1).unwrap()
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(3, 0)).unwrap(),
        parse_object(&member2).unwrap()
    );
}

#[test]
fn resolves_compressed_entry_declared_in_extended_object_stream() {
    let mut pdf = Pdf::open(std::io::Cursor::new(objstm_extends_chain_pdf())).unwrap();

    assert_eq!(
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(42)
    );
    assert_eq!(
        pdf.resolve(ObjectRef::new(3, 0)).unwrap(),
        Object::Integer(99)
    );
}

fn objstm_extends_chain_pdf() -> Vec<u8> {
    decode_hex_fixture(include_str!(
        "../../../tests/fixtures/compat/objstm-extends-chain.pdf.hex"
    ))
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

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}
