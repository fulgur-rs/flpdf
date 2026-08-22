//! qpdf 11.9.0 password-recovery parity tests.

use flpdf::{EncryptMethod, EncryptParams, Pdf, PdfOpenOptions, PdfWriter};
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
fn reader_accepts_rc4_with_correct_password_without_write_opt_in() {
    let encrypted = v3_rc4_encrypted_with_raw_password(b"password");
    let pdf = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: b"password".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .expect("qpdf accepts an authenticated RC4 read without its write-only opt-in");

    assert!(pdf.uses_weak_crypto());
}

#[test]
fn reader_recovery_accepts_rc4_without_write_opt_in() {
    let encrypted = v3_rc4_encrypted_with_raw_password(b"caf\xe9");
    let result = Pdf::open_with_options(
        Cursor::new(encrypted),
        PdfOpenOptions {
            password: "café".as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        },
    );

    let pdf = result.expect("qpdf accepts authenticated RC4 reads without its write opt-in");
    assert!(pdf.uses_weak_crypto());
}
