use crate::cache::{CacheEntry, ObjectCache};
use crate::error::EncryptedError;
use crate::parser::{parse_indirect_object, Parser};
use crate::security::standard::{
    check_user_password, check_user_password_v4, StandardHandlerInputs,
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
}

/// Options for opening a PDF document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfOpenOptions {
    /// Enable xref/trailer repair when strict parsing fails.
    pub repair: bool,
    /// Password bytes supplied to the Standard security handler.
    pub password: Vec<u8>,
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
        };
        pdf.authenticate_if_encrypted(&options.password)?;
        Ok(pdf)
    }

    fn authenticate_if_encrypted(&mut self, password: &[u8]) -> Result<()> {
        let Some(encrypt) = self.encrypt_dictionary()? else {
            return Ok(());
        };

        let inputs = standard_handler_inputs(&encrypt, self.trailer())?;
        if inputs.v == 4 && inputs.r == 4 {
            check_user_password_v4(password, &inputs)?;
        } else {
            check_user_password(password, &inputs)?;
        }
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
        self.compressed_member_parents
            .insert(object_ref, (parent_ref, parent_index));
        self.cache.set_resolved(object_ref, object.clone());
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
    let encrypt_metadata = match encrypt.get("EncryptMetadata") {
        Some(Object::Boolean(value)) => *value,
        Some(_) => {
            return Err(EncryptedError::Malformed {
                reason: "/EncryptMetadata entry is not a boolean".into(),
            }
            .into())
        }
        None => true,
    };

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
