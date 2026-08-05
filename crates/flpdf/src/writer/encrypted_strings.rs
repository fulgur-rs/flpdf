//! qpdf correspondence: QPDFWriter.cc:1567-1599 and 1761-1796 encrypted string emission;
//! QPDFWriter.cc:2244-2256 `/Encrypt` dictionary emission.

use crate::object::{write_hex_string, write_name_escaped, write_string_value};
use crate::security::standard::{encrypt_cipher_bytes, ObjectKeyAlg, StringEncryptCipher};
use crate::writer::encryption_state::WriterEncryptionState;
use crate::writer::{EncryptionContext, WriteCipher};
use crate::{Dictionary, Object, ObjectRef};

/// Writer-owned adapter that encrypts strings while an emitted object's data
/// key is active, without changing the source [`Object`] tree.
#[allow(dead_code)] // Wired into the full and linearized production loops in later tasks.
pub(crate) struct EncryptedStringEmitter {
    state: WriterEncryptionState,
    cipher: WriteCipher,
    static_aes_iv: bool,
}

#[allow(dead_code)] // Wired into the full and linearized production loops in later tasks.
impl EncryptedStringEmitter {
    pub(crate) fn from_context(ctx: &EncryptionContext) -> Self {
        Self {
            state: WriterEncryptionState::new(
                true,
                ctx.file_key.clone(),
                crate::writer::cipher_needs_aes_iv(ctx.cipher),
                ctx.encryption_v,
                ctx.encryption_r,
            ),
            cipher: ctx.cipher,
            static_aes_iv: ctx.static_aes_iv,
        }
    }

    pub(crate) fn write_object(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &Object,
        qdf: bool,
    ) -> crate::Result<()> {
        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(state, cipher, static_aes_iv, out, plaintext)
                };
                if qdf {
                    object.try_write_pdf_qdf_with_string_writer(out, 0, &mut write_string)
                } else {
                    object.try_write_pdf_with_string_writer(out, &mut write_string)
                }
            })
    }

    pub(crate) fn write_stream_dict(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        dict: &Dictionary,
        qdf: bool,
        refiltered: bool,
    ) -> crate::Result<()> {
        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(state, cipher, static_aes_iv, out, plaintext)
                };
                if qdf {
                    dict.try_write_pdf_stream_qdf_with_string_writer(out, 0, &mut write_string)
                } else {
                    dict.try_write_pdf_stream_with_string_writer(out, refiltered, &mut write_string)
                }
            })
    }

    #[cfg(test)]
    fn current_data_key_for_test(&self) -> Option<&[u8]> {
        self.state.current_data_key()
    }
}

fn write_encrypted_or_plain_string(
    state: &WriterEncryptionState,
    cipher: WriteCipher,
    static_aes_iv: bool,
    out: &mut Vec<u8>,
    plaintext: &[u8],
) -> crate::Result<()> {
    let Some(data_key) = state.current_data_key() else {
        write_string_value(out, plaintext);
        return Ok(());
    };
    let ciphertext = encrypt_string(cipher, static_aes_iv, data_key, plaintext)?;
    serialize_encrypted_string(out, &ciphertext, crate::writer::cipher_needs_aes_iv(cipher));
    Ok(())
}

fn encrypt_string(
    cipher: WriteCipher,
    static_aes_iv: bool,
    data_key: &[u8],
    plaintext: &[u8],
) -> crate::Result<Vec<u8>> {
    let mut bytes = plaintext.to_vec();
    let mut iv = if static_aes_iv {
        crate::pipeline::aes::static_initialization_vector()
    } else {
        [0; 16]
    };
    if crate::writer::cipher_needs_aes_iv(cipher) && !static_aes_iv {
        getrandom::getrandom(&mut iv).map_err(|error| {
            crate::Error::Unsupported(format!(
                "OS CSPRNG (getrandom) unavailable for AES IV generation: {error}"
            ))
        })?;
    }
    match cipher {
        WriteCipher::PerObject(ObjectKeyAlg::Rc4) => {
            encrypt_cipher_bytes(&mut bytes, StringEncryptCipher::Rc4 { key: data_key }, &iv)?;
        }
        WriteCipher::PerObject(ObjectKeyAlg::Aes) => {
            let key: &[u8; 16] = data_key.try_into().map_err(|_| {
                crate::Error::Unsupported("V=4 AES-128 data key is not 16 bytes".to_string())
            })?;
            encrypt_cipher_bytes(&mut bytes, StringEncryptCipher::Aes128 { key }, &iv)?;
        }
        WriteCipher::FileKeyAes256 => {
            let key: &[u8; 32] = data_key.try_into().map_err(|_| {
                crate::Error::Unsupported("V=5 AES-256 data key is not 32 bytes".to_string())
            })?;
            encrypt_cipher_bytes(&mut bytes, StringEncryptCipher::Aes256 { key }, &iv)?;
        }
    }
    Ok(bytes)
}

/// Serialize encrypted bytes using qpdf's cipher-specific representation:
/// AES ciphertext is always hexadecimal; RC4 retains normal string heuristics.
#[allow(dead_code)] // Wired into the full and linearized production loops in later tasks.
pub(crate) fn serialize_encrypted_string(out: &mut Vec<u8>, ciphertext: &[u8], use_aes: bool) {
    if use_aes {
        write_hex_string(out, ciphertext);
    } else {
        write_string_value(out, ciphertext);
    }
}

/// Serialize the `/Encrypt` dictionary with qpdf's compact direct layout.
///
/// The five binary security-handler fields are hexadecimal even when their
/// bytes are printable. This policy deliberately applies only to direct
/// dictionary entries: nested values retain ordinary object serialization.
pub(crate) fn write_encryption_dictionary(out: &mut Vec<u8>, dict: &Dictionary) {
    const HEX_ENCRYPT_KEYS: [&[u8]; 5] = [b"O", b"U", b"OE", b"UE", b"Perms"];

    out.extend_from_slice(b"<<");
    for (key, value) in dict.iter() {
        out.extend_from_slice(b" /");
        write_name_escaped(out, key);
        out.push(b' ');
        match value {
            Object::String(bytes) if HEX_ENCRYPT_KEYS.contains(&key) => {
                write_hex_string(out, bytes);
            }
            _ => value.write_pdf(out),
        }
    }
    out.extend_from_slice(b" >>");
}

#[cfg(test)]
mod tests {
    use super::{serialize_encrypted_string, write_encryption_dictionary, EncryptedStringEmitter};
    use crate::encrypt_setup::{CopyEncryptionSource, EncryptMethod, EncryptParams};
    use crate::security::standard::{
        decrypt_cipher_bytes, per_object_key, ObjectKeyAlg, StringCipher,
    };
    use crate::writer::{
        build_copy_encryption_context, build_encryption_context, EncryptionContext, WriteCipher,
        WriteOptions,
    };
    use crate::{Dictionary, Object, ObjectRef};

    fn fixed_context(
        file_key: Vec<u8>,
        cipher: WriteCipher,
        encryption_v: i32,
        encryption_r: i32,
    ) -> EncryptionContext {
        EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key,
            cipher,
            encryption_v,
            encryption_r,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        }
    }

    fn parse_string(bytes: &[u8]) -> Vec<u8> {
        match crate::parse_object(bytes).expect("emitted string syntax must parse") {
            Object::String(value) => value,
            other => panic!("expected emitted string, got {other:?}"),
        }
    }

    fn parse_dict_string(bytes: &[u8], key: &str) -> Vec<u8> {
        match crate::parse_object(bytes).expect("emitted dictionary syntax must parse") {
            Object::Dictionary(dict) => match dict.get(key) {
                Some(Object::String(value)) => value.clone(),
                other => panic!("expected /{key} string, got {other:?}"),
            },
            other => panic!("expected emitted dictionary, got {other:?}"),
        }
    }

    fn decrypt_emitted_string(
        serialized: &[u8],
        emitted_ref: ObjectRef,
        file_key: &[u8],
        cipher: WriteCipher,
    ) -> Vec<u8> {
        decrypt_ciphertext(parse_string(serialized), emitted_ref, file_key, cipher)
    }

    fn decrypt_ciphertext(
        mut ciphertext: Vec<u8>,
        emitted_ref: ObjectRef,
        file_key: &[u8],
        cipher: WriteCipher,
    ) -> Vec<u8> {
        match cipher {
            WriteCipher::PerObject(alg) => {
                let data_key = per_object_key(
                    file_key,
                    emitted_ref.number,
                    u32::from(emitted_ref.generation),
                    alg,
                );
                match alg {
                    ObjectKeyAlg::Rc4 => {
                        decrypt_cipher_bytes(&mut ciphertext, StringCipher::Rc4 { key: &data_key })
                            .expect("RC4 ciphertext must decrypt")
                    }
                    ObjectKeyAlg::Aes => {
                        let key: [u8; 16] = data_key
                            .try_into()
                            .expect("fixed AES-128 key must derive 16 bytes");
                        decrypt_cipher_bytes(&mut ciphertext, StringCipher::Aes128 { key: &key })
                            .expect("AES-128 ciphertext must decrypt");
                    }
                }
            }
            WriteCipher::FileKeyAes256 => {
                let key: [u8; 32] = file_key
                    .try_into()
                    .expect("fixed AES-256 file key must be 32 bytes");
                decrypt_cipher_bytes(&mut ciphertext, StringCipher::Aes256 { key: &key })
                    .expect("AES-256 ciphertext must decrypt");
            }
        }
        ciphertext
    }

    #[test]
    fn encrypted_representation_is_cipher_driven_not_content_driven() {
        let mut aes = Vec::new();
        serialize_encrypted_string(&mut aes, b"printable", true);
        assert_eq!(aes, b"<7072696e7461626c65>");

        let mut rc4 = Vec::new();
        serialize_encrypted_string(&mut rc4, b"printable", false);
        assert_eq!(rc4, b"(printable)");
    }

    #[test]
    fn encryption_dictionary_hex_encodes_only_the_five_direct_binary_keys() {
        let mut nested = Dictionary::new();
        nested.insert("O", Object::String(b"nested".to_vec()));

        let mut dict = Dictionary::new();
        for key in ["O", "U", "OE", "UE", "Perms"] {
            dict.insert(key, Object::String(b"printable".to_vec()));
        }
        dict.insert("Custom", Object::String(b"custom".to_vec()));
        dict.insert("Nested", Object::Dictionary(nested));

        let mut wire = Vec::new();
        write_encryption_dictionary(&mut wire, &dict);

        for key in [b"O".as_slice(), b"U", b"OE", b"UE", b"Perms"] {
            let expected = [key, b" <7072696e7461626c65>"].concat();
            assert!(
                wire.windows(expected.len()).any(|part| part == expected),
                "direct /{} must be hexadecimal: {}",
                String::from_utf8_lossy(key),
                String::from_utf8_lossy(&wire),
            );
        }
        assert!(wire
            .windows(b"/Custom (custom)".len())
            .any(|part| part == b"/Custom (custom)"));
        assert!(wire
            .windows(b"/Nested << /O (nested) >>".len())
            .any(|part| part == b"/Nested << /O (nested) >>"));
    }

    #[test]
    fn aes128_object_emission_round_trips_without_mutating_the_object() {
        let emitted_ref = ObjectRef::new(10, 0);
        let file_key = vec![0x42; 16];
        let cipher = WriteCipher::PerObject(ObjectKeyAlg::Aes);
        let context = fixed_context(file_key.clone(), cipher, 4, 4);
        let object = Object::String(b"AES-128 plaintext".to_vec());
        let before = object.clone();
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        emitter
            .write_object(&mut out, emitted_ref, None, &object, false)
            .expect("AES-128 object emission");

        assert!(out.starts_with(b"<"), "AES ciphertext must use hex syntax");
        assert_eq!(
            decrypt_emitted_string(&out, emitted_ref, &file_key, cipher),
            b"AES-128 plaintext"
        );
        assert_eq!(object, before, "emission must not mutate the object tree");
    }

    #[test]
    fn rc4_qdf_object_emission_round_trips() {
        let emitted_ref = ObjectRef::new(7, 0);
        let file_key = vec![0x11, 0x22, 0x33, 0x44, 0x55];
        let cipher = WriteCipher::PerObject(ObjectKeyAlg::Rc4);
        let context = fixed_context(file_key.clone(), cipher, 1, 2);
        let object = Object::String(b"RC4 plaintext".to_vec());
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        emitter
            .write_object(&mut out, emitted_ref, None, &object, true)
            .expect("RC4 QDF object emission");

        assert_eq!(
            decrypt_emitted_string(&out, emitted_ref, &file_key, cipher),
            b"RC4 plaintext"
        );
    }

    #[test]
    fn aes256_nested_qdf_object_emission_round_trips() {
        let emitted_ref = ObjectRef::new(44, 0);
        let file_key: Vec<u8> = (0..32).collect();
        let cipher = WriteCipher::FileKeyAes256;
        let context = fixed_context(file_key.clone(), cipher, 5, 6);
        let mut dict = Dictionary::new();
        dict.insert("Value", Object::String(b"AES-256 plaintext".to_vec()));
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        emitter
            .write_object(&mut out, emitted_ref, None, &Object::Dictionary(dict), true)
            .expect("AES-256 QDF object emission");

        assert_eq!(
            decrypt_ciphertext(
                parse_dict_string(&out, "Value"),
                emitted_ref,
                &file_key,
                cipher,
            ),
            b"AES-256 plaintext"
        );
    }

    #[test]
    fn stream_dictionary_callbacks_encrypt_compact_and_qdf_strings() {
        let emitted_ref = ObjectRef::new(12, 0);
        let file_key = vec![0x21; 16];
        let cipher = WriteCipher::PerObject(ObjectKeyAlg::Rc4);
        let context = fixed_context(file_key.clone(), cipher, 2, 3);
        let mut dict = Dictionary::new();
        dict.insert("Label", Object::String(b"stream label".to_vec()));
        dict.insert("Length", Object::Integer(5));

        for qdf in [false, true] {
            let mut emitter = EncryptedStringEmitter::from_context(&context);
            let mut out = Vec::new();
            emitter
                .write_stream_dict(&mut out, emitted_ref, None, &dict, qdf, true)
                .expect("stream dictionary emission");

            let ciphertext = parse_dict_string(&out, "Label");
            assert_eq!(
                decrypt_ciphertext(ciphertext, emitted_ref, &file_key, cipher),
                b"stream label"
            );
        }
    }

    #[test]
    fn object_stream_member_emits_plaintext_without_an_individual_key() {
        let context = fixed_context(
            vec![0x42; 16],
            WriteCipher::PerObject(ObjectKeyAlg::Aes),
            4,
            4,
        );
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        emitter
            .write_object(
                &mut out,
                ObjectRef::new(10, 0),
                Some(3),
                &Object::String(b"printable".to_vec()),
                false,
            )
            .expect("ObjStm member emission");

        assert_eq!(out, b"(printable)");
    }

    #[test]
    fn callback_error_clears_the_current_data_key() {
        let context = fixed_context(vec![0x5a; 31], WriteCipher::FileKeyAes256, 5, 6);
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        let error = emitter
            .write_object(
                &mut out,
                ObjectRef::new(44, 0),
                None,
                &Object::String(b"plaintext".to_vec()),
                false,
            )
            .expect_err("invalid AES-256 key must fail in the string callback");

        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message == "V=5 AES-256 data key is not 32 bytes"
        ));
        assert_eq!(emitter.current_data_key_for_test(), None);
    }

    #[test]
    fn encryption_context_records_exact_standard_handler_revision() {
        let cases = [
            (EncryptParams::v4_aes128(b"u", b"o"), (4, 4)),
            (EncryptParams::v5_r6(b"u", b"o"), (5, 6)),
            (EncryptParams::v5_r5(b"u", b"o"), (5, 5)),
            (
                EncryptParams::rc4(EncryptMethod::V1Rc440, b"u", b"o"),
                (1, 2),
            ),
            (
                EncryptParams::rc4(EncryptMethod::V2Rc4128, b"u", b"o"),
                (2, 3),
            ),
            (
                EncryptParams::rc4(EncryptMethod::V4Rc4128, b"u", b"o"),
                (4, 4),
            ),
        ];

        for (params, expected) in cases {
            let context = build_encryption_context(
                &WriteOptions::default(),
                &params,
                10,
                None,
                b"0123456789abcdef",
            )
            .expect("encryption context");
            assert_eq!(
                (context.encryption_v, context.encryption_r),
                expected,
                "wrong V/R for {:?}",
                params.method
            );
        }
    }

    #[test]
    fn copied_v4_aes_context_records_v4_r4() {
        let source = CopyEncryptionSource {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x31; 16],
            id0: b"0123456789abcdef".to_vec(),
            object_key_alg: ObjectKeyAlg::Aes,
        };

        let context = build_copy_encryption_context(&source, &WriteOptions::default(), 10).unwrap();

        assert_eq!((context.encryption_v, context.encryption_r), (4, 4));
    }
}
