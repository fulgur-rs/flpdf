//! qpdf correspondence: the canonical `QPDF::copyForeignObject` graph copy lives here.
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

// Stack-safety constants for `ForeignObjectCopier`'s two recursion hubs
// (`reserve_objects`, `replace_foreign_indirect_objects`), mirroring
// `parser.rs`'s own `STACK_RED_ZONE`/`STACK_GROWTH_SIZE` values (kept as
// separate local constants rather than imported cross-module, matching that
// file's own precedent, since this pair's scope is limited to this file).
const OBJECT_COPY_STACK_RED_ZONE: usize = 32 * 1024;
const OBJECT_COPY_STACK_GROWTH_SIZE: usize = 1024 * 1024;

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
    let visiting = target.take_foreign_object_visiting(source_id);
    if !visiting.is_empty() {
        // qpdf persists `ObjCopier::object_map`/`visiting` on the
        // destination and never rolls either back on failure
        // (`libqpdf/QPDF.cc:2019-2093` holds one mutable reference to the
        // shared `m->object_copiers[...]` entry for the whole call). A
        // `reserveObjects` traversal failure -- a foreign reserved object, a
        // cross-document child, or an unresolvable reference -- leaves the
        // still-unwound ancestor chain in `visiting`
        // (`reserve_objects_inner`'s `self.visiting.remove` sits after the
        // fallible recursive call and is skipped on `Err`, mirroring qpdf's
        // own unguarded `obj_copier.visiting.erase` at the end of
        // `reserveObjects`, `QPDF.cc:2150`). qpdf checks exactly this before
        // any traversal begins and throws rather than let a later call
        // re-run reservation against a partially populated `object_map`,
        // which would see those entries as already-copied, skip real work,
        // and silently return an unfinished copy as success
        // (`QPDF.cc:2066-2069`). Put the untouched state back so this
        // poisoned condition persists for every subsequent call against this
        // source too, matching qpdf: there is no unpoisoning path either
        // here or there.
        target.set_foreign_object_map(source_id, object_map);
        target.set_foreign_object_visiting(source_id, visiting);
        return Err(Error::Internal(
            "obj_copier.visiting is not empty at the beginning of copyForeignObject".to_owned(),
        ));
    }
    let mut copier = ForeignObjectCopier {
        target,
        source_id,
        object_map,
        visiting,
        direct_visiting: Vec::new(),
        to_copy: Vec::new(),
    };
    let result = copier.run(foreign);
    let object_map = copier.object_map;
    let visiting = copier.visiting;
    copier.target.set_foreign_object_map(source_id, object_map);
    copier
        .target
        .set_foreign_object_visiting(source_id, visiting);
    result
}

struct ForeignObjectCopier<'a, R: Read + Seek + 'static> {
    target: &'a mut Pdf<R>,
    /// The root's owning document identity (qpdf's `other.m->unique_id`,
    /// `libqpdf/QPDF.cc:2060-2065`) -- `object_map`/`visiting` are keyed by
    /// bare source `ObjectRef` numbers under the assumption that every
    /// indirect node encountered belongs to this one document, the same
    /// assumption qpdf's own `reserveObjects`/`replaceForeignIndirectObjects`
    /// make. [`ObjectHandle::replace_key`]/`QPDF_Array`'s mutators run the
    /// same shallow `checkOwnership`
    /// (`libqpdf/QPDFObjectHandle.cc:2355-2365`, `QPDF_Array.cc:10-26`) qpdf
    /// itself does at insertion time, which only compares each mutated
    /// handle's own owning document -- it does not, in qpdf either, walk
    /// into a directly-inserted container's descendants. A foreign indirect
    /// object several direct hops below an otherwise-accepted direct value
    /// can therefore still reach a document's graph uncaught at construction
    /// time (in qpdf too, per `QPDF::copyForeignObject`'s own documented
    /// advice to the caller), so `reserve_objects` re-validates each node's
    /// owner here as a flpdf-specific defense qpdf itself does not need at
    /// this boundary (qpdf's own `reserveObjects`/
    /// `replaceForeignIndirectObjects`, `QPDF.cc:2101-2213`, have no such
    /// check; see `docs/qpdf-correspondence.md`).
    source_id: u64,
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

    // `reserve_objects` is the sole recursion hub for reservation: every
    // nested descent, including a chain of *separate* indirect objects
    // (`A -> B -> C -> ...`, each a distinct object number, not nested
    // containers within one object) recurses only through it via
    // `reserve_children`. A parsed document's own container-nesting limit
    // (`MAX_PARSE_DEPTH`, `parser.rs`) bounds direct nesting *within* one
    // object, but does not bound this kind of chain: each hop is a fresh,
    // independently-parsed indirect object, so a document with a
    // sufficiently long reference chain can drive this recursion arbitrarily
    // deep with no qpdf-parity depth limit to stop it (qpdf's own
    // `reserveObjects`, `libqpdf/QPDF.cc:2101-2151`, has no bound here
    // either). Wrapping this hub in `stacker::maybe_grow` -- the same
    // Rust-implementation-safety mechanism already used for `parser.rs`'s
    // `object`, `reader.rs`'s live-object resolution, and
    // `object_handle.rs`'s unparse family -- keeps a sufficiently long
    // (but non-cyclic; `visiting` already bounds cycles) chain from
    // exhausting the caller's stack and aborting the process.
    fn reserve_objects(&mut self, foreign: ObjectHandle, top: bool) -> Result<()> {
        stacker::maybe_grow(
            OBJECT_COPY_STACK_RED_ZONE,
            OBJECT_COPY_STACK_GROWTH_SIZE,
            || self.reserve_objects_inner(foreign, top),
        )
    }

    fn reserve_objects_inner(&mut self, foreign: ObjectHandle, top: bool) -> Result<()> {
        foreign.try_dereference()?;
        if foreign.is_reserved() {
            return Err(Error::System(
                "QPDF: attempting to copy a foreign reserved object".to_owned(),
            ));
        }
        // qpdf's `checkOwnership` (see `source_id`'s own doc) runs at
        // construction time, before a value from a second document could
        // ever be attached anywhere reachable from the root being copied, so
        // `reserveObjects`/`replaceForeignIndirectObjects` never need to
        // re-check it. `ObjectHandle::replace_key` performs no equivalent
        // check yet, so a caller can attach an indirect handle from a
        // *second* source `Pdf` into this graph. Without this check, that
        // child's bare `ObjectRef` would be looked up in `object_map`/
        // `visiting` as if it belonged to `source_id`; if the two documents
        // happen to share an object number, the child would be silently
        // treated as already-copied and mapped to the wrong document's
        // object instead of being copied itself. A `None` owner is
        // permitted, mirroring qpdf's own leniency for an ownerless value
        // (`item_qpdf == nullptr`, `libqpdf/QPDFObjectHandle.cc:2356-2364`).
        if let Some(owner_id) = foreign.owning_pdf_unique_id() {
            if owner_id != self.source_id {
                return Err(Error::System(
                    "QPDF::copyForeign encountered an object owned by a different document"
                        .to_owned(),
                ));
            }
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

            let target_ref;
            if let Some(&existing_target_ref) = self.object_map.get(&source_ref) {
                let mapped = self.target.get_object_handle(existing_target_ref);
                if !(top && is_page && mapped.is_null()) {
                    return Ok(());
                }
                target_ref = existing_target_ref;
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
                target_ref = mapped.object_ref().ok_or_else(|| {
                    Error::Internal("foreign copier created a direct reservation".to_owned())
                })?;
                // cov:ignore-end
                self.object_map.insert(source_ref, target_ref);
            }

            self.visiting.insert(source_ref);
            if !top && is_page {
                // A nested `/Page` reservation never enters `to_copy` (qpdf's
                // own `reserveObjects` returns without queuing it too,
                // `QPDF.cc:2124-2132`), so `run()`'s replace loop -- the only
                // other call site that dirty-marks a freshly reserved
                // object -- never reaches it. Left unmarked, this indirect
                // null placeholder is registered in the handle registry and
                // referenced by the copied ancestor, yet never scheduled for
                // canonical writer output: a full rewrite would emit the
                // reference but not the placeholder object itself, leaving it
                // dangling (see `make_indirect_from_object_handle`'s own doc
                // on this exact failure mode).
                self.target.mark_object_dirty(target_ref);
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
            // qpdf's `QPDF_Dictionary::getKeys()` omits values for which
            // `isNull()` is true (`libqpdf/QPDF_Dictionary.cc:118-125`).
            // This includes indirect references that resolve to null, so
            // those children must not be reserved before replacement either.
            for (_, item) in entries {
                if item.try_is_null()? {
                    continue;
                }
                self.reserve_objects(item, false)?;
            }
        } else if let Some(dictionary) = foreign.as_stream_dict() {
            self.reserve_objects(dictionary, false)?;
        }
        Ok(())
    }

    // The sole recursion hub for replacement, mirroring `reserve_objects`'s
    // own hub/wrap split above and for the identical reason: `top=false`
    // only short-circuits an *indirect* child through a plain map lookup
    // (no recursion), but a genuinely direct value -- an array, dictionary,
    // or stream dictionary entry with no object identity of its own --
    // still recurses back through this function via `replace_foreign_value`,
    // and a long run of such direct nesting has the same unbounded-stack
    // exposure `reserve_objects`'s own doc explains.
    fn replace_foreign_indirect_objects(
        &mut self,
        foreign: ObjectHandle,
        top: bool,
    ) -> Result<ObjectHandle> {
        stacker::maybe_grow(
            OBJECT_COPY_STACK_RED_ZONE,
            OBJECT_COPY_STACK_GROWTH_SIZE,
            || self.replace_foreign_indirect_objects_inner(foreign, top),
        )
    }

    fn replace_foreign_indirect_objects_inner(
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
                // qpdf obtains stream-dictionary keys through the same
                // `QPDF_Dictionary::getKeys()` filter, so direct and indirect
                // null values are not copied (`libqpdf/QPDF.cc:2200-2213`,
                // `libqpdf/QPDF_Dictionary.cc:118-125`).
                if value.try_is_null()? {
                    continue;
                }
                let replacement = self.replace_foreign_indirect_objects(value, false)?;
                destination_dictionary.replace_key(&key, replacement)?;
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
                // `QPDF_Dictionary::getKeys()` excludes direct and indirect
                // null values (`QPDF_Dictionary.cc:118-125`). Resolve the
                // value exactly once here so the copy never reserves or
                // reattaches a key qpdf would not visit.
                if value.try_is_null()? {
                    continue;
                }
                let replacement = self.replace_foreign_indirect_objects(value, false)?;
                copied.replace_key(&key, replacement)?;
            }
            return Ok(copied);
        }

        // `ObjectValue::Reference` -- an indirect object whose own resolved
        // value is itself a bare reference to another object, e.g. via the
        // legacy `Pdf::set_object(holder, Object::Reference(target))` API --
        // has no qpdf counterpart: a live `QPDFObjectHandle` can never carry
        // this shape (see `ObjectValue::Reference`'s own doc). Falling
        // through to the generic scalar copy below would preserve `target`'s
        // raw source-document `ObjectRef` verbatim (`shallow_copy` clones an
        // unrecognized value as-is) without reserving or remapping it, so the
        // copied holder would point at whatever object happens to share that
        // number in the destination document -- an unrelated object, or a
        // dangling reference -- rather than a copy of the actual source
        // target. Reject explicitly instead, matching the direct-stream
        // rejection above for the same reason: this is a value shape qpdf's
        // copy machinery cannot express, not one this port can silently
        // reinterpret.
        if foreign.as_reference().is_some() {
            return Err(Error::System(
                "QPDF::copyForeign encountered an object whose value is itself a reference"
                    .to_owned(),
            ));
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
    copy_objects_with_seed(source, target, refs, &BTreeMap::new())
}

/// Like [`copy_objects`], but `seed` pre-populates the renumbering map with
/// source refs that must remap to an *existing* target object rather than a
/// freshly copied one.
///
/// A seeded ref in `refs` is neither given a new target number nor copied
/// (the loops below skip it): its entry in `seed` is the only mapping used,
/// so an edge from another copied object that points at it resolves to the
/// target's own existing object instead of becoming `Object::Null`
/// (unseeded, unmapped refs) or a redundant duplicate (seeded but still
/// copied). Used by the multi-source `--pages --preserve-unreferenced`
/// merge (`job/page_merge.rs`) to let a preserved primary orphan's reference
/// to the primary's original Catalog/Pages root resolve to the merge
/// target's own Catalog/Pages root, matching qpdf's in-place primary
/// mutation (`QPDFJob.cc:2481-2489`) where that reference was never
/// severed in the first place.
pub(crate) fn copy_objects_with_seed<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    refs: &BTreeSet<ObjectRef>,
    seed: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
    // Next free target object number: one past the current maximum.
    let base = target
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        + 1;

    // Pre-allocate a fresh target number for every non-seeded ref in the
    // set, iterating in sorted order (BTreeSet) for deterministic output.
    // Building the complete map before rewriting is what makes cycles safe.
    // Allocation is bounded by the `u32` object-number space; exhaustion is
    // an error rather than a silent wraparound.
    let mut map: BTreeMap<ObjectRef, ObjectRef> = seed.clone();
    let mut offset = 0usize;
    for &src_ref in refs {
        if map.contains_key(&src_ref) {
            continue;
        }
        map.insert(
            src_ref,
            ObjectRef::new(alloc_target_number(base, offset)?, 0),
        );
        offset += 1;
    }

    // Resolve each non-seeded source object, rewrite its references in
    // place, and store it. `resolve` already returns an owned `Object`, so
    // rewriting in place avoids a second deep clone of (potentially large)
    // stream payloads. A seeded ref is not re-copied: the target already
    // owns the object it maps to.
    for &src_ref in refs {
        if seed.contains_key(&src_ref) {
            continue;
        }
        let mut obj = source.resolve(src_ref)?;
        rewrite_refs(&mut obj, 0, &map)?;
        target.set_object(map[&src_ref], obj);
    }

    Ok(map)
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
    use crate::{ObjectHandle, ObjectStreamMode, PdfWriter};
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
        first.replace_key(b"/Next", second.clone()).unwrap();
        second.replace_key(b"/Next", first.clone()).unwrap();

        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/SharedA", shared.clone()).unwrap();
        root.replace_key(b"/SharedB", shared.clone()).unwrap();
        root.replace_key(b"/Cycle", first.clone()).unwrap();

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
    fn copy_foreign_object_omits_indirect_null_dictionary_keys_like_qpdf_get_keys() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let target_refs_before = target.object_refs();
        let indirect_null = source
            .make_indirect_object_handle(ObjectHandle::null())
            .expect("indirect null");
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/IndirectNull", indirect_null)
            .expect("attach indirect null");

        let copied = target
            .copy_foreign_object(&root)
            .expect("copy dictionary with an indirect null entry");

        assert!(!copied.has_key(b"/IndirectNull"));
        assert_eq!(
            target.object_refs().len(),
            target_refs_before.len() + 1,
            "qpdf getKeys excludes the null child before reservation"
        );
    }

    #[test]
    fn copy_foreign_stream_omits_indirect_null_dictionary_keys_like_qpdf_get_keys() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let target_refs_before = target.object_refs();
        let indirect_null = source
            .make_indirect_object_handle(ObjectHandle::null())
            .expect("indirect null");
        let stream = source.new_stream().expect("stream");
        stream
            .as_stream_dict()
            .expect("stream dictionary")
            .replace_key(b"/IndirectNull", indirect_null)
            .expect("attach indirect null");

        let copied = target
            .copy_foreign_object(&stream)
            .expect("copy stream with an indirect null dictionary entry");

        assert!(!copied
            .as_stream_dict()
            .expect("copied stream dictionary")
            .has_key(b"/IndirectNull"));
        assert_eq!(
            target.object_refs().len(),
            target_refs_before.len() + 1,
            "qpdf getKeys excludes the null stream-dictionary child before reservation"
        );
    }

    #[test]
    fn replace_foreign_stream_omits_indirect_null_dictionary_keys_like_qpdf_get_keys() {
        let mut source = minimal_pdf();
        let indirect_null = source
            .make_indirect_object_handle(ObjectHandle::null())
            .expect("indirect null");
        let source_stream = source.new_stream().expect("source stream");
        source_stream
            .as_stream_dict()
            .expect("source stream dictionary")
            .replace_key(b"/IndirectNull", indirect_null.clone())
            .expect("attach indirect null");
        assert!(source_stream
            .as_stream_dict()
            .expect("source stream dictionary")
            .try_get_key(b"/IndirectNull")
            .expect("read source stream dictionary entry")
            .is_null());

        let mut target = minimal_pdf();
        let target_stream = target.new_stream().expect("target stream");
        let target_value = target
            .make_indirect_object_handle(ObjectHandle::integer(7))
            .expect("target replacement");
        let source_stream_ref = source_stream.object_ref().expect("source stream identity");
        let source_null_ref = indirect_null.object_ref().expect("source null identity");
        let target_stream_ref = target_stream.object_ref().expect("target stream identity");
        let target_value_ref = target_value.object_ref().expect("target value identity");

        let copied = {
            let mut copier = ForeignObjectCopier {
                target: &mut target,
                source_id: source.unique_id(),
                object_map: BTreeMap::from([
                    (source_stream_ref, target_stream_ref),
                    (source_null_ref, target_value_ref),
                ]),
                visiting: BTreeSet::new(),
                direct_visiting: Vec::new(),
                to_copy: Vec::new(),
            };
            copier
                .replace_foreign_indirect_objects(source_stream, true)
                .expect("replace foreign stream")
        };

        assert!(!copied
            .as_stream_dict()
            .expect("copied stream dictionary")
            .has_key(b"/IndirectNull"));
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
            // These defensive-invariant tests never exercise the ownership
            // check (their `foreign` handles are either contextless direct
            // scalars or already fail before reaching it), so this value is
            // never read; `0` is a placeholder, not a real document identity.
            source_id: 0,
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
        direct.replace_key(b"/Self", direct.clone()).unwrap();
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
        a.replace_key(b"/B", b.clone()).unwrap();
        b.replace_key(b"/A", a.clone()).unwrap();
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/A", a).unwrap();

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
        root.replace_key(b"/Stream", direct_stream).unwrap();

        let error = target
            .copy_foreign_object(&root)
            .expect_err("a direct stream has no qpdf foreign-copy route");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign encountered a direct stream object"));
    }

    #[test]
    fn copy_foreign_object_rejects_a_bare_reference_value() {
        // `ObjectValue::Reference` -- an indirect object whose own resolved
        // value is itself a bare reference, constructed the way
        // `Pdf::set_object(holder, Object::Reference(target))`-based
        // holder-chain redirects do -- has no qpdf counterpart. Falling
        // through to the generic scalar-copy branch would preserve the raw
        // source-document `ObjectRef` verbatim, unremapped, into the
        // destination document.
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();

        let redirect_ref = ObjectRef::new(10, 0);
        source.set_object(redirect_ref, Object::Reference(ObjectRef::new(1, 0)));
        let redirect = source.get_object_handle(redirect_ref);
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Redirect", redirect).unwrap();

        let error = target
            .copy_foreign_object(&root)
            .expect_err("a bare-reference value has no qpdf foreign-copy route");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign encountered an object whose value is itself a reference"));
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
                source_id: source.unique_id(),
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
    fn copy_foreign_object_rejects_a_child_owned_by_a_different_document() {
        // `ObjectHandle::replace_key`'s own `checkOwnership`
        // (`libqpdf/QPDFObjectHandle.cc:2355-2365`) rejects attaching an
        // indirect handle from a *second* source `Pdf` directly as a
        // dictionary value, so this seeds `root`'s dictionary with the
        // foreign child through the dictionary constructor instead --
        // qpdf's `checkOwnership` is a shallow, insertion-time check with no
        // construction-time equivalent (see `ForeignObjectCopier::source_id`'s
        // own doc), so a value already present when a dictionary is minted
        // indirect never passes through it. Without the reservation-time
        // ownership check this test exercises, this child's bare `ObjectRef`
        // would be looked up in a map keyed for the root's source document;
        // both `minimal_pdf` instances share the same object numbering
        // (1..=3), so the collision would silently map `/Foreign` back to
        // the *root's own* copy instead of copying the second document's
        // distinct object.
        let mut source = minimal_pdf();
        let mut other_source = minimal_pdf();
        let mut target = minimal_pdf();

        let foreign_child = other_source.get_object_handle(ObjectRef::new(1, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/Foreign".to_vec(),
                foreign_child,
            )]))
            .expect("root");

        let error = target
            .copy_foreign_object(&root)
            .expect_err("a child owned by a different document than the root must be rejected");
        assert!(matches!(error, Error::System(message)
            if message == "QPDF::copyForeign encountered an object owned by a different document"));
    }

    #[test]
    fn copy_foreign_object_poisons_retry_after_a_reservation_phase_failure() {
        // Regression test for round-3 Codex finding #1: qpdf never rolls
        // `ObjCopier::object_map`/`visiting` back when `copyForeignObject`
        // fails partway (`libqpdf/QPDF.cc:2019-2093` holds one mutable
        // reference to the persistent per-source `ObjCopier` for the whole
        // call). A `reserveObjects` traversal failure leaves the ancestor
        // chain's refs in `visiting`, and qpdf's *next* call for that same
        // source checks `visiting.empty()` before doing any work and throws
        // (`QPDF.cc:2066-2069`) rather than let a later call see the
        // previous attempt's partial `object_map` as already-copied and
        // silently return an unfinished graph as success.
        let mut source = minimal_pdf();
        let mut other_source = minimal_pdf();
        let mut target = minimal_pdf();

        // See `copy_foreign_object_rejects_a_child_owned_by_a_different_
        // document` above for why the foreign child is seeded through the
        // dictionary constructor rather than `replace_key`.
        let foreign_child = other_source.get_object_handle(ObjectRef::new(1, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/Foreign".to_vec(),
                foreign_child,
            )]))
            .expect("root");

        let first_error = target
            .copy_foreign_object(&root)
            .expect_err("a child owned by a different document than the root must be rejected");
        assert!(matches!(&first_error, Error::System(message)
            if message == "QPDF::copyForeign encountered an object owned by a different document"));

        // Retrying the same root against the same source must not silently
        // "succeed" by treating the first attempt's partial `object_map` (the
        // root's own null reservation, inserted before the failing child was
        // reached) as a complete copy.
        let retry_error = target
            .copy_foreign_object(&root)
            .expect_err("a source poisoned by a reservation-phase failure must stay rejected");
        assert!(matches!(retry_error, Error::Internal(message)
            if message == "obj_copier.visiting is not empty at the beginning of copyForeignObject"));
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
        page.replace_key(b"/Type", ObjectHandle::name(b"Page".to_vec()))
            .unwrap();
        page.replace_key(b"/Hidden", hidden.clone()).unwrap();
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Page", page.clone()).unwrap();

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
        root.replace_key(b"/Page", page.clone()).unwrap();

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
    fn copy_foreign_object_dirty_marks_a_nested_page_null_reservation() {
        // Regression test for round-3 Codex finding #2: a nested `/Page`
        // reservation created while copying some other, unrelated root
        // returns before ever entering `to_copy` (`QPDF.cc:2124-2132`
        // mirrors this early return). Without an explicit dirty mark, that
        // fresh indirect null placeholder -- now referenced by the copied
        // ancestor -- would be dropped from a full rewrite, leaving the
        // reference dangling in the written output.
        //
        // The holder is an *array* member, not a dictionary key: qpdf's own
        // `QPDF_Dictionary::getKeys()` hides any key whose value currently
        // resolves to null (`QPDF_Dictionary.cc:118-127`), so a dict-keyed
        // reference to a still-null placeholder self-heals by disappearing
        // from serialization entirely, regardless of dirty state. Arrays
        // have no such elision (`QPDF_Array` keeps every position), so an
        // array-held nested-page reference is the shape that actually
        // exercises the dangling-reference failure the finding describes.
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let page = source.get_object_handle(ObjectRef::new(3, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/Kids".to_vec(),
                ObjectHandle::array(vec![page.clone()]),
            )]))
            .expect("root");

        let copied_root = target
            .copy_foreign_object(&root)
            .expect("copy root with nested page boundary");
        let kids = copied_root.get_key(b"/Kids");
        let boundary_ref = kids
            .as_array()
            .expect("Kids is an array")
            .first()
            .expect("Kids has one element")
            .object_ref()
            .expect("nested page reservation is indirect");

        assert!(
            target.dirty_object_refs().contains(&boundary_ref),
            "nested /Page reservation must be scheduled for writer output"
        );

        // Link the copied root into the target's reachable graph (a floating
        // handle proves nothing about a full rewrite: the canonical writer
        // discovers objects by walking from `/Root`, so the copied root
        // itself must be reachable for its `/Kids` array to be discovered
        // and put to the dangling-reference test the finding describes).
        let catalog_ref = target.root_ref().expect("target has a root");
        let catalog = target.get_object_handle(catalog_ref);
        catalog
            .replace_key(b"/CopiedRoot", copied_root.clone())
            .expect("copy_foreign_object mints a target-owned handle, same document as catalog");
        target.mark_object_dirty(catalog_ref);

        let mut writer = PdfWriter::new(&mut target);
        writer.set_object_stream_mode(ObjectStreamMode::Disable);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let written_ref = writer
            .get_renumbered_obj_gen(boundary_ref)
            .expect("query renumbering")
            .expect(
                "nested page placeholder must survive a full rewrite instead of \
                 leaving the copied ancestor's array reference dangling",
            );
        let out = writer.get_buffer().expect("take full-rewrite output");

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        assert!(reopened.object_refs().contains(&written_ref));
        assert!(reopened
            .resolve(written_ref)
            .expect("resolve written placeholder")
            .is_null());
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
            .replace_key(b"/Filter", ObjectHandle::name(b"FlateDecode".to_vec()))
            .unwrap();
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", stream.clone()).unwrap();

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
        root.replace_key(b"/Stream", stream.clone()).unwrap();

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
        immediate_root
            .replace_key(b"/Stream", immediate_stream.clone())
            .unwrap();
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
        root.replace_key(b"/Stream", stream).unwrap();

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
        root.replace_key(b"/Stream", source_stream.clone()).unwrap();

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
            root.replace_key(b"/Stream", source_stream).unwrap();
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
    fn copy_foreign_original_stream_reports_late_source_failures_to_destination() {
        let mut source = pdf_with_stream(b"source bytes that will be truncated");
        let mut target = minimal_pdf();
        let source_stream = source.get_object_handle(ObjectRef::new(4, 0));
        let root = source
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("root");
        root.replace_key(b"/Stream", source_stream.clone())
            .expect("attach source stream to root");

        let copied_stream = target
            .copy_foreign_object(&root)
            .expect("copy original stream graph")
            .get_key(b"/Stream");
        let expected_offset = u64::try_from(source_stream.get_parsed_offset())
            .expect("source stream must retain its data offset");

        source
            .resolver
            .with_reader_mut(|reader| reader.get_mut().clear());
        let _ = copied_stream
            .get_raw_stream_data()
            .expect_err("truncated source must fail during deferred read");

        let target_messages: Vec<_> = target
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        assert!(
            target_messages
                .iter()
                .any(|message| message.contains("unexpected EOF reading stream data")),
            "destination must own deferred source warnings: {target_messages:?}"
        );
        assert!(target
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| {
                diagnostic.offset == Some(expected_offset)
                    && diagnostic
                        .message
                        .contains("unexpected EOF reading stream data")
            }));
        assert!(source.repair_diagnostics().entries().is_empty());
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
        root.replace_key(b"/Array", array).unwrap();

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
        root.replace_key(b"/Kids", pages).unwrap();

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

    #[test]
    fn copy_foreign_object_survives_a_long_indirect_reference_chain_on_a_small_stack() {
        // A valid (non-cyclic) chain of *separate* indirect objects
        // (A -> B -> C -> ... -> N) keeps every hop on `reserve_objects`'s
        // call stack; `visiting` only bounds cycles, and no parsed-container
        // nesting limit bounds a chain across distinct object numbers (see
        // `reserve_objects`'s own doc). Run the copy on a deliberately small
        // stack: without `stacker::maybe_grow`, this depth reliably aborts
        // the process with a stack overflow instead of returning a `Result`.
        const CHAIN_DEPTH: usize = 2_000;

        let outcome = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut source = minimal_pdf();
                let mut leaf = source
                    .make_indirect_object_handle(ObjectHandle::integer(0))
                    .expect("leaf");
                for _ in 0..CHAIN_DEPTH {
                    let node = source
                        .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
                        .expect("chain node");
                    node.replace_key(b"/Next", leaf.clone()).unwrap();
                    leaf = node;
                }
                let mut target = minimal_pdf();
                let copied = target
                    .copy_foreign_object(&leaf)
                    .expect("a long acyclic chain must not overflow the caller stack");
                let is_indirect = copied.is_indirect();
                // Drop glue for this N-deep `Rc` chain recurses just as
                // deeply and is not `stacker`-protected anywhere in this
                // crate (see `live_file_parser_accepts_qpdfs_500_container_
                // limit_on_a_small_stack`'s own `ManuallyDrop` note for the
                // identical concern with the parser's own tree). Leak
                // everything reachable from this frame rather than letting
                // it drop on this deliberately small stack.
                std::mem::forget(source);
                std::mem::forget(target);
                std::mem::forget(leaf);
                std::mem::forget(copied);
                is_indirect
            })
            .expect("spawn small-stack copier thread")
            .join()
            .expect("copy_foreign_object must not overflow the caller stack");

        assert!(outcome);
    }
}
