use crate::cache::{CacheEntry, ObjectCache};
use crate::error::EncryptedError;
use crate::parser::{parse_indirect_object, Parser};
use crate::security::standard::{
    check_owner_password, check_owner_password_r5, check_owner_password_r6,
    check_owner_password_v4, check_user_password, check_user_password_r5, check_user_password_r6,
    check_user_password_v4, decrypt_cipher_bytes, decrypt_strings_in_object, per_object_key,
    ObjectKeyAlg, StandardHandlerInputs, StandardHandlerR5Inputs, StringCipher,
};
use crate::{
    load_xref_and_trailer, load_xref_and_trailer_with_repair, Diagnostics, Dictionary, Error,
    Object, ObjectRef, Result, XrefForm, XrefOffset,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};

/// Lazily parsed PDF document handle.
///
/// `Pdf` is the core type of the crate. Opening a document only reads the cross-reference
/// table and the trailer; individual objects are parsed on first access via
/// [`Pdf::resolve`]. The same handle is what every higher-level helper
/// ([`crate::pages`], [`crate::outline`], [`crate::fonts`], [`crate::write_pdf`])
/// consumes.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{ObjectRef, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// println!("version {}", pdf.version());
/// let catalog = pdf.resolve(pdf.root_ref().expect("root"))?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Pdf<R: Read + Seek> {
    reader: R,
    version: String,
    trailer: Dictionary,
    startxref: u64,
    last_xref_form: XrefForm,
    repair_diagnostics: Diagnostics,
    cache: ObjectCache,
    compressed_member_parents: BTreeMap<ObjectRef, (ObjectRef, u32)>,
    source_xref_offsets: Vec<(ObjectRef, u64)>,
    source_xref_entries: BTreeMap<ObjectRef, XrefOffset>,
    encryption: Option<EncryptionState>,
}

#[derive(Debug, Clone)]
struct EncryptionState {
    file_key: Vec<u8>,
    stream_mode: EncryptionMode,
    string_mode: EncryptionMode,
    crypt_filters: BTreeMap<String, EncryptionMode>,
    encrypt_metadata: bool,
    encrypt_ref: Option<ObjectRef>,
    weak_crypto: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncryptionMode {
    Rc4,
    Aes128,
    Identity,
    Aes256,
}

/// Options for opening a PDF document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfOpenOptions {
    /// Enable xref/trailer repair when strict parsing fails.
    pub repair: bool,
    /// Password bytes supplied to the Standard security handler.
    pub password: Vec<u8>,
    /// Permit deprecated RC4-backed handlers and revision 5 AES-256.
    pub allow_weak_crypto: bool,
}

impl<R: Read + Seek> Pdf<R> {
    /// Open a document strictly: parse the cross-reference and trailer, but do not run
    /// the recovery heuristics. Returns an [`Error`] if the document is malformed.
    pub fn open(reader: R) -> Result<Self> {
        Self::open_with_options(reader, PdfOpenOptions::default())
    }

    /// Open a document, falling back to qpdf-style xref/trailer recovery when the
    /// strict parse fails. Diagnostics from the recovery pass are stored on the handle
    /// and exposed via [`Pdf::repair_diagnostics`].
    pub fn open_with_repair(reader: R) -> Result<Self> {
        Self::open_with_options(
            reader,
            PdfOpenOptions {
                repair: true,
                ..PdfOpenOptions::default()
            },
        )
    }

    /// Alias for [`Pdf::open_with_repair`].
    pub fn open_best_effort(reader: R) -> Result<Self> {
        Self::open_with_repair(reader)
    }

    /// Open a document with explicit repair and password options.
    pub fn open_with_options(reader: R, options: PdfOpenOptions) -> Result<Self> {
        Self::open_with_repair_mode(reader, options)
    }

    /// Diagnostics emitted while opening the document — typically warnings from the
    /// xref/trailer recovery path. Always non-empty when the parse hit a soft failure.
    pub fn repair_diagnostics(&self) -> &Diagnostics {
        &self.repair_diagnostics
    }

    /// Whether this document authenticated an `/Encrypt` dictionary while opening.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.is_some()
    }

    /// Whether opening this document required the weak-crypto opt-in.
    pub fn uses_weak_crypto(&self) -> bool {
        self.encryption
            .as_ref()
            .is_some_and(|encryption| encryption.weak_crypto)
    }

    fn open_with_repair_mode(mut reader: R, options: PdfOpenOptions) -> Result<Self> {
        let loaded = if options.repair {
            load_xref_and_trailer_with_repair(&mut reader, options.repair)?
        } else {
            load_xref_and_trailer(&mut reader)?
        };
        let source_xref_entries = loaded.entries.clone();
        let source_xref_offsets = loaded
            .entries
            .iter()
            .filter_map(|(object_ref, offset)| match offset {
                crate::XrefOffset::Free { .. } => None,
                crate::XrefOffset::Offset(offset) => Some((*object_ref, *offset)),
                crate::XrefOffset::Compressed { .. } => None,
            })
            .collect();
        let cache = ObjectCache::from_offsets(&loaded.entries);
        let mut pdf = Self {
            reader,
            version: loaded.version,
            trailer: loaded.trailer,
            startxref: loaded.startxref,
            last_xref_form: loaded.last_xref_form,
            repair_diagnostics: loaded.repair_diagnostics,
            cache,
            compressed_member_parents: BTreeMap::new(),
            source_xref_offsets,
            source_xref_entries,
            encryption: None,
        };
        pdf.authenticate_if_encrypted(&options)?;
        Ok(pdf)
    }

    fn authenticate_if_encrypted(&mut self, options: &PdfOpenOptions) -> Result<()> {
        let encrypt_ref = self.trailer().get_ref("Encrypt");
        let Some(encrypt) = self.encrypt_dictionary()? else {
            return Ok(());
        };

        let revision = required_revision(&encrypt)?;
        let crypt_filters = crypt_filter_modes(&encrypt, revision)?;
        let (file_key, stream_mode, string_mode, encrypt_metadata, weak_crypto) =
            if matches!(revision, 5 | 6) {
                let inputs = standard_handler_r5_inputs(&encrypt)?;
                let (stream_mode, string_mode) = standard_r5_or_r6_modes(&encrypt)?;
                let encrypt_metadata = encrypt_metadata_flag(&encrypt)?;
                let weak_crypto = revision == 5;
                if weak_crypto && !options.allow_weak_crypto {
                    return Err(EncryptedError::WeakCryptoNotAllowed.into());
                }
                let file_key = if revision == 5 {
                    check_user_password_r5(&options.password, &inputs)
                        .or_else(|err| retry_owner_password_r5(err, &options.password, &inputs))?
                } else {
                    check_user_password_r6(&options.password, &inputs)
                        .or_else(|err| retry_owner_password_r6(err, &options.password, &inputs))?
                };
                (
                    file_key,
                    stream_mode,
                    string_mode,
                    encrypt_metadata,
                    weak_crypto,
                )
            } else {
                let inputs = standard_handler_inputs(&encrypt, self.trailer())?;
                let (stream_mode, string_mode) = standard_v4_or_legacy_modes(&encrypt)?;
                let encrypt_metadata = inputs.encrypt_metadata;
                let weak_crypto = matches!(stream_mode, EncryptionMode::Rc4)
                    || matches!(string_mode, EncryptionMode::Rc4)
                    || crypt_filters
                        .values()
                        .any(|mode| matches!(mode, EncryptionMode::Rc4));
                if weak_crypto && !options.allow_weak_crypto {
                    return Err(EncryptedError::WeakCryptoNotAllowed.into());
                }
                let file_key = if inputs.v == 4 && inputs.r == 4 {
                    check_user_password_v4(&options.password, &inputs)
                        .or_else(|err| retry_owner_password_v4(err, &options.password, &inputs))?
                } else {
                    check_user_password(&options.password, &inputs)
                        .or_else(|err| retry_owner_password(err, &options.password, &inputs))?
                };
                (
                    file_key,
                    stream_mode,
                    string_mode,
                    encrypt_metadata,
                    weak_crypto,
                )
            };
        self.encryption = Some(EncryptionState {
            file_key,
            stream_mode,
            string_mode,
            crypt_filters,
            encrypt_metadata,
            encrypt_ref,
            weak_crypto,
        });
        Ok(())
    }

    fn encrypt_dictionary(&mut self) -> Result<Option<Dictionary>> {
        match self.trailer().get("Encrypt").cloned() {
            None => Ok(None),
            Some(Object::Dictionary(dict)) => Ok(Some(dict)),
            Some(Object::Reference(object_ref)) => match self.resolve(object_ref)? {
                Object::Dictionary(dict) => Ok(Some(dict)),
                _ => Err(EncryptedError::Malformed {
                    reason: "/Encrypt object is not a dictionary".into(),
                }
                .into()),
            },
            Some(_) => Err(EncryptedError::Malformed {
                reason: "/Encrypt entry is not a dictionary or reference".into(),
            }
            .into()),
        }
    }

    /// PDF version header as written in the first line of the file (e.g. `"1.7"`).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The trailer dictionary (or the dictionary attached to the trailing xref stream
    /// for cross-reference-stream documents). This is where you'd reach for `/Root`,
    /// `/Info`, `/Size`, `/ID`, etc.
    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    pub(crate) fn startxref(&self) -> u64 {
        self.startxref
    }

    pub(crate) fn previous_xref_offset(&self) -> u64 {
        self.startxref()
    }

    pub(crate) fn last_xref_form(&self) -> XrefForm {
        self.last_xref_form
    }

    pub(crate) fn source_xref_offsets(&self) -> Vec<(ObjectRef, u64)> {
        self.source_xref_offsets.clone()
    }

    pub(crate) fn source_xref_entries(&self) -> BTreeMap<ObjectRef, XrefOffset> {
        self.source_xref_entries.clone()
    }

    pub(crate) fn compressed_parent(&self, object_ref: ObjectRef) -> Option<(ObjectRef, u32)> {
        self.compressed_member_parents.get(&object_ref).copied()
    }

    /// Replace `object_ref` with `object` in the in-memory object cache.
    ///
    /// The original on-disk bytes are not touched; an incremental rewrite via
    /// [`crate::write_pdf`] will see the updated value when it walks the cache and emit
    /// a new revision for the touched object.
    pub fn set_object(&mut self, object_ref: ObjectRef, object: Object) {
        if let Some(CacheEntry::Compressed { stream, index }) =
            self.cache.entry(object_ref).cloned()
        {
            let stream_ref = ObjectRef::new(stream, 0);
            let (parent_ref, parent_index) = self
                .compressed_parent_for_entry(stream_ref, index)
                .unwrap_or((stream_ref, index));
            self.compressed_member_parents
                .insert(object_ref, (parent_ref, parent_index));
        }
        self.cache.set_resolved(object_ref, object);
    }

    pub fn delete_object(&mut self, object_ref: ObjectRef) {
        if object_ref.number == 0
            || matches!(
                self.cache.entry(object_ref),
                Some(CacheEntry::Deleted | CacheEntry::Missing)
            )
        {
            return;
        }
        self.cache.set_deleted(object_ref);
    }

    pub(crate) fn source_bytes(&mut self) -> Result<Vec<u8>> {
        self.reader.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Number of objects currently resolved in the cache. Useful when you want to
    /// confirm that lazy resolution actually deferred work.
    pub fn resolved_count(&self) -> usize {
        self.cache.resolved_count()
    }

    pub(crate) fn resolved_object_refs(&self) -> Vec<ObjectRef> {
        self.cache
            .entries()
            .iter()
            .filter_map(|(object_ref, entry)| {
                if matches!(entry, crate::cache::CacheEntry::Resolved(_)) {
                    Some(*object_ref)
                } else {
                    None
                }
            })
            .collect()
    }

    pub(crate) fn deleted_object_refs(&self) -> Vec<ObjectRef> {
        self.cache.deleted_refs()
    }

    /// Every object reference known from the cross-reference table, including objects
    /// that have not yet been parsed.
    pub fn object_refs(&self) -> Vec<ObjectRef> {
        self.cache.object_refs()
    }

    /// `/Root` as listed in the trailer, when present.
    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer.get_ref("Root")
    }

    /// Locate the linearization hint dictionary if this document is linearized
    /// ("fast web view"). Returns `Ok(None)` for non-linearized documents.
    ///
    /// This resolves object `(1, 0)` and inspects its `/Linearized` entry.
    pub fn linearized_hint_ref(&mut self) -> Result<Option<ObjectRef>> {
        let candidate = ObjectRef::new(1, 0);
        let object = self.resolve(candidate)?;
        let Object::Dictionary(dict) = object else {
            return Ok(None);
        };

        let Some(linearized) = dict.get("Linearized") else {
            return Ok(None);
        };

        Ok(match linearized {
            Object::Integer(value) if *value > 0 => Some(candidate),
            Object::Real(value) if value.is_finite() && *value > 0.0 => Some(candidate),
            Object::Boolean(value) if *value => Some(candidate),
            _ => None,
        })
    }

    /// Resolve `object_ref` to its concrete value, parsing on demand.
    ///
    /// Resolution caches the result so subsequent calls are constant-time. Unknown,
    /// freed, or compressed-but-broken entries return [`Object::Null`] rather than an
    /// error, matching the behavior the PDF spec mandates for missing objects (§7.3.10).
    pub fn resolve(&mut self, object_ref: ObjectRef) -> Result<Object> {
        match self.cache.entry(object_ref).cloned() {
            Some(CacheEntry::Resolved(object)) => Ok(object),
            Some(CacheEntry::Unresolved { offset }) => {
                self.reader.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                self.reader.read_to_end(&mut bytes)?;
                let (parsed_ref, object) = parse_indirect_object(&bytes)?;
                if parsed_ref != object_ref {
                    return Ok(Object::Null);
                }
                let object = self.decrypt_resolved_object(object_ref, object)?;
                self.cache.set_resolved(object_ref, object.clone());
                Ok(object)
            }
            Some(CacheEntry::Compressed { stream, index }) => {
                self.resolve_compressed_entry(object_ref, stream, index)
            }
            Some(CacheEntry::Missing | CacheEntry::Deleted) | None => Ok(Object::Null),
            Some(CacheEntry::Reserved) => Ok(Object::Null),
        }
    }

    fn resolve_compressed_entry(
        &mut self,
        object_ref: ObjectRef,
        stream: u32,
        index: u32,
    ) -> Result<Object> {
        let stream_ref = ObjectRef::new(stream, 0);
        let stream_object = match self.cache.entry(stream_ref).cloned() {
            Some(CacheEntry::Resolved(object)) => object,
            Some(CacheEntry::Unresolved { offset }) => {
                self.reader.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                self.reader.read_to_end(&mut bytes)?;
                let (parsed_ref, object) = parse_indirect_object(&bytes)?;
                if parsed_ref != stream_ref {
                    return Ok(Object::Null);
                }

                let object = self.decrypt_resolved_object(stream_ref, object)?;
                self.cache.set_resolved(stream_ref, object.clone());
                object
            }
            Some(CacheEntry::Compressed { .. }) => return Ok(Object::Null),
            Some(CacheEntry::Missing | CacheEntry::Deleted) | None => return Ok(Object::Null),
            Some(CacheEntry::Reserved) => return Ok(Object::Null),
        };

        let Object::Stream(stream_object) = stream_object else {
            return Ok(Object::Null);
        };

        let (parent_ref, parent_index, object) =
            self.parse_object_stream_chain_entry(stream_ref, &stream_object, index)?;
        let object = self.decrypt_resolved_object(object_ref, object)?;
        self.compressed_member_parents
            .insert(object_ref, (parent_ref, parent_index));
        self.cache.set_resolved(object_ref, object.clone());
        Ok(object)
    }

    fn decrypt_resolved_object(&self, object_ref: ObjectRef, mut object: Object) -> Result<Object> {
        let Some(encryption) = &self.encryption else {
            return Ok(object);
        };
        if Some(object_ref) == encryption.encrypt_ref {
            return Ok(object);
        }

        decrypt_object_strings(
            object_ref,
            &mut object,
            encryption.string_mode,
            &encryption.file_key,
            encryption.encrypt_ref,
        )?;
        if let Object::Stream(stream) = &mut object {
            if !encryption.encrypt_metadata && is_metadata_stream(&stream.dict) {
                return Ok(object);
            }
            if stream_has_explicit_crypt_filter(&stream.dict) {
                apply_explicit_crypt_filters(object_ref, stream, encryption)?;
            } else {
                decrypt_stream_bytes(
                    object_ref,
                    &mut stream.data,
                    encryption.stream_mode,
                    &encryption.file_key,
                )?;
            }
        }
        Ok(object)
    }

    fn parse_object_stream_chain_entry(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        target_index: u32,
    ) -> Result<(ObjectRef, u32, Object)> {
        let (member_stream_ref, member_index, member_stream) =
            self.object_stream_chain_member(stream_ref, stream_object, target_index)?;
        let object = parse_object_stream_entry(&member_stream, member_index)?;
        Ok((member_stream_ref, member_index, object))
    }

    fn compressed_parent_for_entry(
        &mut self,
        stream_ref: ObjectRef,
        target_index: u32,
    ) -> Result<(ObjectRef, u32)> {
        let stream_object = self.resolve(stream_ref)?;
        let Object::Stream(stream_object) = stream_object else {
            return Err(Error::parse(0, "compressed parent is not an object stream"));
        };
        let (parent_ref, parent_index, _) =
            self.object_stream_chain_member(stream_ref, &stream_object, target_index)?;
        Ok((parent_ref, parent_index))
    }

    fn object_stream_chain_member(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        target_index: u32,
    ) -> Result<(ObjectRef, u32, crate::Stream)> {
        let mut streams = Vec::new();
        self.collect_object_stream_chain(
            stream_ref,
            stream_object,
            &mut streams,
            &mut BTreeSet::new(),
        )?;

        let target_index = usize::try_from(target_index)
            .map_err(|_| Error::parse(0, "compressed object index does not fit usize"))?;
        let mut remaining = target_index;
        for (member_stream_ref, member_stream) in streams {
            let member_count = object_stream_count(&member_stream)?;
            if remaining < member_count {
                let member_index = u32::try_from(remaining)
                    .map_err(|_| Error::parse(0, "compressed object index does not fit u32"))?;
                return Ok((member_stream_ref, member_index, member_stream));
            }
            remaining -= member_count;
        }

        Err(Error::parse(
            0,
            "compressed object index out of range for object stream chain",
        ))
    }

    fn collect_object_stream_chain(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        streams: &mut Vec<(ObjectRef, crate::Stream)>,
        seen: &mut BTreeSet<ObjectRef>,
    ) -> Result<()> {
        if !seen.insert(stream_ref) {
            return Err(Error::parse(0, "object stream /Extends cycle"));
        }

        if let Some(parent_ref) = stream_object.dict.get_ref("Extends") {
            let parent_object = self.resolve(parent_ref)?;
            let Object::Stream(parent_stream) = parent_object else {
                return Err(Error::parse(0, "object stream /Extends is not a stream"));
            };
            self.collect_object_stream_chain(parent_ref, &parent_stream, streams, seen)?;
        }

        streams.push((stream_ref, stream_object.clone()));
        Ok(())
    }
}

fn decrypt_object_strings(
    object_ref: ObjectRef,
    object: &mut Object,
    mode: EncryptionMode,
    file_key: &[u8],
    encrypt_ref: Option<ObjectRef>,
) -> Result<()> {
    match mode {
        EncryptionMode::Rc4 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Rc4,
            );
            decrypt_strings_in_object(
                object_ref,
                object,
                StringCipher::Rc4 { key: &key },
                encrypt_ref,
            )
        }
        EncryptionMode::Aes128 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Aes,
            );
            let key = aes128_object_key(&key)?;
            decrypt_strings_in_object(
                object_ref,
                object,
                StringCipher::Aes128 { key: &key },
                encrypt_ref,
            )
        }
        EncryptionMode::Identity => Ok(()),
        EncryptionMode::Aes256 => {
            let key = aes256_file_key(file_key)?;
            decrypt_strings_in_object(
                object_ref,
                object,
                StringCipher::Aes256 { key: &key },
                encrypt_ref,
            )
        }
    }
}

fn decrypt_stream_bytes(
    object_ref: ObjectRef,
    bytes: &mut Vec<u8>,
    mode: EncryptionMode,
    file_key: &[u8],
) -> Result<()> {
    match mode {
        EncryptionMode::Rc4 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Rc4,
            );
            decrypt_cipher_bytes(bytes, StringCipher::Rc4 { key: &key })
        }
        EncryptionMode::Aes128 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Aes,
            );
            let key = aes128_object_key(&key)?;
            decrypt_cipher_bytes(bytes, StringCipher::Aes128 { key: &key })
        }
        EncryptionMode::Identity => Ok(()),
        EncryptionMode::Aes256 => {
            let key = aes256_file_key(file_key)?;
            decrypt_cipher_bytes(bytes, StringCipher::Aes256 { key: &key })
        }
    }
}

fn apply_explicit_crypt_filters(
    object_ref: ObjectRef,
    stream: &mut crate::Stream,
    encryption: &EncryptionState,
) -> Result<()> {
    let decoded = crate::filters::decode_stream_data_with_crypt_filter(
        &stream.dict,
        &stream.data,
        |decode_params, bytes| {
            let mode = explicit_crypt_mode(encryption, decode_params)?;
            let mut decrypted = bytes.to_vec();
            decrypt_stream_bytes(object_ref, &mut decrypted, mode, &encryption.file_key)?;
            Ok(decrypted)
        },
    )?;
    stream.data = decoded;
    stream.dict.remove("Filter");
    stream.dict.remove("DecodeParms");
    Ok(())
}

fn explicit_crypt_mode(
    encryption: &EncryptionState,
    decode_params: Option<&Object>,
) -> Result<EncryptionMode> {
    let Some(Object::Dictionary(params)) = decode_params else {
        return Ok(EncryptionMode::Identity);
    };
    let name = match params.get("Name") {
        None => return Ok(EncryptionMode::Identity),
        Some(Object::Name(name)) => {
            std::str::from_utf8(name).map_err(|_| EncryptedError::Malformed {
                reason: "/Crypt /DecodeParms /Name is not valid UTF-8".into(),
            })?
        }
        Some(_) => {
            return Err(EncryptedError::Malformed {
                reason: "/Crypt /DecodeParms /Name is not a name".into(),
            }
            .into())
        }
    };
    if name == "Identity" {
        return Ok(EncryptionMode::Identity);
    }
    encryption.crypt_filters.get(name).copied().ok_or_else(|| {
        EncryptedError::Malformed {
            reason: format!("/CF entry '{name}' not found"),
        }
        .into()
    })
}

fn stream_has_explicit_crypt_filter(dict: &Dictionary) -> bool {
    match dict.get("Filter") {
        Some(Object::Name(name)) => name == b"Crypt",
        Some(Object::Array(filters)) => filters
            .iter()
            .any(|filter| matches!(filter, Object::Name(name) if name == b"Crypt")),
        _ => false,
    }
}

fn is_metadata_stream(dict: &Dictionary) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(name)) if name.as_slice() == b"Metadata")
}

fn aes256_file_key(file_key: &[u8]) -> Result<[u8; 32]> {
    file_key.try_into().map_err(|_| {
        EncryptedError::Malformed {
            reason: "AES-256 file key is not 32 bytes".into(),
        }
        .into()
    })
}

fn aes128_object_key(key: &[u8]) -> Result<[u8; 16]> {
    key.try_into().map_err(|_| {
        EncryptedError::Malformed {
            reason: "AES-128 object key is not 16 bytes".into(),
        }
        .into()
    })
}

fn parse_object_stream_entry(stream_object: &crate::Stream, target_index: u32) -> Result<Object> {
    let stream_data = crate::filters::decode_stream_data(&stream_object.dict, &stream_object.data)?;

    let stream_object_count = object_stream_count(stream_object)?;
    let stream_data_first = parse_non_negative_i64(
        stream_object
            .dict
            .get("First")
            .ok_or(Error::Missing("Object stream /First"))?,
        "Object stream /First",
    )?;

    let object_count = stream_object_count;
    let first = usize::try_from(stream_data_first)
        .map_err(|_| Error::parse(0, "Object stream /First does not fit usize"))?;

    let mut header_parser = Parser::new(&stream_data);
    let mut object_offsets = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let _object_number = parse_non_negative_u64(
            header_parser.integer_for_indirect()?,
            "object stream object number",
        )?;
        let object_offset = parse_non_negative_u64(
            header_parser.integer_for_indirect()?,
            "object stream object offset",
        )?;
        object_offsets.push(object_offset);
    }

    let target_index = usize::try_from(target_index)
        .map_err(|_| Error::parse(0, "compressed object index does not fit usize"))?;
    if target_index >= object_offsets.len() {
        return Err(Error::parse(
            0,
            "compressed object index out of range for this stream",
        ));
    }

    let start = first
        .checked_add(
            usize::try_from(object_offsets[target_index])
                .map_err(|_| Error::parse(0, "object stream offset does not fit usize"))?,
        )
        .ok_or_else(|| Error::parse(0, "compressed object offset overflow"))?;

    if start > stream_data.len() {
        return Err(Error::parse(0, "compressed object offset out of range"));
    }

    let mut object_parser = Parser::new(&stream_data[start..]);
    object_parser.object()
}

fn standard_handler_inputs<'a>(
    encrypt: &'a Dictionary,
    trailer: &'a Dictionary,
) -> Result<StandardHandlerInputs<'a>> {
    let filter = required_name(encrypt, "Filter")?;
    let v = required_integer(encrypt, "V")?;
    let r = required_integer(encrypt, "R")?;
    if filter != "Standard" || !matches!((v, r), (1 | 2, 2 | 3) | (4, 4)) {
        return Err(EncryptedError::UnsupportedHandler {
            filter: filter.to_string(),
            v,
            r,
            cfm: crypt_filter_method(encrypt),
        }
        .into());
    }

    let length_bits = match encrypt.get("Length") {
        Some(Object::Integer(value)) => *value,
        Some(_) => {
            return Err(EncryptedError::Malformed {
                reason: "/Length entry is not an integer".into(),
            }
            .into())
        }
        None => 40,
    };
    let p =
        i32::try_from(required_integer(encrypt, "P")?).map_err(|_| EncryptedError::Malformed {
            reason: "/P entry is out of i32 range".into(),
        })?;
    let u = required_32_byte_string(encrypt, "U")?;
    let o = required_32_byte_string(encrypt, "O")?;
    let id0 = first_file_id(trailer)?;
    let encrypt_metadata = encrypt_metadata_flag(encrypt)?;

    Ok(StandardHandlerInputs {
        v,
        r,
        length_bits,
        p,
        id0,
        u,
        o,
        encrypt_metadata,
    })
}

fn standard_handler_r5_inputs(encrypt: &Dictionary) -> Result<StandardHandlerR5Inputs<'_>> {
    let filter = required_name(encrypt, "Filter")?;
    let v = required_integer(encrypt, "V")?;
    let r = required_integer(encrypt, "R")?;
    if filter != "Standard" || v != 5 || !matches!(r, 5 | 6) {
        return Err(EncryptedError::UnsupportedHandler {
            filter: filter.to_string(),
            v,
            r,
            cfm: crypt_filter_method(encrypt),
        }
        .into());
    }

    Ok(StandardHandlerR5Inputs {
        u: required_48_byte_string(encrypt, "U")?,
        o: required_48_byte_string(encrypt, "O")?,
        ue: required_32_byte_string(encrypt, "UE")?,
        oe: required_32_byte_string(encrypt, "OE")?,
    })
}

fn encrypt_metadata_flag(encrypt: &Dictionary) -> Result<bool> {
    match encrypt.get("EncryptMetadata") {
        Some(Object::Boolean(value)) => Ok(*value),
        Some(_) => Err(EncryptedError::Malformed {
            reason: "/EncryptMetadata entry is not a boolean".into(),
        }
        .into()),
        None => Ok(true),
    }
}

fn required_revision(encrypt: &Dictionary) -> Result<i64> {
    required_integer(encrypt, "R")
}

fn standard_v4_or_legacy_modes(encrypt: &Dictionary) -> Result<(EncryptionMode, EncryptionMode)> {
    if required_integer(encrypt, "V").ok() != Some(4) {
        return Ok((EncryptionMode::Rc4, EncryptionMode::Rc4));
    }
    Ok((
        v4_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StmF")?)?,
        v4_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StrF")?)?,
    ))
}

fn standard_r5_or_r6_modes(encrypt: &Dictionary) -> Result<(EncryptionMode, EncryptionMode)> {
    Ok((
        r5_or_r6_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StmF")?)?,
        r5_or_r6_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StrF")?)?,
    ))
}

fn v4_mode_for_selector(encrypt: &Dictionary, selector: Option<String>) -> Result<EncryptionMode> {
    let Some(selector) = selector else {
        return Ok(EncryptionMode::Identity);
    };
    if selector == "Identity" {
        return Ok(EncryptionMode::Identity);
    }
    let cfm = crypt_filter_method_for_name(encrypt, &selector)?.unwrap_or_else(|| "V2".into());
    match cfm.as_str() {
        "V2" => Ok(EncryptionMode::Rc4),
        "AESV2" => Ok(EncryptionMode::Aes128),
        "Identity" => Ok(EncryptionMode::Identity),
        _ => unsupported_crypt_filter(encrypt, Some(cfm)),
    }
}

fn r5_or_r6_mode_for_selector(
    encrypt: &Dictionary,
    selector: Option<String>,
) -> Result<EncryptionMode> {
    let Some(selector) = selector else {
        return Ok(EncryptionMode::Aes256);
    };
    if selector == "Identity" {
        return Ok(EncryptionMode::Identity);
    }
    let cfm = crypt_filter_method_for_name(encrypt, &selector)?.unwrap_or_else(|| "AESV3".into());
    match cfm.as_str() {
        "AESV3" => Ok(EncryptionMode::Aes256),
        "Identity" => Ok(EncryptionMode::Identity),
        _ => unsupported_crypt_filter(encrypt, Some(cfm)),
    }
}

fn crypt_filter_selector(encrypt: &Dictionary, key: &str) -> Result<Option<String>> {
    match encrypt.get(key) {
        None => Ok(None),
        Some(Object::Name(name)) => Ok(Some(String::from_utf8_lossy(name).to_string())),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a name"),
        }
        .into()),
    }
}

fn crypt_filter_method_for_name(encrypt: &Dictionary, name: &str) -> Result<Option<String>> {
    let Some(Object::Dictionary(cf)) = encrypt.get("CF") else {
        return Err(EncryptedError::Malformed {
            reason: format!("/CF entry '{name}' not found"),
        }
        .into());
    };
    let Some(Object::Dictionary(filter)) = cf.get(name) else {
        return Err(EncryptedError::Malformed {
            reason: format!("/CF entry '{name}' not found"),
        }
        .into());
    };
    match filter.get("CFM") {
        None => Ok(None),
        Some(Object::Name(cfm)) => Ok(Some(String::from_utf8_lossy(cfm).to_string())),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/CF/{name}/CFM entry is not a name"),
        }
        .into()),
    }
}

fn crypt_filter_modes(
    encrypt: &Dictionary,
    revision: i64,
) -> Result<BTreeMap<String, EncryptionMode>> {
    let mut modes = BTreeMap::new();
    let Some(Object::Dictionary(cf)) = encrypt.get("CF") else {
        return Ok(modes);
    };
    for (name, value) in cf.iter() {
        let name = std::str::from_utf8(name)
            .map_err(|_| EncryptedError::Malformed {
                reason: "/CF entry name is not valid UTF-8".into(),
            })?
            .to_string();
        let Object::Dictionary(filter) = value else {
            return Err(EncryptedError::Malformed {
                reason: format!("/CF entry '{name}' is not a dictionary"),
            }
            .into());
        };
        let cfm = match filter.get("CFM") {
            None if revision >= 5 => "AESV3".to_string(),
            None => "V2".to_string(),
            Some(Object::Name(cfm)) => String::from_utf8_lossy(cfm).to_string(),
            Some(_) => {
                return Err(EncryptedError::Malformed {
                    reason: format!("/CF/{name}/CFM entry is not a name"),
                }
                .into())
            }
        };
        let mode = match (revision, cfm.as_str()) {
            (_, "Identity") => EncryptionMode::Identity,
            (5 | 6, "AESV3") => EncryptionMode::Aes256,
            (5 | 6, _) => unsupported_crypt_filter(encrypt, Some(cfm))?,
            (_, "V2") => EncryptionMode::Rc4,
            (_, "AESV2") => EncryptionMode::Aes128,
            (_, _) => unsupported_crypt_filter(encrypt, Some(cfm))?,
        };
        modes.insert(name, mode);
    }
    Ok(modes)
}

fn unsupported_crypt_filter<T>(encrypt: &Dictionary, cfm: Option<String>) -> Result<T> {
    Err(EncryptedError::UnsupportedHandler {
        filter: required_name(encrypt, "Filter")?.to_string(),
        v: required_integer(encrypt, "V")?,
        r: required_integer(encrypt, "R")?,
        cfm,
    }
    .into())
}

fn retry_owner_password(
    err: Error,
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    match err {
        Error::Encrypted(EncryptedError::BadPassword) => check_owner_password(password, inputs),
        err => Err(err),
    }
}

fn retry_owner_password_v4(
    err: Error,
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    match err {
        Error::Encrypted(EncryptedError::BadPassword) => check_owner_password_v4(password, inputs),
        err => Err(err),
    }
}

fn retry_owner_password_r5(
    err: Error,
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    match err {
        Error::Encrypted(EncryptedError::BadPassword) => check_owner_password_r5(password, inputs),
        err => Err(err),
    }
}

fn retry_owner_password_r6(
    err: Error,
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    match err {
        Error::Encrypted(EncryptedError::BadPassword) => check_owner_password_r6(password, inputs),
        err => Err(err),
    }
}

fn required_integer(dict: &Dictionary, key: &'static str) -> Result<i64> {
    match dict.get(key) {
        Some(Object::Integer(value)) => Ok(*value),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not an integer"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn required_name<'a>(dict: &'a Dictionary, key: &'static str) -> Result<&'a str> {
    match dict.get(key) {
        Some(Object::Name(name)) => std::str::from_utf8(name).map_err(|_| {
            EncryptedError::Malformed {
                reason: format!("/{key} entry is not valid UTF-8"),
            }
            .into()
        }),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a name"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn required_32_byte_string<'a>(dict: &'a Dictionary, key: &'static str) -> Result<&'a [u8; 32]> {
    match dict.get(key) {
        Some(Object::String(bytes)) => bytes.as_slice().try_into().map_err(|_| {
            EncryptedError::Malformed {
                reason: format!("/{key} entry is not 32 bytes"),
            }
            .into()
        }),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a string"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn required_48_byte_string<'a>(dict: &'a Dictionary, key: &'static str) -> Result<&'a [u8; 48]> {
    match dict.get(key) {
        Some(Object::String(bytes)) => bytes.as_slice().try_into().map_err(|_| {
            EncryptedError::Malformed {
                reason: format!("/{key} entry is not 48 bytes"),
            }
            .into()
        }),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a string"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn first_file_id(trailer: &Dictionary) -> Result<&[u8]> {
    match trailer.get("ID") {
        Some(Object::Array(ids)) => match ids.first() {
            Some(Object::String(id0)) => Ok(id0),
            Some(_) => Err(EncryptedError::Malformed {
                reason: "/ID first entry is not a string".into(),
            }
            .into()),
            None => Err(EncryptedError::Malformed {
                reason: "/ID array is empty".into(),
            }
            .into()),
        },
        Some(_) => Err(EncryptedError::Malformed {
            reason: "/ID entry is not an array".into(),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: "missing /ID entry".into(),
        }
        .into()),
    }
}

fn crypt_filter_method(encrypt: &Dictionary) -> Option<String> {
    let Some(Object::Dictionary(cf)) = encrypt.get("CF") else {
        return None;
    };
    let Object::Dictionary(std_cf) = cf.get("StdCF")? else {
        return None;
    };
    let Object::Name(cfm) = std_cf.get("CFM")? else {
        return None;
    };
    Some(String::from_utf8_lossy(cfm).to_string())
}

fn object_stream_count(stream_object: &crate::Stream) -> Result<usize> {
    usize::try_from(parse_non_negative_i64(
        stream_object
            .dict
            .get("N")
            .ok_or(Error::Missing("Object stream /N"))?,
        "Object stream /N",
    )?)
    .map_err(|_| Error::parse(0, "Object stream /N does not fit usize"))
}

fn parse_non_negative_i64(value: &crate::Object, context: &str) -> Result<i64> {
    let crate::Object::Integer(integer) = value else {
        return Err(Error::parse(0, format!("{context} is not integer")));
    };
    if *integer < 0 {
        return Err(Error::parse(0, format!("{context} is negative")));
    }
    Ok(*integer)
}

fn parse_non_negative_u64(value: i64, context: &str) -> Result<u64> {
    if value < 0 {
        return Err(Error::parse(0, format!("{context} is negative")));
    }
    Ok(value as u64)
}
