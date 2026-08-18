//! qpdf correspondence: QPDFOutlineObjectHelper.cc compatibility surface split from the document helper.
//! The pre-1.0 flat, configurable-depth outline API was removed in favor of
//! qpdf-compatible [`OutlineTree`] materialization.
//!
//! [`OutlineItem::title`], [`OutlineItem::count`], [`OutlineItem::dest`], and
//! [`OutlineItem::dest_page`] recompute fresh from the live
//! [`OutlineItem::object`] handle on every call, matching
//! `getTitle`/`getCount`/`getDest`/`getDestPage`'s lack of caching
//! (`libqpdf/QPDFOutlineObjectHelper.cc`) — a destination, title, or count
//! mutated through a live [`crate::ObjectHandle`] between two calls is
//! reflected on the second call. Only [`OutlineItem::parent`] and
//! [`OutlineItem::kids`] are captured once at tree-construction time,
//! matching `getParent`/`getKids`, which qpdf itself builds once in its
//! constructor and returns from cached members.
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
    /// Mirror qpdf `getTitle()`. Decodes `/Title` fresh from [`Self::object`]
    /// every call; returns an empty string when the key is absent.
    ///
    /// # Errors
    ///
    /// Propagates errors resolving `/Title` or emitting a type-mismatch warning.
    pub fn title<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<String> {
        helper.resolve_item_title(&self.object)
    }

    /// Mirror qpdf `getCount()`. Reads `/Count` fresh from [`Self::object`]
    /// every call; returns `0` when the key is absent.
    ///
    /// # Errors
    ///
    /// Propagates errors resolving `/Count` or emitting a type-mismatch warning.
    pub fn count<R: Read + Seek>(&self, helper: &mut OutlineDocumentHelper<'_, R>) -> Result<i32> {
        helper.resolve_item_count(&self.object)
    }

    /// Mirror qpdf `getDest()`. Resolves `/Dest`, or else a `/A` `GoTo`
    /// action's `/D`, fresh from [`Self::object`] every call, following a
    /// name or string result through the catalog's named-destination tables.
    ///
    /// # Errors
    ///
    /// Propagates errors resolving the destination or the catalog's
    /// named-destination tables.
    pub fn dest<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<ObjectHandle> {
        helper.resolve_item_dest(&self.object)
    }

    /// Mirror qpdf `getDestPage()`. Calls [`Self::dest`] fresh every call and
    /// extracts its first array item, without resolving the page operand.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::dest`].
    pub fn dest_page<R: Read + Seek>(
        &self,
        helper: &mut OutlineDocumentHelper<'_, R>,
    ) -> Result<ObjectHandle> {
        let dest = self.dest(helper)?;
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
            item.dest_page(helper)?.object_ref(),
        ))
    }

    fn build_by_page<R: Read + Seek>(
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
            let built = self.build_by_page(helper)?;
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
    /// [`OutlineItem::dest_page`] live, exactly like qpdf) and cached for the
    /// lifetime of this tree, matching qpdf's own
    /// `QPDFOutlineDocumentHelper::getOutlinesForPage`/`initializeByPage`
    /// contract (`libqpdf/QPDFOutlineDocumentHelper.cc:35-59`), which lazily
    /// builds `m->by_page` on first use and never invalidates it. A
    /// destination mutated through a live [`crate::ObjectHandle`] after this
    /// method has already been called is not reflected in later results from
    /// *this* method — qpdf has the identical hazard for the same reason —
    /// but is reflected by a direct [`OutlineItem::dest_page`] call, which
    /// always recomputes.
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
