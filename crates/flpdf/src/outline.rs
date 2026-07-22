//! The pre-1.0 flat, configurable-depth outline API was removed in favor of
//! qpdf-compatible [`OutlineTree`] materialization.
//!
//! ```compile_fail
//! use flpdf::outline::{outline_items, outline_items_with_max_depth};
//! ```

use crate::{Object, ObjectRef};
use std::ops::Index;

/// Stable index of an item within an [`OutlineTree`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlineId(pub(crate) usize);

/// One materialized outline item.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// Indirect source identity, or `None` for a direct outline value.
    pub source_ref: Option<ObjectRef>,
    /// Parent item in the arena; top-level items have no parent.
    pub parent: Option<OutlineId>,
    /// Child items in raw `/First` then `/Next` order.
    pub kids: Vec<OutlineId>,
    /// Raw object obtained by resolving this outline cursor exactly once.
    pub object: Object,
    /// Decoded `/Title`, or an empty string when unavailable.
    pub title: String,
    /// Raw `/Count` converted to qpdf's signed 32-bit accessor shape.
    pub count: i32,
    /// qpdf-compatible resolved destination, or [`Object::Null`].
    pub dest: Object,
}

impl OutlineItem {
    /// Mirror qpdf `getDestPage()` without resolving the page operand.
    pub fn dest_page(&self) -> Object {
        match &self.dest {
            Object::Array(items) if !items.is_empty() => items[0].clone(),
            _ => Object::Null,
        }
    }
}

/// Arena-backed materialization of a document outline.
#[derive(Debug)]
pub struct OutlineTree {
    pub(crate) items: Vec<OutlineItem>,
    pub(crate) roots: Vec<OutlineId>,
}

impl OutlineTree {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            roots: Vec::new(),
        }
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
