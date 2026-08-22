//! qpdf 11.9.0 password-recovery parity tests, except one gate-pinning
//! test noted inline where it appears.

use flpdf::{EncryptMethod, EncryptParams, EncryptedError, Pdf, PdfOpenOptions, PdfWriter};
use std::io::Cursor;

fn minimal_fixture() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
    )
    .unwrap()
}

fn v4_encrypted_with_raw_password(password: &[u8]) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(minimal_fixture())).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(
        password.to_vec(),
        b"owner".to_vec(),
    ));
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    writer.get_buffer().unwrap()
}

fn v3_rc4_encrypted_with_raw_password(password: &[u8]) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(minimal_fixture())).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_encryption_parameters(EncryptParams::rc4(
        EncryptMethod::V2Rc4128,
        password.to_vec(),
        b"owner".to_vec(),
    ));
    writer.set_output_memory().unwrap();
    writer.write().unwrap();
    writer.get_buffer().unwrap()
}

#[test]
fn reader_recovers_pdfdoc_password_from_utf8_input() {
    let encrypted = v4_encrypted_with_raw_password(b"caf\xe9");
    let result = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: "café".as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    );

    if let Err(error) = result {
        panic!("qpdf-compatible password recovery should authenticate: {error}");
    }
}

#[test]
fn reader_recovers_mac_roman_password_from_utf8_input() {
    let encrypted = v4_encrypted_with_raw_password(b"caf\x8e");
    let result = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: "café".as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    );

    if let Err(error) = result {
        panic!("qpdf-compatible MacRoman password recovery should authenticate: {error}");
    }
}

// NOT a qpdf-parity assertion, unlike the rest of this file: live qpdf
// 11.9.0 opens this same RC4 file with the correct password and no
// --allow-weak-crypto (its allow_weak_crypto concept lives only on the
// QPDFJob write path, QPDFJob.cc:2725-2763; the QPDF read/authenticate
// path has no such gate at all). flpdf's library-level
// WeakCryptoNotAllowed on read is an flpdf-only gate whose qpdf basis is
// under review (flpdf-zzdz). This test pins flpdf's current behavior so a
// change to that gate is a deliberate, visible diff here -- it does not
// assert that behavior is qpdf-correct.
#[test]
fn reader_stops_recovery_at_flpdfs_own_weak_crypto_gate_pending_flpdf_zzdz() {
    let encrypted = v3_rc4_encrypted_with_raw_password(b"caf\xe9");
    let result = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: "café".as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    );

    assert!(matches!(
        result,
        Err(flpdf::Error::Encrypted(
            EncryptedError::WeakCryptoNotAllowed
        ))
    ));
}
