//! qpdf 11.9.0 password-recovery parity tests.

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

#[test]
fn reader_stops_recovery_when_a_repaired_password_hits_weak_crypto_policy() {
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
