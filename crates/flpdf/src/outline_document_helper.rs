//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and exposes a cycle-safe,
//! iterable handle over the document outline (bookmark) tree, mirroring qpdf's
//! `QPDFOutlineDocumentHelper`. It materializes the tree into owned
//! [`OutlineNode`]s; navigation (`children`, `parent`, `count`, `dest`) lives on
//! each node, mirroring `QPDFOutlineObjectHelper`.

use crate::name_number_tree::read_name_tree;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Default recursion limit for outline materialization. Matches
/// [`crate::outline::DEFAULT_MAX_OUTLINE_DEPTH`]. True unbounded/iterative deep
/// walking (1000+ levels, cycle diagnostics) is tracked by flpdf-9hc.14.7.
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = crate::outline::DEFAULT_MAX_OUTLINE_DEPTH;

/// Indirection/`/D` nesting bound when resolving a destination. Mirrors the
/// constant in `outline_dest_remap`. Only exists to make malformed/cyclic
/// `/D` structures terminate instead of overflowing the stack.
const MAX_DEST_RESOLVE_DEPTH: usize = 64;

/// One materialized node of the outline tree (a bookmark).
///
/// Mirrors qpdf's `QPDFOutlineObjectHelper`. `children` are the resolved
/// `/First`->`/Next` chain; `parent` is the owning item's ref (`None` for
/// top-level items). `count` is the raw `/Count` value
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
    /// Parent item ref; `None` for top-level items.
    pub parent: Option<ObjectRef>,
    /// Resolved explicit destination (`/Dest`, or a `/A` GoTo action's `/D`),
    /// or `None` when absent or a still-unresolved named destination.
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
        Ok(root.get_ref("First"))
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
        let Some(first) = self.outline_root_first()? else {
            return Ok(Vec::new());
        };
        let mut visited = BTreeSet::new();
        self.build_siblings(first, 0, None, &mut visited, max_depth)
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
            let dest_src = dict.get("Dest").cloned();
            let action_src = dict.get("A").cloned();
            // `dict` (and thus the &mut self.pdf borrow) is no longer used past
            // this point - owned values only from here on.
            let count = resolve_int(self.pdf, count_src)?.unwrap_or(0);
            let dest = self.resolve_node_dest(dest_src, action_src)?;

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
                dest,
                children,
            });
            current = next;
        }
        Ok(nodes)
    }

    /// Resolve a node's destination from `/Dest`, else a `/A` GoTo action's `/D`.
    /// Named/string destinations are resolved in a later task (return `None` here).
    fn resolve_node_dest(
        &mut self,
        dest: Option<Object>,
        action: Option<Object>,
    ) -> Result<Option<Dest>> {
        if let Some(d) = dest {
            if let Some(found) = self.dest_from_value(&d, MAX_DEST_RESOLVE_DEPTH)? {
                return Ok(Some(found));
            }
        }
        if let Some(a) = action {
            let action_obj = match a {
                Object::Reference(r) => self.pdf.resolve(r)?,
                other => other,
            };
            if let Some(adict) = action_obj.as_dict() {
                let is_goto = matches!(adict.get("S"), Some(Object::Name(n)) if n == b"GoTo");
                if is_goto {
                    if let Some(d) = adict.get("D").cloned() {
                        if let Some(found) = self.dest_from_value(&d, MAX_DEST_RESOLVE_DEPTH)? {
                            return Ok(Some(found));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Resolve a destination value (array / indirect / dict `/D`) to a [`Dest`].
    /// Named (`Name`/`String`) destinations are resolved against the catalog name
    /// tree / legacy `/Dests` dict via [`Self::resolve_named_dest`].
    fn dest_from_value(&mut self, value: &Object, depth: usize) -> Result<Option<Dest>> {
        if depth == 0 {
            return Ok(None);
        }
        match value {
            Object::Array(arr) => Ok(Some(Dest { array: arr.clone() })),
            Object::Reference(r) => {
                let concrete = self.pdf.resolve(*r)?;
                self.dest_from_value(&concrete, depth - 1)
            }
            Object::Dictionary(d) => match d.get("D").cloned() {
                Some(inner) => self.dest_from_value(&inner, depth - 1),
                None => Ok(None),
            },
            Object::Name(name) => self.resolve_named_dest(name.clone()),
            Object::String(name) => self.resolve_named_dest(name.clone()),
            _ => Ok(None),
        }
    }

    /// Resolve a named destination `name` to an explicit [`Dest`].
    ///
    /// Tries the modern catalog `/Names`->`/Dests` name tree first (PDF 1.2),
    /// then the legacy catalog `/Dests` dictionary (PDF 1.1). A name-tree or
    /// `/Dests` value may be the dest array directly or a `<< /D array >>` dict.
    fn resolve_named_dest(&mut self, name: Vec<u8>) -> Result<Option<Dest>> {
        // 1. Modern: catalog /Names /Dests name tree.
        if let Some(names_ref) = self.catalog_ref("Names")? {
            if let Object::Dictionary(names) = self.pdf.resolve(names_ref)? {
                if let Some(dests_root) = names.get("Dests").cloned() {
                    let entries = read_name_tree(
                        self.pdf,
                        dests_root,
                        |_pdf, value| Ok(Some(value)),
                        DEFAULT_MAX_OUTLINE_DEPTH,
                    )?;
                    for (key, value) in entries {
                        if key == name {
                            return self.dest_from_value(&value, MAX_DEST_RESOLVE_DEPTH);
                        }
                    }
                }
            }
        }
        // 2. Legacy: catalog /Dests dict.
        if let Some(dests_ref) = self.catalog_ref("Dests")? {
            if let Object::Dictionary(dests) = self.pdf.resolve(dests_ref)? {
                if let Some(value) = dests.get(&name).cloned() {
                    return self.dest_from_value(&value, MAX_DEST_RESOLVE_DEPTH);
                }
            }
        }
        Ok(None)
    }

    /// Resolve a catalog key's value to an object ref (only if it is a reference).
    fn catalog_ref(&mut self, key: &str) -> Result<Option<ObjectRef>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve_borrowed(catalog_ref)? else {
            return Ok(None);
        };
        Ok(catalog.get_ref(key))
    }

    /// Pre-order iterator over every materialized node (owned). Each yielded
    /// node has its `children` cleared — the flattened view is linear and
    /// `depth` conveys structure; use [`get_root`](Self::get_root) or
    /// [`walk`](Self::walk) when you need populated `children`.
    pub fn iter(&mut self) -> Result<impl Iterator<Item = OutlineNode>> {
        let roots = self.get_root()?;
        let mut flat = Vec::new();
        for node in &roots {
            flatten_preorder(node, &mut flat);
        }
        Ok(flat.into_iter())
    }

    /// Visit every node pre-order, passing `(node, depth)` to `visitor`. The
    /// visited nodes have populated `children`. Mirrors a qpdf outline walk.
    pub fn walk<F: FnMut(&OutlineNode, usize)>(&mut self, mut visitor: F) -> Result<()> {
        let roots = self.get_root()?;
        for node in &roots {
            walk_node(node, &mut visitor);
        }
        Ok(())
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

/// Push `node` (with `children` cleared) then its descendants, pre-order.
fn flatten_preorder(node: &OutlineNode, out: &mut Vec<OutlineNode>) {
    out.push(OutlineNode {
        object_ref: node.object_ref,
        depth: node.depth,
        title: node.title.clone(),
        count: node.count,
        parent: node.parent,
        dest: node.dest.clone(),
        children: Vec::new(),
    });
    for child in &node.children {
        flatten_preorder(child, out);
    }
}

/// Invoke `visitor(node, node.depth)` then recurse into children, pre-order.
fn walk_node<F: FnMut(&OutlineNode, usize)>(node: &OutlineNode, visitor: &mut F) {
    visitor(node, node.depth);
    for child in &node.children {
        walk_node(child, visitor);
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
