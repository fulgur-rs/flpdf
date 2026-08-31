use std::fs;
use std::path::Path;

#[test]
fn encryption_ownership_has_qpdf_shaped_tree_without_legacy_routes() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    for required in [
        "encryption.rs",
        "encryption/state.rs",
        "encryption/standard.rs",
        "encryption/keys.rs",
        "encryption/crypt_filters.rs",
        "encryption/password.rs",
        "encryption/permissions.rs",
        "encryption/primitives.rs",
        "encryption/rc4.rs",
    ] {
        assert!(
            src.join(required).is_file(),
            "missing canonical route: {required}"
        );
    }

    for removed in [
        "encrypt_setup.rs",
        "permissions.rs",
        "security.rs",
        "security",
        "security/mod.rs",
        "security/password.rs",
        "security/primitives.rs",
        "security/rc4.rs",
        "security/standard.rs",
    ] {
        assert!(
            !src.join(removed).exists(),
            "legacy route remains: {removed}"
        );
    }

    let lib = fs::read_to_string(src.join("lib.rs")).expect("read crate root");
    assert!(lib.contains("pub mod encryption;"));
    assert!(!lib.contains("pub mod encrypt_setup;"));
    assert!(!lib.contains("pub mod permissions;"));
    assert!(!lib.contains("pub(crate) mod security;"));
}

#[test]
fn aes_production_consumers_have_one_pipeline_owner() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let standard = std::fs::read_to_string(src.join("encryption/standard.rs"))
        .expect("read standard encryption implementation");
    let state = std::fs::read_to_string(src.join("encryption/state.rs"))
        .expect("read encryption state implementation");
    let primitives = std::fs::read_to_string(src.join("encryption/primitives.rs"))
        .expect("read encryption primitives implementation");

    for forbidden in [
        "cbc::{Decryptor, Encryptor}",
        "Decryptor<",
        "Encryptor<",
        "aes128_cbc_encrypt_with_iv",
        "aes256_cbc_encrypt_with_iv",
        "aes256_ecb_encrypt_block",
    ] {
        assert!(
            !standard.contains(forbidden),
            "direct AES orchestration remains in standard.rs: {forbidden}"
        );
    }
    assert!(
        standard.contains("PlAesPdf"),
        "standard.rs must call the canonical AES pipeline"
    );
    assert!(
        state.contains("PlAesPdf"),
        "encryption state must verify /Perms through the canonical AES pipeline"
    );
    assert!(
        !primitives.contains("aes256_ecb_encrypt_block")
            && !primitives.contains("aes256_ecb_decrypt_block"),
        "AES block helpers remain outside pipeline/aes.rs"
    );
}
