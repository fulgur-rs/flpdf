use crate::cache::{CacheEntry, ObjectCache};
use crate::parser::parse_indirect_object;
use crate::{load_xref_and_trailer, Dictionary, Object, ObjectRef, Result};
use std::io::{Read, Seek, SeekFrom};

pub struct Pdf<R: Read + Seek> {
    reader: R,
    version: String,
    trailer: Dictionary,
    cache: ObjectCache,
}

impl<R: Read + Seek> Pdf<R> {
    pub fn open(mut reader: R) -> Result<Self> {
        let loaded = load_xref_and_trailer(&mut reader)?;
        let cache = ObjectCache::from_offsets(&loaded.entries);
        Ok(Self {
            reader,
            version: loaded.version,
            trailer: loaded.trailer,
            cache,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    pub fn resolved_count(&self) -> usize {
        self.cache.resolved_count()
    }

    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer.get_ref("Root")
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
            Some(CacheEntry::Missing | CacheEntry::Deleted) | None => Ok(Object::Null),
            Some(CacheEntry::Reserved) => Ok(Object::Null),
        }
    }
}
