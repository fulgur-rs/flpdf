//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and exposes a cycle-safe,
//! iterable handle over the document outline (bookmark) tree, mirroring qpdf's
//! `QPDFOutlineDocumentHelper`. It materializes the tree into owned
//! [`OutlineNode`]s; navigation (`children`, `parent`, `count`, `dest`) lives on
//! each node, mirroring `QPDFOutlineObjectHelper`.

use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Default recursion limit for outline materialization. Matches
/// [`crate::outline::DEFAULT_MAX_OUTLINE_DEPTH`]. True unbounded/iterative deep
/// walking (1000+ levels, cycle diagnostics) is tracked by flpdf-9hc.14.7.
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = crate::outline::DEFAULT_MAX_OUTLINE_DEPTH;

/// One materialized node of the outline tree (a bookmark).
///
/// Mirrors qpdf's `QPDFOutlineObjectHelper`. `children` are the resolved
/// `/First`->`/Next` chain; `parent` is the containing item's ref (the
/// `/Outlines` dict ref for top-level items). `count` is the raw `/Count` value
/// (0 when absent), whose sign indicates open/closed per ISO 32000-1 section
/// 12.3.3.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineNode {
    /// Object ref of this outline item dictionary.
    pub object_ref: ObjectRef,
    /// Zero for top-level items, increasing per nesting level.
    pub depth: usize,
    /// `/Title` decoded with `from_utf8_lossy`; empty string when absent.
    pub title: String,
    /// Raw `/Count` value; `0` when absent.
    pub count: i64,
    /// Parent item ref; the `/Outlines` dict ref for top-level items. `None`
    /// only if a future caller constructs a node outside the tree.
    pub parent: Option<ObjectRef>,
    /// Resolved destination (set in a later task); `None` until then.
    pub dest: Option<Dest>,
    /// Child nodes in `/First`->`/Next` order.
    pub children: Vec<OutlineNode>,
}

/// A resolved explicit destination, e.g. `[pageRef /Fit ...]`. Mirrors the
/// array form qpdf `getDest` yields after resolving `/Dest`, `/A /GoTo /D`, and
/// named destinations.
#[derive(Debug, Clone, PartialEq)]
pub struct Dest {
    /// The explicit destination array. Element 0 is normally the page ref.
    pub array: Vec<Object>,
}

impl Dest {
    /// The destination page ref (array element 0), if it is an indirect ref.
    /// Mirrors qpdf `getDestPage`.
    pub fn page(&self) -> Option<ObjectRef> {
        self.array.first().and_then(Object::as_ref_id)
    }
}

/// High-level outline helper for a document. See module docs.
pub struct OutlineDocumentHelper<'a, R: Read + Seek> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> OutlineDocumentHelper<'a, R> {
    /// Wrap a document for outline access. Prefer [`Pdf::outline`].
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return `true` if the catalog has an `/Outlines` dictionary with at least
    /// one top-level item (a resolvable `/First`). Mirrors qpdf `hasOutlines`.
    pub fn has_outlines(&mut self) -> Result<bool> {
        Ok(self.outline_root_first()?.is_some())
    }

    /// Resolve the catalog `/Outlines` dict's `/First` child ref, if any.
    fn outline_root_first(&mut self) -> Result<Option<ObjectRef>> {
        Ok(self.outline_root_ref_and_first()?.map(|(_, first)| first))
    }

    /// Resolve the catalog `/Outlines` dict ref together with its `/First` child
    /// ref. Returns `None` unless both the `/Outlines` dict and a resolvable
    /// `/First` exist. The dict ref becomes the `parent` of top-level items.
    fn outline_root_ref_and_first(&mut self) -> Result<Option<(ObjectRef, ObjectRef)>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve_borrowed(catalog_ref)? else {
            return Ok(None);
        };
        let Some(outlines_ref) = catalog.get_ref("Outlines") else {
            return Ok(None);
        };
        let Object::Dictionary(root) = self.pdf.resolve_borrowed(outlines_ref)? else {
            return Ok(None);
        };
        Ok(root.get_ref("First").map(|first| (outlines_ref, first)))
    }

    /// Materialize and return the top-level outline nodes (qpdf
    /// `getTopLevelOutlines`). "root" is this top-level vector; the `/Outlines`
    /// dict itself is not a navigable item and is not wrapped.
    pub fn get_root(&mut self) -> Result<Vec<OutlineNode>> {
        self.get_root_with_max_depth(DEFAULT_MAX_OUTLINE_DEPTH)
    }

    /// Like [`get_root`](Self::get_root) with a caller-supplied recursion limit.
    /// Returns [`crate::Error::Unsupported`] if the limit is exceeded.
    pub fn get_root_with_max_depth(&mut self, max_depth: usize) -> Result<Vec<OutlineNode>> {
        let Some((root_ref, first)) = self.outline_root_ref_and_first()? else {
            return Ok(Vec::new());
        };
        let mut visited = BTreeSet::new();
        self.build_siblings(first, 0, Some(root_ref), &mut visited, max_depth)
    }

    /// Build a `/First`->`/Next` sibling chain into owned nodes.
    fn build_siblings(
        &mut self,
        start: ObjectRef,
        depth: usize,
        parent: Option<ObjectRef>,
        visited: &mut BTreeSet<ObjectRef>,
        max_depth: usize,
    ) -> Result<Vec<OutlineNode>> {
        if depth >= max_depth {
            return Err(crate::Error::Unsupported(format!(
                "outline depth exceeds maximum of {max_depth} at {start}"
            )));
        }
        let mut nodes = Vec::new();
        let mut current = Some(start);
        while let Some(current_ref) = current {
            if !visited.insert(current_ref) {
                break; // cycle - stop this chain
            }
            let Object::Dictionary(dict) = self.pdf.resolve_borrowed(current_ref)? else {
                break;
            };
            // IMPORTANT (borrow order): `dict` borrows `self.pdf` (it is a
            // `resolve_borrowed` reference). Extract EVERY value we need into
            // owned locals here, ending the `dict` borrow, BEFORE any
            // `self.pdf.resolve(...)` call below - otherwise the borrow checker
            // rejects it. A later task adds `dest_src`/`action_src` here.
            let title = read_title(dict.get("Title"));
            let first = dict.get_ref("First");
            let next = dict.get_ref("Next");
            let count_src = dict.get("Count").cloned();
            // `dict` (and thus the &mut self.pdf borrow) is no longer used past
            // this point - owned values only from here on.
            let count = resolve_int(self.pdf, count_src)?.unwrap_or(0);

            let children = match first {
                Some(first) => {
                    self.build_siblings(first, depth + 1, Some(current_ref), visited, max_depth)?
                }
                None => Vec::new(),
            };

            nodes.push(OutlineNode {
                object_ref: current_ref,
                depth,
                title,
                count,
                parent,
                dest: None,
                children,
            });
            current = next;
        }
        Ok(nodes)
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level outline helper for this document.
    pub fn outline(&mut self) -> OutlineDocumentHelper<'_, R> {
        OutlineDocumentHelper::new(self)
    }
}

/// `/Title` decode: qpdf yields an empty string when absent. Only a direct
/// `Object::String` is decoded here; absent/other yields an empty string.
fn read_title(value: Option<&Object>) -> String {
    match value {
        Some(Object::String(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        Some(_) | None => String::new(),
    }
}

/// Resolve one level of indirection and read an integer (review rule 2/3).
fn resolve_int<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<Option<i64>> {
    match value {
        Some(Object::Reference(r)) => Ok(pdf.resolve(r)?.as_integer()),
        Some(other) => Ok(other.as_integer()),
        None => Ok(None),
    }
}
