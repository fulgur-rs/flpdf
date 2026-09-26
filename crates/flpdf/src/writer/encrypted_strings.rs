//! Writer-side encryption and serialization of PDF strings.
//!
//! qpdf correspondence: QPDFWriter.cc:785-803 encryption-dictionary binary-key hex selection, QPDFWriter.cc:1567-1599 string-unparse, QPDFWriter.cc:1761-1796 object data-key lifecycle, and QPDFWriter.cc:2244-2256 encryption-dictionary emission responsibilities.
//!

use crate::encryption::standard::{encrypt_cipher_bytes, ObjectKeyAlg, StringEncryptCipher};
use crate::object_handle::ObjectHandle;
use crate::pdf_syntax::{write_hex_string, write_name_escaped, write_string_value};
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::writer::encryption_state::WriterEncryptionState;
use crate::writer::output::OutputSink;
use crate::writer::{
    EncryptionContext, ObjectWriterEmission, StreamDictionaryOptions, WriteCipher, WriterOptions,
};
use crate::ObjectRef;

type AesIvGenerator = dyn FnMut(&mut [u8; 16]) -> Result<(), getrandom::Error>;

#[derive(Clone, Copy)]
pub(crate) struct StreamDictOptions {
    qdf: bool,
    dictionary: StreamDictionaryOptions,
    encrypt_strings: bool,
}

impl StreamDictOptions {
    pub(crate) const fn new(
        qdf: bool,
        dictionary: StreamDictionaryOptions,
        encrypt_strings: bool,
    ) -> Self {
        Self {
            qdf,
            dictionary,
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

    /// QDF object emission variant keyed by qpdf's complete raw source
    /// identity. The ordinary `ObjectRef` sibling remains available for
    /// callers whose source graph is already inside the `N G R` projection;
    /// QDF live emission must use this route so out-of-range generations are
    /// still references rather than inlined values.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_handle_object_with_qpdf_obj_gen_map(
        &mut self,
        out: &mut OutputSink<'_>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        map: &dyn Fn(QpdfObjGen) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<QpdfObjGen>,
    ) -> crate::Result<()> {
        self.write_handle_object_with_qpdf_obj_gen_map_and_mode(
            out,
            emitted_ref,
            object_stream_index,
            object,
            true,
            map,
            removed_refs,
        )
    }

    /// Raw-identity object emission for either compact or QDF output. The
    /// linearization writer uses this for ordinary non-QDF objects as well as
    /// QDF objects, so an out-of-range source generation is never narrowed to
    /// `ObjectRef` during serialization.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_handle_object_with_qpdf_obj_gen_map_and_mode(
        &mut self,
        out: &mut OutputSink<'_>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        qdf: bool,
        map: &dyn Fn(QpdfObjGen) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<QpdfObjGen>,
    ) -> crate::Result<()> {
        if emitted_ref == self.encrypt_ref {
            return write_encryption_dictionary_handle(out, object); // cov:ignore: the canonical body emits /Encrypt through its dedicated unencrypted dictionary path
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut OutputSink<'_>, plaintext: &[u8]| {
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
                    object.unparse_object_qdf_with_qpdf_obj_gen_map_and_removed_with_string_writer(
                        out,
                        0,
                        map,
                        removed_refs,
                        &mut write_string,
                    )
                } else {
                    object.unparse_object_with_qpdf_obj_gen_map_and_removed_with_string_writer(
                        out,
                        map,
                        removed_refs,
                        &mut write_string,
                    )
                }
            })
    }

    /// Standard-writer dynamic object emission with the current object's
    /// encryption key and qpdf's direct-stream framing boundary.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub(crate) fn write_handle_object_with_dynamic_ref_map_and_direct_stream_writer(
        &mut self,
        out: &mut OutputSink<'_>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        map: &mut crate::writer::object::DynamicObjectRefMap<'_>,
        removed_refs: &std::collections::BTreeSet<ObjectRef>,
        direct_stream_writer: &mut dyn crate::writer::object::DynamicDirectStreamWriter,
    ) -> crate::Result<()> {
        if emitted_ref == self.encrypt_ref {
            return write_encryption_dictionary_handle(out, object); // cov:ignore: the canonical body emits /Encrypt through its dedicated unencrypted dictionary path.
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut OutputSink<'_>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                crate::writer::object::unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                    object,
                    out,
                    map,
                    removed_refs,
                    &mut write_string,
                    direct_stream_writer,
                )
            })
    }

    /// Emit a page or `/Contents` array holder that owns direct streams while
    /// keeping qpdf's per-object string data key active. The stream payload is
    /// deliberately handled by the content-container helper's raw-stream
    /// route; only dictionary strings use this callback, matching the legacy
    /// direct-stream writer's encryption boundary.
    #[allow(dead_code)] // legacy ObjectRef adapter remains covered by serializer tests
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_handle_content_container_with_ref_map(
        &mut self,
        out: &mut OutputSink<'_>,
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
                let mut write_string = |out: &mut OutputSink<'_>, plaintext: &[u8]| {
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

    /// Content-container emission keyed by qpdf's raw source identity. This
    /// is the live QDF/compact counterpart of the legacy `ObjectRef` wrapper;
    /// direct stream framing and string encryption remain at the same writer
    /// boundary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_handle_content_container_with_qpdf_obj_gen_map(
        &mut self,
        out: &mut OutputSink<'_>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        object: &ObjectHandle,
        options: &WriterOptions,
        map: &dyn Fn(QpdfObjGen) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<QpdfObjGen>,
    ) -> crate::Result<()> {
        if emitted_ref == self.encrypt_ref {
            return write_encryption_dictionary_handle(out, object); // cov:ignore: the pre-scanned page-content container cannot be the /Encrypt object
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut OutputSink<'_>, plaintext: &[u8]| {
                    write_encrypted_or_plain_string(
                        state,
                        cipher,
                        static_aes_iv,
                        aes_iv_generator,
                        out,
                        plaintext,
                    )
                };
                crate::writer::plain::body::emit_content_container_from_handle_with_qpdf_obj_gen_map_and_string_writer(
                    object,
                    options,
                    out,
                    map,
                    removed_refs,
                    &mut write_string,
                )
            })
    }

    /// QDF stream-dictionary emission keyed by qpdf's raw object identity.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_handle_stream_dict_with_qpdf_obj_gen_map(
        &mut self,
        out: &mut OutputSink<'_>,
        emitted_ref: ObjectRef,
        object_stream_index: Option<u32>,
        dict: &ObjectHandle,
        options: StreamDictOptions,
        map: &dyn Fn(QpdfObjGen) -> crate::Result<ObjectRef>,
        removed_refs: &std::collections::BTreeSet<QpdfObjGen>,
        length_ref: Option<ObjectRef>,
    ) -> crate::Result<()> {
        if options.qdf && !options.encrypt_strings {
            return dict
                .unparse_stream_body_qdf_with_qpdf_obj_gen_map_and_removed_and_length_with_options(
                    out,
                    0,
                    map,
                    removed_refs,
                    length_ref,
                    options.dictionary,
                );
        }

        if !options.qdf && !options.encrypt_strings {
            return dict.unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options(
                out,
                options.dictionary,
                map,
                removed_refs,
            );
        }

        let cipher = self.cipher;
        let static_aes_iv = self.static_aes_iv;
        let aes_iv_generator = self.aes_iv_generator.as_mut();
        self.state
            .with_object_data_key(emitted_ref.number, object_stream_index, |state| {
                let mut write_string = |out: &mut OutputSink<'_>, plaintext: &[u8]| {
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
                    dict.unparse_stream_body_qdf_with_qpdf_obj_gen_map_and_removed_and_length_with_string_writer_with_options(
                        out,
                        0,
                        map,
                        removed_refs,
                        length_ref,
                        options.dictionary,
                        &mut write_string,
                    )
                } else {
                    dict.unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_string_writer(
                        out,
                        options.dictionary,
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
    out: &mut OutputSink<'_>,
    plaintext: &[u8],
) -> crate::Result<()> {
    let Some(data_key) = state.current_data_key() else {
        return write_string_value(out, plaintext);
    };
    let ciphertext = encrypt_string(cipher, static_aes_iv, aes_iv_generator, data_key, plaintext)?;
    serialize_encrypted_string(out, &ciphertext, crate::writer::cipher_needs_aes_iv(cipher))
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
            encrypt_cipher_bytes(&mut bytes, StringEncryptCipher::Aes { key: data_key }, &iv)?;
        }
        WriteCipher::FileKeyAes256 => {
            encrypt_cipher_bytes(&mut bytes, StringEncryptCipher::Aes { key: data_key }, &iv)?;
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
pub(crate) fn serialize_encrypted_string(
    out: &mut OutputSink<'_>,
    ciphertext: &[u8],
    use_aes: bool,
) -> crate::Result<()> {
    if use_aes {
        write_hex_string(out, ciphertext)?;
    } else {
        write_string_value(out, ciphertext)?;
    }
    Ok(())
}

/// Serialize an ObjectHandle-backed `/Encrypt` dictionary with the same
/// direct-entry hex policy as the canonical encryption writer. The handle
/// tree is kept as the source of truth; nested values and indirect references
/// use the canonical ObjectHandle writer rather than materializing `Object`.
pub(crate) fn write_encryption_dictionary_handle(
    out: &mut OutputSink<'_>,
    handle: &ObjectHandle,
) -> crate::Result<()> {
    const HEX_ENCRYPT_KEYS: [&[u8]; 5] = [b"/O", b"/U", b"/OE", b"/UE", b"/Perms"];

    let Some(entries) = handle.as_dictionary() else {
        return Err(crate::Error::System(
            "encryption handle does not contain a dictionary".to_string(),
        ));
    };

    out.write_bytes(b"<<")?;
    for (key, value) in entries {
        let key_without_slash = key.strip_prefix(b"/").unwrap_or(&key);
        out.write_bytes(b" /")?;
        write_name_escaped(out, key_without_slash)?;
        out.write_bytes(b" ")?;
        if HEX_ENCRYPT_KEYS.contains(&key.as_slice()) {
            if let Some(bytes) = value.as_string() {
                write_hex_string(out, &bytes)?;
                continue;
            } // cov:ignore: LLVM attributes the covered hex-key string branch to its continue terminator.
        } // cov:ignore: LLVM attributes the covered non-string encryption-key fallback to this closing branch.
        value.unparse_object(out)?;
    }
    out.write_bytes(b" >>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::standard::ObjectKeyAlg;
    use crate::writer::{StreamDictionaryOptions, WriteCipher};
    use std::collections::BTreeSet;
    use std::rc::Rc;

    #[test]
    fn unencrypted_qdf_stream_dict_uses_the_shared_dictionary_policy() {
        let context = EncryptionContext {
            encrypt_dict: ObjectHandle::dictionary(Vec::new()),
            file_key: vec![1; 5],
            cipher: WriteCipher::PerObject(ObjectKeyAlg::Rc4),
            encryption_v: 2,
            encryption_r: 3,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: b"id".to_vec(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let dict = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                ),
                (b"/Length".to_vec(), ObjectHandle::integer(3)),
            ]),
            Rc::new(b"abc".to_vec()),
        );
        let mut output = Vec::new();
        crate::writer::output::with_buffer_sink(&mut output, |out| {
            emitter.write_handle_stream_dict_with_qpdf_obj_gen_map(
                out,
                ObjectRef::new(3, 0),
                None,
                &dict,
                StreamDictOptions::new(true, StreamDictionaryOptions::new(true, true), false),
                // cov:ignore-start: the direct test stream dictionary has no indirect child, so this callback is not invoked.
                &|object_gen| {
                    Ok(object_gen
                        .to_object_ref()
                        .expect("direct test dictionary has no raw child"))
                },
                // cov:ignore-end
                &BTreeSet::new(),
                None,
            )
        })
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("/Filter /FlateDecode"));
        assert!(!text.contains("ASCIIHexDecode"));
    }

    #[test]
    fn content_container_encrypts_dictionary_strings_but_keeps_direct_stream_data_raw() {
        let mut pdf = crate::Pdf::empty().expect("create content-container reference owner");
        let mapped = pdf
            .make_indirect_object_handle(ObjectHandle::integer(7))
            .expect("create mapped content-container child");
        let mapped_ref = mapped.object_ref().unwrap();
        let context = EncryptionContext {
            encrypt_dict: ObjectHandle::dictionary(Vec::new()),
            file_key: vec![1; 5],
            cipher: WriteCipher::PerObject(ObjectKeyAlg::Rc4),
            encryption_v: 2,
            encryption_r: 3,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: b"id".to_vec(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let direct_stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(8))]),
            Rc::new(b"raw-data".to_vec()),
        );
        let container = ObjectHandle::dictionary(vec![
            (
                b"/Label".to_vec(),
                ObjectHandle::string(b"secret-label".to_vec()),
            ),
            (b"/Contents".to_vec(), direct_stream),
            (b"/Mapped".to_vec(), mapped),
        ]);
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut output = Vec::new();

        crate::writer::output::with_buffer_sink(&mut output, |out| {
            emitter.write_handle_content_container_with_ref_map(
                out,
                ObjectRef::new(3, 0),
                None,
                &container,
                &WriterOptions::default(),
                &|object_ref| {
                    assert_eq!(object_ref, mapped_ref);
                    Ok(ObjectRef::new(7, 0))
                },
                &BTreeSet::new(),
            )
        })
        .expect("encrypted content-container emission");

        assert!(output
            .windows(b"stream\nraw-data\nendstream".len())
            .any(|window| window == b"stream\nraw-data\nendstream"));
        assert!(!output
            .windows(b"secret-label".len())
            .any(|window| window == b"secret-label"));
        assert!(output
            .windows(b"7 0 R".len())
            .any(|window| window == b"7 0 R"));
    }

    #[test]
    fn string_writer_without_an_object_data_key_uses_plain_pdf_string_syntax() {
        let state = WriterEncryptionState::new(false, Vec::new(), false, 0, 0);
        let mut iv_generator = |_iv: &mut [u8; 16]| Ok(());
        let mut output = Vec::new();

        crate::writer::output::with_buffer_sink(&mut output, |out| {
            write_encrypted_or_plain_string(
                &state,
                WriteCipher::PerObject(ObjectKeyAlg::Rc4),
                true,
                &mut iv_generator,
                out,
                b"plain text",
            )
        })
        .expect("plain string emission without a current data key");

        assert_eq!(output, b"(plain text)");
    }

    #[test]
    fn aes_iv_generation_errors_cross_the_writer_error_boundary() {
        let mut generator = |_iv: &mut [u8; 16]| Err(getrandom::Error::UNEXPECTED);
        let mut iv = [0_u8; 16];
        let error = fill_aes_iv(&mut generator, &mut iv).expect_err("CSPRNG failure");
        assert!(error.to_string().contains("OS CSPRNG"));
    }

    #[test]
    fn encryption_dictionary_hex_keys_use_hex_string_syntax() -> crate::Result<()> {
        let dictionary = ObjectHandle::dictionary(vec![
            (b"/O".to_vec(), ObjectHandle::string(vec![0x01, 0xab])),
            (b"/V".to_vec(), ObjectHandle::integer(4)),
        ]);
        let mut output = Vec::new();
        crate::writer::output::with_buffer_sink(&mut output, |out| {
            write_encryption_dictionary_handle(out, &dictionary)
        })?;
        assert!(String::from_utf8_lossy(&output).contains("/O <01ab>"));
        Ok(())
    }
}
