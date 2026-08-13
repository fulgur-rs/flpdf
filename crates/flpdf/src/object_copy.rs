//! qpdf correspondence: the canonical `QPDF::copyForeignObject` graph copy lives here; the older raw-object closure copier remains below as a compatibility route until its consumers migrate.
//! Cross-document deep object copier (identity-preserving reservation + cycle handling).
//!
//! [`copy_objects`] copies a pre-closed set of source [`ObjectRef`]s into a
//! target [`Pdf`], assigning fresh object numbers and returning the
//! source→target renumber map.  It is the building block beneath single-page
//! extract and multi-document merge: callers first compute the curated object
//! set (e.g. via [`page_object_closure`](crate::page_closure::page_object_closure))
//! and hand it to the copier.
//!
//! `copy_foreign_object` is the qpdf-shaped `ObjectHandle` route. It owns
//! the live foreign graph traversal, per-source identity map, `/Pages`
//! boundary, destination reservations, and deferred stream source dispatch
//! (`libqpdf/QPDF.cc:2019-2272`). qpdf's `ot_reserved` value is an internal
//! construction sentinel, not a user-visible object value; this port keeps
//! the equivalent reservation as a destination-owned indirect null slot and
//! replaces that slot in place before returning it. qpdf's
//! `reserveObjects` uses `newIndirectNull()` for every non-stream indirect
//! object, including Page boundaries; a nested Page is simply not traversed.
//! A foreign reserved sentinel is rejected during reservation with qpdf's
//! exact error contract.
//!
//! # Boundary semantics
//!
//! The provided `refs` set is treated as **both the work-list and the
//! boundary**.  The copier does not re-traverse the graph to discover new
//! objects, so it never follows `/Parent` up the page tree or pulls in sibling
//! pages.  A reference inside a copied object that points *outside* `refs`
//! (e.g. a cross-page link's sibling-page `/Contents`) is replaced with
//! [`Object::Null`]; repairing link semantics is a higher layer's job.
//!
//! # Cycle handling
//!
//! Because the full set is known up front, every target number is allocated
//! *before* any reference is rewritten.  Cycles (A→B→A) therefore need no
//! special bookkeeping: both endpoints already have target numbers when their
//! references are remapped.
//!
//! `copy_foreign_object` handles indirect cycles the same way, through its
//! destination reservation map. A cycle formed entirely of *direct*
//! dictionaries or arrays — constructible with [`ObjectHandle::replace_key`],
//! though not by any parser, since a direct value has no addressable identity
//! for another direct value to reference — has no reservation slot to close
//! the loop through. Both the reservation and replacement passes therefore
//! track their own currently-active direct objects and reject a repeat with
//! an error instead of recursing without bound. qpdf's `QPDF::reserveObjects`
//! and `QPDF::replaceForeignIndirectObjects` (`libqpdf/QPDF.cc:2101-2213`)
//! have no equivalent tracking, so this bound has no qpdf counterpart; it
//! guards a shape only the public API can produce, not one qpdf itself
//! defends against.
//!
//! # Independence
//!
//! Each call uses a fresh map, so copying the same source set twice produces
//! independent, non-shared target copies.

use crate::object::{Dictionary, MAX_INLINE_DEPTH};
use crate::object_handle::{ObjectHandle, ObjectValue};
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Copy one canonical foreign object into `target`, retaining qpdf's
/// per-source object identity map on the destination document.
///
/// This is intentionally separate from [`copy_objects`]. The latter accepts a
/// legacy, pre-closed `ObjectRef` set and rewrites raw [`Object`] values. This
/// function follows the live `ObjectHandle` graph instead, matching qpdf's
/// `QPDF::reserveObjects` and `QPDF::replaceForeignIndirectObjects`
/// (`libqpdf/QPDF.cc:2101-2210`).
pub(crate) fn copy_foreign_object<R: Read + Seek>(
    target: &mut Pdf<R>,
    foreign: &ObjectHandle,
) -> Result<ObjectHandle> {
    if !foreign.is_indirect() {
        return Err(Error::System(
            "QPDF::copyForeign called with direct object handle".to_owned(),
        ));
    }
    let source_id = foreign.owning_pdf_unique_id().ok_or_else(|| {
        Error::System("QPDF::copyForeign called with object with no owning PDF".to_owned())
    })?;
    if source_id == target.unique_id() {
        return Err(Error::System(
            "QPDF::copyForeign called with object from this QPDF".to_owned(),
        ));
    }

    let object_map = target.take_foreign_object_map(source_id);
    let mut copier = ForeignObjectCopier {
        target,
        object_map,
        visiting: BTreeSet::new(),
        direct_visiting: Vec::new(),
        to_copy: Vec::new(),
    };
    let result = copier.run(foreign);
    let object_map = copier.object_map;
    copier.target.set_foreign_object_map(source_id, object_map);
    result
}

struct ForeignObjectCopier<'a, R: Read + Seek + 'static> {
    target: &'a mut Pdf<R>,
    object_map: BTreeMap<ObjectRef, ObjectRef>,
    visiting: BTreeSet<ObjectRef>,
    direct_visiting: Vec<ObjectHandle>,
    to_copy: Vec<ObjectHandle>,
}

impl<R: Read + Seek + 'static> ForeignObjectCopier<'_, R> {
    fn run(&mut self, foreign: &ObjectHandle) -> Result<ObjectHandle> {
        self.reserve_objects(foreign.clone(), true)?;
        if !self.visiting.is_empty() {
            return Err(Error::Internal(
                "foreign object copier retained a visiting object".to_owned(),
            ));
        }

        for source in std::mem::take(&mut self.to_copy) {
            let source_ref = source.object_ref().ok_or_else(|| {
                Error::Internal("foreign copier queued a direct object".to_owned())
            })?;
            let replacement = self.replace_foreign_indirect_objects(source.clone(), true)?;
            if source.as_stream_dict().is_none() {
                // cov:ignore-start: `reserve_objects` inserts every `to_copy` entry's
                // reservation into `object_map` before queuing it; this guards that
                // invariant instead of indexing and panicking if it is ever broken.
                let target_ref = *self.object_map.get(&source_ref).ok_or_else(|| {
                    Error::Internal(
                        "foreign copier reservation missing for a queued object".to_owned(),
                    )
                })?;
                // cov:ignore-end
                self.target
                    .resolver
                    .replace_object(target_ref, replacement)?;
                self.target.mark_object_dirty(target_ref);
            }
        }

        let Some(source_ref) = foreign.object_ref() else {
            return Err(Error::System(
                "QPDF::copyForeign called with direct object handle".to_owned(),
            ));
        };
        let Some(&target_ref) = self.object_map.get(&source_ref) else {
            self.target.resolver.push_warning(
                "unexpected reference to /Pages object while copying foreign object; replacing with null",
            )?;
            return Ok(ObjectHandle::null());
        };
        Ok(self.target.get_object_handle(target_ref))
    }

    fn reserve_objects(&mut self, foreign: ObjectHandle, top: bool) -> Result<()> {
        foreign.try_dereference()?;
        if foreign.is_reserved() {
            return Err(Error::System(
                "QPDF: attempting to copy a foreign reserved object".to_owned(),
            ));
        }
        if foreign.try_is_dictionary_of_type(b"Pages", b"")? {
            return Ok(());
        }

        if let Some(source_ref) = foreign.object_ref() {
            let is_page = foreign.try_is_dictionary_of_type(b"Page", b"")?;
            let is_stream = foreign.as_stream_dict().is_some();
            if self.visiting.contains(&source_ref) {
                return Ok(());
            }

            if let Some(&target_ref) = self.object_map.get(&source_ref) {
                let mapped = self.target.get_object_handle(target_ref);
                if !(top && is_page && mapped.is_null()) {
                    return Ok(());
                }
            } else {
                let mapped = if is_stream {
                    self.target.new_stream()?
                } else {
                    // qpdf's reserveObjects uses newIndirectNull for all
                    // non-stream objects. The null is replaced in place after
                    // graph traversal (`QPDF.cc:2124-2132,2185-2189`).
                    self.target
                        .make_indirect_from_object_handle(ObjectHandle::null())?
                };
                // cov:ignore-start: every reservation branch above returns an indirect handle;
                // this protects the canonical map invariant if a future allocator changes.
                let target_ref = mapped.object_ref().ok_or_else(|| {
                    Error::Internal("foreign copier created a direct reservation".to_owned())
                })?;
                // cov:ignore-end
                self.object_map.insert(source_ref, target_ref);
            }

            self.visiting.insert(source_ref);
            if !top && is_page {
                self.visiting.remove(&source_ref);
                return Ok(());
            }
            self.to_copy.push(foreign.clone());
            self.reserve_children(&foreign)?;
            self.visiting.remove(&source_ref);
            return Ok(());
        }

        if self
            .direct_visiting
            .iter()
            .any(|active| active.is_same_object_as(&foreign))
        {
            return Ok(());
        }
        self.direct_visiting.push(foreign.clone());
        let result = self.reserve_children(&foreign);
        self.direct_visiting.pop();
        result
    }

    fn reserve_children(&mut self, foreign: &ObjectHandle) -> Result<()> {
        if let Some(items) = foreign.as_array() {
            for item in items {
                self.reserve_objects(item, false)?;
            }
        } else if let Some(entries) = foreign.as_dictionary() {
            for (_, item) in entries {
                self.reserve_objects(item, false)?;
            }
        } else if let Some(dictionary) = foreign.as_stream_dict() {
            self.reserve_objects(dictionary, false)?;
        }
        Ok(())
    }

    fn replace_foreign_indirect_objects(
        &mut self,
        foreign: ObjectHandle,
        top: bool,
    ) -> Result<ObjectHandle> {
        foreign.try_dereference()?;
        if !top {
            if let Some(source_ref) = foreign.object_ref() {
                return Ok(self
                    .object_map
                    .get(&source_ref)
                    .map(|target_ref| self.target.get_object_handle(*target_ref))
                    .unwrap_or_else(ObjectHandle::null));
            }
        }

        // Every indirect reference is already resolved by the `!top` branch
        // above, so only a genuinely direct (non-indirect) value can recurse
        // back into an ancestor still under construction here. qpdf's own
        // `replaceForeignIndirectObjects` (`QPDF.cc:2158-2213`) has no
        // visited-set at all — a direct cycle reaching it would also recurse
        // unbounded — so this bound has no qpdf counterpart; it exists
        // because the public `ObjectHandle::replace_key` API can construct a
        // direct cycle that no parsed PDF can express in the first place.
        // This mirrors reservation's `direct_visiting` guard (`reserve_objects`),
        // but cannot mirror its `Ok(())` short-circuit: reservation can safely
        // stop descending because the enclosing frame is already reserving
        // that subtree, while replacement is still mid-construction of the
        // ancestor's copy and has no finite value to hand back for the cycle.
        if foreign.object_ref().is_none() {
            if self
                .direct_visiting
                .iter()
                .any(|active| active.is_same_object_as(&foreign))
            {
                return Err(Error::System(
                    "QPDF::copyForeign encountered a direct object cycle".to_owned(),
                ));
            }
            self.direct_visiting.push(foreign.clone());
            let result = self.replace_foreign_value(foreign);
            self.direct_visiting.pop();
            return result;
        }

        self.replace_foreign_value(foreign)
    }

    /// The container/scalar dispatch shared by both `top` states of
    /// [`Self::replace_foreign_indirect_objects`], split out so the cycle
    /// guard there wraps every recursive descent, including the one this
    /// function itself performs into array items, dictionary values, and
    /// stream dictionary entries.
    fn replace_foreign_value(&mut self, foreign: ObjectHandle) -> Result<ObjectHandle> {
        if let Some(source_dictionary) = foreign.as_stream_dict() {
            if !foreign.is_indirect() {
                return Err(Error::System(
                    "QPDF::copyForeign encountered a direct stream object".to_owned(),
                ));
            }
            // cov:ignore-start: an indirect stream always carries its object identity,
            // and `reserve_objects` always reserves a stream (`new_stream`) into
            // `object_map` before queuing it in `to_copy`; these guard those two
            // invariants instead of unwrapping/indexing and panicking if either is
            // ever broken.
            let source_ref = foreign.object_ref().ok_or_else(|| {
                Error::Internal("foreign stream has no object reference".to_owned())
            })?;
            let target_ref = *self.object_map.get(&source_ref).ok_or_else(|| {
                Error::Internal("foreign stream reservation is missing".to_owned())
            })?;
            // cov:ignore-end
            let destination = self.target.get_object_handle(target_ref);
            let destination_dictionary = destination.as_stream_dict().ok_or_else(|| {
                Error::Internal("foreign stream reservation is not a stream".to_owned())
            })?;
            for (key, value) in source_dictionary.as_dictionary().unwrap_or_default() {
                let replacement = self.replace_foreign_indirect_objects(value, false)?;
                destination_dictionary.replace_key(&key, replacement);
            }
            self.target
                .resolver
                .copy_stream_data(&destination, &foreign)?;
            self.target.mark_object_dirty(target_ref);
            return Ok(destination);
        }

        if let Some(items) = foreign.as_array() {
            let mut copied = Vec::with_capacity(items.len());
            for item in items {
                copied.push(self.replace_foreign_indirect_objects(item, false)?);
            }
            return Ok(self
                .target
                .resolver
                .direct_object_handle(ObjectValue::Array(copied)));
        }

        if let Some(entries) = foreign.as_dictionary() {
            // qpdf's own dictionary branch builds the copy through
            // `result.replaceKey` (`libqpdf/QPDF.cc:2192-2196`), not a raw
            // map insert, so a direct-null replacement -- the /Pages
            // boundary above, or any other unmapped indirect child --
            // removes the key instead of storing an explicit null, per
            // `QPDF_Dictionary::replaceKey` (`libqpdf/QPDF_Dictionary.cc:
            // 135-146`: `value.isNull() && !value.isIndirect()` erases the
            // key). The stream-dictionary branch above already goes through
            // `replace_key` for this reason; mirror it here so a copied
            // plain dictionary's `has_key` matches qpdf's own copy instead
            // of retaining a key qpdf would have omitted.
            let copied = self
                .target
                .resolver
                .direct_object_handle(ObjectValue::Dictionary(BTreeMap::new()));
            for (key, value) in entries {
                let replacement = self.replace_foreign_indirect_objects(value, false)?;
                copied.replace_key(&key, replacement);
            }
            return Ok(copied);
        }

        let direct = foreign.shallow_copy()?;
        // cov:ignore-start: shallow_copy always returns a direct value, so this
        // is an invariant guard for a future ObjectHandle state.
        let value = direct.direct_value_clone()?.ok_or_else(|| {
            Error::Internal("foreign scalar copy did not produce a direct value".to_owned())
        })?;
        // cov:ignore-end
        Ok(self.target.resolver.direct_object_handle(value))
    }
}

/// Copy the pre-closed object set `refs` from `source` into `target`, assigning
/// fresh target object numbers, and return the source→target renumber map.
///
/// References inside copied objects are rewritten: those landing in `refs` are
/// remapped to their new target number, while references outside `refs` are
/// replaced with [`Object::Null`].  Stream byte payloads are copied verbatim.
///
/// # Errors
///
/// Returns [`Err`] only if [`Pdf::resolve`] itself fails for a ref in `refs`
/// (an I/O or parse error), or if the target object-number space would overflow
/// `u32`.  Refs that are unknown, freed, or otherwise unresolvable do **not**
/// error: [`Pdf::resolve`] yields [`Object::Null`] for them, so they are simply
/// copied as `Null`.
///
/// Callers normally obtain `refs` from
/// [`page_object_closure`](crate::page_closure::page_object_closure) (one page's
/// transitive object set) and feed the copied pages into
/// [`splice_pages`](crate::page_splice::splice_pages) on the target. Note that
/// deduplication of shared child objects happens only **within a single
/// `copy_objects` call**: copying overlapping closures across separate calls
/// yields independent, non-shared target copies. See the runnable
/// `examples/merge_pdfs.rs`.
///
/// # Examples
///
/// ```no_run
/// use std::collections::BTreeSet;
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{copy_objects, page_closure, pages, Pdf};
///
/// let mut source = Pdf::open(BufReader::new(File::open("source.pdf")?))?;
/// let mut target = Pdf::open(BufReader::new(File::open("target.pdf")?))?;
/// let page_ref = pages::page_refs(&mut source)?[0];
/// let closure = page_closure::page_object_closure(&mut source, page_ref)?;
/// let renumber = copy_objects(&mut source, &mut target, &closure)?;
/// println!("copied {} objects", renumber.len());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn copy_objects<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    refs: &BTreeSet<ObjectRef>,
) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
    // Next free target object number: one past the current maximum.
    let base = target
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        + 1;

    // Pre-allocate a fresh target number for every ref in the set, iterating in
    // sorted order (BTreeSet) for deterministic output.  Building the complete
    // map before rewriting is what makes cycles safe.  Allocation is bounded by
    // the `u32` object-number space; exhaustion is an error rather than a
    // silent wraparound.
    let mut map: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    for (offset, &src_ref) in refs.iter().enumerate() {
        map.insert(
            src_ref,
            ObjectRef::new(alloc_target_number(base, offset)?, 0),
        );
    }

    // Resolve each source object, rewrite its references in place, and store it.
    // `resolve` already returns an owned `Object`, so rewriting in place avoids
    // a second deep clone of (potentially large) stream payloads.
    for &src_ref in refs {
        let mut obj = source.resolve(src_ref)?;
        rewrite_refs(&mut obj, 0, &map)?;
        target.set_object(map[&src_ref], obj);
    }

    Ok(map)
}

/// Copy foreign objects while retaining qpdf's per-source object identity map.
///
/// `QPDF::copyForeignObject` stores its map on the destination `QPDF`, keyed
/// by the source document's unique identity. Page insertion uses this variant;
/// the public [`copy_objects`] helper intentionally remains a fresh-copy API.
pub(crate) fn copy_foreign_objects<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    refs: &BTreeSet<ObjectRef>,
) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
    let source_id = source.unique_id();
    let mut map = target.take_foreign_object_map(source_id);
    let copied = (|| {
        let mut to_copy = Vec::new();
        let mut base = None;

        for &source_ref in refs {
            if let std::collections::btree_map::Entry::Vacant(entry) = map.entry(source_ref) {
                let base = match base {
                    Some(base) => base,
                    None => {
                        let first = target.next_available_object_ref()?.number;
                        base = Some(first);
                        first
                    }
                };
                let target_ref = ObjectRef::new(alloc_target_number(base, to_copy.len())?, 0);
                // Reserve before resolving any source object so cycles can be
                // rewritten through the complete map, as qpdf does.
                target.set_object(target_ref, Object::Null);
                entry.insert(target_ref);
                to_copy.push(source_ref);
            }
        }

        for source_ref in to_copy {
            let mut object = source.resolve(source_ref)?;
            rewrite_refs(&mut object, 0, &map)?;
            target.set_object(map[&source_ref], object);
        }

        Ok(refs
            .iter()
            .map(|source_ref| (*source_ref, map[source_ref]))
            .collect())
    })();
    target.set_foreign_object_map(source_id, map);
    copied
}

/// Deep-rewrite every [`Object::Reference`] in `obj` *in place*: refs present in
/// `map` are remapped, refs outside `map` become [`Object::Null`].  Stream byte
/// payloads are left untouched (never cloned); scalars are unchanged.
pub(crate) fn rewrite_refs(
    obj: &mut Object,
    depth: usize,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(format!(
            "cross-document copy: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(r) => {
            let replacement = match map.get(r) {
                Some(&t) => Object::Reference(t),
                None => Object::Null,
            };
            *obj = replacement;
        }
        Object::Array(items) => {
            for item in items.iter_mut() {
                rewrite_refs(item, depth + 1, map)?;
            }
        }
        Object::Dictionary(dict) => rewrite_dict(dict, depth + 1, map)?,
        Object::Stream(stream) => rewrite_dict(&mut stream.dict, depth + 1, map)?,
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::RealLiteral { .. }
        | Object::Name(_)
        | Object::String(_)
        | Object::Operator(_)
        | Object::InlineImage(_) => {}
    }
    Ok(())
}

/// Rewrite every value of `dict` via [`rewrite_refs`] in place, preserving keys.
///
/// This one-level fan-out helper forwards the **same** `depth` it received to
/// each value: its caller [`rewrite_refs`] already incremented `depth` when
/// descending into the dictionary, and each value re-enters [`rewrite_refs`]
/// where the shared depth guard is re-checked.
fn rewrite_dict(
    dict: &mut Dictionary,
    depth: usize,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    for value in dict.values_mut() {
        rewrite_refs(value, depth, map)?;
    }
    Ok(())
}

/// Compute the target object number for the `offset`-th member of the copy set,
/// counting up from `base`.  Returns [`Err`] when the allocation would overflow
/// the `u32` object-number space rather than wrapping or panicking.
fn alloc_target_number(base: u32, offset: usize) -> Result<u32> {
    u32::try_from(offset)
        .ok()
        .and_then(|o| base.checked_add(o))
        .ok_or_else(|| {
            Error::Unsupported(
                "cross-document copy exhausted the u32 object-number space".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::MAX_INLINE_DEPTH;
    use crate::ObjectHandle;
    use std::io::Cursor;
    use std::rc::Rc;

    fn minimal_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).unwrap()
    }

    fn pdf_with_stream(data: &[u8]) -> Pdf<Cursor<Vec<u8>>> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = bytes.len();
        bytes.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", data.len()).as_bytes(),
        );
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).unwrap()
    }

    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    #[test]
    fn alloc_target_number_counts_up_from_base() {
        assert_eq!(alloc_target_number(5, 0).unwrap(), 5);
        assert_eq!(alloc_target_number(5, 3).unwrap(), 8);
    }

    #[test]
    fn alloc_target_number_errors_on_overflow() {
        assert!(alloc_target_number(u32::MAX, 1).is_err());
        assert!(alloc_target_number(u32::MAX - 2, 5).is_err());
    }

    #[test]
    fn rewrite_refs_errors_on_excessive_nesting() {
        let map: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
        let mut obj = nested_arrays(MAX_INLINE_DEPTH + 5);
        let err = rewrite_refs(&mut obj, 0, &map);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn rewrite_refs_accepts_nesting_up_to_the_limit() {
        let mut map = BTreeMap::new();
        map.insert(ObjectRef::new(3, 0), ObjectRef::new(99, 0));
        // Bury one Reference so it is visited at exactly inline depth
        // MAX_INLINE_DEPTH (the deepest accepted level under the strict `>`
        // guard); it must be remapped, not errored.
        let mut obj = Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]);
        for _ in 0..(MAX_INLINE_DEPTH - 1) {
            obj = Object::Array(vec![obj]);
        }
        rewrite_refs(&mut obj, 0, &map).unwrap();
        // Unwrap the nested arrays down to the deepest element and confirm the
        // in-limit Reference was remapped to 99 0 R (not replaced with Null).
        let mut cur = &obj;
        loop {
            match cur {
                Object::Array(items) if items.len() == 1 => cur = &items[0],
                other => {
                    assert_eq!(other, &Object::Reference(ObjectRef::new(99, 0)));
                    break;
                }
            }
        }
    }

    #[test]
    fn foreign_copy_restores_existing_map_after_rewrite_failure() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let source_id = source.unique_id();
        copy_foreign_objects(
            &mut source,
            &mut target,
            &BTreeSet::from([ObjectRef::new(3, 0)]),
        )
        .unwrap();

        source.set_object(ObjectRef::new(4, 0), nested_arrays(MAX_INLINE_DEPTH + 5));
        assert!(copy_foreign_objects(
            &mut source,
            &mut target,
            &BTreeSet::from([ObjectRef::new(4, 0)]),
        )
        .is_err());

        assert!(target
            .take_foreign_object_map(source_id)
            .contains_key(&ObjectRef::new(3, 0)));
    }

    #[test]
    fn copy_foreign_object_preserves_shared_children_and_cycles() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();

        let shared = source
            .make_indirect_object_handle(ObjectHandle::integer(7))
            .expect("shared child");
        let first = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("first cycle node");
        let second = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("second cycle node");
        first.replace_key(b"/Next", second.clone());
        second.replace_key(b"/Next", first.clone());

        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/SharedA", shared.clone());
        root.replace_key(b"/SharedB", shared.clone());
        root.replace_key(b"/Cycle", first.clone());

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy foreign object");
        let copied_again = target
            .copy_foreign_object(&root)
            .expect("reuse foreign object map");

        assert!(copied.is_same_object_as(&copied_again));
        let copied_shared_a = copied.get_key(b"/SharedA");
        let copied_shared_b = copied.get_key(b"/SharedB");
        assert!(copied_shared_a.is_same_object_as(&copied_shared_b));
        assert_ne!(copied_shared_a.object_ref(), shared.object_ref());

        let copied_first = copied.get_key(b"/Cycle");
        let copied_second = copied_first.get_key(b"/Next");
        assert_eq!(
            copied_second.get_key(b"/Next").object_ref(),
            copied_first.object_ref()
        );
    }

    #[test]
    fn copy_foreign_object_matches_qpdf_input_classification_and_pages_boundary() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();

        let direct_error = target
            .copy_foreign_object(&ObjectHandle::integer(1))
            .expect_err("direct input must be rejected");
        assert!(matches!(direct_error, Error::System(message)
            if message == "QPDF::copyForeign called with direct object handle"));

        let owned = target.get_object_handle(ObjectRef::new(3, 0));
        let owned_error = target
            .copy_foreign_object(&owned)
            .expect_err("destination-owned input must be rejected");
        assert!(matches!(owned_error, Error::System(message)
            if message == "QPDF::copyForeign called with object from this QPDF"));

        let pages = source.get_object_handle(ObjectRef::new(2, 0));
        let copied_pages = target
            .copy_foreign_object(&pages)
            .expect("Pages boundary is a warning/null result");
        assert!(copied_pages.is_null());
        assert!(target
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("unexpected reference to /Pages object while copying foreign object")));
    }

    #[test]
    fn copy_foreign_object_rejects_a_foreign_reserved_object_before_reservation() {
        let source = minimal_pdf();
        let mut target = minimal_pdf();
        let reserved = source
            .new_reserved()
            .expect("source reserved construction sentinel");
        let target_refs_before = target.object_refs();

        let error = target
            .copy_foreign_object(&reserved)
            .expect_err("qpdf rejects a foreign reserved object during reservation");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF: attempting to copy a foreign reserved object"));
        assert_eq!(target.object_refs(), target_refs_before);
    }

    fn empty_copier<'a>(
        target: &'a mut Pdf<Cursor<Vec<u8>>>,
    ) -> ForeignObjectCopier<'a, Cursor<Vec<u8>>> {
        ForeignObjectCopier {
            target,
            object_map: BTreeMap::new(),
            visiting: BTreeSet::new(),
            direct_visiting: Vec::new(),
            to_copy: Vec::new(),
        }
    }

    #[test]
    fn foreign_copier_defensive_invariants_are_reported() {
        let error = {
            let mut target = minimal_pdf();
            let mut copier = empty_copier(&mut target);
            copier.visiting.insert(ObjectRef::new(900, 0));
            copier
                .run(&ObjectHandle::integer(1))
                .expect_err("a stale visiting set must be rejected")
        };
        assert!(matches!(error, Error::Internal(message)
            if message == "foreign object copier retained a visiting object"));

        let error = {
            let mut target = minimal_pdf();
            let mut copier = empty_copier(&mut target);
            copier.to_copy.push(ObjectHandle::integer(1));
            copier
                .run(&ObjectHandle::integer(2))
                .expect_err("a direct queued object must be rejected")
        };
        assert!(matches!(error, Error::Internal(message)
            if message == "foreign copier queued a direct object"));

        let error = {
            let mut target = minimal_pdf();
            let mut copier = empty_copier(&mut target);
            copier
                .run(&ObjectHandle::integer(3))
                .expect_err("a direct copier root must be rejected")
        };
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign called with direct object handle"));
    }

    #[test]
    fn foreign_copier_stops_reservation_on_a_direct_identity_cycle() {
        // NOTE: `direct.replace_key(b"/Self", direct.clone())` is a no-op —
        // `ObjectHandle::is_direct_value_alias` silently rejects a direct
        // handle aliasing itself, so `direct` never actually gains a "/Self"
        // key. This test pre-seeds `direct_visiting` below to exercise the
        // guard mechanically in isolation; it does not prove a naturally
        // constructed cycle reaches it. That organic coverage — a genuine
        // multi-hop direct cycle flowing through `reserve_objects` via the
        // public `copy_foreign_object` entry point — comes from
        // `copy_foreign_object_rejects_a_direct_identity_cycle_during_replacement`,
        // whose two-dictionary cycle also traverses reservation before
        // reaching the replacement-phase guard under test there.
        let direct = ObjectHandle::dictionary(Vec::new());
        direct.replace_key(b"/Self", direct.clone());
        let mut target = minimal_pdf();
        let mut copier = empty_copier(&mut target);
        copier.direct_visiting.push(direct.clone());

        copier
            .reserve_objects(direct, true)
            .expect("direct identity cycles are bounded during reservation");
    }

    #[test]
    fn copy_foreign_object_rejects_a_direct_identity_cycle_during_replacement() {
        // A single direct handle cannot alias itself through `replace_key`
        // (`ObjectHandle::is_direct_value_alias` silently no-ops that
        // insertion), but two direct dictionaries can still reference each
        // other, producing a genuine identity cycle with no indirect object
        // anywhere on the path. Reservation's `direct_visiting` guard (see
        // `foreign_copier_stops_reservation_on_a_direct_identity_cycle`)
        // lets reservation finish despite the cycle. Without an equivalent
        // guard here, the replacement phase recurses `a -> b -> a -> b ...`
        // without end. qpdf's own `replaceForeignIndirectObjects`
        // (`QPDF.cc:2158-2213`) has no visited-set at all, so this same
        // input would also recurse unbounded there; this is a flpdf-only
        // bound (see the `direct_visiting` field doc), not a qpdf parity
        // restoration, because no parsed PDF can produce a direct cycle in
        // the first place (only the public `ObjectHandle` API can).
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let a = ObjectHandle::dictionary(Vec::new());
        let b = ObjectHandle::dictionary(Vec::new());
        a.replace_key(b"/B", b.clone());
        b.replace_key(b"/A", a.clone());
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/A", a);

        let error = target
            .copy_foreign_object(&root)
            .expect_err("a direct identity cycle must not recurse unbounded during replacement");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign encountered a direct object cycle"));
    }

    #[test]
    fn copy_foreign_object_rejects_a_direct_stream_child() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let direct_stream = ObjectHandle::stream(
            ObjectHandle::dictionary(Vec::new()),
            Rc::new(b"direct stream".to_vec()),
        );
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", direct_stream);

        let error = target
            .copy_foreign_object(&root)
            .expect_err("a direct stream has no qpdf foreign-copy route");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign encountered a direct stream object"));
    }

    #[test]
    fn foreign_copier_rejects_a_non_stream_destination_reservation() {
        let source = minimal_pdf();
        let source_stream = source.new_stream().expect("source stream");
        let source_ref = source_stream.object_ref().expect("source stream identity");
        let mut target = minimal_pdf();
        let wrong_destination = target
            .make_indirect_object_handle(ObjectHandle::integer(7))
            .expect("wrong destination");
        let wrong_ref = wrong_destination
            .object_ref()
            .expect("wrong destination identity");
        let error = {
            let mut copier = ForeignObjectCopier {
                target: &mut target,
                object_map: BTreeMap::from([(source_ref, wrong_ref)]),
                visiting: BTreeSet::new(),
                direct_visiting: Vec::new(),
                to_copy: Vec::new(),
            };
            copier
                .replace_foreign_indirect_objects(source_stream, true)
                .expect_err("a stream must map to a stream destination")
        };
        assert!(matches!(error, Error::Internal(message)
            if message == "foreign stream reservation is not a stream"));
    }

    #[test]
    fn copy_foreign_object_rejects_an_unowned_indirect_handle() {
        let mut target = minimal_pdf();
        let unowned = ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), -1);

        let error = target
            .copy_foreign_object(&unowned)
            .expect_err("an indirect handle without an owning PDF must be rejected");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign called with object with no owning PDF"));
    }

    #[test]
    fn copy_foreign_object_stops_at_nested_page_boundaries() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();

        let hidden = source
            .make_indirect_object_handle(ObjectHandle::integer(99))
            .expect("hidden page child");
        let page = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("nested page");
        page.replace_key(b"/Type", ObjectHandle::name(b"Page".to_vec()));
        page.replace_key(b"/Hidden", hidden.clone());
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Page", page.clone());

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy root with nested page");
        let copied_page = copied.get_key(b"/Page");
        assert!(copied_page.is_null());
        assert!(copied_page.object_ref().is_some());
        assert!(!target
            .object_refs()
            .iter()
            .any(|object_ref| *object_ref != ObjectRef::new(1, 0)
                && target.get_object_handle(*object_ref).as_integer() == Some(99)));
    }

    #[test]
    fn copy_foreign_object_recopies_a_nested_page_when_it_becomes_top_level() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let page = source.get_object_handle(ObjectRef::new(3, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Page", page.clone());

        let copied_root = target
            .copy_foreign_object(&root)
            .expect("copy root with nested page boundary");
        let boundary_page = copied_root.get_key(b"/Page");
        assert!(boundary_page.is_null());
        assert!(boundary_page.is_indirect());

        let copied_page = target
            .copy_foreign_object(&page)
            .expect("copy page as a top-level object");
        assert!(copied_page.is_same_object_as(&boundary_page));
        assert!(!copied_page.is_null());
        assert_eq!(
            copied_page.get_key(b"/Type").as_name(),
            Some(b"Page".to_vec())
        );
        assert!(copied_page.get_key(b"/Parent").is_null());
    }

    #[test]
    fn copy_foreign_object_shares_stream_buffers_without_copying_the_graph() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let data = Rc::new(b"foreign stream bytes".to_vec());
        let stream = source
            .new_stream_with_data(Rc::clone(&data))
            .expect("source stream");
        stream
            .as_stream_dict()
            .expect("source stream dictionary")
            .replace_key(b"/Filter", ObjectHandle::name(b"FlateDecode".to_vec()));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", stream.clone());

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy stream graph");
        let copied_stream = copied.get_key(b"/Stream");
        assert!(copied_stream.is_indirect());
        assert!(Rc::ptr_eq(
            &copied_stream.as_stream_data().expect("copied buffer"),
            &data
        ));
        assert_eq!(
            copied_stream
                .as_stream_dict()
                .expect("copied stream dictionary")
                .get_key(b"/Filter")
                .as_name(),
            Some(b"FlateDecode".to_vec())
        );
    }

    #[test]
    fn copy_foreign_object_defers_provider_streams_and_supports_immediate_copy() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let calls = Rc::new(std::cell::RefCell::new(0usize));
        let calls_for_provider = Rc::clone(&calls);
        let stream = source.new_stream().expect("source stream");
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, _suppress_warnings, _will_retry| {
                    *calls_for_provider.borrow_mut() += 1;
                    pipeline
                        .write(b"deferred foreign bytes")
                        .map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)?;
                    Ok(true)
                },
                None,
                None,
            )
            .expect("source provider");
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", stream.clone());

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy provider graph");
        let copied_stream = copied.get_key(b"/Stream");
        assert!(copied_stream.as_stream_data().is_none());
        assert_eq!(*calls.borrow(), 0);
        assert_eq!(
            copied_stream
                .get_raw_stream_data()
                .expect("deferred stream bytes")
                .as_ref(),
            b"deferred foreign bytes"
        );
        assert_eq!(*calls.borrow(), 1);

        let mut immediate_source = minimal_pdf();
        let mut immediate_target = minimal_pdf();
        let immediate_stream = immediate_source.new_stream().expect("immediate stream");
        immediate_stream
            .replace_stream_data_with_callback(|_pipeline| Ok(()), None, None)
            .expect("immediate provider");
        immediate_source.set_immediate_copy_from(true);
        let immediate_root = immediate_source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("immediate root");
        immediate_root.replace_key(b"/Stream", immediate_stream.clone());
        let immediate_copy = immediate_target
            .copy_foreign_object(&immediate_root)
            .expect("immediate foreign copy");
        assert!(Rc::ptr_eq(
            &immediate_stream
                .as_stream_data()
                .expect("materialized immediate source"),
            &immediate_copy
                .get_key(b"/Stream")
                .as_stream_data()
                .expect("shared immediate buffer")
        ));
    }

    #[test]
    fn copy_foreign_object_preserves_provider_failures_until_destination_read() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let stream = source.new_stream().expect("source stream");
        stream
            .replace_stream_data_with_retry_callback(
                |_pipeline, _suppress, _retry| {
                    Err(Error::System("foreign provider failed".to_owned()))
                },
                None,
                None,
            )
            .expect("source provider");
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", stream);

        let copied = target
            .copy_foreign_object(&root)
            .expect("provider failure must stay deferred");
        let error = copied
            .get_key(b"/Stream")
            .get_raw_stream_data()
            .expect_err("destination read must propagate the foreign provider error");
        assert!(matches!(error, Error::System(message) if message == "foreign provider failed"));
    }

    #[test]
    fn copy_foreign_object_keeps_original_file_streams_lazy() {
        let mut source = pdf_with_stream(b"original foreign bytes");
        let mut target = minimal_pdf();
        let source_stream = source.get_object_handle(ObjectRef::new(4, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", source_stream.clone());

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy original stream graph");
        let copied_stream = copied.get_key(b"/Stream");
        assert!(copied_stream.as_stream_data().is_none());
        assert_eq!(
            copied_stream
                .get_raw_stream_data()
                .expect("original stream bytes")
                .as_ref(),
            b"original foreign bytes"
        );
    }

    #[test]
    fn copy_foreign_object_original_file_streams_are_not_bound_to_the_source_pdf() {
        let mut target = minimal_pdf();
        let copied_stream = {
            let mut source = pdf_with_stream(b"source lifetime bytes");
            let source_stream = source.get_object_handle(ObjectRef::new(4, 0));
            let root = source
                .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
                .expect("root");
            root.replace_key(b"/Stream", source_stream);
            target
                .copy_foreign_object(&root)
                .expect("copy original stream graph")
                .get_key(b"/Stream")
        };

        assert_eq!(
            copied_stream
                .get_raw_stream_data()
                .expect("original source stream remains readable after source Pdf drop")
                .as_ref(),
            b"source lifetime bytes"
        );
    }

    #[test]
    fn copy_foreign_object_rebuilds_direct_containers_with_destination_children() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let shared = source
            .make_indirect_object_handle(ObjectHandle::integer(11))
            .expect("shared child");
        let nested = ObjectHandle::dictionary(vec![
            (b"/Name".to_vec(), ObjectHandle::name(b"Nested".to_vec())),
            (b"/Shared".to_vec(), shared.clone()),
        ]);
        let array = ObjectHandle::array(vec![nested.clone(), shared.clone()]);
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Array", array);

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy direct containers");
        let copied_array = copied.get_key(b"/Array");
        let copied_items = copied_array.as_array().expect("copied array");
        let copied_nested = copied_items[0].clone();
        let copied_shared_from_dict = copied_nested.get_key(b"/Shared");
        let copied_shared_from_array = copied_items[1].clone();
        assert!(copied_shared_from_dict.is_same_object_as(&copied_shared_from_array));
        assert_eq!(
            copied_nested.get_key(b"/Name").as_name(),
            Some(b"Nested".to_vec())
        );
        assert_ne!(copied_shared_from_array.object_ref(), shared.object_ref());
    }

    #[test]
    fn copy_foreign_object_omits_a_dictionary_key_left_unreserved_by_a_pages_boundary() {
        // qpdf's own dictionary branch of `replaceForeignIndirectObjects`
        // (`libqpdf/QPDF.cc:2192-2196`) builds the copy through
        // `result.replaceKey`, whose direct-null rule
        // (`QPDF_Dictionary::replaceKey`, `libqpdf/QPDF_Dictionary.cc:
        // 135-146`) *removes* a key replaced with a direct null rather than
        // storing it. `/Kids` here points at a `/Pages` object, which
        // `reserve_objects` deliberately never reserves (the /Pages
        // boundary), so replacement's `!top` branch falls back to
        // `ObjectHandle::null()` -- a direct null -- for it.
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();

        let pages = source.get_object_handle(ObjectRef::new(2, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Kids", pages);

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy root referencing an unreserved /Pages object");

        assert!(
            !copied.has_key(b"/Kids"),
            "a direct-null replacement must remove the key, matching qpdf's replaceKey"
        );
    }

    #[test]
    fn copy_foreign_object_copies_top_level_scalar_as_a_destination_handle() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let scalar = source
            .make_indirect_object_handle(ObjectHandle::integer(123))
            .expect("scalar");

        let copied = target.copy_foreign_object(&scalar).expect("copy scalar");
        assert!(copied.is_indirect());
        assert_eq!(copied.as_integer(), Some(123));
        assert!(!copied.is_same_object_as(&scalar));
    }
}
