//! qpdf correspondence: QPDFWriter.cc:785-803 encryption-dictionary binary-key hex selection, QPDFWriter.cc:1567-1599 string-unparse, QPDFWriter.cc:1761-1796 object data-key lifecycle, and QPDFWriter.cc:2244-2256 encryption-dictionary emission responsibilities.

use crate::encryption::standard::{encrypt_cipher_bytes, ObjectKeyAlg, StringEncryptCipher};
use crate::object_handle::ObjectHandle;
use crate::pdf_syntax::{write_hex_string, write_name_escaped, write_string_value};
use crate::writer::encryption_state::WriterEncryptionState;
use crate::writer::{EncryptionContext, ObjectWriterEmission, WriteCipher, WriterOptions};
use crate::ObjectRef;

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
/// key is active, without changing the source ObjectHandle graph.
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
}

impl EncryptionContext {
    /// Return the context's canonical `/Encrypt` dictionary handle.
    pub(crate) fn encrypt_dict_handle(&self) -> ObjectHandle {
        self.encrypt_dict.clone()
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

/// Serialize an ObjectHandle-backed `/Encrypt` dictionary with the same
/// direct-entry hex policy as the canonical encryption writer. The handle
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
