//! qpdf correspondence: QPDFWriter.cc:785-803 encryption-dictionary binary-key hex selection, QPDFWriter.cc:1567-1599 string-unparse, QPDFWriter.cc:1761-1796 object data-key lifecycle, and QPDFWriter.cc:2244-2256 encryption-dictionary emission responsibilities.

use crate::encryption::standard::{encrypt_cipher_bytes, ObjectKeyAlg, StringEncryptCipher};
use crate::object::{write_hex_string, write_name_escaped, write_string_value};
use crate::object_handle::ObjectHandle;
use crate::writer::encryption_state::WriterEncryptionState;
use crate::writer::{EncryptionContext, ObjectWriterEmission, WriteCipher, WriterOptions};
use crate::{Dictionary, Object, ObjectRef};

type AesIvGenerator = dyn FnMut(&mut [u8; 16]) -> Result<(), getrandom::Error>;

#[derive(Clone, Copy)]
pub(crate) struct StreamDictOptions {
    qdf: bool,
    refiltered: bool,
    encrypt_strings: bool,
}

impl StreamDictOptions {
    pub(crate) const fn new(qdf: bool, refiltered: bool, encrypt_strings: bool) -> Self {
        Self {
            qdf,
            refiltered,
            encrypt_strings,
        }
    }
}

/// Writer-owned adapter that encrypts strings while an emitted object's data
/// key is active, without changing the source [`Object`] tree.
pub(crate) struct EncryptedStringEmitter {
    state: WriterEncryptionState,
    cipher: WriteCipher,
    static_aes_iv: bool,
    aes_iv_generator: Box<AesIvGenerator>,
    encrypt_ref: ObjectRef,
}

impl EncryptedStringEmitter {
    pub(crate) fn from_context(ctx: &EncryptionContext) -> Self {
        Self::from_context_with_boxed_iv_generator(ctx, Box::new(|iv| getrandom::fill(iv)))
    }

    fn from_context_with_boxed_iv_generator(
        ctx: &EncryptionContext,
        aes_iv_generator: Box<AesIvGenerator>,
    ) -> Self {
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
            aes_iv_generator,
            encrypt_ref: ctx.encrypt_ref,
        }
    }

    #[cfg(test)]
    fn from_context_with_iv_generator(
        ctx: &EncryptionContext,
        aes_iv_generator: impl FnMut(&mut [u8; 16]) -> Result<(), getrandom::Error> + 'static,
    ) -> Self {
        Self::from_context_with_boxed_iv_generator(ctx, Box::new(aes_iv_generator))
    }

    #[cfg(test)]
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
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                if qdf {
                    object.try_write_pdf_qdf_with_string_writer(out, 0, &mut write_string)
                } else {
                    object.try_write_pdf_with_string_writer(out, &mut write_string)
                }
            })
    }

    /// Emit an ObjectHandle tree with qpdf's per-object string-encryption
    /// lifecycle. The handle walker retains indirect identity and the source
    /// graph is never materialized or mutated. `/Encrypt` itself is emitted
    /// through [`write_encryption_dictionary_handle`] and therefore stays
    /// plaintext.
    #[allow(dead_code)] // consumed by the writer ObjectHandle cutover
    pub(crate) fn write_handle_object(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        qdf: bool,
    ) -> crate::Result<()> {
        if emitted_ref == self.encrypt_ref {
            return write_encryption_dictionary_handle(out, object);
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                if qdf {
                    object.write_object_qdf_with_string_writer(out, 0, &mut write_string)
                } else {
                    object.write_object_with_string_writer(out, &mut write_string)
                }
            })
    }

    /// Handle-based object emission with the writer's output reference map and
    /// qpdf's removed-reference null policy threaded through the same string
    /// encryption lifecycle.
    #[allow(clippy::too_many_arguments)] // emission identity, qdf layout, mapping, and encryption remain separate qpdf dimensions
    pub(crate) fn write_handle_object_with_ref_map(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        qdf: bool,
        map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<ObjectRef>,
    ) -> crate::Result<()> {
        if emitted_ref == self.encrypt_ref {
            return write_encryption_dictionary_handle(out, object);
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                if qdf {
                    object.write_object_qdf_with_ref_map_and_removed_with_string_writer(
                        out,
                        0,
                        map,
                        removed_refs,
                        &mut write_string,
                    )
                } else {
                    object.write_object_with_ref_map_and_removed_with_string_writer(
                        out,
                        map,
                        removed_refs,
                        &mut write_string,
                    )
                }
            })
    }

    /// Emit a page or `/Contents` array holder that owns direct streams while
    /// keeping qpdf's per-object string data key active. The stream payload is
    /// deliberately handled by the content-container helper's raw-stream
    /// route; only dictionary strings use this callback, matching the legacy
    /// direct-stream writer's encryption boundary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_handle_content_container_with_ref_map(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        options: &WriterOptions,
        map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<ObjectRef>,
    ) -> crate::Result<()> {
        if emitted_ref == self.encrypt_ref {
            return write_encryption_dictionary_handle(out, object); // cov:ignore: the pre-scanned page-content container cannot be the /Encrypt object
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                crate::writer::plain::body::emit_content_container_from_handle_with_ref_map_and_string_writer(
                    object,
                    options,
                    out,
                    map,
                    removed_refs,
                    &mut write_string,
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn write_stream_dict(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        dict: &Dictionary,
        options: StreamDictOptions,
    ) -> crate::Result<()> {
        if !options.encrypt_strings {
            if options.qdf {
                dict.write_pdf_stream_qdf(out, 0);
            } else {
                dict.write_pdf_stream(out, options.refiltered);
            }
            return Ok(());
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                if options.qdf {
                    dict.try_write_pdf_stream_qdf_with_string_writer(out, 0, &mut write_string)
                } else {
                    dict.try_write_pdf_stream_with_string_writer(
                        out,
                        options.refiltered,
                        &mut write_string,
                    )
                }
            })
    }

    /// Emit an ObjectHandle stream dictionary with the same encryption switch
    /// used by the legacy writer. Stream payload bytes are intentionally not
    /// handled here; the caller must put the handle's payload through the
    /// canonical stream pipeline, and `encrypt_strings` is the cleartext
    /// metadata exemption selected by qpdf's stream writer.
    #[allow(dead_code)] // consumed by the writer ObjectHandle cutover
    pub(crate) fn write_handle_stream_dict(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        dict: &ObjectHandle,
        options: StreamDictOptions,
    ) -> crate::Result<()> {
        if !options.encrypt_strings {
            if options.qdf {
                return dict.write_stream_body_qdf(out, 0);
            }
            return dict.write_stream_body(out, options.refiltered);
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                if options.qdf {
                    dict.write_stream_body_qdf_with_string_writer(out, 0, &mut write_string)
                } else {
                    dict.write_stream_body_with_string_writer(
                        out,
                        options.refiltered,
                        &mut write_string,
                    )
                }
            })
    }

    /// Handle-based stream-dictionary emission with output-reference mapping
    /// and removed-reference visibility. `length_ref` is used only by QDF's
    /// synthetic stream-length holder.
    #[allow(clippy::too_many_arguments)] // keeps the qpdf stream-dictionary contract explicit at this boundary
    pub(crate) fn write_handle_stream_dict_with_ref_map(
        &mut self,
        out: &mut Vec<u8>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        dict: &ObjectHandle,
        options: StreamDictOptions,
        map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> crate::Result<()> {
        if !options.encrypt_strings {
            if options.qdf {
                return dict.write_stream_body_qdf_with_ref_map_and_removed_and_length(
                    out,
                    0,
                    map,
                    removed_refs,
                    length_ref,
                );
            }
            return dict.write_stream_body_with_ref_map_and_removed(
                out,
                options.refiltered,
                map,
                removed_refs,
            );
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut Vec<u8>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                if options.qdf {
                    dict.write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                        out,
                        0,
                        map,
                        removed_refs,
                        length_ref,
                        &mut write_string,
                    )
                } else {
                    dict.write_stream_body_with_ref_map_and_removed_with_string_writer(
                        out,
                        options.refiltered,
                        map,
                        removed_refs,
                        &mut write_string,
                    )
                }
            })
    }

    #[cfg(test)]
    fn current_data_key_for_test(&self) -> Option<&[u8]> {
        self.state.current_data_key()
    }
}

impl EncryptionContext {
    /// Return the context's `/Encrypt` snapshot as a canonical direct handle
    /// tree. This is an additive view for the future ObjectHandle writer route;
    /// the existing legacy dictionary remains owned by the current consumer
    /// until that cutover lands.
    #[allow(dead_code)]
    pub(crate) fn encrypt_dict_handle(&self) -> ObjectHandle {
        let entries = self
            .encrypt_dict
            .iter()
            .map(|(key, value)| (key.to_vec(), object_to_handle(value)))
            .collect();
        ObjectHandle::dictionary(entries)
    }
}

fn object_to_handle(object: &Object) -> ObjectHandle {
    match object {
        Object::Null => ObjectHandle::null(),
        Object::Boolean(value) => ObjectHandle::boolean(*value),
        Object::Integer(value) => ObjectHandle::integer(*value),
        Object::Real(value) => ObjectHandle::real(*value),
        Object::RealLiteral { value, literal } => {
            ObjectHandle::real_literal(*value, literal.clone())
        }
        Object::Name(value) => ObjectHandle::name(value.clone()),
        Object::String(value) => ObjectHandle::string(value.clone()),
        Object::Reference(value) => {
            ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(*value))
        }
        Object::Operator(value) => ObjectHandle::operator(value.clone()),
        Object::InlineImage(value) => ObjectHandle::inline_image(value.clone()),
        Object::Array(values) => ObjectHandle::array(values.iter().map(object_to_handle).collect()),
        Object::Dictionary(dict) => ObjectHandle::dictionary(
            dict.iter()
                .map(|(key, value)| (key.to_vec(), object_to_handle(value)))
                .collect(),
        ),
        Object::Stream(stream) => ObjectHandle::stream(
            object_to_handle(&Object::Dictionary(stream.dict.clone())),
            std::rc::Rc::new(stream.data.clone()),
        ),
    }
}

fn write_encrypted_or_plain_string(
    state: &WriterEncryptionState,
    cipher: WriteCipher,
    static_aes_iv: bool,
    aes_iv_generator: &mut AesIvGenerator,
    out: &mut Vec<u8>,
    plaintext: &[u8],
) -> crate::Result<()> {
    let Some(data_key) = state.current_data_key() else {
        write_string_value(out, plaintext);
        return Ok(());
    };
    let ciphertext = encrypt_string(cipher, static_aes_iv, aes_iv_generator, data_key, plaintext)?;
    serialize_encrypted_string(out, &ciphertext, crate::writer::cipher_needs_aes_iv(cipher));
    Ok(())
}

fn encrypt_string(
    cipher: WriteCipher,
    static_aes_iv: bool,
    aes_iv_generator: &mut AesIvGenerator,
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
        fill_aes_iv(aes_iv_generator, &mut iv)?;
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

fn fill_aes_iv(aes_iv_generator: &mut AesIvGenerator, iv: &mut [u8; 16]) -> crate::Result<()> {
    aes_iv_generator(iv).map_err(|error| {
        crate::Error::Unsupported(format!(
            "OS CSPRNG (getrandom) unavailable for AES IV generation: {error}"
        ))
    })
}

/// Serialize encrypted bytes using qpdf's cipher-specific representation:
/// AES ciphertext is always hexadecimal; RC4 retains normal string heuristics.
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

/// Serialize an ObjectHandle-backed `/Encrypt` dictionary with the same
/// direct-entry hex policy as [`write_encryption_dictionary`]. The handle
/// tree is kept as the source of truth; nested values and indirect references
/// use the canonical ObjectHandle writer rather than materializing `Object`.
pub(crate) fn write_encryption_dictionary_handle(
    out: &mut Vec<u8>,
    handle: &ObjectHandle,
) -> crate::Result<()> {
    const HEX_ENCRYPT_KEYS: [&[u8]; 5] = [b"/O", b"/U", b"/OE", b"/UE", b"/Perms"];

    let Some(entries) = handle.as_dictionary() else {
        return Err(crate::Error::System(
            "encryption handle does not contain a dictionary".to_string(),
        ));
    };

    out.extend_from_slice(b"<<");
    for (key, value) in entries {
        let key_without_slash = key.strip_prefix(b"/").unwrap_or(&key);
        out.extend_from_slice(b" /");
        write_name_escaped(out, key_without_slash);
        out.push(b' ');
        if HEX_ENCRYPT_KEYS.contains(&key.as_slice()) {
            if let Some(bytes) = value.as_string() {
                write_hex_string(out, &bytes);
                continue;
            }
        }
        value.write_object(out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        serialize_encrypted_string, write_encryption_dictionary,
        write_encryption_dictionary_handle, EncryptedStringEmitter, StreamDictOptions,
    };
    use crate::encryption::standard::{
        decrypt_cipher_bytes, per_object_key, ObjectKeyAlg, StringCipher,
    };
    use crate::encryption::{CopyEncryptionSource, EncryptMethod, EncryptParams};
    use crate::writer::{
        build_copy_encryption_context, build_encryption_context, EncryptionContext,
        ObjectWriterEmission, WriteCipher, WriterOptions,
    };
    use crate::{Dictionary, Object, ObjectHandle, ObjectRef, Stream};

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
        crate::parse_object(bytes)
            .expect("emitted string syntax must parse")
            .as_string()
            .expect("emitted object must be a string")
            .to_vec()
    }

    fn parse_dict_string(bytes: &[u8], key: &str) -> Vec<u8> {
        let object = crate::parse_object(bytes).expect("emitted dictionary syntax must parse");
        object
            .as_dict()
            .expect("emitted object must be a dictionary")
            .get(key)
            .and_then(Object::as_string)
            .expect("requested encryption dictionary key must be a string")
            .to_vec()
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
                "direct binary encryption-dictionary key must be hexadecimal",
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
    fn handle_encryption_dictionary_matches_legacy_direct_encoding() {
        let mut nested = Dictionary::new();
        nested.insert("O", Object::String(b"nested".to_vec()));

        let mut dict = Dictionary::new();
        for key in ["O", "U", "OE", "UE", "Perms"] {
            dict.insert(key, Object::String(b"printable".to_vec()));
        }
        dict.insert("Custom", Object::String(b"custom".to_vec()));
        dict.insert("Nested", Object::Dictionary(nested));

        let mut context = fixed_context(
            vec![0x42; 16],
            WriteCipher::PerObject(ObjectKeyAlg::Aes),
            4,
            4,
        );
        context.encrypt_dict = dict.clone();
        let handle = context.encrypt_dict_handle();

        let mut legacy = Vec::new();
        write_encryption_dictionary(&mut legacy, &dict);
        let mut handle_wire = Vec::new();
        write_encryption_dictionary_handle(&mut handle_wire, &handle)
            .expect("direct encryption dictionary handle must serialize");

        assert_eq!(handle_wire, legacy);
    }

    #[test]
    fn handle_encryption_dictionary_rejects_non_dictionary() {
        let mut wire = Vec::new();
        let error = write_encryption_dictionary_handle(
            &mut wire,
            &ObjectHandle::string(b"not a dictionary".to_vec()),
        )
        .expect_err("an encryption dictionary writer needs a dictionary handle");

        assert!(matches!(
            error,
            crate::Error::System(message)
                if message == "encryption handle does not contain a dictionary"
        ));
        assert!(wire.is_empty());
    }

    #[test]
    fn handle_encryption_dictionary_falls_back_for_a_non_string_hex_key() {
        let handle = ObjectHandle::dictionary(vec![(b"O".to_vec(), ObjectHandle::integer(7))]);
        let mut wire = Vec::new();
        write_encryption_dictionary_handle(&mut wire, &handle)
            .expect("a non-string encryption entry still uses the handle writer");
        assert_eq!(wire, b"<< /O 7 >>");
    }

    #[test]
    fn object_to_handle_preserves_every_legacy_object_variant() {
        let mut nested_dict = Dictionary::new();
        nested_dict.insert("Value", Object::Integer(1));
        let objects = [
            (Object::Null, b"null".as_slice()),
            (Object::Boolean(true), b"true".as_slice()),
            (Object::Integer(7), b"7".as_slice()),
            (Object::Real(0.5), b"0.5".as_slice()),
            (
                Object::RealLiteral {
                    value: 0.4,
                    literal: b".4".to_vec(),
                },
                b".4".as_slice(),
            ),
            (Object::Name(b"Name".to_vec()), b"/Name".as_slice()),
            (Object::String(b"text".to_vec()), b"(text)".as_slice()),
            (Object::Reference(ObjectRef::new(4, 0)), b"4 0 R".as_slice()),
            (Object::Operator(b"q".to_vec()), b"q".as_slice()),
            (Object::InlineImage(b"BI".to_vec()), b"BI".as_slice()),
            (Object::Array(vec![Object::Integer(1)]), b"[ 1 ]".as_slice()),
            (
                Object::Dictionary(nested_dict),
                b"<< /Value 1 >>".as_slice(),
            ),
            (
                Object::Stream(Stream::new(
                    {
                        let mut dict = Dictionary::new();
                        dict.insert("Length", Object::Integer(0));
                        dict
                    },
                    Vec::new(),
                )),
                b"<< /Length 0 >>".as_slice(),
            ),
        ];

        for (object, expected) in objects {
            let handle = super::object_to_handle(&object);
            let mut out = Vec::new();
            handle
                .write_object(&mut out)
                .expect("converted object handle must unparse");
            assert_eq!(out, expected, "unexpected conversion for {object:?}");
        }
    }

    #[test]
    fn handle_object_emission_round_trips_rc4_aes128_and_aes256() {
        let cases = [
            (
                vec![0x11; 5],
                WriteCipher::PerObject(ObjectKeyAlg::Rc4),
                1,
                2,
            ),
            (
                vec![0x22; 16],
                WriteCipher::PerObject(ObjectKeyAlg::Aes),
                4,
                4,
            ),
            (vec![0x33; 32], WriteCipher::FileKeyAes256, 5, 6),
        ];

        for (file_key, cipher, encryption_v, encryption_r) in cases {
            let emitted_ref = ObjectRef::new(10, 0);
            let context = fixed_context(file_key.clone(), cipher, encryption_v, encryption_r);
            let object = ObjectHandle::string(b"ObjectHandle plaintext".to_vec());
            let mut emitter = EncryptedStringEmitter::from_context(&context);
            let mut out = Vec::new();

            emitter
                .write_handle_object(&mut out, emitted_ref, None, &object, false)
                .expect("ObjectHandle string emission");

            assert_eq!(
                decrypt_emitted_string(&out, emitted_ref, &file_key, cipher),
                b"ObjectHandle plaintext"
            );
            assert_eq!(
                object.as_string().expect("source handle is a string"),
                b"ObjectHandle plaintext"
            );
        }
    }

    #[test]
    fn handle_object_emission_encrypts_nested_strings_without_materializing_children() {
        let emitted_ref = ObjectRef::new(18, 0);
        let file_key = vec![0x44; 16];
        let cipher = WriteCipher::PerObject(ObjectKeyAlg::Aes);
        let context = fixed_context(file_key.clone(), cipher, 4, 4);
        let object = ObjectHandle::dictionary(vec![
            (
                b"Title".to_vec(),
                ObjectHandle::string(b"top-level".to_vec()),
            ),
            (
                b"Nested".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::string(b"nested".to_vec())]),
            ),
        ]);
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        emitter
            .write_handle_object(&mut out, emitted_ref, None, &object, true)
            .expect("nested ObjectHandle emission");

        let emitted = crate::parse_object(&out).expect("emitted object must parse");
        let dict = emitted
            .as_dict()
            .expect("emitted object must be a dictionary");
        assert_eq!(
            decrypt_ciphertext(
                dict.get("Title")
                    .and_then(Object::as_string)
                    .expect("encrypted title")
                    .to_vec(),
                emitted_ref,
                &file_key,
                cipher,
            ),
            b"top-level"
        );
        let nested = dict
            .get("Nested")
            .and_then(Object::as_array)
            .and_then(|items| items.first())
            .and_then(Object::as_string)
            .expect("encrypted nested string");
        assert_eq!(
            decrypt_ciphertext(nested.to_vec(), emitted_ref, &file_key, cipher),
            b"nested"
        );
        assert_eq!(
            object
                .get_key(b"/Title")
                .as_string()
                .expect("source title remains available"),
            b"top-level"
        );
    }

    #[test]
    fn handle_object_emission_keeps_encrypt_and_object_stream_members_plain() {
        let context = fixed_context(
            vec![0x42; 16],
            WriteCipher::PerObject(ObjectKeyAlg::Aes),
            4,
            4,
        );
        let object = ObjectHandle::string(b"plain".to_vec());
        let encrypt_object = ObjectHandle::dictionary(vec![(
            b"O".to_vec(),
            ObjectHandle::string(b"printable".to_vec()),
        )]);
        let mut emitter = EncryptedStringEmitter::from_context(&context);

        let mut encrypt_output = Vec::new();
        emitter
            .write_handle_object(
                &mut encrypt_output,
                context.encrypt_ref,
                None,
                &encrypt_object,
                false,
            )
            .expect("/Encrypt object must remain cleartext");
        assert_eq!(encrypt_output, b"<< /O <7072696e7461626c65> >>");

        let identity_map = |object_ref: ObjectRef| Ok(object_ref);
        let mut mapped_encrypt_output = Vec::new();
        emitter
            .write_handle_object_with_ref_map(
                &mut mapped_encrypt_output,
                context.encrypt_ref,
                None,
                &encrypt_object,
                false,
                &identity_map,
                &std::collections::BTreeSet::new(),
            )
            .expect("mapped /Encrypt object must remain cleartext");
        assert_eq!(mapped_encrypt_output, encrypt_output);

        let mut member_output = Vec::new();
        emitter
            .write_handle_object(
                &mut member_output,
                ObjectRef::new(10, 0),
                Some(3),
                &object,
                false,
            )
            .expect("ObjStm member must remain cleartext");
        assert_eq!(member_output, b"(plain)");
    }

    #[test]
    fn handle_stream_dictionary_switches_between_encrypted_and_cleartext_metadata() {
        let emitted_ref = ObjectRef::new(12, 0);
        let file_key = vec![0x21; 16];
        let cipher = WriteCipher::PerObject(ObjectKeyAlg::Aes);
        let context = fixed_context(file_key.clone(), cipher, 4, 4);
        let dict = ObjectHandle::dictionary(vec![
            (
                b"MetadataMarker".to_vec(),
                ObjectHandle::string(b"metadata-dictionary-secret".to_vec()),
            ),
            (b"Length".to_vec(), ObjectHandle::integer(5)),
        ]);

        for qdf in [false, true] {
            let mut cleartext = Vec::new();
            EncryptedStringEmitter::from_context(&context)
                .write_handle_stream_dict(
                    &mut cleartext,
                    emitted_ref,
                    None,
                    &dict,
                    StreamDictOptions::new(qdf, false, false),
                )
                .expect("cleartext metadata dictionary emission");
            assert!(cleartext
                .windows(b"metadata-dictionary-secret".len())
                .any(|window| window == b"metadata-dictionary-secret"));

            let mut encrypted = Vec::new();
            EncryptedStringEmitter::from_context(&context)
                .write_handle_stream_dict(
                    &mut encrypted,
                    emitted_ref,
                    None,
                    &dict,
                    StreamDictOptions::new(qdf, false, true),
                )
                .expect("encrypted metadata dictionary emission");
            let ciphertext = parse_dict_string(&encrypted, "MetadataMarker");
            assert_eq!(
                decrypt_ciphertext(ciphertext, emitted_ref, &file_key, cipher),
                b"metadata-dictionary-secret"
            );
        }

        let identity_map = |object_ref: ObjectRef| Ok(object_ref);
        let mut mapped_cleartext = Vec::new();
        EncryptedStringEmitter::from_context(&context)
            .write_handle_stream_dict_with_ref_map(
                &mut mapped_cleartext,
                emitted_ref,
                None,
                &dict,
                StreamDictOptions::new(true, false, false),
                &identity_map,
                &std::collections::BTreeSet::new(),
                None,
            )
            .expect("mapped cleartext metadata dictionary emission");
        assert!(mapped_cleartext.starts_with(b"<<\n"));
    }

    #[test]
    fn handle_object_callback_errors_clear_the_current_data_key() {
        let context = fixed_context(vec![0x5a; 31], WriteCipher::FileKeyAes256, 5, 6);
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();

        let error = emitter
            .write_handle_object(
                &mut out,
                ObjectRef::new(44, 0),
                None,
                &ObjectHandle::string(b"plaintext".to_vec()),
                false,
            )
            .expect_err("invalid AES-256 handle key must fail in the string callback");

        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message == "V=5 AES-256 data key is not 32 bytes"
        ));
        assert!(out.is_empty());
        assert_eq!(emitter.current_data_key_for_test(), None);
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
                .write_stream_dict(
                    &mut out,
                    emitted_ref,
                    None,
                    &dict,
                    StreamDictOptions::new(qdf, true, true),
                )
                .expect("stream dictionary emission");

            let ciphertext = parse_dict_string(&out, "Label");
            assert_eq!(
                decrypt_ciphertext(ciphertext, emitted_ref, &file_key, cipher),
                b"stream label"
            );
        }
    }

    #[test]
    fn cleartext_stream_dictionary_keeps_nested_strings_plain() {
        let emitted_ref = ObjectRef::new(12, 0);
        let context = fixed_context(
            vec![0x21; 16],
            WriteCipher::PerObject(ObjectKeyAlg::Aes),
            4,
            4,
        );
        let mut dict = Dictionary::new();
        dict.insert(
            "MetadataMarker",
            Object::String(b"metadata-dictionary-secret".to_vec()),
        );
        dict.insert("Length", Object::Integer(5));

        for qdf in [false, true] {
            let mut emitter = EncryptedStringEmitter::from_context(&context);
            let mut out = Vec::new();
            emitter
                .write_stream_dict(
                    &mut out,
                    emitted_ref,
                    None,
                    &dict,
                    StreamDictOptions::new(qdf, false, false),
                )
                .expect("cleartext stream dictionary emission");

            assert!(
                out.windows(b"metadata-dictionary-secret".len())
                    .any(|window| window == b"metadata-dictionary-secret"),
                "cleartext metadata dictionary strings must bypass the object cipher"
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
    fn iv_rng_failure_clears_key_and_does_not_contaminate_later_emission() {
        use std::cell::Cell;
        use std::rc::Rc;

        let mut context = fixed_context(
            vec![0x42; 16],
            WriteCipher::PerObject(ObjectKeyAlg::Aes),
            4,
            4,
        );
        context.static_aes_iv = false;
        let rng_calls = Rc::new(Cell::new(0));
        let observed_calls = Rc::clone(&rng_calls);
        let mut emitter =
            EncryptedStringEmitter::from_context_with_iv_generator(&context, move |iv| {
                let call = observed_calls.get();
                observed_calls.set(call + 1);
                if call == 0 {
                    return Err(getrandom::Error::UNSUPPORTED);
                }
                *iv = crate::pipeline::aes::static_initialization_vector();
                Ok(())
            });

        let mut failed_output = Vec::new();
        let error = emitter
            .write_object(
                &mut failed_output,
                ObjectRef::new(10, 0),
                None,
                &Object::String(b"failed plaintext".to_vec()),
                false,
            )
            .expect_err("injected IV RNG failure must propagate");
        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message
                    == format!(
                        "OS CSPRNG (getrandom) unavailable for AES IV generation: {}",
                        getrandom::Error::UNSUPPORTED
                    )
        ));
        assert!(
            failed_output.is_empty(),
            "failed emission must write no token"
        );
        assert_eq!(emitter.current_data_key_for_test(), None);
        assert_eq!(rng_calls.get(), 1);

        let mut member_output = Vec::new();
        emitter
            .write_object(
                &mut member_output,
                ObjectRef::new(11, 0),
                Some(0),
                &Object::String(b"later member".to_vec()),
                false,
            )
            .expect("later ObjStm-member emission");
        assert_eq!(member_output, b"(later member)");
        assert_eq!(rng_calls.get(), 1, "ObjStm member must not draw an IV");
        assert_eq!(emitter.current_data_key_for_test(), None);

        let later_ref = ObjectRef::new(12, 0);
        let mut later_output = Vec::new();
        emitter
            .write_object(
                &mut later_output,
                later_ref,
                None,
                &Object::String(b"later top-level".to_vec()),
                false,
            )
            .expect("later top-level emission must recover cleanly");
        assert_eq!(rng_calls.get(), 2);
        assert_eq!(
            decrypt_emitted_string(&later_output, later_ref, &context.file_key, context.cipher),
            b"later top-level"
        );
        assert_eq!(emitter.current_data_key_for_test(), None);
    }

    #[test]
    fn invalid_aes128_data_key_error_clears_the_current_data_key() {
        let context = fixed_context(Vec::new(), WriteCipher::PerObject(ObjectKeyAlg::Aes), 4, 4);
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
            .expect_err("short AES-128 data key must fail in the string callback");

        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message == "V=4 AES-128 data key is not 16 bytes"
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
                &WriterOptions::default(),
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

            let handle = context.encrypt_dict_handle();
            let mut legacy_wire = Vec::new();
            write_encryption_dictionary(&mut legacy_wire, &context.encrypt_dict);
            let mut handle_wire = Vec::new();
            write_encryption_dictionary_handle(&mut handle_wire, &handle)
                .expect("generated encryption dictionary handle");
            assert_eq!(handle_wire, legacy_wire);
        }
    }

    #[test]
    fn copied_v4_aes_context_records_v4_r4() {
        let mut encrypt_dict = Dictionary::new();
        encrypt_dict.insert("V", Object::Integer(4));
        encrypt_dict.insert("R", Object::Integer(4));
        encrypt_dict.insert("Length", Object::Integer(128));
        encrypt_dict.insert("P", Object::Integer(-4));
        encrypt_dict.insert("O", Object::String(vec![0x4f; 32]));
        encrypt_dict.insert("U", Object::String(vec![0x55; 32]));
        let source = CopyEncryptionSource {
            encrypt_dict,
            file_key: vec![0x31; 16],
            id0: b"0123456789abcdef".to_vec(),
            object_key_alg: ObjectKeyAlg::Aes,
        };

        let context =
            build_copy_encryption_context(&source, &WriterOptions::default(), 10, None).unwrap();

        assert_eq!((context.encryption_v, context.encryption_r), (4, 4));
        let handle = context.encrypt_dict_handle();
        let mut legacy_wire = Vec::new();
        write_encryption_dictionary(&mut legacy_wire, &context.encrypt_dict);
        let mut handle_wire = Vec::new();
        write_encryption_dictionary_handle(&mut handle_wire, &handle)
            .expect("copied encryption dictionary handle");
        assert_eq!(handle_wire, legacy_wire);
    }
}
