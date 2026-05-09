use crate::cache::{CacheEntry, ObjectCache};
use crate::parser::{parse_indirect_object, Parser};
use crate::{
    load_xref_and_trailer, load_xref_and_trailer_with_repair, Diagnostics, Dictionary, Error,
    Object, ObjectRef, Result, XrefForm, XrefOffset,
};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

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

impl<R: Read + Seek> Pdf<R> {
    pub fn open(reader: R) -> Result<Self> {
        Self::open_with_repair_mode(reader, false)
    }

    pub fn open_with_repair(reader: R) -> Result<Self> {
        Self::open_with_repair_mode(reader, true)
    }

    pub fn open_best_effort(reader: R) -> Result<Self> {
        Self::open_with_repair_mode(reader, true)
    }

    pub fn repair_diagnostics(&self) -> &Diagnostics {
        &self.repair_diagnostics
    }

    fn open_with_repair_mode(mut reader: R, allow_repair: bool) -> Result<Self> {
        let loaded = if allow_repair {
            load_xref_and_trailer_with_repair(&mut reader, allow_repair)?
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
        Ok(Self {
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
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

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

    pub fn set_object(&mut self, object_ref: ObjectRef, object: Object) {
        if let Some(CacheEntry::Compressed { stream, index }) =
            self.cache.entry(object_ref).cloned()
        {
            self.compressed_member_parents
                .insert(object_ref, (ObjectRef::new(stream, 0), index));
        }
        self.cache.set_resolved(object_ref, object);
    }

    pub(crate) fn source_bytes(&mut self) -> Result<Vec<u8>> {
        self.reader.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

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

    pub fn object_refs(&self) -> Vec<ObjectRef> {
        self.cache.object_refs()
    }

    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer.get_ref("Root")
    }

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

        let object = parse_object_stream_entry(&stream_object, index)?;
        self.compressed_member_parents
            .insert(object_ref, (stream_ref, index));
        self.cache.set_resolved(object_ref, object.clone());
        Ok(object)
    }
}

fn parse_object_stream_entry(stream_object: &crate::Stream, target_index: u32) -> Result<Object> {
    let stream_data = crate::filters::decode_stream_data(&stream_object.dict, &stream_object.data)?;

    let stream_object_count = parse_non_negative_i64(
        stream_object
            .dict
            .get("N")
            .ok_or(Error::Missing("Object stream /N"))?,
        "Object stream /N",
    )?;
    let stream_data_first = parse_non_negative_i64(
        stream_object
            .dict
            .get("First")
            .ok_or(Error::Missing("Object stream /First"))?,
        "Object stream /First",
    )?;

    let object_count = usize::try_from(stream_object_count)
        .map_err(|_| Error::parse(0, "Object stream /N does not fit usize"))?;
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
