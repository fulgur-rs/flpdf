//! qpdf correspondence: QPDFOutlineObjectHelper.cc getTitle/getCount/getDest/getDestPage, split from the document helper's QPDFOutlineDocumentHelper.cc responsibilities.
//! The pre-1.0 flat, configurable-depth outline API was removed in favor of
//! qpdf-compatible [`OutlineTree`] materialization.
//!
//! [`OutlineItem::get_title`], [`OutlineItem::get_count`],
//! [`OutlineItem::get_dest`], and [`OutlineItem::get_dest_page`] recompute
//! fresh from the live [`OutlineItem::object`] handle on every call, matching
//! `getTitle`/`getCount`/`getDest`/`getDestPage`'s lack of caching
//! (`libqpdf/QPDFOutlineObjectHelper.cc:47-98`) — a destination, title, or
//! count mutated through a live [`crate::ObjectHandle`] between two calls is
//! reflected on the second call. Only [`OutlineItem::parent`] and
//! [`OutlineItem::kids`] are captured once at tree-construction time,
//! matching `getParent`/`getKids`, which qpdf itself builds once in its
//! constructor and returns from cached members.
//!
//! `OutlineItem` itself holds no `&mut Pdf<R>` — it is an arena entry, not a
//! qpdf-style live object helper — so each accessor takes
//! `helper: &mut OutlineDocumentHelper<'_, R>` in place of qpdf's
//! `QPDFOutlineObjectHelper::m->dh` reference. [`OutlineItem::get_dest`]
//! implements qpdf's `getDest()` body directly (the `/Dest`-or-`/A` GoTo
//! extraction) and delegates only the name/string branch to
//! `OutlineDocumentHelper::resolve_named_dest`, exactly as qpdf's
//! `getDest()` delegates to `m->dh.resolveNamedDest()`.
//!
//! ```compile_fail
//! use flpdf::outline_object_helper::{outline_items, outline_items_with_max_depth};
//! ```

use crate::outline_document_helper::OutlineDocumentHelper;
use crate::{ObjectHandle, ObjectRef, Result};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek};
use std::ops::Index;
use std::sync::OnceLock;

/// Extract an outline dictionary's `/key` entry, or `None` when the key is
/// absent. Mirrors qpdf's `hasKey(key)`-gated `getKey(key)` shape used by
/// `getTitle()`/`getCount()`/`getDest()`'s `/Dest` check
/// (`libqpdf/QPDFOutlineObjectHelper.cc:52-54,87-88,97-98`): `try_has_key`
/// alone decides, so a non-dictionary `object` reports through `try_has_key`'s
/// own qpdf-matching `typeWarning("dictionary", "returning false for a key
/// containment request")` instead of being silently swallowed by an upfront
/// dictionary check. Shared by [`OutlineItem::get_title`],
/// [`OutlineItem::get_count`], and [`OutlineItem::get_dest`]'s `/Dest` arm —
/// NOT its `/A` arm, which qpdf reads unconditionally rather than
/// `hasKey`-gated (see [`OutlineItem::get_dest`]).
fn outline_dict_key(object: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    if !object.try_has_key(key)? {
        return Ok(None);
    }
    Ok(Some(object.try_get_key(key)?))
}

/// Decode an already-resolved outline `/Title` value.
fn title_from_handle(value: &ObjectHandle) -> Result<String> {
    if let Some(bytes) = value.as_string() {
        Ok(String::from_utf8_lossy(&crate::pdf_string::utf8_value(&bytes)).into_owned())
    } else {
        value.type_warning("string", "returning empty string")?;
        Ok(String::new())
    }
}

/// Decode an already-resolved outline `/Count` value.
fn count_from_handle(value: &ObjectHandle) -> Result<i32> {
    value.try_get_int_value_as_int()
}

/// Resolve a node's `/A` action to its GoTo destination, mirroring the
/// inline `/A` branch of qpdf's `getDest()`
/// (`libqpdf/QPDFOutlineObjectHelper.cc:47-58`): the action must be a
/// dictionary with `/S /GoTo` and a `/D` entry. `action` is the raw,
/// unconditional `getKey("/A")` result (qpdf never `hasKey`-gates this read,
/// unlike `/Title`/`/Count`/`/Dest`; see [`outline_dict_key`]), so a
/// non-dictionary receiver's warning here is `try_get_key`'s own
/// `typeWarning("dictionary", "returning null for attempted key
/// retrieval")`, not `try_has_key`'s "containment request" text.
fn goto_action_dest<R: Read + Seek>(
    helper: &mut OutlineDocumentHelper<'_, R>,
    action: ObjectHandle,
) -> Result<Option<ObjectHandle>> {
    let action = helper.resolve_value_handle(action)?;
    if action.try_as_dictionary()?.is_none() {
        return Ok(None);
    }
    // Resolve the selected action subtype through the canonical document
    // resolver before applying qpdf's name predicate.
    let subtype = helper.resolve_value_handle(action.try_get_key(b"/S")?)?;
    if !subtype.try_is_name_and_equals(b"GoTo")? {
        return Ok(None);
    }
    if !action.try_has_key(b"/D")? {
        return Ok(None);
    }
    Ok(Some(action.try_get_key(b"/D")?))
}

/// Stable index of an item within an [`OutlineTree`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlineId(pub(crate) usize);

/// One materialized outline item.
#[derive(Debug, Clone)]
pub struct OutlineItem {
    /// Indirect source identity, or `None` for a direct outline value.
    pub source_ref: Option<ObjectRef>,
    /// Parent item in the arena; top-level items have no parent.
    pub parent: Option<OutlineId>,
    /// Child items in raw `/First` then `/Next` order.
    pub kids: Vec<OutlineId>,
    /// Live qpdf object handle obtained by resolving this outline cursor exactly once.
    pub object: ObjectHandle,
}

impl OutlineItem {
    /// Mirror qpdf `getTitle()` (`libqpdf/QPDFOutlineObjectHelper.cc:91-98`).
    /// Decodes `/Title` fresh from [`Self::object`] every call, resolving
    /// one level of indirection off the fetched value; returns an empty
    /// string when the key is absent.
    ///
    /// # Errors
    ///
    /// Propagates errors resolving `/Title` or emitting a type-mismatch warning.
    pub fn get_title<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<String> {
        let Some(value) = outline_dict_key(&self.object, b"/Title")? else {
            return Ok(String::new());
        };
        helper.resolve_handle(&value)?;
        title_from_handle(&value)
    }

    /// Mirror qpdf `getCount()` (`libqpdf/QPDFOutlineObjectHelper.cc:81-88`).
    /// Reads `/Count` fresh from [`Self::object`] every call, resolving one
    /// level of indirection off the fetched value; returns `0` when the key
    /// is absent.
    ///
    /// # Errors
    ///
    /// Propagates errors resolving `/Count` or emitting a type-mismatch warning.
    pub fn get_count<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<i32> {
        let Some(value) = outline_dict_key(&self.object, b"/Count")? else {
            return Ok(0);
        };
        helper.resolve_handle(&value)?;
        count_from_handle(&value)
    }

    /// Mirror qpdf `getDest()` (`libqpdf/QPDFOutlineObjectHelper.cc:47-69`).
    /// Resolves `/Dest`, or else a `/A` `GoTo` action's `/D`, fresh from
    /// [`Self::object`] every call, following a name or string result
    /// through the catalog's named-destination tables via
    /// `OutlineDocumentHelper::resolve_named_dest` — exactly like
    /// qpdf's own `if (dest.isName() || dest.isString())` dispatch to
    /// `m->dh.resolveNamedDest()`. A candidate that is neither name nor
    /// string (an explicit destination array, typically) is returned as-is.
    /// `/A` is only read when `/Dest` is absent, matching qpdf's
    /// `if (hasKey("/Dest")) {...} else if ((A = getKey("/A"))...)`.
    ///
    /// # Errors
    ///
    /// Propagates errors resolving the destination or the catalog's
    /// named-destination tables.
    pub fn get_dest<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<ObjectHandle> {
        let dest_src = outline_dict_key(&self.object, b"/Dest")?;
        let candidate = match dest_src {
            Some(dest) => Some(dest),
            None => goto_action_dest(helper, self.object.try_get_key(b"/A")?)?,
        };
        let Some(candidate) = candidate else {
            return Ok(ObjectHandle::null());
        };

        let candidate = helper.resolve_value_handle(candidate)?;
        let is_named = candidate.try_as_name()?.is_some() || candidate.as_string().is_some();
        let dest = if is_named {
            helper.resolve_named_dest(candidate)?
        } else {
            candidate
        };
        helper.resolve_handle(&dest)?;
        Ok(dest)
    }

    /// Mirror qpdf `getDestPage()` (`libqpdf/QPDFOutlineObjectHelper.cc:71-78`).
    /// Calls [`Self::get_dest`] fresh every call and extracts its first
    /// array item, without resolving the page operand.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::get_dest`].
    pub fn get_dest_page<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<ObjectHandle> {
        let dest = self.get_dest(helper)?;
        Ok(dest
            .as_array()
            .and_then(|items| items.into_iter().next())
            .unwrap_or_else(ObjectHandle::null))
    }
}

/// Arena-backed materialization of a document outline.
#[derive(Debug)]
pub struct OutlineTree {
    pub(crate) items: Vec<OutlineItem>,
    pub(crate) roots: Vec<OutlineId>,
    by_page: OnceLock<BTreeMap<Option<ObjectRef>, Vec<OutlineId>>>,
}

impl OutlineTree {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            roots: Vec::new(),
            by_page: OnceLock::new(),
        }
    }

    fn normalize_page_key(page: Option<ObjectRef>) -> Option<ObjectRef> {
        page.filter(|reference| *reference != ObjectRef::new(0, 0))
    }

    fn page_key<R: Read + Seek>(
        item: &OutlineItem,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<Option<ObjectRef>> {
        Ok(Self::normalize_page_key(
            item.get_dest_page(helper)?.object_ref(),
        ))
    }

    // qpdf-deviation: `initializeByPage`'s cache placement has no counterpart
    // here — qpdf's cache lives on `QPDFOutlineDocumentHelper`, which a
    // caller can keep across many `getOutlinesForPage` calls
    // (`libqpdf/QPDFOutlineDocumentHelper.cc:35-59`), but [`Pdf::outline`]
    // mints a fresh `OutlineDocumentHelper` on every call, so caching there
    // instead would never hit. The cache stays on this arena-lifetime
    // `OutlineTree` instead, which callers already hold across repeated page
    // lookups.
    fn initialize_by_page<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<BTreeMap<Option<ObjectRef>, Vec<OutlineId>>> {
        let mut index = BTreeMap::<Option<ObjectRef>, Vec<OutlineId>>::new();
        let mut queue: VecDeque<OutlineId> = self.roots.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            let key = Self::page_key(&self[id], helper)?;
            index.entry(key).or_default().push(id);
            queue.extend(self[id].kids.iter().copied());
        }
        Ok(index)
    }

    fn by_page<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<&BTreeMap<Option<ObjectRef>, Vec<OutlineId>>> {
        if self.by_page.get().is_none() {
            let built = self.initialize_by_page(helper)?;
            let _ = self.by_page.set(built);
        }
        Ok(self
            .by_page
            .get()
            .expect("populated by the check-then-set above"))
    }

    /// Return outlines targeting `page` in qpdf breadth-first order.
    ///
    /// `None` represents qpdf's `QPDFObjGen(0, 0)` bucket and therefore also
    /// contains destinations whose page operand is not an indirect reference.
    ///
    /// The page-to-outline mapping is computed once (calling each item's
    /// [`OutlineItem::get_dest_page`] live, exactly like qpdf) and cached for
    /// the lifetime of this tree, matching qpdf's own
    /// `QPDFOutlineDocumentHelper::getOutlinesForPage`/`initializeByPage`
    /// contract (`libqpdf/QPDFOutlineDocumentHelper.cc:35-59`), which lazily
    /// builds `m->by_page` on first use and never invalidates it. A
    /// destination mutated through a live [`crate::ObjectHandle`] after this
    /// method has already been called is not reflected in later results from
    /// *this* method — qpdf has the identical hazard for the same reason —
    /// but is reflected by a direct [`OutlineItem::get_dest_page`] call,
    /// which always recomputes.
    ///
    /// # Errors
    ///
    /// Propagates errors from resolving any item's destination while
    /// building the page index.
    pub fn get_outlines_for_page<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
        page: Option<ObjectRef>,
    ) -> Result<impl Iterator<Item = (OutlineId, &OutlineItem)>> {
        Ok(self
            .by_page(helper)?
            .get(&Self::normalize_page_key(page))
            .into_iter()
            .flatten()
            .copied()
            .map(|id| (id, &self[id])))
    }

    /// Top-level items in raw `/First` then `/Next` order.
    pub fn roots(&self) -> &[OutlineId] {
        &self.roots
    }

    /// Return an item when `id` belongs to this tree.
    pub fn get(&self, id: OutlineId) -> Option<&OutlineItem> {
        self.items.get(id.0)
    }

    /// Iterate over every item in pre-order, yielding one-based depth.
    pub fn preorder(&self) -> OutlineTreeIter<'_> {
        OutlineTreeIter {
            tree: self,
            stack: self.roots.iter().rev().map(|&id| (1, id)).collect(),
        }
    }
}

impl Index<OutlineId> for OutlineTree {
    type Output = OutlineItem;

    fn index(&self, id: OutlineId) -> &Self::Output {
        &self.items[id.0]
    }
}

/// Lossless pre-order view over an [`OutlineTree`].
pub struct OutlineTreeIter<'a> {
    tree: &'a OutlineTree,
    stack: Vec<(usize, OutlineId)>,
}

impl<'a> Iterator for OutlineTreeIter<'a> {
    type Item = (usize, OutlineId, &'a OutlineItem);

    fn next(&mut self) -> Option<Self::Item> {
        let (depth, id) = self.stack.pop()?;
        let item = &self.tree[id];
        self.stack
            .extend(item.kids.iter().rev().map(|&kid| (depth + 1, kid)));
        Some((depth, id, item))
    }
}
