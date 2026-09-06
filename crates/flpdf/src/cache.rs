//! qpdf correspondence: QPDF.cc xref-backed object cache represented as a standalone Rust module.
use crate::{ObjectHandle, ObjectRef, XrefEntry};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub enum CacheEntry {
    Unresolved {
        offset: u64,
    },
    Compressed {
        stream: u32,
        index: u32,
    },
    /// qpdf's resolved `QPDFObject` cache cell. The handle, not a detached
    /// value snapshot, is the identity-preserving source of truth
    /// (`QPDF.cc:1843-1857,1980-1993`).
    Resolved(ObjectHandle),
    Missing,
    Reserved,
    /// A qpdf-style removal retained by the compatibility cache so repeated
    /// enumeration cannot resurrect the erased object reference.
    Deleted,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectCache {
    entries: BTreeMap<ObjectRef, CacheEntry>,
    deleted_refs: BTreeSet<ObjectRef>,
}

impl ObjectCache {
    pub fn from_offsets(offsets: &BTreeMap<ObjectRef, XrefEntry>) -> Self {
        let entries = offsets
            .iter()
            .map(|(object_ref, xref_entry)| (*object_ref, Self::entry_from_xref(*xref_entry)))
            .collect();
        Self {
            entries,
            deleted_refs: BTreeSet::new(),
        }
    }

    fn entry_from_xref(xref_entry: XrefEntry) -> CacheEntry {
        match xref_entry {
            XrefEntry::Uncompressed { offset } => CacheEntry::Unresolved { offset },
            XrefEntry::Compressed { stream, index } => CacheEntry::Compressed { stream, index },
            XrefEntry::Free { .. } => {
                unreachable!("reader effective xref cannot contain free entries")
            }
        }
    }

    /// Reconcile the compatibility cache metadata with the resolver's live xref after recovery.
    ///
    /// Reconstruction replaces the source offsets owned by the canonical resolver. The
    /// compatibility metadata must follow those entries before a later legacy read, while preserving
    /// caller-owned values and transient resolution guards that do not come from the source.
    pub(crate) fn synchronize_with_xref(&mut self, xref: &BTreeMap<ObjectRef, XrefEntry>) {
        let previous = std::mem::take(&mut self.entries);
        let mut entries = BTreeMap::new();

        for (object_ref, previous_entry) in previous {
            let entry = if self.deleted_refs.contains(&object_ref) {
                CacheEntry::Deleted
            } else {
                match previous_entry {
                    CacheEntry::Resolved(object) => CacheEntry::Resolved(object),
                    CacheEntry::Reserved => CacheEntry::Reserved,
                    CacheEntry::Unresolved { .. }
                    | CacheEntry::Compressed { .. }
                    | CacheEntry::Missing
                    | CacheEntry::Deleted => xref
                        .get(&object_ref)
                        .copied()
                        .map(Self::entry_from_xref)
                        .unwrap_or(CacheEntry::Missing),
                }
            };
            entries.insert(object_ref, entry);
        }

        for (object_ref, xref_entry) in xref {
            if !entries.contains_key(object_ref) && !self.deleted_refs.contains(object_ref) {
                entries.insert(*object_ref, Self::entry_from_xref(*xref_entry));
            }
        }

        self.entries = entries;
    }

    /// Return refs as if the cache had been reconciled with a resolver xref,
    /// without mutating the cache. Direct [`crate::ObjectHandle`] resolution can
    /// reconstruct the canonical xref without holding `&mut Pdf`, so the
    /// read-only enumeration APIs need this view until a later mutable path
    /// performs the eager synchronization.
    pub(crate) fn refs_after_xref_recovery(
        &self,
        xref: &BTreeMap<ObjectRef, XrefEntry>,
        live_only: bool,
    ) -> Vec<ObjectRef> {
        let mut refs = BTreeSet::new();

        for (object_ref, previous_entry) in &self.entries {
            let include = if self.deleted_refs.contains(object_ref) {
                Self::include_in_ref_view(&CacheEntry::Deleted, live_only)
            } else {
                match previous_entry {
                    CacheEntry::Resolved(_) | CacheEntry::Reserved => {
                        Self::include_in_ref_view(previous_entry, live_only)
                    }
                    CacheEntry::Unresolved { .. }
                    | CacheEntry::Compressed { .. }
                    | CacheEntry::Missing
                    | CacheEntry::Deleted => xref
                        .get(object_ref)
                        .copied()
                        .map(Self::entry_from_xref)
                        .is_some_and(|entry| Self::include_in_ref_view(&entry, live_only)),
                }
            };

            if include {
                refs.insert(*object_ref);
            }
        }

        for (object_ref, xref_entry) in xref {
            if !self.entries.contains_key(object_ref)
                && !self.deleted_refs.contains(object_ref)
                && Self::include_in_ref_view(&Self::entry_from_xref(*xref_entry), live_only)
            {
                refs.insert(*object_ref);
            }
        }

        refs.into_iter().collect()
    }

    fn include_in_ref_view(entry: &CacheEntry, live_only: bool) -> bool {
        if live_only {
            !matches!(
                entry,
                CacheEntry::Deleted | CacheEntry::Missing | CacheEntry::Reserved
            )
        } else {
            !matches!(entry, CacheEntry::Missing)
        }
    }

    pub fn entry(&self, object_ref: ObjectRef) -> Option<&CacheEntry> {
        self.entries.get(&object_ref)
    }

    pub fn set_resolved(&mut self, object_ref: ObjectRef, handle: ObjectHandle) {
        self.deleted_refs.remove(&object_ref);
        self.entries
            .insert(object_ref, CacheEntry::Resolved(handle));
    }

    #[cfg(test)]
    pub(crate) fn set_missing(&mut self, object_ref: ObjectRef) {
        self.entries.insert(object_ref, CacheEntry::Missing);
    }

    #[cfg(test)]
    pub(crate) fn set_compressed(&mut self, object_ref: ObjectRef, stream: u32, index: u32) {
        self.entries
            .insert(object_ref, CacheEntry::Compressed { stream, index });
    }

    /// Mark `object_ref` as resolution-in-progress. A re-entrant
    /// [`resolve`](crate::Pdf::resolve) for the same ref then hits the
    /// `Reserved => Null` arm instead of recursing, breaking indirect cycles
    /// (e.g. cyclic stream `/Length` holder chains).
    #[cfg(test)]
    pub(crate) fn set_reserved(&mut self, object_ref: ObjectRef) {
        self.entries.insert(object_ref, CacheEntry::Reserved);
    }

    /// Restore `object_ref` to the unresolved (lazy) state at `offset`. Used to
    /// undo a [`set_reserved`](Self::set_reserved) guard when a resolution
    /// attempt fails hard, so the entry does not linger as `Reserved` (which a
    /// later resolve would read as `Null`) and a retry re-errors consistently.
    #[cfg(test)]
    pub(crate) fn set_unresolved(&mut self, object_ref: ObjectRef, offset: u64) {
        self.entries
            .insert(object_ref, CacheEntry::Unresolved { offset });
    }

    /// Retain a qpdf `removeObject` tombstone in the compatibility cache.
    /// Resolver removal erases its canonical cache slot; this companion state
    /// keeps the legacy enumeration from exposing that reference again.
    pub fn set_deleted(&mut self, object_ref: ObjectRef) {
        self.entries.insert(object_ref, CacheEntry::Deleted);
        self.deleted_refs.insert(object_ref);
    }

    /// Forget a persistent removal when qpdf-style `replaceObject` installs a
    /// new value under the same object generation.
    pub(crate) fn clear_deleted(&mut self, object_ref: ObjectRef) {
        if self.deleted_refs.remove(&object_ref) {
            self.entries.remove(&object_ref);
        }
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

#[cfg(test)]
pub(crate) mod test_support {
    use super::{CacheEntry, ObjectCache};
    use crate::ObjectRef;

    pub(crate) fn stale_deleted_entry(object_ref: ObjectRef) -> ObjectCache {
        let mut cache = ObjectCache::default();
        cache.entries.insert(object_ref, CacheEntry::Deleted);
        cache
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheEntry, ObjectCache};
    use crate::{ObjectHandle, ObjectRef, XrefEntry};
    use std::collections::BTreeMap;

    #[test]
    fn synchronize_with_xref_preserves_mutations_and_refreshes_source_entries() {
        let resolved_ref = ObjectRef::new(1, 0);
        let reserved_ref = ObjectRef::new(2, 0);
        let deleted_ref = ObjectRef::new(3, 0);
        let unresolved_ref = ObjectRef::new(4, 0);
        let compressed_ref = ObjectRef::new(5, 0);
        let missing_ref = ObjectRef::new(6, 0);
        let new_ref = ObjectRef::new(7, 0);
        let mut initial = BTreeMap::new();
        initial.insert(resolved_ref, XrefEntry::Uncompressed { offset: 10 });
        initial.insert(reserved_ref, XrefEntry::Uncompressed { offset: 20 });
        initial.insert(deleted_ref, XrefEntry::Uncompressed { offset: 30 });
        initial.insert(unresolved_ref, XrefEntry::Uncompressed { offset: 40 });
        initial.insert(
            compressed_ref,
            XrefEntry::Compressed {
                stream: 9,
                index: 1,
            },
        );
        initial.insert(missing_ref, XrefEntry::Uncompressed { offset: 60 });
        let mut cache = ObjectCache::from_offsets(&initial);
        cache.set_resolved(resolved_ref, ObjectHandle::integer(1));
        cache.set_reserved(reserved_ref);
        cache.set_deleted(deleted_ref);
        cache.set_unresolved(unresolved_ref, 999);
        cache.set_compressed(compressed_ref, 99, 99);
        cache.set_missing(missing_ref);

        let mut live = BTreeMap::new();
        live.insert(resolved_ref, XrefEntry::Uncompressed { offset: 101 });
        live.insert(reserved_ref, XrefEntry::Uncompressed { offset: 202 });
        live.insert(deleted_ref, XrefEntry::Uncompressed { offset: 303 });
        live.insert(unresolved_ref, XrefEntry::Uncompressed { offset: 404 });
        live.insert(
            compressed_ref,
            XrefEntry::Compressed {
                stream: 15,
                index: 2,
            },
        );
        live.insert(new_ref, XrefEntry::Uncompressed { offset: 707 });
        cache.synchronize_with_xref(&live);

        assert!(matches!(
            cache.entry(resolved_ref),
            Some(CacheEntry::Resolved(handle)) if handle.as_integer() == Some(1)
        ));
        assert!(matches!(
            cache.entry(reserved_ref),
            Some(CacheEntry::Reserved)
        ));
        assert!(matches!(
            cache.entry(deleted_ref),
            Some(CacheEntry::Deleted)
        ));
        assert!(matches!(
            cache.entry(unresolved_ref),
            Some(CacheEntry::Unresolved { offset: 404 })
        ));
        assert!(matches!(
            cache.entry(compressed_ref),
            Some(CacheEntry::Compressed {
                stream: 15,
                index: 2
            })
        ));
        assert!(matches!(
            cache.entry(missing_ref),
            Some(CacheEntry::Missing)
        ));
        assert!(matches!(
            cache.entry(new_ref),
            Some(CacheEntry::Unresolved { offset: 707 })
        ));
        assert_eq!(
            cache.object_refs(),
            (1..=7)
                .map(|number| ObjectRef::new(number, 0))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            cache.refs_after_xref_recovery(&live, false),
            (1..=5)
                .chain(std::iter::once(7))
                .map(|number| ObjectRef::new(number, 0))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            cache.refs_after_xref_recovery(&live, true),
            [1, 4, 5, 7]
                .into_iter()
                .map(|number| ObjectRef::new(number, 0))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[should_panic(expected = "reader effective xref cannot contain free entries")]
    fn source_free_xref_entry_is_unreachable_during_cache_construction() {
        ObjectCache::entry_from_xref(XrefEntry::Free { next: 0 });
    }
}
