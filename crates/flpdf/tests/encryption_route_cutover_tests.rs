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
