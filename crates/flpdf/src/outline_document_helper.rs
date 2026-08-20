//! qpdf correspondence: QPDFOutlineDocumentHelper.cc and QPDFOutlineObjectHelper.cc responsibilities split with outline_object_helper.rs.
//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and materializes the document
//! outline (bookmarks) into an arena-backed [`crate::OutlineTree`], mirroring
//! qpdf's raw-object traversal for direct and indirect outline values.
//!
//! # Example
//!
//! ```no_run
//! use flpdf::Pdf;
//! use std::io::Cursor;
//!
//! # fn f(bytes: Vec<u8>) -> flpdf::Result<()> {
//! let mut pdf = Pdf::open(Cursor::new(bytes))?;
//! let mut helper = pdf.outline();
//! if helper.has_outlines()? {
//!     let tree = helper.get_tree()?;
//!     for (depth, _id, item) in tree.preorder() {
//!         let title = item.title(&mut helper)?;
//!         println!("{:indent$}{}", "", title, indent = (depth - 1) * 2);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! qpdf-incompatible outline policy APIs were removed before flpdf 1.0.
//!
//! ```compile_fail
//! use flpdf::Dest;
//! ```
//!
//! ```compile_fail
//! use flpdf::{check_legacy_dests, check_name_tree_dests, check_outline_links};
//! ```
//!
//! ```compile_fail
//! use flpdf::{prune_outline_se, prune_outline_se_with_max_depth};
//! ```
//!
//! ```compile_fail
//! # use flpdf::Pdf;
//! # use std::io::Cursor;
//! # let mut pdf = Pdf::open(Cursor::new(Vec::<u8>::new())).unwrap();
//! let _ = pdf.outline().get_root_with_max_depth(10);
//! ```

use crate::nntree::HandleNameTree;
use crate::outline_object_helper::{OutlineId, OutlineItem, OutlineTree};
use crate::{ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

const QPDF_MAX_EXPANDED_OUTLINE_DEPTH: usize = 50;

fn object_key(object: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    let value = object.try_get_key(key)?;
    if value.try_is_null()? {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

/// Detect an actual repeat among **direct** outline sibling handles by live
/// identity, mirroring `form_field_object_helper.rs`'s
/// `mark_direct_node_seen` (`ObjectRef`-keyed seen sets never terminate a
/// `/Next` cycle built entirely from direct dictionaries: a direct handle's
/// `object_ref()` is always `None`, the same `QPDFObjGen::set` gap
/// documented there). Real PDF bytes cannot produce two direct dictionaries
/// that reciprocally reference each other, but it is reachable in memory
/// through the public [`ObjectHandle::replace_key`] API. Unlike that helper
/// (which raises an error), this returns a bool like the existing `ObjectRef`
/// seen-set checks in [`OutlineDocumentHelper::chase_and_mark_seen`] (used by
/// both `get_tree`'s and `build_item`'s sibling walks), so a cycle here
/// silently stops the walk the same way a repeated indirect reference
/// already does — matching qpdf's own `QPDFObjGen::set`-guarded constructor
/// loop, which also just stops (`libqpdf/QPDFOutlineDocumentHelper.cc:16-21`).
fn mark_direct_sibling_seen(direct_seen: &mut Vec<ObjectHandle>, current: &ObjectHandle) -> bool {
    if !current.is_direct() {
        return true;
    }
    if direct_seen
        .iter()
        .any(|handle: &ObjectHandle| handle.is_same_object_as(current))
    {
        return false;
    }
    direct_seen.push(current.clone());
    true
}

/// The two "already visited" trackers a single outline sibling walk needs,
/// bundled into one value so [`OutlineDocumentHelper::chase_and_mark_seen`]
/// takes one seen-state argument instead of two: an `ObjectRef`-keyed set
/// for indirect targets, and a direct-identity `Vec` (see
/// [`mark_direct_sibling_seen`]) for direct ones. Both `get_tree`'s
/// top-level walk and `build_item`'s per-frame sibling walk need exactly
/// this pair. Neither field has an independent qpdf analog beyond
/// `QPDFObjGen::set` itself (`libqpdf/QPDFOutlineDocumentHelper.cc:16-21`);
/// bundling them is a Rust-side container choice, not a qpdf structure.
#[derive(Default)]
struct SiblingSeen {
    seen: BTreeSet<ObjectRef>,
    direct_seen: Vec<ObjectHandle>,
}

/// High-level outline helper for a document. See module docs.
///
/// Named-destination lookups cache the catalog's `/Dests` dictionary and
/// `/Names/Dests` name tree for the lifetime of *this instance*, matching
/// qpdf's `QPDFOutlineDocumentHelper` (whose equivalent cache lives as long
/// as the caller holds that C++ object). [`Pdf::outline`] mints a new
/// instance on every call, so that cache only spans calls made through the
/// same `&mut OutlineDocumentHelper` — callers that want the caching
/// benefit across many [`crate::OutlineItem::dest`] calls (as
/// [`crate::json_inspect::build_outlines_section`] does for a whole
/// outline-tree walk) must hold one instance and reuse it, the same way a
/// qpdf caller reuses one `QPDFOutlineDocumentHelper`.
pub struct OutlineDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    /// Cached resolved `/Dests` catalog entry, mirroring
    /// `QPDFOutlineDocumentHelper::Members::dest_dict`
    /// (`libqpdf/QPDFOutlineDocumentHelper.cc:60-63`): fetched once on first
    /// use (whatever it resolves to, including null) and reused for every
    /// later named-destination lookup in this session.
    dest_dict: Option<ObjectHandle>,
    /// Cached `/Names/Dests` name tree, mirroring
    /// `QPDFOutlineDocumentHelper::Members::names_dest`
    /// (`libqpdf/QPDFOutlineDocumentHelper.cc:70-77`): built once a valid
    /// name tree is found and reused thereafter. Unlike `dest_dict`, an
    /// absent or malformed `/Names`/`/Names/Dests` leaves this `None`, so
    /// (matching qpdf's `nullptr ==` guard) every call keeps retrying the
    /// catalog lookup until a valid tree is found.
    names_dest: Option<HandleNameTree>,
}

impl<'a, R: Read + Seek> OutlineDocumentHelper<'a, R> {
    /// Wrap a document for outline access. Prefer [`Pdf::outline`].
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self {
            pdf,
            dest_dict: None,
            names_dest: None,
        }
    }

    /// Return `true` if the resolved catalog `/Outlines` dictionary has a
    /// non-null `/First` value. Mirrors qpdf `hasOutlines` construction.
    ///
    /// # Errors
    ///
    /// Propagates errors from resolving the catalog and `/Outlines` cursor.
    pub fn has_outlines(&mut self) -> Result<bool> {
        let Some(outlines) = self.catalog_outlines()? else {
            return Ok(false);
        };
        self.pdf.resolve_object_handle(&outlines)?;
        if outlines.try_as_dictionary()?.is_none() {
            return Ok(false);
        }
        if !outlines.try_has_key(b"/First")? {
            return Ok(false);
        }
        // Chase the same `Pdf::set_object` + `shallow_copy` redirect every
        // other exit path in this file already chases via
        // `resolve_value_handle` before inspecting the resolved value:
        // `try_is_null` dereferences only the receiver itself, so a direct
        // reference-valued `/First` whose terminal target is null would
        // otherwise report non-null here while `get_tree` (which chases
        // inside `materialize_item`) terminal-chases the same cursor and
        // produces zero roots — the two would disagree on the same document.
        let first = outlines.try_get_key(b"/First")?;
        let first = self.resolve_value_handle(first)?;
        Ok(!first.try_is_null()?)
    }

    fn resolve_handle(&mut self, handle: &ObjectHandle) -> Result<()> {
        self.pdf.resolve_object_handle(handle)
    }

    /// Follow the temporary bare-reference redirect that `Pdf::set_object`
    /// can install in a canonical slot. Parsed qpdf objects never contain
    /// this state: a normal indirect child resolves in one hop and is
    /// returned unchanged. Keeping this compatibility chase handle-native
    /// lets the outline consumer preserve live identity without reopening the
    /// raw `Object` route while the legacy replacement bridge is removed.
    // qpdf-deviation: chases a Pdf::set_object bare-reference redirect that
    // has no qpdf counterpart (QPDF::replaceObject rejects indirect
    // replacement, libqpdf/QPDF.cc:1986-1991).
    fn resolve_value_handle(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        self.pdf.resolve_object_handle_to_terminal(&handle)
    }

    /// Chase `cursor` to its terminal target (see [`Self::resolve_value_handle`])
    /// and record that terminal's identity in the appropriate "already
    /// visited" set — `seen` when the terminal is indirect, `direct_seen`
    /// (via [`mark_direct_sibling_seen`]) when it is direct — **before** any
    /// further work runs on `cursor`. This mirrors qpdf's own ordering: its
    /// outline constructor records `QPDFObjGen::set` identity as the first
    /// act per node (`libqpdf/QPDFOutlineDocumentHelper.cc:16-21`), before
    /// anything else touches that node. Chasing only inside
    /// [`Self::materialize_item`] (downstream of the identity check) let a
    /// direct reference-valued cursor's *wrapper* identity be recorded
    /// instead of its resolved target's, so a self-referential `/Next` chain
    /// hidden behind such a wrapper was visited and appended twice before
    /// the seen-set caught it — one full extra iteration late.
    ///
    /// Returns `Ok(None)` once `cursor`'s terminal target has already been
    /// seen and the walk must stop there, else `Ok(Some(chased_cursor))` for
    /// the caller to materialize.
    fn chase_and_mark_seen(
        &mut self,
        cursor: ObjectHandle,
        seen: &mut SiblingSeen,
    ) -> Result<Option<ObjectHandle>> {
        let cursor = self.resolve_value_handle(cursor)?;
        if let Some(reference) = cursor.object_ref() {
            if !seen.seen.insert(reference) {
                return Ok(None);
            }
        } else if !mark_direct_sibling_seen(&mut seen.direct_seen, &cursor) {
            return Ok(None);
        }
        Ok(Some(cursor))
    }

    fn catalog_handle(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.pdf.get_object_handle(catalog_ref);
        self.resolve_handle(&catalog)?;
        if catalog.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        Ok(Some(catalog))
    }

    fn catalog_outlines(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(catalog) = self.catalog_handle()? else {
            return Ok(None);
        };
        if !catalog.try_has_key(b"/Outlines")? {
            return Ok(None);
        }
        Ok(Some(catalog.try_get_key(b"/Outlines")?))
    }

    /// Materialize the qpdf-compatible outline arena.
    ///
    /// # Errors
    ///
    /// Propagates outline-resolution errors.
    pub fn get_tree(&mut self) -> Result<OutlineTree> {
        let mut tree = OutlineTree::new();
        let Some(outlines) = self.catalog_outlines()? else {
            return Ok(tree);
        };
        self.resolve_handle(&outlines)?;
        if outlines.try_as_dictionary()?.is_none() {
            return Ok(tree);
        }
        if !outlines.try_has_key(b"/First")? {
            return Ok(tree);
        }
        let first = outlines.try_get_key(b"/First")?;
        // Chased for the same reason as `has_outlines`'s own `/First` read
        // (see its doc): a direct reference-valued `first` whose terminal
        // target is null would otherwise pass this check unrecognized. The
        // loop below already produces the correct (empty) result for that
        // shape on its own — `materialize_item` chases and detects the
        // null, so `build_item` returns `None` and the loop breaks before
        // pushing a root — but chasing here too keeps this early-return
        // consistent with `has_outlines`'s check on the same value instead
        // of relying on that fallback.
        let first = self.resolve_value_handle(first)?;
        if first.try_is_null()? {
            return Ok(tree);
        }
        let mut cursor = first;

        let mut top_level_seen = SiblingSeen::default();
        let mut constructor_seen = BTreeSet::new();
        loop {
            let Some(chased) = self.chase_and_mark_seen(cursor, &mut top_level_seen)? else {
                break;
            };

            let Some(id) = self.build_item(chased, None, &mut tree, &mut constructor_seen)? else {
                break;
            };
            tree.roots.push(id);
            let Some(next) = object_key(&tree[id].object, b"/Next")? else {
                break;
            };
            cursor = next;
        }
        Ok(tree)
    }

    /// Materialize one item and all descendants using an explicit frame stack.
    /// The stack preserves qpdf's constructor seen-set placement without using
    /// one native call frame per outline level.
    fn build_item(
        &mut self,
        cursor: ObjectHandle,
        parent: Option<OutlineId>,
        tree: &mut OutlineTree,
        constructor_seen: &mut BTreeSet<ObjectRef>,
    ) -> Result<Option<OutlineId>> {
        let Some(root) = self.materialize_item(cursor, parent, tree)? else {
            return Ok(None);
        };
        if let Some(reference) = tree[root].source_ref {
            if !constructor_seen.insert(reference) {
                return Ok(Some(root));
            }
        }

        struct Frame {
            owner: OutlineId,
            next: Option<ObjectHandle>,
            depth: usize,
            seen: SiblingSeen,
        }

        let mut frames = Vec::new();
        let first = object_key(&tree[root].object, b"/First")?;
        if first.is_some() {
            frames.push(Frame {
                owner: root,
                next: first,
                depth: 2,
                seen: SiblingSeen::default(),
            });
        }

        while !frames.is_empty() {
            let next_cursor = frames.last_mut().and_then(|frame| frame.next.take());
            let Some(cursor) = next_cursor else {
                frames.pop();
                continue;
            };
            let (owner, child_depth, chased) = {
                let frame = frames
                    .last_mut()
                    .expect("outline construction frame exists");
                let owner = frame.owner;
                let child_depth = frame.depth;
                let chased = self.chase_and_mark_seen(cursor, &mut frame.seen)?;
                (owner, child_depth, chased)
            };
            let Some(cursor) = chased else {
                frames
                    .last_mut()
                    .expect("outline construction frame exists")
                    .next = None;
                continue;
            };
            let Some(child) = self.materialize_item(cursor, Some(owner), tree)? else {
                continue;
            };
            tree.items[owner.0].kids.push(child);

            let expand_child = if child_depth > QPDF_MAX_EXPANDED_OUTLINE_DEPTH {
                false
            } else if let Some(reference) = tree[child].source_ref {
                constructor_seen.insert(reference)
            } else {
                true
            };

            // qpdf advances the parent's raw child `/Next` chain even when the
            // child's constructor seen check prevented that child expanding.
            frames
                .last_mut()
                .expect("outline construction frame exists")
                .next = object_key(&tree[child].object, b"/Next")?;

            if expand_child {
                let first = object_key(&tree[child].object, b"/First")?;
                if first.is_some() {
                    frames.push(Frame {
                        owner: child,
                        next: first,
                        depth: child_depth + 1,
                        seen: SiblingSeen::default(),
                    });
                }
            }
        }

        Ok(Some(root))
    }

    fn materialize_item(
        &mut self,
        cursor: ObjectHandle,
        parent: Option<OutlineId>,
        tree: &mut OutlineTree,
    ) -> Result<Option<OutlineId>> {
        // Chase a direct handle whose own resolved value is itself a bare
        // `Object::Reference` (installed via `shallow_copy` on a
        // `Pdf::set_object`-redirected holder, then set as `/First`/`/Next`
        // through the public `ObjectHandle::replace_key` API) to its real
        // target, the same way every other exit path in this file already
        // chases this shape via `resolve_value_handle`. Both callers
        // (`get_tree`'s loop and `build_item`'s frame loop) already chase
        // `cursor` through `chase_and_mark_seen` before calling here, so
        // this re-chase is normally a redundant no-op —
        // `resolve_object_handle_to_terminal` returns an already-terminal
        // handle unchanged — kept so this function's own `source_ref`
        // capture stays correct standalone, independent of caller
        // discipline. `object_ref()` is captured AFTER chasing so cycle
        // detection (`source_ref`, used by `build_item`'s
        // `constructor_seen`) keys off the terminal identity, not the
        // pre-chase holder.
        let cursor = self.resolve_value_handle(cursor)?;
        let source_ref = cursor.object_ref();
        if cursor.try_is_null()? {
            return Ok(None);
        }
        let id = OutlineId(tree.items.len());
        tree.items.push(OutlineItem {
            source_ref,
            parent,
            kids: Vec::new(),
            object: cursor,
        });

        Ok(Some(id))
    }

    /// Extract an outline dictionary's `/key` entry, or `None` when the
    /// object is not a dictionary or lacks that key. Shared by
    /// [`Self::resolve_item_title`], [`Self::resolve_item_count`], and
    /// [`Self::resolve_item_dest`].
    fn outline_dict_key(object: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
        if object.try_as_dictionary()?.is_none() || !object.try_has_key(key)? {
            return Ok(None);
        }
        Ok(Some(object.try_get_key(key)?))
    }

    /// Decode `/Title` from a live outline object, mirroring qpdf's
    /// `getTitle()` (`libqpdf/QPDFOutlineObjectHelper.cc:91-98`), which
    /// re-reads `/Title` on every call rather than caching it. Backs
    /// [`crate::OutlineItem::title`].
    ///
    /// # Errors
    ///
    /// Propagates errors resolving `/Title` or emitting a type-mismatch warning.
    pub(crate) fn resolve_item_title(&mut self, object: &ObjectHandle) -> Result<String> {
        let title_src = Self::outline_dict_key(object, b"/Title")?;
        resolve_title(self.pdf, title_src)
    }

    /// Read `/Count` from a live outline object, mirroring qpdf's
    /// `getCount()` (`libqpdf/QPDFOutlineObjectHelper.cc:81-88`), which
    /// re-reads `/Count` on every call rather than caching it. Backs
    /// [`crate::OutlineItem::count`].
    ///
    /// # Errors
    ///
    /// Propagates errors resolving `/Count` or emitting a type-mismatch warning.
    pub(crate) fn resolve_item_count(&mut self, object: &ObjectHandle) -> Result<i32> {
        let count_src = Self::outline_dict_key(object, b"/Count")?;
        resolve_count(self.pdf, count_src)
    }

    /// Resolve a live outline object's destination, mirroring qpdf's
    /// `getDest()` (`libqpdf/QPDFOutlineObjectHelper.cc:47-69`), which
    /// re-reads `/Dest`/`/A` on every call rather than caching the result.
    /// Backs [`crate::OutlineItem::dest`].
    ///
    /// # Errors
    ///
    /// Propagates errors resolving the destination or the catalog's
    /// named-destination tables.
    pub(crate) fn resolve_item_dest(&mut self, object: &ObjectHandle) -> Result<ObjectHandle> {
        let dest_src = Self::outline_dict_key(object, b"/Dest")?;
        let action_src = Self::outline_dict_key(object, b"/A")?;
        let dest = self.resolve_node_dest(dest_src, action_src)?;
        self.resolve_handle(&dest)?;
        Ok(dest)
    }

    /// Resolve a node's destination from `/Dest`, else a `/A` GoTo action's `/D`.
    fn resolve_node_dest(
        &mut self,
        dest: Option<ObjectHandle>,
        action: Option<ObjectHandle>,
    ) -> Result<ObjectHandle> {
        let Some(candidate) = (if let Some(dest) = dest {
            Some(dest)
        } else {
            self.goto_action_dest(action)?
        }) else {
            return Ok(ObjectHandle::null());
        };

        let candidate = self.resolve_value_handle(candidate)?;
        if let Some(name) = candidate.try_as_name()? {
            return self.resolve_legacy_node_dest(&name);
        }
        if let Some(bytes) = candidate.as_string() {
            return self.resolve_name_tree_node_dest(&bytes);
        }
        Ok(candidate)
    }

    fn goto_action_dest(&mut self, action: Option<ObjectHandle>) -> Result<Option<ObjectHandle>> {
        let Some(action) = action else {
            return Ok(None);
        };
        let action = self.resolve_value_handle(action)?;
        if action.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        // Chase the same `Pdf::set_object` redirect this file's other
        // dest-resolution call sites already chase (`resolve_node_dest`'s
        // `candidate`, `resolve_legacy_node_dest`'s `value`,
        // `resolve_name_tree_node_dest`'s `found`, and this function's own
        // `action` above): `try_is_name_and_equals` only dereferences its
        // receiver once and never follows a bare-reference-valued result.
        let subtype = self.resolve_value_handle(action.try_get_key(b"/S")?)?;
        if !subtype.try_is_name_and_equals(b"GoTo")? {
            return Ok(None);
        }
        if !action.try_has_key(b"/D")? {
            return Ok(None);
        }
        Ok(Some(action.try_get_key(b"/D")?))
    }

    /// Return the cached `/Dests` catalog entry, fetching and caching it on
    /// first use (matching qpdf's `dest_dict.isInitialized()` guard, which
    /// caches the fetch outcome unconditionally, including a missing entry).
    fn cached_dest_dict(&mut self) -> Result<ObjectHandle> {
        if let Some(dests) = &self.dest_dict {
            return Ok(dests.clone());
        }
        let dests = match self.catalog_value_handle(b"/Dests")? {
            Some(dests) => self.resolve_value_handle(dests)?,
            None => ObjectHandle::null(),
        };
        self.dest_dict = Some(dests.clone());
        Ok(dests)
    }

    fn resolve_legacy_node_dest(&mut self, name: &[u8]) -> Result<ObjectHandle> {
        let dests = self.cached_dest_dict()?;
        if dests.try_as_dictionary()?.is_none() {
            return Ok(ObjectHandle::null());
        }
        let mut key = Vec::with_capacity(name.len() + 1);
        key.push(b'/');
        key.extend_from_slice(name);
        if !dests.try_has_key(&key)? {
            return Ok(ObjectHandle::null());
        }
        // Chase the selected entry the same way every other exit path in
        // this file does (`resolve_node_dest`'s `candidate`,
        // `goto_action_dest`'s `action`, `resolve_name_tree_node_dest`'s
        // `found`): a `Pdf::set_object` legacy redirect can still sit behind
        // this dictionary entry even after `dests` itself was chased above.
        let value = dests.try_get_key(&key)?;
        self.resolve_value_handle(value)
    }

    fn resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<ObjectHandle> {
        let lookup =
            crate::pdf_string::normalized_utf8_value(&crate::pdf_string::utf8_value(bytes));
        if self.names_dest.is_none() {
            let Some(names) = self.catalog_value_handle(b"/Names")? else {
                return Ok(ObjectHandle::null());
            };
            let names = self.resolve_value_handle(names)?;
            if names.try_as_dictionary()?.is_none() || !names.try_has_key(b"/Dests")? {
                return Ok(ObjectHandle::null());
            }
            let root = names.try_get_key(b"/Dests")?;
            let root = self.resolve_value_handle(root)?;
            if root.try_as_dictionary()?.is_none() {
                return Ok(ObjectHandle::null());
            }
            self.names_dest = Some(HandleNameTree::new(root, self.pdf.unique_id(), true));
        }
        let tree = self
            .names_dest
            .as_mut()
            .expect("populated by the check above");
        let found = tree.find(&mut *self.pdf, lookup.as_slice())?;
        found
            .map(|value| self.resolve_value_handle(value))
            .transpose()
            .map(|value| value.unwrap_or_else(ObjectHandle::null))
    }

    fn catalog_value_handle(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        let Some(catalog) = self.catalog_handle()? else {
            return Ok(None);
        };
        if !catalog.try_has_key(key)? {
            return Ok(None);
        }
        Ok(Some(catalog.try_get_key(key)?))
    }
}
impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level outline helper for this document.
    pub fn outline(&mut self) -> OutlineDocumentHelper<'_, R> {
        OutlineDocumentHelper::new(self)
    }
}

/// Decode an outline `/Title`, resolving one level of indirection (review rule 2).
fn resolve_title<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<ObjectHandle>) -> Result<String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    pdf.resolve_object_handle(&value)?;
    qpdf_title(&value)
}

fn qpdf_title(value: &ObjectHandle) -> Result<String> {
    if let Some(bytes) = value.as_string() {
        Ok(String::from_utf8_lossy(&crate::pdf_string::utf8_value(&bytes)).into_owned())
    } else {
        value.type_warning("string", "returning empty string")?;
        Ok(String::new())
    }
}

/// Read an outline `/Count`, resolving one level of indirection (review rule 2/3).
fn resolve_count<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<ObjectHandle>) -> Result<i32> {
    let Some(value) = value else {
        return Ok(0);
    };
    pdf.resolve_object_handle(&value)?;
    qpdf_count(&value)
}

fn qpdf_count(value: &ObjectHandle) -> Result<i32> {
    value.try_get_int_value_as_int()
}

#[cfg(test)]
mod tests {
    use super::{mark_direct_sibling_seen, qpdf_count, qpdf_title};
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::{ObjectHandle, ObjectRef};

    /// Mirrors `form_field_object_helper.rs`'s
    /// `direct_seen_set_ignores_indirect_handles_and_tracks_direct_identity`:
    /// an indirect handle is ignored (the `BTreeSet<ObjectRef>` seen-set in
    /// `get_tree`/`build_item` already owns its identity), a direct handle is
    /// recorded on first sight, and an actual repeat of the SAME underlying
    /// allocation is rejected while a distinct direct handle with equal
    /// contents is not treated as a repeat.
    #[test]
    fn direct_sibling_seen_set_ignores_indirect_handles_and_tracks_direct_identity() {
        let mut direct_seen = Vec::new();

        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        assert!(mark_direct_sibling_seen(&mut direct_seen, &indirect));
        assert!(mark_direct_sibling_seen(&mut direct_seen, &indirect));
        assert!(direct_seen.is_empty());

        let direct = ObjectHandle::dictionary(Vec::new());
        assert!(mark_direct_sibling_seen(&mut direct_seen, &direct));
        assert!(!mark_direct_sibling_seen(&mut direct_seen, &direct));

        let other_direct = ObjectHandle::dictionary(Vec::new());
        assert!(mark_direct_sibling_seen(&mut direct_seen, &other_direct));
    }

    #[test]
    fn scalar_warning_sink_failures_propagate() {
        for title in [true, false] {
            let mut pdf = crate::Pdf::empty().unwrap();
            let logger = crate::QPDFLogger::create();
            logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(1))));
            pdf.set_logger(logger);

            let result = if title {
                let value = pdf
                    .lift_object_to_handle(&crate::Object::Integer(42))
                    .unwrap();
                qpdf_title(&value).map(|_| ())
            } else {
                let value = pdf
                    .lift_object_to_handle(&crate::Object::String(b"wrong".to_vec()))
                    .unwrap();
                qpdf_count(&value).map(|_| ())
            };
            assert!(matches!(
                result,
                Err(crate::Error::System(ref message)) if message == "sink write failure 1"
            ));
            assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
        }
    }
}
