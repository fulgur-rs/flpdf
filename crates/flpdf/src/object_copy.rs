//! qpdf correspondence: the canonical `QPDF::copyForeignObject` graph copy lives here.
//! Cross-document deep object copier (identity-preserving reservation + cycle handling).
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
//! # Identity reuse
//!
//! Each source document gets one destination-side map, so copying the same
//! source handle twice reuses the same target identity. Different source
//! documents remain independent because their maps are keyed by source
//! document identity.

use crate::object_handle::{ObjectHandle, ObjectValue};
use crate::{Error, ObjectRef, Pdf, Result};
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
/// This follows the live `ObjectHandle` graph, matching qpdf's
/// `QPDF::reserveObjects` and `QPDF::replaceForeignIndirectObjects`
/// (`libqpdf/QPDF.cc:2101-2210`). This is the implementation behind the
/// public [`Pdf::copy_foreign_object`] method, which is the sole public
/// cross-document copy entry point; keep this free function crate-private
/// so callers do not gain a second, redundant public spelling of the same
/// operation.
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
    copy_foreign_with_source_id(target, source_id, foreign, true, true)
}

/// Copy one primary object for the fresh page-merge target's
/// `--preserve-unreferenced` route.
///
/// qpdf's writer enqueues page-tree containers as ordinary objects when
/// preserving unreferenced objects (`QPDFWriter.cc:2907-2913`), while the
/// public `copyForeignObject` page-copy boundary intentionally stops at
/// `/Pages` (`QPDF.cc:2101-2210`). Keep those responsibilities separate: page
/// insertion retains the boundary, and this writer-preservation traversal
/// carries the otherwise-unreferenced page-tree graph into the fresh target.
pub(crate) fn copy_foreign_object_for_preserve<R: Read + Seek>(
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
    copy_foreign_with_source_id(target, source_id, foreign, true, false)
}

/// Copy a direct or indirect value from a foreign document while retaining the
/// destination's qpdf-shaped per-source copier map.
///
/// qpdf exposes this operation as the `replaceForeignIndirectObjects` half of
/// `QPDF::copyForeignObject` rather than as a separate public method: page-merge
/// metadata values are direct children of the primary Catalog or trailer, while
/// their indirect descendants still belong to the same `ObjCopier` map as the
/// selected pages. Keeping this boundary in `object_copy` lets callers copy a
/// direct array/dictionary without materializing it through the legacy
/// `Object` model (`QPDF.cc:2158-2213`).
pub(crate) fn copy_foreign_value<R: Read + Seek>(
    target: &mut Pdf<R>,
    source_id: u64,
    foreign: &ObjectHandle,
) -> Result<ObjectHandle> {
    if source_id == target.unique_id() {
        return Err(Error::System(
            "QPDF::copyForeign called with object from this QPDF".to_owned(),
        ));
    }
    if let Some(owner_id) = foreign.owning_pdf_unique_id() {
        if owner_id != source_id {
            return Err(Error::System(
                "QPDF::copyForeign encountered an object owned by a different document".to_owned(),
            ));
        }
    }
    copy_foreign_with_source_id(target, source_id, foreign, false, true)
}

#[allow(deprecated)]
fn copy_foreign_with_source_id<R: Read + Seek>(
    target: &mut Pdf<R>,
    source_id: u64,
    foreign: &ObjectHandle,
    require_indirect: bool,
    stop_at_page_tree: bool,
) -> Result<ObjectHandle> {
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
        stop_at_page_tree,
        to_copy: Vec::new(),
    };
    let result = if require_indirect {
        copier.run(foreign)
    } else {
        copier.run_value(foreign)
    };
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
    // Separable at field granularity, so CLAUDE.md's marker policy calls for
    // #[deprecated] here rather than a comment marker: qpdf's
    // reserveObjects/replaceForeignIndirectObjects (`QPDF.cc:2101-2213`) have
    // no direct-container cycle visited set at all. This guard exists only
    // because the public `ObjectHandle::replace_key` API can construct a
    // direct cycle no parsed PDF can express. Its accessors get
    // #[allow(deprecated)] locally rather than spreading unchecked.
    #[deprecated(
        note = "no qpdf counterpart; direct-cycle guard for graphs only constructible via ObjectHandle::replace_key -- do not add new callers"
    )]
    direct_visiting: Vec<ObjectHandle>,
    /// Whether this invocation is the qpdf `copyForeignObject` page boundary.
    /// The writer-preservation traversal disables the boundary so otherwise
    /// unreferenced `/Pages` containers can be carried to a fresh merge target.
    stop_at_page_tree: bool,
    to_copy: Vec<ObjectHandle>,
}

impl<R: Read + Seek + 'static> ForeignObjectCopier<'_, R> {
    fn run(&mut self, foreign: &ObjectHandle) -> Result<ObjectHandle> {
        self.run_value_inner(foreign, true)
    }

    fn run_value(&mut self, foreign: &ObjectHandle) -> Result<ObjectHandle> {
        self.run_value_inner(foreign, false)
    }

    fn run_value_inner(
        &mut self,
        foreign: &ObjectHandle,
        require_indirect: bool,
    ) -> Result<ObjectHandle> {
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

        if let Some(source_ref) = foreign.object_ref() {
            let Some(&target_ref) = self.object_map.get(&source_ref) else {
                self.target.resolver.push_damaged_warning(
                    "unexpected reference to /Pages object while copying foreign object; replacing with null",
                )?;
                return Ok(ObjectHandle::null());
            };
            return Ok(self.target.get_object_handle(target_ref));
        }
        if require_indirect {
            return Err(Error::System(
                "QPDF::copyForeign called with direct object handle".to_owned(),
            ));
        }
        self.replace_foreign_indirect_objects(foreign.clone(), true)
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

    #[allow(deprecated)]
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
        if self.stop_at_page_tree && foreign.try_is_dictionary_of_type(b"Pages", b"")? {
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

    #[allow(deprecated)]
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn preserve_copy_allows_page_tree_containers_without_pages_boundary_warning() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();

        let direct_error = copy_foreign_object_for_preserve(&mut target, &ObjectHandle::integer(1))
            .expect_err("preserve copy must reject a direct object");
        assert!(matches!(direct_error, Error::System(message)
            if message == "QPDF::copyForeign called with direct object handle"));

        let unowned = ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), -1);
        let unowned_error = copy_foreign_object_for_preserve(&mut target, &unowned)
            .expect_err("preserve copy must reject an unowned indirect object");
        assert!(matches!(unowned_error, Error::System(message)
            if message == "QPDF::copyForeign called with object with no owning PDF"));

        let owned = target.get_object_handle(ObjectRef::new(3, 0));
        let same_document_error = copy_foreign_object_for_preserve(&mut target, &owned)
            .expect_err("preserve copy must reject a target-owned object");
        assert!(matches!(same_document_error, Error::System(message)
            if message == "QPDF::copyForeign called with object from this QPDF"));

        let page = source.get_object_handle(ObjectRef::new(3, 0));
        let intermediate = source
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (b"Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
                (b"Kids".to_vec(), ObjectHandle::array(vec![page])),
                (b"Count".to_vec(), ObjectHandle::integer(1)),
            ]))
            .expect("intermediate page tree node");
        source
            .get_object_handle(ObjectRef::new(2, 0))
            .replace_key(b"Kids", ObjectHandle::array(vec![intermediate.clone()]))
            .expect("replace page-tree children");

        let copied = copy_foreign_object_for_preserve(&mut target, &intermediate)
            .expect("preserve traversal must copy a page-tree container");
        assert!(copied.object_ref().is_some());
        target
            .resolve(&copied)
            .expect("resolve preserved page tree");
        assert!(target.repair_diagnostics().entries().is_empty());
        assert_eq!(copied.get_key(b"/Kids").try_array_len().unwrap(), Some(1));
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
    #[allow(deprecated)]
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
                stop_at_page_tree: true,
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

    #[allow(deprecated)]
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
            stop_at_page_tree: true,
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
    #[allow(deprecated)]
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
        // input would also recurse unbounded there; this is a crate-specific
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
    #[allow(deprecated)]
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
                stop_at_page_tree: true,
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
        // qpdf never rolls
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
        // A nested `/Page` reservation created while copying some
        // other, unrelated root
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
        // exercises the dangling-reference failure.
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
        // and exercise the dangling-reference test).
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
        let written_handle = reopened.get_object_handle(written_ref);
        reopened
            .resolve(&written_handle)
            .expect("resolve written placeholder");
        assert!(written_handle.is_null());
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
    fn copy_foreign_value_reuses_the_persistent_map_for_direct_containers() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let shared = source
            .make_indirect_object_handle(ObjectHandle::integer(11))
            .expect("shared child");
        let value = ObjectHandle::dictionary(vec![(b"/Shared".to_vec(), shared.clone())]);

        let copied = copy_foreign_value(&mut target, source.unique_id(), &value)
            .expect("copy direct foreign value");
        let copied_again = copy_foreign_value(&mut target, source.unique_id(), &value)
            .expect("reuse direct foreign value map");

        assert!(copied
            .get_key(b"/Shared")
            .is_same_object_as(&copied_again.get_key(b"/Shared")));
        assert_eq!(copied.get_key(b"/Shared").as_integer(), Some(11));
        assert!(!copied.get_key(b"/Shared").is_same_object_as(&shared));
    }

    #[test]
    fn copy_foreign_value_rejects_wrong_document_identity() {
        let mut source = minimal_pdf();
        let mut target = minimal_pdf();
        let foreign = source.get_object_handle(ObjectRef::new(3, 0));

        let target_id = target.unique_id();
        let same_document = copy_foreign_value(&mut target, target_id, &foreign)
            .expect_err("a target-owned source id must be rejected");
        assert!(matches!(same_document, Error::System(message)
            if message == "QPDF::copyForeign called with object from this QPDF"));

        let wrong_source_id = target_id.wrapping_add(1);
        let wrong_owner = copy_foreign_value(&mut target, wrong_source_id, &foreign)
            .expect_err("a foreign handle owned by another source id must be rejected");
        assert!(matches!(wrong_owner, Error::System(message)
            if message == "QPDF::copyForeign encountered an object owned by a different document"));
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
