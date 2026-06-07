use crate::{Object, ObjectRef, XrefOffset};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub enum CacheEntry {
    Unresolved { offset: u64 },
    Compressed { stream: u32, index: u32 },
    Resolved(Object),
    Missing,
    Reserved,
    Deleted,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectCache {
    entries: BTreeMap<ObjectRef, CacheEntry>,
    deleted_refs: BTreeSet<ObjectRef>,
}

impl ObjectCache {
    pub fn from_offsets(offsets: &BTreeMap<ObjectRef, XrefOffset>) -> Self {
        let entries = offsets
            .iter()
            .map(|(object_ref, offset)| {
                let entry = match offset {
                    XrefOffset::Free { .. } => CacheEntry::Deleted,
                    XrefOffset::Offset(offset) => CacheEntry::Unresolved { offset: *offset },
                    XrefOffset::Compressed { stream, index } => CacheEntry::Compressed {
                        stream: *stream,
                        index: *index,
                    },
                };
                (*object_ref, entry)
            })
            .collect();
        Self {
            entries,
            deleted_refs: BTreeSet::new(),
        }
    }

    pub fn entry(&self, object_ref: ObjectRef) -> Option<&CacheEntry> {
        self.entries.get(&object_ref)
    }

    pub fn set_resolved(&mut self, object_ref: ObjectRef, object: Object) {
        self.deleted_refs.remove(&object_ref);
        self.entries
            .insert(object_ref, CacheEntry::Resolved(object));
    }

    /// Mark `object_ref` as resolution-in-progress. A re-entrant
    /// [`resolve`](crate::Pdf::resolve) for the same ref then hits the
    /// `Reserved => Null` arm instead of recursing, breaking indirect cycles
    /// (e.g. cyclic stream `/Length` holder chains).
    pub(crate) fn set_reserved(&mut self, object_ref: ObjectRef) {
        self.entries.insert(object_ref, CacheEntry::Reserved);
    }

    pub fn set_deleted(&mut self, object_ref: ObjectRef) {
        self.entries.insert(object_ref, CacheEntry::Deleted);
        self.deleted_refs.insert(object_ref);
    }

    pub(crate) fn deleted_refs(&self) -> Vec<ObjectRef> {
        self.deleted_refs.iter().copied().collect()
    }

    pub(crate) fn entries(&self) -> &BTreeMap<ObjectRef, CacheEntry> {
        &self.entries
    }

    pub fn resolved_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| matches!(entry, CacheEntry::Resolved(_)))
            .count()
    }

    pub fn object_refs(&self) -> Vec<ObjectRef> {
        self.entries.keys().copied().collect()
    }
}
