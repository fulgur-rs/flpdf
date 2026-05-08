use crate::{Object, ObjectRef};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum CacheEntry {
    Unresolved { offset: u64 },
    Resolved(Object),
    Missing,
    Reserved,
    Deleted,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectCache {
    entries: BTreeMap<ObjectRef, CacheEntry>,
}

impl ObjectCache {
    pub fn from_offsets(offsets: &BTreeMap<ObjectRef, u64>) -> Self {
        let entries = offsets
            .iter()
            .map(|(object_ref, offset)| (*object_ref, CacheEntry::Unresolved { offset: *offset }))
            .collect();
        Self { entries }
    }

    pub fn entry(&self, object_ref: ObjectRef) -> Option<&CacheEntry> {
        self.entries.get(&object_ref)
    }

    pub fn set_resolved(&mut self, object_ref: ObjectRef, object: Object) {
        self.entries
            .insert(object_ref, CacheEntry::Resolved(object));
    }

    pub fn resolved_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| matches!(entry, CacheEntry::Resolved(_)))
            .count()
    }
}
