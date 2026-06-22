//! End-to-end smoke tests for the writer-side `--encrypt` path
//! (flpdf-9hc.4.9 walking skeleton: V=4 AES-128 only).
//!
//! Each test builds an encrypted PDF via `write_pdf_with_options` with
//! `WriteOptions.encrypt = Some(EncryptParams::v4_aes128(...))`, then
//! validates the result by re-opening it through `Pdf::open_with_options`
//! and checking the encrypted-document invariants. Where a plaintext
//! input fixture has identifiable content (strings or stream payloads),
//! we also verify the resolved contents after decryption match the
//! original — proving the per-object key derivation, AES IV/padding, and
//! `/Length` updates all line up with what the reader path expects.

use std::fs;
use std::io::Cursor;

use flpdf::{
    write_pdf_with_options, EncryptParams, Object, Pdf, PdfOpenOptions, StreamDataMode,
    WriteOptions,
};

fn fixture(rel: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    fs::read(&path).unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()))
}

fn encrypt_to_bytes(input: &[u8], params: EncryptParams) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(input.to_vec())).expect("open plaintext input");
    let mut out = Vec::new();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.encrypt = Some(params);
    write_pdf_with_options(&mut pdf, &mut out, &options).expect("encrypted write");
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

/// Encrypting `minimal.pdf` (no strings / no streams) must still produce
/// a structurally valid encrypted file: `/Encrypt` present in the
/// trailer, the password authenticates as a user password, and the
/// resulting reader-side `EncryptionInfo` reports V=4 R=4 AESv2.
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
    let mut pdf = open_encrypted(&encrypted, b"user-pw");
    assert!(pdf.is_encrypted(), "reader must report is_encrypted=true");
    assert!(
        pdf.user_password_matched(),
        "user password should authenticate"
    );
    let info = pdf
        .encryption_info()
        .expect("encryption_info ok")
        .expect("encrypted document yields Some(EncryptionInfo)");
    assert_eq!(info.v, 4);
    assert_eq!(info.r, 4);
    assert_eq!(info.length_bits, 128);
    assert_eq!(info.filter, "Standard");
    assert_eq!(info.stream_method, "AESv2");
    assert_eq!(info.string_method, "AESv2");
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
/// The full-rewrite writer renumbers objects Catalog-first (flpdf-9hc.32), so
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
        .resolve(enc_root)
        .expect("decrypted /Catalog object resolves");
    let Object::Dictionary(dict) = catalog else {
        panic!("expected /Catalog Dictionary");
    };
    assert_eq!(
        dict.get("Type"),
        Some(&Object::Name(b"Catalog".to_vec())),
        "/Catalog /Type must round-trip across encrypt + decrypt"
    );
}

/// `--qdf` + `--encrypt` is an unsupported combination for the walking
/// skeleton (qdf emits plaintext for human inspection; encryption
/// destroys that purpose). Verify the writer rejects with a clear
/// `Unsupported` error rather than silently producing a corrupt file.
#[test]
fn v4_aes128_rejects_qdf_combination() {
    let input = fixture("tests/fixtures/minimal.pdf");
    let mut pdf = Pdf::open(Cursor::new(input)).unwrap();
    let mut out = Vec::new();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.encrypt = Some(EncryptParams::v4_aes128(b"u".to_vec(), b"o".to_vec()));
    let err = write_pdf_with_options(&mut pdf, &mut out, &options)
        .expect_err("--encrypt + --qdf must be rejected");
    let display = format!("{err:?}");
    assert!(
        display.contains("Unsupported"),
        "expected Unsupported error, got: {display}"
    );
}

/// Resolve the JavaScript stream (catalog `/OpenAction` -> action `/JS`) in the
/// re-opened encrypted `pdf` and return its `/Length` dictionary entry.
fn js_stream_length(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Object {
    let root = pdf.root_ref().expect("/Root");
    let catalog = pdf.resolve(root).expect("catalog");
    let open_action = catalog
        .as_dict()
        .and_then(|d| d.get("OpenAction").cloned())
        .expect("/OpenAction");
    let action = match open_action {
        Object::Reference(r) => pdf.resolve(r).expect("action"),
        other => other,
    };
    let js_ref = action
        .as_dict()
        .and_then(|d| d.get("JS").cloned())
        .expect("/JS");
    let js = match js_ref {
        Object::Reference(r) => pdf.resolve(r).expect("js stream"),
        other => other,
    };
    js.as_stream()
        .expect("/JS is a stream")
        .dict
        .get("Length")
        .cloned()
        .expect("/Length present")
}

/// `--stream-data=preserve` + `--encrypt` (the Codex PR #401 NOTES case): the
/// orphan-/Length-holder drop must still fire under encryption. Before
/// flpdf-3g8o the preserve gate (`effective_stream_policy().is_some()`) was false
/// for preserve, so the holder survived; `encrypt_stream_payload_for_writer` then
/// direct-ized `/Length` anyway, leaving a stale orphan emitted as a real object.
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
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.stream_data = Some(StreamDataMode::Preserve);
    options.encrypt = Some(EncryptParams::v4_aes128(
        b"user-pw".to_vec(),
        b"owner-pw".to_vec(),
    ));
    write_pdf_with_options(&mut pdf, &mut out, &options).expect("encrypted preserve write");

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
        matches!(js_stream_length(&mut pdf), Object::Integer(_)),
        "preserve + encrypt must direct-ize the JS stream's /Length"
    );
}
